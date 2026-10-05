//! The engine: shared state and thread lifecycle.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{anyhow, Context as _};

use odex_config::{ConfigStack, OdexHome, Settings};
use odex_context::{ContextState, Pinned, PlanItem, PromptInputs, SystemParts};
use odex_llm::{ModelHandle, ModelRegistry};
use odex_protocol::*;

use crate::events::{Emitter, EventSink, NullSink};
use crate::prompt;
use crate::rollout::{self, RolloutLine, RolloutWriter};
use crate::sessions::Sessions;
use crate::store::Store;
use crate::thread::{new_id, now_ms, ThreadRt};

#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub home: OdexHome,
    pub profile: Option<String>,
}

/// Error with a JSON-RPC code.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct EngineError {
    pub code: i64,
    pub message: String,
}

impl EngineError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
    pub fn not_found(what: &str) -> Self {
        Self::new(jsonrpc::error_codes::THREAD_NOT_FOUND, format!("{what} not found"))
    }
}

impl From<anyhow::Error> for EngineError {
    fn from(e: anyhow::Error) -> Self {
        if let Some(ee) = e.downcast_ref::<EngineError>() {
            return EngineError::new(ee.code, ee.message.clone());
        }
        EngineError::new(jsonrpc::error_codes::INTERNAL_ERROR, format!("{e:#}"))
    }
}

pub type EResult<T> = Result<T, EngineError>;

pub struct EngineInner {
    pub home: OdexHome,
    pub config: RwLock<ConfigStack>,
    pub registry: Arc<ModelRegistry>,
    pub store: Store,
    pub threads: Mutex<HashMap<String, Arc<ThreadRt>>>,
    emitter: RwLock<Emitter>,
    pub policy: RwLock<odex_execpolicy::Policy>,
    pub sessions: Sessions,
    pub secrets: RwLock<BTreeMap<String, String>>,
    pub kill_switch: AtomicBool,
    pub subagents: crate::subagents::SubagentHub,
    pub file_indexes: Mutex<HashMap<String, odex_file_search::FileIndex>>,
    pub ext: crate::extensions::Extensions,
    pub mcp_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<odex_mcp_client::McpEvent>>>,
}

#[derive(Clone)]
pub struct Engine(pub Arc<EngineInner>);

impl std::ops::Deref for Engine {
    type Target = EngineInner;
    fn deref(&self) -> &EngineInner {
        &self.0
    }
}

impl Engine {
    pub fn new(opts: EngineOptions) -> anyhow::Result<Self> {
        let home = opts.home;
        home.ensure()?;
        let stack = ConfigStack::load(&home, opts.profile.as_deref())?;
        let settings = stack.resolve(None);
        let registry = Arc::new(ModelRegistry::new(settings, stack.presets.clone(), Some(home.models_cache_path())));
        let store = Store::open(&home.db_path()).context("opening the SQLite index")?;
        let (policy, warnings) = odex_execpolicy::Policy::load(&[home.rules_dir()]);
        for w in warnings {
            tracing::warn!("exec policy: {w}");
        }
        let (ext, mcp_rx) = crate::extensions::Extensions::new(home.mcp_tokens_path());
        Ok(Engine(Arc::new(EngineInner {
            ext,
            mcp_rx: Mutex::new(Some(mcp_rx)),
            home,
            config: RwLock::new(stack),
            registry,
            store,
            threads: Mutex::new(HashMap::new()),
            emitter: RwLock::new(Emitter::new(Arc::new(NullSink))),
            policy: RwLock::new(policy),
            sessions: Sessions::default(),
            secrets: RwLock::new(BTreeMap::new()),
            kill_switch: AtomicBool::new(false),
            subagents: Default::default(),
            file_indexes: Mutex::new(HashMap::new()),
        })))
    }

    pub fn set_sink(&self, sink: Arc<dyn EventSink>) {
        *self.emitter.write().unwrap() = Emitter::new(sink);
    }

    pub fn emitter(&self) -> Emitter {
        self.emitter.read().unwrap().clone()
    }

    pub fn set_secrets(&self, secrets: BTreeMap<String, String>) {
        self.registry.set_secrets(secrets.clone());
        *self.secrets.write().unwrap() = secrets;
    }

    /// Reload config from disk (after edits) and push it to the registry.
    pub fn reload_config(&self) -> anyhow::Result<()> {
        // A profile picked on the command line sticks; one picked by the
        // file's `profile` key follows the file (so switching it works).
        let profile = {
            let c = self.config.read().unwrap();
            if c.active_profile != c.user.profile {
                c.active_profile.clone()
            } else {
                None
            }
        };
        let stack = ConfigStack::load(&self.home, profile.as_deref())?;
        self.registry.update_settings(stack.resolve(None));
        *self.config.write().unwrap() = stack;
        Ok(())
    }

    /// Effective settings for a folder (adds the trusted project layer).
    pub fn settings_for(&self, root: Option<&Path>) -> Settings {
        self.config.read().unwrap().resolve(root)
    }

    pub fn user_settings(&self) -> Settings {
        self.config.read().unwrap().resolve(None)
    }

    pub fn is_trusted(&self, p: &Path) -> bool {
        self.config.read().unwrap().is_trusted(p)
    }

    /// Evaluate a command against the exec policy for a thread: the user's
    /// `~/.odex/rules/` plus `.odex/rules/` of the thread's (trusted) project
    /// or worktree. Project rules are read fresh so edits apply immediately.
    pub fn exec_policy_eval(
        &self,
        t: &Thread,
        command: &str,
        shell: odex_execpolicy::ShellKind,
    ) -> odex_execpolicy::Evaluation {
        let dirs = self.project_rule_dirs(t);
        if dirs.is_empty() {
            return self.policy.read().unwrap().evaluate(command, shell);
        }
        let mut all = vec![self.home.rules_dir()];
        all.extend(dirs);
        let (policy, warnings) = odex_execpolicy::Policy::load(&all);
        for w in warnings {
            tracing::warn!("exec policy: {w}");
        }
        policy.evaluate(command, shell)
    }

    fn project_rule_dirs(&self, t: &Thread) -> Vec<PathBuf> {
        // (folder holding .odex/rules, folder whose trust governs it)
        let mut cands: Vec<(PathBuf, PathBuf)> = Vec::new();
        if let Some(wt) = &t.worktree {
            cands.push((PathBuf::from(&wt.path), PathBuf::from(&wt.repo_root)));
        }
        cands.push((PathBuf::from(&t.cwd), PathBuf::from(&t.cwd)));
        if let Some(pid) = &t.project_id {
            if let Ok(p) = self.project(pid) {
                let root = PathBuf::from(p.primary_folder());
                cands.push((root.clone(), root));
            }
        }
        let mut out: Vec<PathBuf> = Vec::new();
        for (dir, trust_root) in cands {
            let rules = dir.join(".odex").join("rules");
            if rules.is_dir() && !out.contains(&rules) && (self.is_trusted(&trust_root) || self.is_trusted(&dir)) {
                out.push(rules);
            }
        }
        out
    }

    // ------------------------------------------------------------- threads

    pub fn loaded(&self, id: &str) -> Option<Arc<ThreadRt>> {
        self.threads.lock().unwrap().get(id).cloned()
    }

    /// Get a thread, loading it from its rollout if needed.
    pub fn thread(&self, id: &str) -> EResult<Arc<ThreadRt>> {
        if let Some(t) = self.loaded(id) {
            return Ok(t);
        }
        let (meta, rollout_path) = match self.store.get_thread(id).map_err(EngineError::from)? {
            Some((t, Some(p))) => (Some(t), PathBuf::from(p)),
            Some((t, None)) => (
                Some(t),
                rollout::find(&self.home.sessions_dir(), id).ok_or_else(|| EngineError::not_found("rollout"))?,
            ),
            None => {
                (None, rollout::find(&self.home.sessions_dir(), id).ok_or_else(|| EngineError::not_found("thread"))?)
            }
        };
        let replay = rollout::read(&rollout_path).map_err(|e| EngineError::from(anyhow!(e)))?;
        let mut thread = replay.thread.or(meta).ok_or_else(|| EngineError::not_found("thread metadata"))?;
        thread.status = ThreadStatus::Idle;
        let writer = RolloutWriter::open(&rollout_path).map_err(|e| EngineError::from(anyhow!(e)))?;
        let rt = Arc::new(ThreadRt::new(
            thread.clone(),
            writer,
            self.home.outputs_dir().join(&thread.id),
            replay.context,
            replay.turns,
        ));
        *rt.plan.lock().unwrap() = replay.plan;
        *rt.original_task.lock().unwrap() = replay.original_task;
        let _ = self.store.upsert_thread(&thread, Some(&rollout_path.to_string_lossy()));
        self.threads.lock().unwrap().insert(thread.id.clone(), rt.clone());
        Ok(rt)
    }

    pub fn create_thread(&self, p: ThreadStartParams) -> EResult<Arc<ThreadRt>> {
        let project = match &p.project_id {
            Some(pid) => Some(self.project(pid)?),
            None => None,
        };
        let cwd =
            p.cwd.clone().or_else(|| project.as_ref().map(|pr| pr.primary_folder().to_string())).unwrap_or_else(|| {
                dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).to_string_lossy().to_string()
            });
        let settings = self.settings_for(Some(Path::new(&cwd)));
        let id = new_id("th");
        let now = now_ms();
        let kind = p.kind.unwrap_or_default();
        let thread = Thread {
            id: id.clone(),
            name: p.name.clone(),
            kind,
            project_id: p.project_id.clone(),
            cwd: cwd.clone(),
            run_mode: RunMode::Local,
            worktree: None,
            branch: None,
            model: p.model.clone(),
            effort: p.effort.or(settings.reasoning_effort),
            permission_mode: p.permission_mode.unwrap_or(settings.permission_mode),
            created_at: now,
            updated_at: now,
            archived: false,
            pinned: false,
            unread: false,
            status: ThreadStatus::Idle,
            preview: String::new(),
            parent_thread_id: p.parent_thread_id.clone(),
            goal: None,
            memories_enabled: settings.memories_enabled,
            ephemeral: p.ephemeral.unwrap_or(matches!(kind, ThreadKind::Side)),
            diff_stats: None,
            usage: TokenUsage::default(),
            last_error: None,
        };
        let writer =
            RolloutWriter::create(&self.home.sessions_dir(), &id).map_err(|e| EngineError::from(anyhow!(e)))?;
        let rollout_path = writer.path().to_string_lossy().to_string();
        let rt = Arc::new(ThreadRt::new(
            thread.clone(),
            writer,
            self.home.outputs_dir().join(&id),
            ContextState::new(),
            vec![],
        ));
        rt.log(RolloutLine::Meta { thread: thread.clone() });
        self.store.upsert_thread(&thread, Some(&rollout_path)).map_err(EngineError::from)?;
        self.threads.lock().unwrap().insert(id, rt.clone());
        Ok(rt)
    }

    /// Persist and broadcast a thread's metadata.
    pub fn publish(&self, rt: &ThreadRt) -> Thread {
        let t = rt.thread();
        let _ = self.store.upsert_thread(&t, None);
        self.emitter().thread_updated(&t);
        t
    }

    pub fn update_thread<F: FnOnce(&mut Thread)>(&self, rt: &ThreadRt, f: F) -> Thread {
        let t = rt.update(f);
        let _ = self.store.upsert_thread(&t, None);
        self.emitter().thread_updated(&t);
        t
    }

    pub fn set_status(&self, rt: &ThreadRt, status: ThreadStatus) {
        if rt.thread().status != status {
            self.update_thread(rt, |t| t.status = status);
        }
    }

    pub fn project(&self, id: &str) -> EResult<Project> {
        self.store
            .projects()
            .map_err(EngineError::from)?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| EngineError::new(jsonrpc::error_codes::INVALID_PARAMS, format!("project {id} not found")))
    }

    /// Project root used for config layering and AGENTS.md discovery.
    pub fn thread_root(&self, t: &Thread) -> PathBuf {
        if let Some(wt) = &t.worktree {
            return PathBuf::from(&wt.path);
        }
        if let Some(pid) = &t.project_id {
            if let Ok(p) = self.project(pid) {
                // the project folder containing cwd, else the primary
                let cwd = PathBuf::from(&t.cwd);
                if let Some(f) = p.folders.iter().find(|f| cwd.starts_with(f)) {
                    return PathBuf::from(f);
                }
                return PathBuf::from(p.primary_folder());
            }
        }
        PathBuf::from(&t.cwd)
    }

    pub fn thread_settings(&self, t: &Thread) -> Settings {
        let root = self.thread_root(t);
        self.settings_for(Some(&root))
    }

    /// Folders the sandbox lets the agent write.
    pub fn writable_roots(&self, t: &Thread, s: &Settings) -> Vec<PathBuf> {
        let mut roots = vec![PathBuf::from(&t.cwd)];
        if let Some(wt) = &t.worktree {
            roots.push(PathBuf::from(&wt.path));
        }
        if let Some(pid) = &t.project_id {
            if let Ok(p) = self.project(pid) {
                roots.extend(p.folders.iter().map(PathBuf::from));
            }
        }
        roots.extend(s.sandbox.writable_roots.iter().cloned());
        roots.push(self.sandbox_temp());
        let mut out: Vec<PathBuf> = Vec::new();
        for r in roots {
            if !out.contains(&r) {
                out.push(r);
            }
        }
        out
    }

    pub fn sandbox_temp(&self) -> PathBuf {
        let p = self.home.tmp_dir().join("sandbox");
        let _ = std::fs::create_dir_all(&p);
        p
    }

    /// Main model for a thread (thread override → role → first discovered).
    pub fn main_model(&self, t: &Thread) -> EResult<ModelHandle> {
        self.registry.resolve_role(ModelRole::Main, t.model.as_deref()).ok_or_else(|| {
            EngineError::new(
                jsonrpc::error_codes::MODEL_UNAVAILABLE,
                "No model is available. Add a vLLM endpoint in Settings → Models & Endpoints (or set [model_providers] in ~/.odex/config.toml) and make sure it is running.",
            )
        })
    }

    pub fn role_model(&self, role: ModelRole, t: Option<&Thread>) -> Option<ModelHandle> {
        if role == ModelRole::Compactor {
            let s = t.map(|t| self.thread_settings(t)).unwrap_or_else(|| self.user_settings());
            if let Some(k) = &s.context.compactor_model {
                if let Some(h) = self.registry.resolve(k) {
                    return Some(h);
                }
            }
        }
        self.registry.resolve_role(role, t.and_then(|t| t.model.as_deref()))
    }

    // -------------------------------------------------------------- prompt

    pub fn system_parts(&self, t: &Thread, s: &Settings, mode: TurnMode) -> SystemParts {
        let root = self.thread_root(t);
        let cwd = PathBuf::from(&t.cwd);
        let window = self.main_model(t).map(|h| h.context_window).unwrap_or(32768);
        let small = window <= odex_tools::specs::SMALL_WINDOW;
        // AGENTS.md never takes more than ~12% of the window
        let doc_cap = s.project_doc_max_bytes.min((window as usize * 3 * 12) / 100);
        let agents_md = if self.is_trusted(&root) || root == cwd {
            prompt::discover_agents_md(&self.home.global_agents_md(), Some(&root), &cwd, doc_cap)
        } else {
            prompt::discover_agents_md(&self.home.global_agents_md(), None, &self.home.root().join("__none__"), doc_cap)
        };
        let mut extra = prompt::environment_context(&prompt::EnvInfo {
            cwd: cwd.clone(),
            shell: s.default_shell.clone(),
            os: prompt::os_name(),
            extra_roots: self
                .writable_roots(t, s)
                .into_iter()
                .filter(|r| *r != cwd && !r.starts_with(self.home.root()))
                .collect(),
            permission_mode: t.permission_mode.as_str().into(),
            network: s.sandbox.network_access || t.permission_mode == PermissionMode::FullAccess,
            git_branch: t.branch.clone(),
        });
        if let Some(ci) = &s.custom_instructions {
            extra.push_str("\n# User instructions\n");
            extra.push_str(ci.trim());
            extra.push('\n');
        }
        let skills = if small { vec![] } else { crate::skills::list(self, Some(&root), s) };
        if !skills.is_empty() {
            extra.push_str("\n# Skills\nSkills are reusable instructions. When a task matches one, read its SKILL.md with read_file before starting.\n");
            for sk in skills.iter().filter(|s| s.enabled) {
                extra.push_str(&format!("- {}: {} ({})\n", sk.name, sk.description, sk.path));
            }
        }
        match mode {
            TurnMode::Plan => {
                extra.push('\n');
                extra.push_str(prompt::PLAN_MODE);
                extra.push('\n');
            }
            TurnMode::Review => {
                extra.push('\n');
                extra.push_str(prompt::REVIEW_PROMPT);
                extra.push('\n');
            }
            TurnMode::Default => {}
        }
        let base = if small { prompt::BASE_PROMPT_COMPACT } else { prompt::BASE_PROMPT };
        SystemParts { base: base.to_string(), extra, agents_md, memories: crate::skills::memories_block(self, t, s) }
    }

    pub fn pinned(&self, rt: &ThreadRt, s: &Settings) -> Pinned {
        let t = rt.thread();
        let plan = rt
            .plan
            .lock()
            .unwrap()
            .1
            .iter()
            .map(|p| PlanItem { step: p.step.clone(), status: plan_status(p.status).into() })
            .collect();
        let notes_path = PathBuf::from(&t.cwd).join(".odex").join("NOTES.md");
        let notes = std::fs::read_to_string(notes_path).ok().map(|n| {
            let max = s.context.notes_max_bytes as usize;
            if n.len() > max {
                let cut: String = n.chars().take(max).collect();
                format!("{cut}\n[… NOTES.md truncated …]")
            } else {
                n
            }
        });
        let goal = t.goal.as_ref().filter(|g| g.status == "active").map(|g| prompt::goal_addendum(&g.objective));
        Pinned { original_task: rt.original_task.lock().unwrap().clone(), goal, plan, notes, working_set: None }
    }

    pub fn context_status(&self, rt: &ThreadRt, ctx: &ContextState) -> ContextStatus {
        let t = rt.thread();
        let s = self.thread_settings(&t);
        let handle = self.main_model(&t).ok();
        let (window, max_out, key, est, reasoning_history, profile) = match &handle {
            Some(h) => (
                h.context_window,
                h.model.max_output_tokens,
                Some(h.model.key.clone()),
                self.registry.estimator(&h.model.key),
                h.model.reasoning_history,
                h.model.tool_profile,
            ),
            None => (
                odex_llm::registry::FALLBACK_CONTEXT_WINDOW,
                8192,
                None,
                Default::default(),
                ReasoningHistory::CurrentTurn,
                ToolProfile::Extended,
            ),
        };
        let budget = odex_context::Budget::new(window, max_out, &s.context);
        let system = self.system_parts(&t, &s, TurnMode::Default);
        let tools = crate::toolexec::tool_specs(self, rt, &t, &s, handle.as_ref(), profile, TurnMode::Default);
        let pinned = self.pinned(rt, &s);
        let inputs = PromptInputs {
            system: &system,
            tools: &tools,
            pinned: &pinned,
            estimator: &est,
            reasoning_history,
            current_turn: rt.turn_counter.load(std::sync::atomic::Ordering::SeqCst),
        };
        let breakdown = ctx.breakdown(&inputs);
        let used = ctx.last_exact.unwrap_or_else(|| breakdown.total());
        ContextStatus {
            model: key,
            window,
            budget: budget.budget(),
            used,
            exact: ctx.last_exact.is_some(),
            percent: used as f64 / window.max(1) as f64,
            prune_at: s.context.prune_at,
            compact_at: s.context.compact_at,
            breakdown,
            compactions: ctx.compactions.clone(),
            prunes: ctx.prunes,
            lazy_tools: false,
        }
    }

    pub async fn emit_context(&self, rt: &ThreadRt) {
        let ctx = rt.ctx.lock().await;
        let st = self.context_status(rt, &ctx);
        drop(ctx);
        self.emitter().context_updated(&rt.id, st);
    }

    pub fn read_response(&self, rt: &ThreadRt, ctx: Option<ContextStatus>) -> ThreadReadResponse {
        let (explanation, plan) = rt.plan.lock().unwrap().clone();
        let _ = explanation;
        ThreadReadResponse {
            thread: rt.thread(),
            turns: rt.turns.lock().unwrap().clone(),
            pending_approvals: rt.pending_approvals.lock().unwrap().values().map(|p| p.params.clone()).collect(),
            context: ctx,
            plan,
            sources: rt.sources.lock().unwrap().values().cloned().collect(),
            followups: rt.followups.lock().unwrap().clone(),
            queued: rt.queue.lock().unwrap().clone(),
        }
    }

    pub fn unload_idle(&self) {
        let mut threads = self.threads.lock().unwrap();
        threads.retain(|_, rt| rt.is_running() || Arc::strong_count(rt) > 1);
    }
}

pub fn plan_status(s: PlanStepStatus) -> &'static str {
    match s {
        PlanStepStatus::Pending => "pending",
        PlanStepStatus::InProgress => "in_progress",
        PlanStepStatus::Completed => "completed",
    }
}

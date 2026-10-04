//! The method registry: every wire method with its params/result types.
//! TypeScript method maps are generated from these tables.

use crate::ext::*;
use crate::methods::*;
use crate::models::*;
use crate::notifications::*;
use crate::server_requests::*;
use crate::workspace::*;

/// Describes one method for codegen.
pub struct MethodDesc {
    pub method: &'static str,
    pub params: fn() -> String,
    pub result: Option<fn() -> String>,
    pub export: fn(&std::path::Path) -> Result<(), ts_rs::ExportError>,
}

macro_rules! requests {
    ($table:ident, $consts:ident { $( $konst:ident = $method:literal : $params:ty => $result:ty ),* $(,)? }) => {
        pub mod $consts {
            $( pub const $konst: &str = $method; )*
        }
        pub static $table: &[MethodDesc] = &[
            $( MethodDesc {
                method: $method,
                params: <$params as ts_rs::TS>::name,
                result: Some(<$result as ts_rs::TS>::name),
                export: |dir| {
                    <$params as ts_rs::TS>::export_all_to(dir)?;
                    <$result as ts_rs::TS>::export_all_to(dir)
                },
            }, )*
        ];
    };
}

macro_rules! notifications {
    ($table:ident, $consts:ident { $( $konst:ident = $method:literal : $params:ty ),* $(,)? }) => {
        pub mod $consts {
            $( pub const $konst: &str = $method; )*
        }
        pub static $table: &[MethodDesc] = &[
            $( MethodDesc {
                method: $method,
                params: <$params as ts_rs::TS>::name,
                result: None,
                export: |dir| <$params as ts_rs::TS>::export_all_to(dir),
            }, )*
        ];
    };
}

requests!(CLIENT_REQUESTS, method {
    INITIALIZE = "initialize": InitializeParams => InitializeResponse,

    THREAD_START = "thread/start": ThreadStartParams => ThreadResponse,
    THREAD_RESUME = "thread/resume": ThreadIdParams => ThreadReadResponse,
    THREAD_READ = "thread/read": ThreadIdParams => ThreadReadResponse,
    THREAD_FORK = "thread/fork": ThreadForkParams => ThreadResponse,
    THREAD_LIST = "thread/list": ThreadListParams => ThreadListResponse,
    THREAD_SEARCH = "thread/search": ThreadSearchParams => ThreadSearchResponse,
    THREAD_ARCHIVE = "thread/archive": ThreadArchiveParams => EmptyResponse,
    THREAD_UNARCHIVE = "thread/unarchive": ThreadIdParams => ThreadResponse,
    THREAD_DELETE = "thread/delete": ThreadIdParams => EmptyResponse,
    THREAD_ROLLBACK = "thread/rollback": ThreadRollbackParams => ThreadReadResponse,
    THREAD_REVERT = "thread/revert": ThreadRollbackParams => ThreadReadResponse,
    THREAD_QUEUE_SET = "thread/queue/set": QueueSetParams => EmptyResponse,
    THREAD_SHELL_COMMAND = "thread/shellCommand": ShellCommandParams => EmptyResponse,
    THREAD_UPDATE = "thread/update": ThreadUpdateParams => ThreadResponse,
    THREAD_COMPACT = "thread/compact": ThreadCompactParams => EmptyResponse,
    THREAD_CONTEXT = "thread/context": ThreadIdParams => ContextGetResponse,
    THREAD_GOAL_SET = "thread/goal/set": GoalSetParams => ThreadResponse,
    THREAD_GOAL_CLEAR = "thread/goal/clear": ThreadIdParams => ThreadResponse,
    THREAD_APPROVE_OVERRIDE = "thread/approveOverride": ApproveOverrideParams => EmptyResponse,
    THREAD_PLAN_DECIDE = "thread/plan/decide": PlanDecisionParams => TurnStartResponse,
    THREAD_INIT_AGENTS_MD = "thread/initAgentsMd": InitAgentsMdParams => TurnStartResponse,

    TURN_START = "turn/start": TurnStartParams => TurnStartResponse,
    TURN_STEER = "turn/steer": TurnSteerParams => EmptyResponse,
    TURN_INTERRUPT = "turn/interrupt": ThreadIdParams => EmptyResponse,
    REVIEW_START = "review/start": ReviewStartParams => TurnStartResponse,

    MODEL_LIST = "model/list": EmptyParams => ModelListResponse,
    PROVIDER_LIST = "provider/list": ProviderListParams => ProviderListResponse,
    PROVIDER_UPSERT = "provider/upsert": ProviderUpsertParams => ProviderListResponse,
    PROVIDER_REMOVE = "provider/remove": ProviderIdParams => ProviderListResponse,
    PROVIDER_TEST = "provider/test": ProviderTestParams => ProviderTestResult,
    DOCTOR_RUN = "doctor/run": DoctorRunParams => DoctorRunResponse,
    PRESET_LIST = "preset/list": EmptyParams => PresetListResponse,

    CONFIG_READ = "config/read": EmptyParams => ConfigReadResponse,
    CONFIG_WRITE = "config/write": ConfigWriteParams => ConfigReadResponse,

    PROJECT_LIST = "project/list": EmptyParams => ProjectListResponse,
    PROJECT_ADD = "project/add": ProjectAddParams => ProjectResponse,
    PROJECT_UPDATE = "project/update": ProjectUpdateParams => ProjectResponse,
    PROJECT_REMOVE = "project/remove": ProjectIdParams => EmptyResponse,
    TRUST_CHECK = "trust/check": PathParams => TrustCheckResponse,
    TRUST_SET = "trust/set": TrustParams => TrustCheckResponse,
    FS_SEARCH = "fs/search": FileSearchParams => FileSearchResponse,

    GIT_STATUS = "git/status": CwdParams => GitStatus,
    GIT_DIFF = "git/diff": GitDiffParams => GitDiffResponse,
    GIT_STAGE = "git/stage": GitPathOpParams => EmptyResponse,
    GIT_UNSTAGE = "git/unstage": GitPathOpParams => EmptyResponse,
    GIT_REVERT = "git/revert": GitPathOpParams => EmptyResponse,
    GIT_COMMIT = "git/commit": GitCommitParams => GitCommitResponse,
    GIT_COMMIT_MESSAGE = "git/commitMessage": CommitMessageParams => CommitMessageResponse,
    GIT_PUSH = "git/push": GitPushParams => CommandOutputResponse,
    GIT_BRANCHES = "git/branches": CwdParams => GitBranchesResponse,
    GIT_LOG = "git/log": GitLogParams => GitLogResponse,
    WORKTREE_HANDOFF = "worktree/handoff": HandoffParams => HandoffResult,
    WORKTREE_LIST = "worktree/list": EmptyParams => WorktreeListResponse,
    WORKTREE_REMOVE = "worktree/remove": ThreadIdParams => EmptyResponse,
    PR_CREATE = "pr/create": PrCreateParams => PrCreateResponse,
    PR_VIEW = "pr/view": PrViewParams => PrViewResponse,
    PR_COMMENT = "pr/comment": PrCommentParams => CommandOutputResponse,
    PR_DRAFT = "pr/draft": CommitMessageParams => PrDraftResponse,

    EXEC_SESSIONS = "exec/sessions": EmptyParams => ExecSessionsResponse,
    EXEC_KILL = "exec/kill": IdParams => EmptyResponse,

    MCP_LIST = "mcp/list": EmptyParams => McpListResponse,
    MCP_UPSERT = "mcp/upsert": McpUpsertParams => McpListResponse,
    MCP_REMOVE = "mcp/remove": NameParams => McpListResponse,
    MCP_RESTART = "mcp/restart": NameParams => McpListResponse,
    MCP_LOGS = "mcp/logs": NameParams => McpLogsResponse,
    MCP_LOGIN = "mcp/login": NameParams => McpLoginResponse,
    MCP_LOGOUT = "mcp/logout": NameParams => EmptyResponse,
    MCP_READ_RESOURCE = "mcp/readResource": McpReadResourceParams => McpReadResourceResponse,

    SKILLS_LIST = "skills/list": SkillsListParams => SkillsListResponse,
    SKILLS_READ = "skills/read": NameParams => SkillReadResponse,
    SKILLS_WRITE = "skills/write": SkillWriteParams => SkillReadResponse,
    SKILLS_DELETE = "skills/delete": NameParams => EmptyResponse,
    SKILLS_IMPORT = "skills/import": SkillImportParams => SkillsListResponse,
    SKILLS_SET_ENABLED = "skills/setEnabled": SetEnabledParams => SkillsListResponse,
    PLUGINS_LIST = "plugins/list": EmptyParams => PluginsListResponse,
    PLUGINS_INSTALL = "plugins/install": PluginInstallParams => PluginResponse,
    PLUGINS_TRUST = "plugins/trust": HookTrustParams => PluginResponse,
    PLUGINS_REMOVE = "plugins/remove": IdParams => EmptyResponse,
    PLUGINS_SET_ENABLED = "plugins/setEnabled": SetEnabledParams => PluginsListResponse,
    HOOKS_LIST = "hooks/list": HooksListParams => HooksListResponse,
    HOOKS_TRUST = "hooks/trust": HookTrustParams => HooksListResponse,

    AUTOMATION_LIST = "automation/list": EmptyParams => AutomationListResponse,
    AUTOMATION_UPSERT = "automation/upsert": AutomationUpsertParams => AutomationResponse,
    AUTOMATION_DELETE = "automation/delete": IdParams => EmptyResponse,
    AUTOMATION_RUN_NOW = "automation/runNow": IdParams => EmptyResponse,
    AUTOMATION_RUNS = "automation/runs": AutomationRunsParams => AutomationRunsResponse,
    AUTOMATION_RUNS_MARK_READ = "automation/runs/markRead": IdsParams => EmptyResponse,
    AUTOMATION_RUNS_ARCHIVE = "automation/runs/archive": IdsParams => EmptyResponse,
    AUTOMATION_VALIDATE_SCHEDULE = "automation/validateSchedule": ScheduleValidateParams => ScheduleValidateResponse,

    MEMORY_LIST = "memory/list": MemoryListParams => MemoryListResponse,
    MEMORY_UPSERT = "memory/upsert": MemoryUpsertParams => MemoryResponse,
    MEMORY_DELETE = "memory/delete": IdParams => EmptyResponse,
    MEMORY_PROPOSE = "memory/propose": ThreadIdParams => MemoryListResponse,

    USAGE_STATS = "usage/stats": UsageStatsParams => UsageStats,

    COMPUTER_USE_STATUS = "computerUse/status": EmptyParams => ComputerUseStatus,
    COMPUTER_USE_WINDOWS = "computerUse/windows": EmptyParams => WindowListResponse,
    COMPUTER_USE_KILL_SWITCH = "computerUse/killSwitch": KillSwitchParams => ComputerUseStatus,
    APPSHOT_CAPTURE = "appshot/capture": AppshotParams => AppshotResponse,
    SANDBOX_STATUS = "sandbox/status": EmptyParams => SandboxStatus,
});

notifications!(SERVER_NOTIFICATIONS, notification {
    THREAD_STARTED = "thread/started": ThreadNotification,
    THREAD_UPDATED = "thread/updated": ThreadNotification,
    THREAD_DELETED = "thread/deleted": ThreadIdNotification,
    TURN_STARTED = "turn/started": TurnNotification,
    TURN_COMPLETED = "turn/completed": TurnNotification,
    ITEM_STARTED = "item/started": ItemNotification,
    ITEM_DELTA = "item/delta": ItemDeltaNotification,
    ITEM_COMPLETED = "item/completed": ItemNotification,
    PLAN_UPDATED = "turn/plan/updated": PlanUpdatedNotification,
    DIFF_UPDATED = "turn/diff/updated": DiffUpdatedNotification,
    CONTEXT_UPDATED = "thread/context/updated": ContextUpdatedNotification,
    TOKEN_USAGE_UPDATED = "thread/tokenUsage/updated": TokenUsageNotification,
    FOLLOWUPS = "thread/followups": FollowupsNotification,
    SOURCES_UPDATED = "thread/sources/updated": SourcesNotification,
    QUEUE_UPDATED = "thread/queue/updated": QueueNotification,
    APPROVAL_RESOLVED = "approval/resolved": ApprovalResolvedNotification,
    MCP_STATUS_UPDATED = "mcp/status/updated": McpStatusNotification,
    AUTOMATION_RUN_UPDATED = "automation/run/updated": AutomationRunNotification,
    MEMORY_PROPOSED = "memory/proposed": MemoriesProposedNotification,
    HOOKS_REVIEW_REQUIRED = "hooks/reviewRequired": HooksReviewNotification,
    COMPUTER_USE_ACTIVE = "computerUse/active": ComputerUseActiveNotification,
    PROVIDERS_UPDATED = "providers/updated": ProvidersNotification,
    PROJECTS_CHANGED = "projects/changed": ProjectsChangedNotification,
    OPEN_URL = "openUrl": OpenUrlNotification,
    LOG = "log": LogNotification,
});

requests!(SERVER_REQUESTS, server_request {
    APPROVAL_REQUEST = "approval/request": ApprovalRequestParams => ApprovalResponse,
    ELICITATION_REQUEST = "elicitation/request": ElicitationRequestParams => ElicitationResponse,
    BROWSER_EXECUTE = "browser/execute": BrowserExecuteParams => BrowserExecuteResponse,
    SECRETS_STORE = "secrets/store": SecretsStoreParams => EmptyResponse,
    TERMINAL_READ = "terminal/read": TerminalReadParams => TerminalReadResponse,
});

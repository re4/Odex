//! `odex-engine`: the Odex agent engine.

use clap::{Parser, Subcommand};

use odex_config::OdexHome;
use odex_core::{Engine, EngineOptions};

#[derive(Parser)]
#[command(name = "odex-engine", version, about = "Odex agent engine for self-hosted models (vLLM)")]
struct Cli {
    /// Config directory (default: $ODEX_HOME or ~/.odex).
    #[arg(long, global = true)]
    home: Option<String>,
    /// Config profile from [profiles].
    #[arg(long, short = 'p', global = true)]
    profile: Option<String>,
    /// Use this OpenAI-compatible endpoint when no provider is configured.
    #[arg(long, global = true)]
    base_url: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the JSON-RPC app-server protocol on stdio (used by the desktop app).
    AppServer {
        /// Disable the automation scheduler.
        #[arg(long)]
        no_scheduler: bool,
    },
    /// Run one task headlessly.
    Exec(odex_exec::ExecArgs),
    /// Check vLLM endpoints and models.
    Doctor {
        /// Model key or served model id.
        #[arg(long)]
        model: Option<String>,
        /// Skip slow checks.
        #[arg(long)]
        quick: bool,
        #[arg(long)]
        json: bool,
    },
    /// Write TypeScript protocol bindings to a directory.
    GenerateTs {
        #[arg(long, default_value = "desktop/shared-types/src/generated")]
        out: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(h) = &cli.home {
        std::env::set_var("ODEX_HOME", h);
    }
    if let Some(u) = &cli.base_url {
        std::env::set_var("ODEX_BASE_URL", u);
    }
    let home = OdexHome::resolve();
    // logs: stderr for app-server (stdout is the protocol) plus a file
    let _ = home.ensure();
    let filter = tracing_subscriber::EnvFilter::try_from_env("ODEX_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,odex=info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_ansi(false).init();

    if let Cmd::GenerateTs { out } = &cli.cmd {
        odex_protocol::codegen::generate_ts(std::path::Path::new(out)).map_err(|e| anyhow::anyhow!("{e}"))?;
        println!("wrote {out}");
        return Ok(());
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let engine = Engine::new(EngineOptions { home, profile: cli.profile.clone() })?;
        match cli.cmd {
            Cmd::AppServer { no_scheduler } => {
                let _bg = engine.start_background(!no_scheduler);
                odex_app_server::run_stdio(engine).await?;
                Ok(())
            }
            Cmd::Exec(args) => {
                let code = odex_exec::run(engine, args).await?;
                std::process::exit(code);
            }
            Cmd::Doctor { model, quick, json } => {
                let r = odex_core::api::doctor_run(
                    &engine,
                    odex_protocol::DoctorRunParams { provider_id: None, model, quick },
                )
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&r)?);
                } else {
                    for rep in &r.reports {
                        println!("{} — {} ({})", rep.provider_id, rep.model_id, rep.base_url);
                        for c in &rep.checks {
                            let mark = match c.status {
                                odex_protocol::CheckStatus::Pass => "PASS",
                                odex_protocol::CheckStatus::Warn => "WARN",
                                odex_protocol::CheckStatus::Fail => "FAIL",
                                odex_protocol::CheckStatus::Skip => "skip",
                            };
                            println!("  [{mark}] {:<30} {} ({}ms)", c.name, c.detail, c.duration_ms);
                        }
                        if let Some(cmd) = &rep.suggested_command {
                            println!("  suggested: {cmd}");
                        }
                    }
                    if r.reports.is_empty() {
                        println!(
                            "No endpoints configured. Set [model_providers] in ~/.odex/config.toml or pass --base-url."
                        );
                    }
                }
                Ok(())
            }
            Cmd::GenerateTs { .. } => unreachable!(),
        }
    })
}

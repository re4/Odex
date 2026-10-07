//! `odex-mock-vllm --port 8000 --model mock-coder --max-model-len 32768 [--rules rules.json]`

use std::time::Duration;

use clap::Parser;
use odex_mock_vllm::{MockConfig, MockModel, MockServer, RulePolicy};

#[derive(Parser)]
#[command(about = "Mock vLLM server for Odex tests")]
struct Args {
    #[arg(long, default_value_t = 8000)]
    port: u16,
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Served model id (repeatable).
    #[arg(long, default_value = "mock-coder")]
    model: Vec<String>,
    #[arg(long, default_value_t = 32768)]
    max_model_len: u32,
    /// JSON rule policy file.
    #[arg(long)]
    rules: Option<std::path::PathBuf>,
    /// Delay between streamed chunks in ms.
    #[arg(long, default_value_t = 2)]
    delay_ms: u64,
    #[arg(long)]
    api_key: Option<String>,
    /// Also serve a mock ComfyUI on this port (0 = any free port).
    #[arg(long)]
    comfy_port: Option<u16>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a = Args::parse();
    let cfg = MockConfig {
        models: a.model.iter().map(|m| MockModel { id: m.clone(), max_model_len: a.max_model_len }).collect(),
        chunk_delay: Duration::from_millis(a.delay_ms),
        api_key: a.api_key.clone(),
        ..Default::default()
    };
    let server = MockServer::start_on(cfg, &format!("{}:{}", a.host, a.port)).await;
    if let Some(p) = &a.rules {
        let text = std::fs::read_to_string(p)?;
        let v: serde_json::Value = serde_json::from_str(&text)?;
        let policy = RulePolicy::from_json(&v).map_err(anyhow::Error::msg)?;
        let policy = std::sync::Arc::new(policy);
        server.set_policy(move |r| policy.reply(r));
    }
    println!("odex-mock-vllm listening on {}", server.url);
    let _comfy = match a.comfy_port {
        Some(port) => {
            let c = odex_mock_vllm::comfy::MockComfy::start_on(&format!("{}:{port}", a.host)).await;
            println!("mock comfyui listening on {}", c.url);
            Some(c)
        }
        None => None,
    };
    tokio::signal::ctrl_c().await?;
    Ok(())
}

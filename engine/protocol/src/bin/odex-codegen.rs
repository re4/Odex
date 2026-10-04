//! `cargo run -p odex-protocol --bin odex-codegen -- <out_dir>`
fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../desktop/shared-types/src/generated".to_string());
    let out = std::path::PathBuf::from(out);
    if let Err(e) = odex_protocol::codegen::generate_ts(&out) {
        eprintln!("codegen failed: {e}");
        std::process::exit(1);
    }
    println!("wrote TypeScript bindings to {}", out.display());
}

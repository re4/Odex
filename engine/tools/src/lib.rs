//! Host-independent pieces of Odex's agent tools: schemas, argument hygiene,
//! output capping and storage, file edits, images. The engine (`odex-core`)
//! wires these to approvals, the sandbox and the UI.

pub mod edit;
pub mod output;
pub mod specs;

use serde_json::Value;

use odex_llm::types::ToolSpec;

/// Parse, repair, coerce and validate tool arguments.
/// Returns the arguments or a precise error message for the model.
pub fn parse_args(spec: &ToolSpec, raw: &str) -> Result<(Value, bool), String> {
    let (mut v, repaired) = odex_llm::repair::parse_lenient(raw)?;
    if !v.is_object() {
        return Err(format!("arguments for `{}` must be a JSON object, got: {}", spec.name, clip(raw, 120)));
    }
    let coerced = odex_llm::validate::coerce(&spec.parameters, &mut v);
    if let Err(errs) = odex_llm::validate::validate(&spec.parameters, &v) {
        return Err(format!(
            "invalid arguments for `{}`: {}. Expected schema: {}",
            spec.name,
            errs.join("; "),
            spec.parameters
        ));
    }
    Ok((v, repaired || coerced))
}

pub fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// One-line argument summary for items, stubs and approvals.
pub fn args_summary(tool: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match tool {
        "shell" | "exec_command" => clip(&s("command"), 160),
        "read_file" => {
            let mut x = s("path");
            if let Some(o) = args.get("offset").and_then(|v| v.as_u64()) {
                x.push_str(&format!(":{o}"));
                if let Some(l) = args.get("limit").and_then(|v| v.as_u64()) {
                    x.push_str(&format!("+{l}"));
                }
            }
            x
        }
        "edit_file" | "write_file" | "view_image" | "list_dir" => s("path"),
        "grep" => format!("{} in {}", clip(&s("pattern"), 80), if s("path").is_empty() { ".".into() } else { s("path") }),
        "glob" => s("pattern"),
        "read_output" => s("ref"),
        "recall" | "search_tools" => clip(&s("query"), 100),
        "apply_patch" => {
            let p = s("patch");
            let files: Vec<&str> = p
                .lines()
                .filter_map(|l| l.strip_prefix("*** Update File: ").or_else(|| l.strip_prefix("*** Add File: ")).or_else(|| l.strip_prefix("*** Delete File: ")))
                .collect();
            clip(&files.join(", "), 160)
        }
        "spawn_agent" => clip(&s("task"), 100),
        "write_stdin" => format!("{} {}", s("session_id"), clip(&s("chars"), 60)),
        _ => clip(&args.to_string(), 160),
    }
}

/// Load an image file, downscale so the longest edge ≤ `max_px`, and return
/// a PNG/JPEG data URL plus dimensions.
pub fn load_image_data_url(path: &std::path::Path, max_px: u32) -> Result<(String, u32, u32), String> {
    use base64::Engine as _;
    let img = image::open(path).map_err(|e| format!("cannot open image {}: {e}", path.display()))?;
    let (w, h) = (img.width(), img.height());
    let img = if w.max(h) > max_px { img.resize(max_px, max_px, image::imageops::FilterType::Triangle) } else { img };
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());
    Ok((format!("data:image/png;base64,{b64}"), img.width(), img.height()))
}

/// Downscale a PNG/JPEG data URL (or raw bytes) to `max_px`.
pub fn downscale_png(bytes: &[u8], max_px: u32) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let img = if img.width().max(img.height()) > max_px { img.resize(max_px, max_px, image::imageops::FilterType::Triangle) } else { img };
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn args_validation_and_repair() {
        let spec = specs::read_file();
        let (v, repaired) = parse_args(&spec, r#"{"path": "a.rs", "limit": "50",}"#).unwrap();
        assert!(repaired);
        assert_eq!(v, json!({"path":"a.rs","limit":50}));
        let e = parse_args(&spec, r#"{"limit": 5}"#).unwrap_err();
        assert!(e.contains("`path` is required"), "{e}");
    }

    #[test]
    fn summaries() {
        assert_eq!(args_summary("read_file", &json!({"path":"a.rs","offset":10,"limit":20})), "a.rs:10+20");
        assert_eq!(
            args_summary("apply_patch", &json!({"patch":"*** Begin Patch\n*** Update File: x.rs\n*** Add File: y.rs\n*** End Patch"})),
            "x.rs, y.rs"
        );
    }
}

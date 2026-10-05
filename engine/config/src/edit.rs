//! Format-preserving config edits (comments and ordering survive).

use std::path::Path;

use anyhow::{anyhow, bail, Context as _};
use odex_protocol::ConfigEdit;
use serde_json::Value as Json;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};

/// Parse `a.b."c.d".e` into segments.
pub fn parse_key_path(path: &str) -> anyhow::Result<Vec<String>> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut chars = path.chars().peekable();
    let mut quoted: Option<char> = None;
    let mut just_closed = false;
    while let Some(c) = chars.next() {
        match quoted {
            Some(q) if c == q => {
                quoted = None;
                just_closed = true;
            }
            Some(_) if c == '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                if !cur.is_empty() {
                    bail!("unexpected quote in key path `{path}`");
                }
                quoted = Some(c);
            }
            None if c == '.' => {
                if cur.is_empty() && !just_closed {
                    bail!("empty segment in key path `{path}`");
                }
                segs.push(std::mem::take(&mut cur));
                just_closed = false;
            }
            None => {
                if just_closed {
                    bail!("garbage after quoted segment in `{path}`");
                }
                cur.push(c)
            }
        }
    }
    if quoted.is_some() {
        bail!("unterminated quote in key path `{path}`");
    }
    if cur.is_empty() && !just_closed {
        bail!("empty segment in key path `{path}`");
    }
    segs.push(cur);
    Ok(segs)
}

pub fn apply_edits(text: &str, edits: &[ConfigEdit]) -> anyhow::Result<String> {
    let mut doc: DocumentMut = text.parse().context("config is not valid TOML")?;
    for e in edits {
        let path = parse_key_path(&e.key_path)?;
        set_path(doc.as_table_mut(), &path, &e.value).with_context(|| format!("applying edit to `{}`", e.key_path))?;
    }
    Ok(doc.to_string())
}

fn set_path(root: &mut dyn TableLike, path: &[String], value: &Json) -> anyhow::Result<()> {
    set_path_in(root, false, path, value)
}

/// `inline` is true when `root` is (inside) an inline table, where nested
/// objects must stay inline.
fn set_path_in(root: &mut dyn TableLike, inline: bool, path: &[String], value: &Json) -> anyhow::Result<()> {
    let (last, parents) = path.split_last().ok_or_else(|| anyhow!("empty key path"))?;
    let mut cur: &mut dyn TableLike = root;
    let mut in_inline = inline;
    for seg in parents {
        let needs_new = match cur.get(seg) {
            None => true,
            Some(item) => !item.is_table_like(),
        };
        if needs_new {
            if value.is_null() {
                return Ok(()); // removing something that doesn't exist
            }
            if in_inline {
                cur.insert(seg, Item::Value(Value::InlineTable(InlineTable::new())));
            } else {
                let mut t = Table::new();
                t.set_implicit(true);
                cur.insert(seg, Item::Table(t));
            }
        }
        let tmp = cur;
        let item = tmp.get_mut(seg).ok_or_else(|| anyhow!("`{seg}` vanished"))?;
        in_inline = in_inline || item.is_inline_table();
        cur = item.as_table_like_mut().ok_or_else(|| anyhow!("`{seg}` is not a table"))?;
    }
    if value.is_null() {
        cur.remove(last);
        return Ok(());
    }
    match value {
        Json::Object(map) if !in_inline => {
            // Replace with a standard table, recursively filled.
            cur.remove(last);
            let mut t = Table::new();
            t.set_implicit(true);
            cur.insert(last, Item::Table(t));
            let child = cur.get_mut(last).and_then(|i| i.as_table_like_mut()).unwrap();
            for (k, v) in map {
                if v.is_null() {
                    continue;
                }
                set_path_in(child, false, std::slice::from_ref(k), v)?;
            }
        }
        _ => {
            let new = json_to_value(value)?;
            // keep existing decor (comments) when replacing a value
            if let Some(Item::Value(old)) = cur.get_mut(last) {
                let decor = old.decor().clone();
                *old = new;
                *old.decor_mut() = decor;
            } else {
                cur.insert(last, Item::Value(new));
            }
        }
    }
    Ok(())
}

pub fn json_to_value(v: &Json) -> anyhow::Result<Value> {
    Ok(match v {
        Json::Bool(b) => Value::from(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(f) = n.as_f64() {
                Value::from(f)
            } else {
                bail!("unsupported number {n}")
            }
        }
        Json::String(s) => Value::from(s.as_str()),
        Json::Array(a) => {
            let mut arr = Array::new();
            for x in a {
                if x.is_null() {
                    continue;
                }
                arr.push(json_to_value(x)?);
            }
            Value::Array(arr)
        }
        Json::Object(m) => {
            let mut t = InlineTable::new();
            for (k, x) in m {
                if x.is_null() {
                    continue;
                }
                t.insert(k, json_to_value(x)?);
            }
            Value::InlineTable(t)
        }
        Json::Null => bail!("null cannot be stored in TOML"),
    })
}

/// Apply edits to a config file atomically, validating the result parses
/// as a valid Odex config before replacing the file.
pub fn write_edits(path: &Path, edits: &[ConfigEdit]) -> anyhow::Result<()> {
    // Read-modify-write must not interleave: concurrent requests would drop
    // each other's keys and race on the temp file.
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let out = apply_edits(&text, edits)?;
    crate::parse_str(&out).context("edit would produce an invalid config")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path).or_else(|_| {
        // rename over an open file can fail on Windows; fall back to copy
        std::fs::copy(&tmp, path).map(|_| ())?;
        std::fs::remove_file(&tmp)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit(k: &str, v: Json) -> ConfigEdit {
        ConfigEdit { key_path: k.into(), value: v }
    }

    #[test]
    fn key_paths() {
        assert_eq!(parse_key_path("a.b").unwrap(), vec!["a", "b"]);
        assert_eq!(
            parse_key_path(r#"projects."C:\\code\\x".trust_level"#).unwrap(),
            vec!["projects", r"C:\code\x", "trust_level"]
        );
        assert_eq!(parse_key_path("models.'qwen.3'.temperature").unwrap(), vec!["models", "qwen.3", "temperature"]);
        assert!(parse_key_path("a..b").is_err());
    }

    #[test]
    fn preserves_comments_and_sets_nested() {
        let src = "# top comment\nmodel = \"a\" # inline\n\n[models.qwen]\n# keep me\ntemperature = 0.7\n";
        let out = apply_edits(
            src,
            &[
                edit("model", json!("b")),
                edit("models.qwen.top_k", json!(20)),
                edit("roles.main", json!("qwen")),
                edit("model_providers.gpu.base_url", json!("http://x:8000/v1")),
            ],
        )
        .unwrap();
        assert!(out.contains("# top comment"));
        assert!(out.contains("# keep me"));
        assert!(out.contains("model = \"b\" # inline"), "{out}");
        assert!(out.contains("top_k = 20"));
        let (cfg, _) = crate::parse_str(&out).unwrap();
        assert_eq!(cfg.roles["main"], "qwen");
        assert_eq!(cfg.model_providers["gpu"].base_url.as_deref(), Some("http://x:8000/v1"));
        assert_eq!(cfg.models["qwen"].top_k, Some(20));
    }

    #[test]
    fn removes_and_replaces_objects() {
        let src = "[models.a]\ntemperature = 0.1\n[models.b]\ntemperature = 0.2\n";
        let out = apply_edits(
            src,
            &[edit("models.a", Json::Null), edit("models.b", json!({"model": "x", "capabilities": {"vision": true}}))],
        )
        .unwrap();
        let (cfg, _) = crate::parse_str(&out).unwrap();
        assert!(!cfg.models.contains_key("a"));
        assert_eq!(cfg.models["b"].model.as_deref(), Some("x"));
        assert_eq!(cfg.models["b"].temperature, None);
        assert_eq!(cfg.models["b"].capabilities.unwrap().vision, Some(true));
    }

    #[test]
    fn edit_inside_inline_table() {
        let src = "[models.a]\ncapabilities = { tools = true }\n";
        let out = apply_edits(src, &[edit("models.a.capabilities.vision", json!(true))]).unwrap();
        let (cfg, _) = crate::parse_str(&out).unwrap();
        let caps = cfg.models["a"].capabilities.unwrap();
        assert_eq!(caps.tools, Some(true));
        assert_eq!(caps.vision, Some(true));
    }

    #[test]
    fn write_rejects_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        write_edits(&p, &[edit("permission_mode", json!("auto"))]).unwrap();
        assert!(write_edits(&p, &[edit("permission_mode", json!("yolo"))]).is_err());
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("auto"));
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn concurrent_writes_keep_every_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let p = path.clone();
                std::thread::spawn(move || {
                    write_edits(
                        &p,
                        &[ConfigEdit { key_path: format!("profiles.p{i}.model"), value: json!(format!("m{i}")) }],
                    )
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for i in 0..16 {
            assert!(text.contains(&format!("m{i}")), "key {i} lost:\n{text}");
        }
    }
}

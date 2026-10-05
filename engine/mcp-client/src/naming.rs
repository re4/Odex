use std::collections::HashSet;

use sha2::{Digest, Sha256};

/// Longest tool name most OpenAI-compatible servers accept.
pub const MAX_TOOL_NAME_LEN: usize = 64;

const HASH_HEX_LEN: usize = 8;

/// Replace every character outside `[a-zA-Z0-9_-]` with `_`.
pub fn sanitize_name_part(s: &str) -> String {
    let out: String =
        s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if out.is_empty() {
        "_".to_string()
    } else {
        out
    }
}

fn short_hash(server: &str, tool: &str, salt: u32) -> String {
    let mut h = Sha256::new();
    h.update(server.as_bytes());
    h.update([0u8]);
    h.update(tool.as_bytes());
    if salt > 0 {
        h.update(salt.to_le_bytes());
    }
    hex::encode(&h.finalize()[..HASH_HEX_LEN / 2])
}

/// Model-facing name for `tool` on `server`: `mcp__<server>__<tool>`, sanitized
/// to `[a-zA-Z0-9_-]` and at most 64 characters. When the name is too long or
/// already in `taken`, it is truncated and suffixed with `_` + 8 hex chars of a
/// hash of the *original* names, so the result is deterministic.
pub fn qualify_tool_name(server: &str, tool: &str, taken: &HashSet<String>) -> String {
    let base = format!("mcp__{}__{}", sanitize_name_part(server), sanitize_name_part(tool));
    if base.len() <= MAX_TOOL_NAME_LEN && !taken.contains(&base) {
        return base;
    }
    // `base` is pure ASCII, so byte slicing is safe.
    let keep = MAX_TOOL_NAME_LEN - HASH_HEX_LEN - 1;
    let prefix = &base[..base.len().min(keep)];
    let mut salt = 0u32;
    loop {
        let candidate = format!("{prefix}_{}", short_hash(server, tool, salt));
        if !taken.contains(&candidate) {
            return candidate;
        }
        salt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(name: &str) -> bool {
        name.len() <= MAX_TOOL_NAME_LEN && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    #[test]
    fn plain_names() {
        let taken = HashSet::new();
        assert_eq!(qualify_tool_name("github", "create_issue", &taken), "mcp__github__create_issue");
        assert_eq!(qualify_tool_name("my server", "get.file/v2", &taken), "mcp__my_server__get_file_v2");
        assert_eq!(qualify_tool_name("srv", "ünïcode", &taken), "mcp__srv___n_code");
        assert_eq!(qualify_tool_name("", "", &taken), "mcp______");
    }

    #[test]
    fn long_names_are_truncated_with_hash() {
        let taken = HashSet::new();
        let tool = "a".repeat(100);
        let q = qualify_tool_name("server", &tool, &taken);
        assert_eq!(q.len(), MAX_TOOL_NAME_LEN);
        assert!(valid(&q));
        assert!(q.starts_with("mcp__server__aaaa"));
        // deterministic
        assert_eq!(q, qualify_tool_name("server", &tool, &taken));
        // different originals with the same truncated prefix differ
        let tool2 = format!("{}b", "a".repeat(99));
        assert_ne!(q, qualify_tool_name("server", &tool2, &taken));
    }

    #[test]
    fn collisions_get_suffix() {
        let mut taken = HashSet::new();
        let a = qualify_tool_name("s", "get.file", &taken);
        taken.insert(a.clone());
        let b = qualify_tool_name("s", "get_file", &taken);
        assert_eq!(a, "mcp__s__get_file");
        assert_ne!(a, b);
        assert!(b.starts_with("mcp__s__get_file_"));
        assert_eq!(b.len(), "mcp__s__get_file_".len() + HASH_HEX_LEN);
        assert!(valid(&b));
        taken.insert(b.clone());
        // even a second collision on the hashed name resolves
        let c = qualify_tool_name("s", "get_file", &taken);
        assert!(!taken.contains(&c));
        assert!(valid(&c));
    }

    #[test]
    fn long_collisions_stay_within_limit() {
        let mut taken = HashSet::new();
        let server = "x".repeat(70);
        let first = qualify_tool_name(&server, "t", &taken);
        taken.insert(first.clone());
        let second = qualify_tool_name(&server, "t", &taken);
        assert_ne!(first, second);
        assert!(valid(&first) && valid(&second));
    }
}

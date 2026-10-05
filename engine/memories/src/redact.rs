//! Best-effort secret redaction for text that is persisted or sent to a model.

use once_cell::sync::Lazy;
use regex::{Captures, Regex};

pub(crate) const REDACTED: &str = "[REDACTED]";

/// Whole-match patterns: the entire match is replaced.
static TOKEN_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    [
        // PEM private keys (terminated, or running to the end of the text).
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
        // OpenAI / Anthropic style keys: sk-..., sk-proj-..., sk-ant-...
        r"\bsk-[A-Za-z0-9][A-Za-z0-9_\-]{15,}",
        // GitHub tokens.
        r"\bgithub_pat_[A-Za-z0-9_]{20,}",
        r"\bgh[pousr]_[A-Za-z0-9]{20,}",
        // Slack tokens.
        r"\bxox[abposr]-[A-Za-z0-9\-]{10,}",
        // AWS access key ids.
        r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        // Google API keys.
        r"\bAIza[0-9A-Za-z_\-]{35}",
        // JWTs (header and payload are base64url JSON, so both start with eyJ).
        r"\beyJ[A-Za-z0-9_\-]{5,}\.eyJ[A-Za-z0-9_\-]{5,}\.[A-Za-z0-9_\-]{5,}",
        // Generic long-lived tokens with well-known prefixes.
        r"\b(?:glpat|npm|hf|pypi)[-_][A-Za-z0-9_\-]{20,}",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("valid redaction regex"))
    .collect()
});

/// `Bearer <token>` / `Basic <token>` authorization values.
static AUTH_SCHEME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=\-]{8,}").expect("valid regex"));

/// `key = value` / `key: value` where the key names a secret.
static KEY_VALUE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?ix)
        \b(?P<key>[a-z0-9_.\-]*?(?:password|passwd|passphrase|secret|token|api[_\-]?key|apikey|access[_\-]?key|private[_\-]?key|client[_\-]?secret|auth[_\-]?token|credentials?))
        (?P<sep>["']?\s*[:=]\s*)
        (?P<val>"[^"\n]*"|'[^'\n]*'|[^\s,;&]+)
        "#,
    )
    .expect("valid regex")
});

/// Credentials embedded in URLs: `scheme://user:pass@host`.
static URL_CREDENTIALS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b([a-z][a-z0-9+.\-]*://[^\s:/@]+):[^\s@/]+@").expect("valid regex"));

/// Replace API keys, tokens, private keys and `password=...`-style values with `[REDACTED]`.
pub fn redact_secrets(text: &str) -> String {
    let mut out = text.to_string();
    for re in TOKEN_PATTERNS.iter() {
        if re.is_match(&out) {
            out = re.replace_all(&out, REDACTED).into_owned();
        }
    }
    out = AUTH_SCHEME.replace_all(&out, |c: &Captures<'_>| format!("{} {REDACTED}", &c[1])).into_owned();
    out = KEY_VALUE
        .replace_all(&out, |c: &Captures<'_>| {
            let val = &c["val"];
            if val.trim_matches(['"', '\'']) == REDACTED || val.trim_matches(['"', '\'']).is_empty() {
                return c[0].to_string();
            }
            format!("{}{}{REDACTED}", &c["key"], &c["sep"])
        })
        .into_owned();
    out = URL_CREDENTIALS.replace_all(&out, |c: &Captures<'_>| format!("{}:{REDACTED}@", &c[1])).into_owned();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_token_shapes() {
        let cases = [
            ("key sk-proj-abcdEFGH1234567890xyz done", "key [REDACTED] done"),
            ("sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA", "[REDACTED]"),
            ("token ghp_0123456789abcdefghijABCDEFGHIJ0123 ok", "token [REDACTED] ok"),
            ("github_pat_11ABCDEFG0123456789_abcdefghijklmnop", "[REDACTED]"),
            ("slack xoxb-1234567890-abcdefghij", "slack [REDACTED]"),
            ("aws AKIAIOSFODNN7EXAMPLE here", "aws [REDACTED] here"),
            ("g AIzaSyA-1234567890abcdefghijklmnopqrstu", "g [REDACTED]"),
            (
                "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U end",
                "jwt [REDACTED] end",
            ),
            ("Authorization: Bearer abc.def-ghi_jkl123", "Authorization: Bearer [REDACTED]"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_secrets(input), expected, "input: {input}");
        }
    }

    #[test]
    fn redacts_key_value_secrets() {
        assert_eq!(redact_secrets("password=hunter2"), "password=[REDACTED]");
        assert_eq!(redact_secrets("DB_PASSWORD = \"p@ss word\""), "DB_PASSWORD = [REDACTED]");
        assert_eq!(redact_secrets("token: abc123, next"), "token: [REDACTED], next");
        assert_eq!(redact_secrets("export GITHUB_TOKEN=whatever"), "export GITHUB_TOKEN=[REDACTED]");
        assert_eq!(redact_secrets(r#"{"api_key": "zzz"}"#), r#"{"api_key": [REDACTED]}"#);
        assert_eq!(redact_secrets("client_secret: s3cr3t"), "client_secret: [REDACTED]");
        assert_eq!(redact_secrets("postgres://app:hunter2@db:5432/x"), "postgres://app:[REDACTED]@db:5432/x");
    }

    #[test]
    fn redacts_private_keys() {
        let pem = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nabc\n-----END RSA PRIVATE KEY-----\nafter";
        assert_eq!(redact_secrets(pem), "before\n[REDACTED]\nafter");
        let open = "x -----BEGIN OPENSSH PRIVATE KEY-----\nAAAA";
        assert_eq!(redact_secrets(open), "x [REDACTED]");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        for text in [
            "Prefers pnpm over npm; uses Rust 1.80 and tokio.",
            "max tokens: 4096 for the utility model",
            "The task-sk-lite module is small",
            "Use the `password` field component from ui/forms",
            "Run `pwd` before scripts",
        ] {
            assert_eq!(redact_secrets(text), text);
        }
    }
}

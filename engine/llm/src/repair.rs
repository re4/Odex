//! Best-effort repair of malformed JSON produced by open models.
//!
//! Handles: code fences, leading prose, trailing commas, raw newlines/tabs in
//! strings, single-quoted strings, Python literals (True/False/None), `//` and
//! `/* */` comments, unquoted keys, and truncation (unclosed strings/brackets).

use serde_json::Value;

/// Parse `text` as JSON, repairing it if needed. Returns the value and
/// whether a repair was applied.
pub fn parse_lenient(text: &str) -> Result<(Value, bool), String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok((Value::Object(Default::default()), true));
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Ok((unwrap_stringified(v), false));
    }
    let repaired = repair(trimmed);
    match serde_json::from_str::<Value>(&repaired) {
        Ok(v) => Ok((unwrap_stringified(v), true)),
        Err(e) => Err(format!("invalid JSON arguments ({e}); after repair: {}", truncate(&repaired, 200))),
    }
}

/// Some models double-encode arguments: `"{\"a\":1}"`. Unwrap one level.
fn unwrap_stringified(v: Value) -> Value {
    if let Value::String(s) = &v {
        let t = s.trim();
        if t.starts_with('{') {
            if let Ok(inner) = serde_json::from_str::<Value>(t) {
                if inner.is_object() {
                    return inner;
                }
            }
        }
    }
    v
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Strip code fences and prose around the first JSON object/array.
fn extract_json_region(s: &str) -> &str {
    let mut t = s.trim();
    if let Some(rest) = t.strip_prefix("```") {
        // drop language tag line
        let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or(rest);
        t = rest.trim_end();
        if let Some(r) = t.strip_suffix("```") {
            t = r.trim_end();
        }
    }
    let start = t.find(['{', '[']);
    match start {
        Some(i) => &t[i..],
        None => t,
    }
}

pub fn repair(input: &str) -> String {
    let s = extract_json_region(input);
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 16);
    let mut stack: Vec<char> = Vec::new(); // expected closers
    let mut i = 0;
    let mut in_str = false;
    let mut quote = '"';
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            match c {
                '\\' => {
                    if i + 1 < chars.len() {
                        let n = chars[i + 1];
                        // keep valid escapes, double invalid ones
                        if matches!(n, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') {
                            out.push('\\');
                            out.push(n);
                        } else if n == '\'' {
                            out.push('\'');
                        } else {
                            out.push_str("\\\\");
                            out.push(n);
                        }
                        i += 2;
                        continue;
                    }
                    out.push_str("\\\\");
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c == quote => {
                    in_str = false;
                    out.push('"');
                }
                '"' => out.push_str("\\\""), // inside single-quoted string
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' => {
                in_str = true;
                quote = c;
                out.push('"');
            }
            '{' => {
                stack.push('}');
                out.push(c);
            }
            '[' => {
                stack.push(']');
                out.push(c);
            }
            '}' | ']' => {
                strip_trailing_comma(&mut out);
                // pop until matching closer (tolerate mismatches)
                if let Some(pos) = stack.iter().rposition(|&x| x == c) {
                    while stack.len() > pos + 1 {
                        let closer = stack.pop().unwrap();
                        strip_trailing_comma(&mut out);
                        out.push(closer);
                    }
                    stack.pop();
                    out.push(c);
                }
                // else: stray closer, drop it
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            c if c.is_alphabetic() || c == '_' || c == '$' => {
                // bare word: literal or unquoted key
                let start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$' || chars[i] == '-')
                {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                // look ahead for ':' → key
                let mut j = i;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ':' && stack.last() == Some(&'}') {
                    out.push('"');
                    out.push_str(&word);
                    out.push('"');
                } else {
                    match word.as_str() {
                        "True" | "true" => out.push_str("true"),
                        "False" | "false" => out.push_str("false"),
                        "None" | "null" | "nil" | "undefined" | "NaN" => out.push_str("null"),
                        _ => {
                            out.push('"');
                            out.push_str(&word);
                            out.push('"');
                        }
                    }
                }
                continue;
            }
            c => out.push(c),
        }
        i += 1;
    }
    if in_str {
        out.push('"');
    }
    // dangling key without value: `{"a": 1, "b"` or `{"a":`
    let t = out.trim_end().to_string();
    out = t;
    if out.ends_with(':') {
        out.push_str("null");
    }
    strip_trailing_comma(&mut out);
    while let Some(closer) = stack.pop() {
        strip_trailing_comma(&mut out);
        if closer == '}' && ends_with_bare_key(&out) {
            out.push_str(":null");
        }
        out.push(closer);
    }
    out
}

fn strip_trailing_comma(out: &mut String) {
    let trimmed_len = out.trim_end().len();
    if out[..trimmed_len].ends_with(',') {
        out.truncate(trimmed_len - 1);
    }
}

/// `{"a":1,"b"` → true (a key string with no colon after it).
fn ends_with_bare_key(s: &str) -> bool {
    let t = s.trim_end();
    if !t.ends_with('"') {
        return false;
    }
    // find the opening quote of this last string
    let bytes: Vec<char> = t.chars().collect();
    let mut i = bytes.len() as isize - 2;
    while i >= 0 {
        if bytes[i as usize] == '"' && (i == 0 || bytes[(i - 1) as usize] != '\\') {
            break;
        }
        i -= 1;
    }
    if i < 0 {
        return false;
    }
    let mut j = i - 1;
    while j >= 0 && bytes[j as usize].is_whitespace() {
        j -= 1;
    }
    j >= 0 && (bytes[j as usize] == '{' || bytes[j as usize] == ',')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p(s: &str) -> Value {
        parse_lenient(s).unwrap().0
    }

    #[test]
    fn valid_passthrough() {
        assert_eq!(parse_lenient(r#"{"a":1}"#).unwrap(), (json!({"a":1}), false));
    }

    #[test]
    fn trailing_commas_and_comments() {
        assert_eq!(p("{\"a\": 1, // c\n \"b\": [1,2,],}"), json!({"a":1,"b":[1,2]}));
        assert_eq!(p("{\"a\": /* x */ 1}"), json!({"a":1}));
    }

    #[test]
    fn raw_newlines_in_strings() {
        assert_eq!(p("{\"content\": \"line1\nline2\tx\"}"), json!({"content":"line1\nline2\tx"}));
    }

    #[test]
    fn truncation() {
        assert_eq!(p(r#"{"path": "a.txt", "content": "hel"#), json!({"path":"a.txt","content":"hel"}));
        assert_eq!(p(r#"{"a": [1, 2"#), json!({"a":[1,2]}));
        assert_eq!(p(r#"{"a": 1, "b""#), json!({"a":1,"b":null}));
        assert_eq!(p(r#"{"a":"#), json!({"a":null}));
    }

    #[test]
    fn python_and_single_quotes() {
        assert_eq!(p("{'a': True, 'b': None, 'c': 'it\\'s'}"), json!({"a":true,"b":null,"c":"it's"}));
    }

    #[test]
    fn unquoted_keys_and_fences() {
        assert_eq!(p("```json\n{path: \"x\", n: 2}\n```"), json!({"path":"x","n":2}));
        assert_eq!(p("Sure! {\"a\": 1}"), json!({"a":1}));
    }

    #[test]
    fn double_encoded() {
        assert_eq!(p(r#""{\"a\":1}""#), json!({"a":1}));
    }

    #[test]
    fn invalid_escape_kept() {
        assert_eq!(p(r#"{"p": "C:\Users\x"}"#), json!({"p": "C:\\Users\\x"}));
    }

    #[test]
    fn empty_is_object() {
        assert_eq!(p("  "), json!({}));
    }
}

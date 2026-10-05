//! Client-side fallback tool-call parsing, used when the server returned tool
//! calls inside `content` instead of `tool_calls` (wrong or missing
//! `--tool-call-parser`). Supports Hermes `<tool_call>` JSON, Qwen3-Coder XML,
//! GLM-4.5 `<arg_key>` XML, Llama-3 JSON, Mistral `[TOOL_CALLS]`, DeepSeek
//! special tokens, Kimi-K2 sections, `<function=...>` tags and pythonic calls.

use serde_json::{Map, Value};

use crate::repair::parse_lenient;
use crate::types::{ToolCall, ToolSpec};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FallbackResult {
    pub calls: Vec<ToolCall>,
    /// Content with the tool-call markup removed.
    pub content: String,
    /// Which format matched.
    pub format: Option<&'static str>,
}

const MARKERS: &[&str] = &[
    "<tool_call>",
    "<tool_call ",
    "<function=",
    "<function_call>",
    "[TOOL_CALLS]",
    "<｜tool▁calls▁begin｜>",
    "<｜tool▁call▁begin｜>",
    "<|tool_calls_section_begin|>",
    "<|tool_call_begin|>",
    "<|python_tag|>",
];

/// Byte offset where tool-call markup starts, or where a *possible* marker
/// prefix begins at the end of the buffer (so streaming can hold it back).
pub fn marker_start(content: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    for m in MARKERS {
        if let Some(i) = content.find(m) {
            best = Some(best.map_or(i, |b: usize| b.min(i)));
        }
    }
    if best.is_some() {
        return best;
    }
    // whole-content JSON call (Llama-3 style) or pythonic list at the start
    let t = content.trim_start();
    let lead = content.len() - t.len();
    if t.starts_with("{\"name\"") || t.starts_with("{ \"name\"") || t.starts_with("```json\n{\"name\"") {
        return Some(lead);
    }
    // partial marker at the very end (e.g. "<tool_ca")
    for m in MARKERS {
        for k in (1..m.len()).rev() {
            if !m.is_char_boundary(k) {
                continue;
            }
            if content.ends_with(&m[..k]) {
                return Some(content.len() - k);
            }
        }
    }
    None
}

/// Try every known format. `tools` restricts accepted names (and guides type
/// coercion of XML parameters); an empty slice accepts any name.
pub fn parse(content: &str, tools: &[ToolSpec]) -> FallbackResult {
    let parsers: &[(&str, ParserFn)] = &[
        ("qwen3_coder", parse_qwen_xml),
        ("glm45", parse_glm),
        ("hermes", parse_hermes),
        ("function_tag", parse_function_tag),
        ("mistral", parse_mistral),
        ("deepseek", parse_deepseek),
        ("kimi_k2", parse_kimi),
        ("llama3_json", parse_llama_json),
        ("pythonic", parse_pythonic),
    ];
    for (name, f) in parsers {
        if let Some((raw, rest)) = f(content) {
            let calls = finalize(raw, tools);
            if !calls.is_empty() {
                return FallbackResult { calls, content: rest.trim().to_string(), format: Some(name) };
            }
        }
    }
    FallbackResult { calls: vec![], content: content.to_string(), format: None }
}

type ParserFn = fn(&str) -> Option<(Vec<RawCall>, String)>;

#[derive(Debug, Clone)]
struct RawCall {
    name: String,
    args: RawArgs,
}

#[derive(Debug, Clone)]
enum RawArgs {
    Json(String),
    /// Untyped string params (XML formats); coerced using the schema.
    Params(Vec<(String, String)>),
    Value(Value),
}

fn finalize(raw: Vec<RawCall>, tools: &[ToolSpec]) -> Vec<ToolCall> {
    let mut out = Vec::new();
    for (i, rc) in raw.into_iter().enumerate() {
        let name = rc.name.trim().trim_start_matches("functions.").to_string();
        let tool = if tools.is_empty() {
            None
        } else {
            match tools
                .iter()
                .find(|t| t.name == name)
                .or_else(|| tools.iter().find(|t| t.name.eq_ignore_ascii_case(&name)))
            {
                Some(t) => Some(t),
                None => continue, // unknown tool name: not a real call
            }
        };
        let name = tool.map(|t| t.name.clone()).unwrap_or(name);
        if name.is_empty() {
            continue;
        }
        let args = match rc.args {
            RawArgs::Json(s) => match parse_lenient(&s) {
                Ok((v, _)) => v.to_string(),
                Err(_) => s,
            },
            RawArgs::Value(v) => v.to_string(),
            RawArgs::Params(ps) => {
                let mut m = Map::new();
                for (k, v) in ps {
                    let ty = tool
                        .and_then(|t| t.parameters.get("properties"))
                        .and_then(|p| p.get(&k))
                        .and_then(|s| s.get("type"))
                        .and_then(|t| t.as_str());
                    m.insert(k, coerce_param(&v, ty));
                }
                Value::Object(m).to_string()
            }
        };
        out.push(ToolCall { id: format!("call_fb_{}_{}", short_id(), i), name, arguments: args });
    }
    out
}

fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

fn coerce_param(raw: &str, ty: Option<&str>) -> Value {
    // XML values often carry one leading/trailing newline
    let v = raw.strip_prefix('\n').unwrap_or(raw);
    let v = v.strip_suffix('\n').unwrap_or(v);
    match ty {
        Some("string") => Value::String(v.to_string()),
        Some("integer") | Some("number") | Some("boolean") | Some("array") | Some("object") => {
            serde_json::from_str(v.trim())
                .or_else(|_| parse_lenient(v.trim()).map(|x| x.0))
                .unwrap_or_else(|_| Value::String(v.to_string()))
        }
        _ => {
            let t = v.trim();
            if t.starts_with('{')
                || t.starts_with('[')
                || t == "true"
                || t == "false"
                || t == "null"
                || t.parse::<f64>().is_ok()
            {
                serde_json::from_str(t).unwrap_or_else(|_| Value::String(v.to_string()))
            } else {
                Value::String(v.to_string())
            }
        }
    }
}

/// Extract blocks between `open` and `close`; an unclosed final block runs to the end.
fn blocks<'a>(s: &'a str, open: &str, close: &str) -> (Vec<&'a str>, String) {
    let mut out = Vec::new();
    let mut rest = String::new();
    let mut cur = s;
    while let Some(i) = cur.find(open) {
        rest.push_str(&cur[..i]);
        let after = &cur[i + open.len()..];
        match after.find(close) {
            Some(j) => {
                out.push(&after[..j]);
                cur = &after[j + close.len()..];
            }
            None => {
                out.push(after);
                cur = "";
            }
        }
    }
    rest.push_str(cur);
    (out, rest)
}

fn parse_hermes(s: &str) -> Option<(Vec<RawCall>, String)> {
    if !s.contains("<tool_call>") {
        return None;
    }
    let (bs, rest) = blocks(s, "<tool_call>", "</tool_call>");
    let mut calls = Vec::new();
    for b in bs {
        let t = b.trim();
        if t.contains("<function=") || t.contains("<arg_key>") {
            return None; // other XML dialects
        }
        if let Ok((v, _)) = parse_lenient(t) {
            if let Some(c) = json_call(&v) {
                calls.push(c);
            }
        }
    }
    Some((calls, rest))
}

/// `{"name": .., "arguments"|"parameters"|"args": ..}`
fn json_call(v: &Value) -> Option<RawCall> {
    let obj = v.as_object()?;
    let name = obj.get("name").or_else(|| obj.get("function")).or_else(|| obj.get("tool"))?.as_str()?.to_string();
    let args = obj
        .get("arguments")
        .or_else(|| obj.get("parameters"))
        .or_else(|| obj.get("args"))
        .or_else(|| obj.get("input"))
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    let args = match args {
        Value::String(s) => RawArgs::Json(s),
        other => RawArgs::Value(other),
    };
    Some(RawCall { name, args })
}

fn parse_qwen_xml(s: &str) -> Option<(Vec<RawCall>, String)> {
    if !s.contains("<function=") {
        return None;
    }
    // Calls may or may not be wrapped in <tool_call>.
    let inner_src = s.replace("<tool_call>", "").replace("</tool_call>", "");
    let (bs, rest) = blocks(&inner_src, "<function=", "</function>");
    let mut calls = Vec::new();
    for b in bs {
        let (name, body) = match b.find('>') {
            Some(i) => (&b[..i], &b[i + 1..]),
            None => continue,
        };
        // JSON body: <function=name>{...}</function>
        let bt = body.trim();
        if bt.starts_with('{') && !bt.contains("<parameter=") {
            calls.push(RawCall { name: name.to_string(), args: RawArgs::Json(bt.to_string()) });
            continue;
        }
        let mut params = Vec::new();
        let (ps, _) = blocks(body, "<parameter=", "</parameter>");
        for p in ps {
            if let Some(i) = p.find('>') {
                params.push((p[..i].trim().to_string(), p[i + 1..].to_string()));
            }
        }
        calls.push(RawCall { name: name.trim().to_string(), args: RawArgs::Params(params) });
    }
    Some((calls, rest))
}

fn parse_function_tag(s: &str) -> Option<(Vec<RawCall>, String)> {
    if !s.contains("<function_call>") {
        return None;
    }
    let (bs, rest) = blocks(s, "<function_call>", "</function_call>");
    let calls = bs.iter().filter_map(|b| parse_lenient(b.trim()).ok().and_then(|(v, _)| json_call(&v))).collect();
    Some((calls, rest))
}

fn parse_glm(s: &str) -> Option<(Vec<RawCall>, String)> {
    if !s.contains("<arg_key>") && !(s.contains("<tool_call>") && !s.contains('{')) {
        return None;
    }
    let (bs, rest) = blocks(s, "<tool_call>", "</tool_call>");
    let mut calls = Vec::new();
    for b in bs {
        let name_end = b.find("<arg_key>").unwrap_or(b.len());
        let name = b[..name_end].trim().to_string();
        if name.is_empty() {
            continue;
        }
        let keys = blocks(&b[name_end..], "<arg_key>", "</arg_key>").0;
        let vals = blocks(&b[name_end..], "<arg_value>", "</arg_value>").0;
        let params = keys.iter().zip(vals.iter()).map(|(k, v)| (k.trim().to_string(), v.to_string())).collect();
        calls.push(RawCall { name, args: RawArgs::Params(params) });
    }
    Some((calls, rest))
}

fn parse_mistral(s: &str) -> Option<(Vec<RawCall>, String)> {
    let i = s.find("[TOOL_CALLS]")?;
    let before = s[..i].to_string();
    let after = s[i + "[TOOL_CALLS]".len()..].trim_start();
    let mut calls = Vec::new();
    if after.starts_with('[') || after.starts_with('{') {
        if let Ok((v, _)) = parse_lenient(after) {
            match v {
                Value::Array(items) => calls.extend(items.iter().filter_map(json_call)),
                obj @ Value::Object(_) => calls.extend(json_call(&obj)),
                _ => {}
            }
        }
    } else {
        // v11+: name[ARGS]{json}[TOOL_CALLS]name2[ARGS]{...}
        for part in after.split("[TOOL_CALLS]") {
            if let Some((name, args)) = part.split_once("[ARGS]") {
                calls.push(RawCall { name: name.trim().to_string(), args: RawArgs::Json(args.trim().to_string()) });
            }
        }
    }
    Some((calls, before))
}

fn parse_deepseek(s: &str) -> Option<(Vec<RawCall>, String)> {
    const BEGIN: &str = "<｜tool▁call▁begin｜>";
    const END: &str = "<｜tool▁call▁end｜>";
    const SEP: &str = "<｜tool▁sep｜>";
    if !s.contains(BEGIN) {
        return None;
    }
    let cleaned = s.replace("<｜tool▁calls▁begin｜>", "").replace("<｜tool▁calls▁end｜>", "");
    let (bs, rest) = blocks(&cleaned, BEGIN, END);
    let mut calls = Vec::new();
    for b in bs {
        let Some((head, tail)) = b.split_once(SEP) else { continue };
        let head = head.trim();
        let (name, args) = if head == "function" {
            // V3: function<sep>name\n```json\n{...}\n```
            let (n, a) = tail.split_once('\n').unwrap_or((tail, "{}"));
            (
                n.trim().to_string(),
                a.trim()
                    .trim_start_matches("```json")
                    .trim_start_matches("```")
                    .trim_end_matches("```")
                    .trim()
                    .to_string(),
            )
        } else {
            // V3.1: name<sep>{...}
            (head.to_string(), tail.trim().to_string())
        };
        calls.push(RawCall { name, args: RawArgs::Json(args) });
    }
    Some((calls, rest))
}

fn parse_kimi(s: &str) -> Option<(Vec<RawCall>, String)> {
    const BEGIN: &str = "<|tool_call_begin|>";
    const END: &str = "<|tool_call_end|>";
    const ARG: &str = "<|tool_call_argument_begin|>";
    if !s.contains(BEGIN) {
        return None;
    }
    let cleaned = s.replace("<|tool_calls_section_begin|>", "").replace("<|tool_calls_section_end|>", "");
    let (bs, rest) = blocks(&cleaned, BEGIN, END);
    let mut calls = Vec::new();
    for b in bs {
        let Some((id, args)) = b.split_once(ARG) else { continue };
        // "functions.get_weather:0"
        let name = id.trim().trim_start_matches("functions.");
        let name = name.rsplit_once(':').map(|(n, _)| n).unwrap_or(name);
        calls.push(RawCall { name: name.to_string(), args: RawArgs::Json(args.trim().to_string()) });
    }
    Some((calls, rest))
}

fn parse_llama_json(s: &str) -> Option<(Vec<RawCall>, String)> {
    let mut t = s.trim();
    t = t.strip_prefix("<|python_tag|>").unwrap_or(t).trim();
    if let Some(r) = t.strip_prefix("```json") {
        t = r.trim().trim_end_matches("```").trim();
    } else if let Some(r) = t.strip_prefix("```") {
        t = r.trim().trim_end_matches("```").trim();
    }
    if !t.starts_with('{') && !t.starts_with('[') {
        return None;
    }
    // Possibly several calls separated by ';'
    let mut calls = Vec::new();
    if let Ok((v, _)) = parse_lenient(t) {
        match v {
            Value::Array(items) => calls.extend(items.iter().filter_map(json_call)),
            obj @ Value::Object(_) => calls.extend(json_call(&obj)),
            _ => {}
        }
    } else {
        for part in t.split(';') {
            if let Ok((v, _)) = parse_lenient(part.trim()) {
                calls.extend(json_call(&v));
            }
        }
    }
    if calls.is_empty() {
        return None;
    }
    Some((calls, String::new()))
}

fn parse_pythonic(s: &str) -> Option<(Vec<RawCall>, String)> {
    let t = s.trim();
    let t = t.strip_prefix("<|python_start|>").unwrap_or(t);
    let t = t.strip_suffix("<|python_end|>").unwrap_or(t).trim();
    if !(t.starts_with('[') && t.ends_with(']')) {
        return None;
    }
    let mut p = PyParser { s: t.as_bytes(), i: 1 };
    let mut calls = Vec::new();
    loop {
        p.ws();
        if p.peek() == Some(b']') {
            break;
        }
        let name = p.ident()?;
        p.ws();
        if p.next()? != b'(' {
            return None;
        }
        let mut args = Map::new();
        loop {
            p.ws();
            if p.peek() == Some(b')') {
                p.i += 1;
                break;
            }
            let key = p.ident()?;
            p.ws();
            if p.next()? != b'=' {
                return None;
            }
            let val = p.value()?;
            args.insert(key, val);
            p.ws();
            match p.next()? {
                b',' => continue,
                b')' => break,
                _ => return None,
            }
        }
        calls.push(RawCall { name, args: RawArgs::Value(Value::Object(args)) });
        p.ws();
        match p.peek() {
            Some(b',') => p.i += 1,
            Some(b']') => break,
            _ => return None,
        }
    }
    Some((calls, String::new()))
}

struct PyParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl PyParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn next(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }
    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\t' | b'\r')) {
            self.i += 1;
        }
    }
    fn ident(&mut self) -> Option<String> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'.') {
            self.i += 1;
        }
        if self.i == start {
            return None;
        }
        Some(String::from_utf8_lossy(&self.s[start..self.i]).to_string())
    }
    fn value(&mut self) -> Option<Value> {
        self.ws();
        match self.peek()? {
            b'"' | b'\'' => {
                let q = self.next()?;
                let mut out = Vec::new();
                loop {
                    let c = self.next()?;
                    if c == b'\\' {
                        let n = self.next()?;
                        out.push(match n {
                            b'n' => b'\n',
                            b't' => b'\t',
                            b'r' => b'\r',
                            other => other,
                        });
                    } else if c == q {
                        break;
                    } else {
                        out.push(c);
                    }
                }
                Some(Value::String(String::from_utf8_lossy(&out).to_string()))
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    if self.peek()? == b']' {
                        self.i += 1;
                        break;
                    }
                    items.push(self.value()?);
                    self.ws();
                    match self.next()? {
                        b',' => continue,
                        b']' => break,
                        _ => return None,
                    }
                }
                Some(Value::Array(items))
            }
            b'{' => {
                self.i += 1;
                let mut m = Map::new();
                loop {
                    self.ws();
                    if self.peek()? == b'}' {
                        self.i += 1;
                        break;
                    }
                    let k = match self.value()? {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    self.ws();
                    if self.next()? != b':' {
                        return None;
                    }
                    let v = self.value()?;
                    m.insert(k, v);
                    self.ws();
                    match self.next()? {
                        b',' => continue,
                        b'}' => break,
                        _ => return None,
                    }
                }
                Some(Value::Object(m))
            }
            _ => {
                let start = self.i;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'.' || c == b'-' || c == b'+' || c == b'_')
                {
                    self.i += 1;
                }
                let tok = String::from_utf8_lossy(&self.s[start..self.i]).to_string();
                match tok.as_str() {
                    "True" | "true" => Some(Value::Bool(true)),
                    "False" | "false" => Some(Value::Bool(false)),
                    "None" | "null" => Some(Value::Null),
                    _ => {
                        if let Ok(i) = tok.parse::<i64>() {
                            Some(Value::from(i))
                        } else if let Ok(f) = tok.parse::<f64>() {
                            Some(Value::from(f))
                        } else {
                            None
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tools() -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "get_weather".into(),
                description: String::new(),
                parameters: json!({"type":"object","properties":{"city":{"type":"string"},"days":{"type":"integer"}}}),
            },
            ToolSpec {
                name: "write_file".into(),
                description: String::new(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}}}),
            },
        ]
    }

    fn args(c: &ToolCall) -> Value {
        serde_json::from_str(&c.arguments).unwrap()
    }

    #[test]
    fn hermes() {
        let r = parse("Let me check.\n<tool_call>\n{\"name\": \"get_weather\", \"arguments\": {\"city\": \"Paris\"}}\n</tool_call>", &tools());
        assert_eq!(r.format, Some("hermes"));
        assert_eq!(r.calls[0].name, "get_weather");
        assert_eq!(args(&r.calls[0]), json!({"city":"Paris"}));
        assert_eq!(r.content, "Let me check.");
    }

    #[test]
    fn hermes_two_calls_unclosed() {
        let r = parse(
            "<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"A\"}}</tool_call><tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"B\"",
            &tools(),
        );
        assert_eq!(r.calls.len(), 2);
        assert_eq!(args(&r.calls[1]), json!({"city":"B"}));
    }

    #[test]
    fn qwen3_coder_xml_types() {
        let r = parse(
            "<tool_call>\n<function=get_weather>\n<parameter=city>\nNew York\n</parameter>\n<parameter=days>\n3\n</parameter>\n</function>\n</tool_call>",
            &tools(),
        );
        assert_eq!(r.format, Some("qwen3_coder"));
        assert_eq!(args(&r.calls[0]), json!({"city":"New York","days":3}));
    }

    #[test]
    fn qwen_xml_keeps_string_content_verbatim() {
        let r = parse(
            "<tool_call><function=write_file><parameter=path>a.json</parameter><parameter=content>\n{\"x\": 1}\n</parameter></function></tool_call>",
            &tools(),
        );
        assert_eq!(args(&r.calls[0])["content"], json!("{\"x\": 1}"));
    }

    #[test]
    fn glm() {
        let r = parse("<tool_call>get_weather\n<arg_key>city</arg_key>\n<arg_value>Paris</arg_value>\n<arg_key>days</arg_key>\n<arg_value>2</arg_value>\n</tool_call>", &tools());
        assert_eq!(r.format, Some("glm45"));
        assert_eq!(args(&r.calls[0]), json!({"city":"Paris","days":2}));
    }

    #[test]
    fn llama3_json() {
        let r = parse("{\"name\": \"get_weather\", \"parameters\": {\"city\": \"Rome\"}}", &tools());
        assert_eq!(r.format, Some("llama3_json"));
        assert_eq!(args(&r.calls[0]), json!({"city":"Rome"}));
        let r = parse("<|python_tag|>{\"name\": \"get_weather\", \"parameters\": {\"city\": \"Oslo\"}}", &tools());
        assert_eq!(args(&r.calls[0]), json!({"city":"Oslo"}));
    }

    #[test]
    fn mistral_both_formats() {
        let r = parse("[TOOL_CALLS] [{\"name\": \"get_weather\", \"arguments\": {\"city\": \"Lyon\"}}]", &tools());
        assert_eq!(args(&r.calls[0]), json!({"city":"Lyon"}));
        let r = parse("[TOOL_CALLS]get_weather[ARGS]{\"city\": \"Nice\"}", &tools());
        assert_eq!(args(&r.calls[0]), json!({"city":"Nice"}));
    }

    #[test]
    fn deepseek_v3_and_v31() {
        let v3 = "<｜tool▁calls▁begin｜><｜tool▁call▁begin｜>function<｜tool▁sep｜>get_weather\n```json\n{\"city\": \"Kyiv\"}\n```<｜tool▁call▁end｜><｜tool▁calls▁end｜>";
        let r = parse(v3, &tools());
        assert_eq!(r.format, Some("deepseek"));
        assert_eq!(args(&r.calls[0]), json!({"city":"Kyiv"}));
        let v31 = "<｜tool▁calls▁begin｜><｜tool▁call▁begin｜>get_weather<｜tool▁sep｜>{\"city\": \"Riga\"}<｜tool▁call▁end｜><｜tool▁calls▁end｜>";
        assert_eq!(args(&parse(v31, &tools()).calls[0]), json!({"city":"Riga"}));
    }

    #[test]
    fn kimi() {
        let s = "<|tool_calls_section_begin|><|tool_call_begin|>functions.get_weather:0<|tool_call_argument_begin|>{\"city\": \"Lima\"}<|tool_call_end|><|tool_calls_section_end|>";
        let r = parse(s, &tools());
        assert_eq!(r.format, Some("kimi_k2"));
        assert_eq!(args(&r.calls[0]), json!({"city":"Lima"}));
    }

    #[test]
    fn pythonic() {
        let r =
            parse("[get_weather(city=\"San Francisco\", days=2), write_file(path='a.txt', content='hi\\n')]", &tools());
        assert_eq!(r.format, Some("pythonic"));
        assert_eq!(r.calls.len(), 2);
        assert_eq!(args(&r.calls[0]), json!({"city":"San Francisco","days":2}));
        assert_eq!(args(&r.calls[1]), json!({"path":"a.txt","content":"hi\n"}));
    }

    #[test]
    fn unknown_tool_names_rejected() {
        let r = parse("<tool_call>{\"name\": \"rm_rf\", \"arguments\": {}}</tool_call>", &tools());
        assert!(r.calls.is_empty());
        let r = parse("Here is an example: [foo(a=1)]", &tools());
        assert!(r.calls.is_empty());
    }

    #[test]
    fn plain_text_untouched() {
        let r = parse("All done! The tests pass.", &tools());
        assert!(r.calls.is_empty());
        assert_eq!(r.content, "All done! The tests pass.");
    }

    #[test]
    fn markers() {
        assert_eq!(marker_start("hi <tool_call>{"), Some(3));
        assert_eq!(marker_start("hi <tool_ca"), Some(3));
        assert_eq!(marker_start("{\"name\": \"x\"}"), Some(0));
        assert_eq!(marker_start("plain text"), None);
    }
}

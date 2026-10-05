//! Text snapshots of a page: an outline of interactive and text elements
//! with short refs (`[e12]`) the agent can act on.

use std::collections::HashMap;

use serde_json::Value;

use crate::events::truncate;

/// What a ref points at.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RefTarget {
    /// `backendDOMNodeId` from the accessibility tree.
    Backend(i64),
    /// CSS selector (DOM-walk fallback tags elements with `data-odex-ref`).
    Selector(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RefEntry {
    pub target: RefTarget,
    /// e.g. `button "Save"`.
    pub label: String,
}

/// A rendered outline before budgeting.
#[derive(Debug, Default)]
pub(crate) struct Outline {
    /// (indent depth, text without the leading `- `)
    pub lines: Vec<(usize, String)>,
    pub refs: HashMap<String, RefEntry>,
    /// Links whose href the AX tree did not report: (line index, backend node id).
    pub missing_href: Vec<(usize, i64)>,
}

const INTERACTIVE: &[&str] = &[
    "button",
    "link",
    "textbox",
    "searchbox",
    "combobox",
    "listbox",
    "option",
    "checkbox",
    "radio",
    "switch",
    "slider",
    "spinbutton",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "tab",
    "treeitem",
    "PopUpButton",
    "DisclosureTriangle",
    "ToggleButton",
];

const CONTAINERS: &[&str] = &[
    "navigation",
    "main",
    "banner",
    "contentinfo",
    "complementary",
    "form",
    "search",
    "region",
    "dialog",
    "alertdialog",
    "alert",
    "list",
    "listitem",
    "table",
    "row",
    "article",
    "group",
    "tablist",
    "tabpanel",
    "menu",
    "menubar",
    "toolbar",
    "radiogroup",
    "tree",
    "grid",
    "status",
];

/// Containers that are only worth a header line when they have a name.
const NAMED_ONLY: &[&str] = &["region", "group", "form", "article", "status", "tabpanel"];

const INLINE: &[&str] = &[
    "generic",
    "none",
    "presentation",
    "GenericContainer",
    "Ignored",
    "strong",
    "emphasis",
    "code",
    "mark",
    "subscript",
    "superscript",
    "time",
    "Abbr",
    "deletion",
    "insertion",
    "LabelText",
    "label",
    "Section",
    "Div",
    "Pre",
];

enum Item {
    Text(String),
    /// (depth, text, backend id of a link whose href is still unknown)
    Line(usize, String, Option<i64>),
}

type Lines = Vec<(usize, String, Option<i64>)>;

struct Renderer<'a> {
    nodes: HashMap<&'a str, &'a Value>,
    out: Outline,
    next_ref: usize,
}

/// Render `Accessibility.getFullAXTree` nodes into an outline.
pub(crate) fn render_ax_tree(nodes: &[Value]) -> Outline {
    let mut r = Renderer { nodes: HashMap::new(), out: Outline::default(), next_ref: 0 };
    for n in nodes {
        if let Some(id) = n["nodeId"].as_str() {
            r.nodes.insert(id, n);
        }
    }
    let root =
        nodes.iter().find(|n| n.get("parentId").and_then(Value::as_str).is_none()).and_then(|n| n["nodeId"].as_str());
    if let Some(root) = root {
        let items = r.render(root, 0, 0);
        let lines = finalize(items, 0);
        r.push_lines(lines);
    }
    r.out
}

impl<'a> Renderer<'a> {
    fn push_lines(&mut self, lines: Lines) {
        for (depth, text, missing_href) in lines {
            if let Some(backend) = missing_href {
                self.out.missing_href.push((self.out.lines.len(), backend));
            }
            self.out.lines.push((depth, text));
        }
    }

    fn children(&self, node: &'a Value) -> Vec<&'a str> {
        node["childIds"].as_array().map(|ids| ids.iter().filter_map(Value::as_str).collect()).unwrap_or_default()
    }

    fn render_children(&mut self, node: &'a Value, depth: usize, level: usize) -> Vec<Item> {
        let mut items = Vec::new();
        for child in self.children(node) {
            items.extend(self.render(child, depth, level + 1));
        }
        items
    }

    fn render(&mut self, id: &str, depth: usize, level: usize) -> Vec<Item> {
        let Some(node) = self.nodes.get(id).copied() else {
            return Vec::new();
        };
        if level > 250 {
            return Vec::new();
        }
        let role = node["role"]["value"].as_str().unwrap_or_default();
        let name = clean(node["name"]["value"].as_str().unwrap_or_default());
        if node["ignored"].as_bool() == Some(true) {
            return self.render_children(node, depth, level);
        }
        match role {
            "StaticText" | "text" => return if name.is_empty() { Vec::new() } else { vec![Item::Text(name)] },
            "InlineTextBox" => return Vec::new(),
            "LineBreak" => return vec![Item::Text(" ".to_string())],
            _ => {}
        }
        let focusable_named = prop(node, "focusable").and_then(Value::as_bool) == Some(true)
            && !name.is_empty()
            && matches!(role, "generic" | "GenericContainer");
        if INTERACTIVE.contains(&role) || focusable_named {
            return self.interactive(node, role, &name, depth);
        }
        if INLINE.contains(&role) {
            return self.render_children(node, depth, level);
        }
        match role {
            "heading" => {
                let level_attr = prop(node, "level").and_then(Value::as_i64).map(|l| format!(" [level={l}]"));
                let text = format!("heading {}{}", quote(&name), level_attr.unwrap_or_default());
                return vec![Item::Line(depth, text, None)];
            }
            "img" | "image" | "graphics-document" => {
                return if name.is_empty() {
                    Vec::new()
                } else {
                    vec![Item::Line(depth, format!("img {}", quote(&name)), None)]
                };
            }
            _ => {}
        }
        let is_container = CONTAINERS.contains(&role) && (!NAMED_ONLY.contains(&role) || !name.is_empty());
        if is_container {
            let inner = finalize(self.render_children(node, depth + 1, level), depth + 1);
            let header = if name.is_empty() { role.to_string() } else { format!("{role} {}", quote(&name)) };
            // `listitem: text` instead of a header plus a single text line.
            if inner.len() == 1 && inner[0].1.starts_with("text: ") && inner[0].2.is_none() {
                let text = inner[0].1.trim_start_matches("text: ").to_string();
                return vec![Item::Line(depth, format!("{header}: {text}"), None)];
            }
            if inner.is_empty() && name.is_empty() {
                return Vec::new();
            }
            let mut items = vec![Item::Line(depth, format!("{header}:"), None)];
            items.extend(inner.into_iter().map(|(d, t, m)| Item::Line(d, t, m)));
            return items;
        }
        // Block-level element without its own line (paragraph, cell, document...).
        let inner = finalize(self.render_children(node, depth, level), depth);
        inner.into_iter().map(|(d, t, m)| Item::Line(d, t, m)).collect()
    }

    fn interactive(&mut self, node: &'a Value, role: &str, name: &str, depth: usize) -> Vec<Item> {
        let role_label = match role {
            "PopUpButton" => "combobox",
            "DisclosureTriangle" | "ToggleButton" => "button",
            "generic" | "GenericContainer" => "clickable",
            other => other,
        };
        let mut line = format!("{role_label} {}", quote(name));
        let backend = node["backendDOMNodeId"].as_i64();
        if let Some(backend) = backend {
            self.next_ref += 1;
            let r = format!("e{}", self.next_ref);
            line.push_str(&format!(" [{r}]"));
            self.out.refs.insert(
                r,
                RefEntry { target: RefTarget::Backend(backend), label: format!("{role_label} {}", quote(name)) },
            );
        }
        let value = node["value"]["value"].as_str().map(clean).unwrap_or_default();
        if !value.is_empty()
            && matches!(role_label, "textbox" | "searchbox" | "combobox" | "spinbutton" | "slider" | "listbox")
        {
            line.push_str(&format!(" value={}", quote(&truncate(&value, 120))));
        }
        if matches!(role, "checkbox" | "radio" | "switch" | "menuitemcheckbox" | "menuitemradio") {
            let checked = prop(node, "checked").map(tristate).unwrap_or_else(|| "false".to_string());
            line.push_str(&format!(" checked={checked}"));
        }
        if let Some(p) = prop(node, "pressed") {
            line.push_str(&format!(" pressed={}", tristate(p)));
        }
        // A closed native <select> always reports expanded=false; skip that noise.
        if let Some(e) = prop(node, "expanded").and_then(Value::as_bool).filter(|e| *e || role_label != "combobox") {
            line.push_str(&format!(" expanded={e}"));
        }
        if matches!(role, "tab" | "option" | "treeitem")
            && prop(node, "selected").and_then(Value::as_bool) == Some(true)
        {
            line.push_str(" selected");
        }
        if prop(node, "disabled").and_then(Value::as_bool) == Some(true) {
            line.push_str(" disabled");
        }
        if prop(node, "focused").and_then(Value::as_bool) == Some(true) {
            line.push_str(" focused");
        }
        if matches!(role_label, "combobox" | "listbox") {
            let mut options = Vec::new();
            self.collect_options(node, &mut options, 0);
            if !options.is_empty() {
                let shown: Vec<String> = options.iter().take(25).map(|o| quote(o)).collect();
                let more = if options.len() > 25 { format!(", …{} more", options.len() - 25) } else { String::new() };
                line.push_str(&format!(" options=[{}{more}]", shown.join(", ")));
            }
        }
        let mut missing = None;
        if role == "link" {
            match prop(node, "url").and_then(Value::as_str).filter(|u| !u.is_empty()) {
                Some(url) => line.push_str(&format!(" href={}", truncate(url, 200))),
                None => missing = backend,
            }
        }
        vec![Item::Line(depth, line, missing)]
    }

    fn collect_options(&self, node: &'a Value, out: &mut Vec<String>, level: usize) {
        if level > 6 {
            return;
        }
        for child in self.children(node) {
            if let Some(c) = self.nodes.get(child) {
                let role = c["role"]["value"].as_str().unwrap_or_default();
                if matches!(role, "option" | "MenuListOption") {
                    let n = clean(c["name"]["value"].as_str().unwrap_or_default());
                    if !n.is_empty() {
                        out.push(n);
                    }
                } else {
                    self.collect_options(c, out, level + 1);
                }
            }
        }
    }
}

/// Merge runs of inline text into `text:` lines.
fn finalize(items: Vec<Item>, depth: usize) -> Lines {
    let mut out = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Lines| {
        let t = clean(text);
        if !t.is_empty() {
            out.push((depth, format!("text: {}", truncate(&t, 300)), None));
        }
        text.clear();
    };
    for item in items {
        match item {
            Item::Text(t) => {
                if !text.is_empty() && !text.ends_with(' ') && !t.starts_with(' ') {
                    text.push(' ');
                }
                text.push_str(&t);
            }
            Item::Line(d, line, missing) => {
                flush(&mut text, &mut out);
                // Drop a label text line that only repeats this control's name.
                let repeats_label = match (out.last(), quoted_name(&line)) {
                    (Some((ld, last, None)), Some(name)) => *ld == d && last.strip_prefix("text: ") == Some(name),
                    _ => false,
                };
                if repeats_label {
                    out.pop();
                }
                out.push((d, line, missing));
            }
        }
    }
    flush(&mut text, &mut out);
    out
}

/// `Name` from a line like `textbox "Name" [e2]`.
fn quoted_name(line: &str) -> Option<&str> {
    let start = line.find('"')? + 1;
    let len = line[start..].find('"')?;
    Some(&line[start..start + len]).filter(|n| !n.is_empty())
}

fn prop<'v>(node: &'v Value, name: &str) -> Option<&'v Value> {
    node["properties"].as_array()?.iter().find(|p| p["name"] == name).map(|p| &p["value"]["value"])
}

fn tristate(v: &Value) -> String {
    match v {
        Value::Bool(b) => b.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn quote(s: &str) -> String {
    format!("\"{}\"", truncate(s, 120).replace('"', "'"))
}

/// Join outline lines into at most `max_chars`. Returns the text and how
/// many lines were left out.
pub(crate) fn budget_join(lines: &[(usize, String)], max_chars: usize) -> (String, usize) {
    let mut out = String::new();
    for (i, (depth, text)) in lines.iter().enumerate() {
        let line = format!("{}- {}\n", "  ".repeat((*depth).min(12)), text);
        if out.len() + line.len() > max_chars {
            return (out, lines.len() - i);
        }
        out.push_str(&line);
    }
    (out, 0)
}

/// Fallback when the accessibility tree is unavailable: walk the DOM in the
/// page, tag interactive elements with `data-odex-ref` and return a flat list.
pub(crate) const DOM_WALK_JS: &str = r#"(() => {
  const MAX = 4000;
  const out = [];
  let n = 0;
  document.querySelectorAll('[data-odex-ref]').forEach(e => e.removeAttribute('data-odex-ref'));
  const clean = (s, max) => { s = (s || '').replace(/\s+/g, ' ').trim(); return s.length > max ? s.slice(0, max - 1) + '…' : s; };
  const visible = (el) => {
    const s = getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden') return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 || r.height > 0 || s.display === 'contents';
  };
  const INPUT_ROLES = {checkbox:'checkbox', radio:'radio', button:'button', submit:'button', reset:'button', image:'button', range:'slider', number:'spinbutton', search:'searchbox', hidden:null, file:'button', color:'button'};
  const roleOf = (el) => {
    const explicit = el.getAttribute('role');
    if (explicit) return explicit.split(' ')[0];
    const tag = el.tagName.toLowerCase();
    if (tag === 'a' && el.hasAttribute('href')) return 'link';
    if (tag === 'button' || tag === 'summary') return 'button';
    if (tag === 'select') return el.multiple ? 'listbox' : 'combobox';
    if (tag === 'textarea') return 'textbox';
    if (tag === 'input') { const t = (el.type || 'text').toLowerCase(); return t in INPUT_ROLES ? INPUT_ROLES[t] : 'textbox'; }
    if (/^h[1-6]$/.test(tag)) return 'heading';
    if (el.isContentEditable && !(el.parentElement && el.parentElement.isContentEditable)) return 'textbox';
    return null;
  };
  const INTERACTIVE = new Set(['link','button','combobox','listbox','textbox','searchbox','checkbox','radio','slider','spinbutton','switch','tab','menuitem','menuitemcheckbox','menuitemradio','option','treeitem']);
  const nameOf = (el) => {
    const aria = el.getAttribute('aria-label');
    if (aria) return aria;
    const by = el.getAttribute('aria-labelledby');
    if (by) { const t = by.split(/\s+/).map(id => (document.getElementById(id) || {}).innerText || '').join(' ').trim(); if (t) return t; }
    if (el.labels && el.labels.length) return Array.from(el.labels).map(l => l.innerText).join(' ');
    const tag = el.tagName;
    if (tag === 'INPUT' && ['button','submit','reset'].includes(el.type)) return el.value;
    if (!['SELECT','TEXTAREA','INPUT'].includes(tag)) { const t = el.innerText || el.textContent; if (t && t.trim()) return t; }
    return el.getAttribute('placeholder') || el.getAttribute('title') || el.getAttribute('alt') || el.getAttribute('name') || '';
  };
  const walk = (el) => {
    for (const child of el.childNodes) {
      if (out.length >= MAX) return;
      if (child.nodeType === 3) { const t = clean(child.textContent, 300); if (t) out.push({k:'text', t}); continue; }
      if (child.nodeType !== 1) continue;
      const tag = child.tagName.toUpperCase();
      if (['SCRIPT','STYLE','NOSCRIPT','TEMPLATE','SVG','HEAD','IFRAME'].includes(tag)) continue;
      if (!visible(child)) continue;
      const role = roleOf(child);
      if (role && INTERACTIVE.has(role)) {
        const ref = 'e' + (++n);
        child.setAttribute('data-odex-ref', ref);
        const item = {k:'node', ref, role, name: clean(nameOf(child), 120)};
        if (tag === 'SELECT') {
          item.value = (child.selectedOptions[0] || {}).text || '';
          item.options = Array.from(child.options).slice(0, 25).map(o => clean(o.text, 60));
        } else if ('value' in child && ['textbox','searchbox','spinbutton','slider'].includes(role)) {
          item.value = child.type === 'password' ? (child.value ? '••••' : '') : String(child.value || '');
        }
        if (['checkbox','radio','switch'].includes(role)) item.checked = typeof child.checked === 'boolean' ? child.checked : child.getAttribute('aria-checked') === 'true';
        if (role === 'link' && child.href) item.href = String(child.href);
        if (child.disabled) item.disabled = true;
        out.push(item);
        continue;
      }
      if (role === 'heading') { out.push({k:'heading', level: Number(tag[1]) || 2, name: clean(child.innerText, 200)}); continue; }
      if (tag === 'IMG') { if (child.alt) out.push({k:'img', name: clean(child.alt, 120)}); continue; }
      const display = getComputedStyle(child).display;
      const block = !display.startsWith('inline') && display !== 'contents';
      if (block) out.push({k:'break'});
      walk(child);
      if (block) out.push({k:'break'});
    }
  };
  if (document.body) walk(document.body);
  return JSON.stringify(out);
})()"#;

/// Render the JSON produced by [`DOM_WALK_JS`].
pub(crate) fn render_dom_walk(items: &[Value]) -> Outline {
    let mut out = Outline::default();
    let mut text = String::new();
    let flush = |text: &mut String, lines: &mut Vec<(usize, String)>| {
        let t = clean(text);
        if !t.is_empty() {
            lines.push((0, format!("text: {}", truncate(&t, 300))));
        }
        text.clear();
    };
    for item in items {
        match item["k"].as_str().unwrap_or_default() {
            "text" => {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(item["t"].as_str().unwrap_or_default());
            }
            "break" => flush(&mut text, &mut out.lines),
            "heading" => {
                flush(&mut text, &mut out.lines);
                let name = item["name"].as_str().unwrap_or_default();
                out.lines.push((0, format!("heading {} [level={}]", quote(name), item["level"].as_i64().unwrap_or(2))));
            }
            "img" => {
                flush(&mut text, &mut out.lines);
                out.lines.push((0, format!("img {}", quote(item["name"].as_str().unwrap_or_default()))));
            }
            "node" => {
                flush(&mut text, &mut out.lines);
                let r = item["ref"].as_str().unwrap_or_default().to_string();
                let role = item["role"].as_str().unwrap_or("element");
                let name = item["name"].as_str().unwrap_or_default();
                let label = format!("{role} {}", quote(name));
                let mut line = format!("{label} [{r}]");
                if let Some(v) = item["value"].as_str().filter(|v| !v.is_empty()) {
                    line.push_str(&format!(" value={}", quote(&truncate(v, 120))));
                }
                if let Some(c) = item["checked"].as_bool() {
                    line.push_str(&format!(" checked={c}"));
                }
                if let Some(opts) = item["options"].as_array().filter(|o| !o.is_empty()) {
                    let shown: Vec<String> = opts.iter().filter_map(Value::as_str).map(quote).collect();
                    line.push_str(&format!(" options=[{}]", shown.join(", ")));
                }
                if item["disabled"].as_bool() == Some(true) {
                    line.push_str(" disabled");
                }
                if let Some(h) = item["href"].as_str() {
                    line.push_str(&format!(" href={}", truncate(h, 200)));
                }
                if out.lines.last().is_some_and(|(_, last)| last.strip_prefix("text: ") == Some(name)) {
                    out.lines.pop();
                }
                out.lines.push((0, line));
                out.refs.insert(
                    r.clone(),
                    RefEntry { target: RefTarget::Selector(format!("[data-odex-ref=\"{r}\"]")), label },
                );
            }
            _ => {}
        }
    }
    flush(&mut text, &mut out.lines);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, parent: Option<&str>, role: &str, name: &str, children: &[&str]) -> Value {
        let mut n = json!({
            "nodeId": id,
            "ignored": false,
            "role": {"type": "role", "value": role},
            "name": {"type": "computedString", "value": name},
            "childIds": children,
            "backendDOMNodeId": id.parse::<i64>().unwrap() + 100,
            "properties": []
        });
        if let Some(p) = parent {
            n["parentId"] = json!(p);
        }
        n
    }

    fn with_prop(mut n: Value, name: &str, value: Value) -> Value {
        n["properties"].as_array_mut().unwrap().push(json!({"name": name, "value": {"type": "x", "value": value}}));
        n
    }

    fn tree() -> Vec<Value> {
        let mut ignored = node("9", Some("1"), "generic", "", &["10"]);
        ignored["ignored"] = json!(true);
        let mut text_input = node("6", Some("1"), "textbox", "Name", &[]);
        text_input["value"] = json!({"type": "string", "value": "Ada"});
        vec![
            node("1", None, "RootWebArea", "Fixture", &["2", "3", "4", "5", "8", "6", "7", "9", "11"]),
            with_prop(node("2", Some("1"), "heading", "Welcome", &["20"]), "level", json!(1)),
            node("20", Some("2"), "StaticText", "Welcome", &[]),
            node("3", Some("1"), "paragraph", "", &["30", "31", "32"]),
            node("30", Some("3"), "StaticText", "Hello", &[]),
            node("31", Some("3"), "strong", "", &["33"]),
            node("33", Some("31"), "StaticText", "big", &[]),
            node("32", Some("3"), "StaticText", "world.", &[]),
            node("8", Some("1"), "LabelText", "", &["80"]),
            node("80", Some("8"), "StaticText", "Name", &[]),
            node("4", Some("1"), "button", "Click me", &["40"]),
            node("40", Some("4"), "StaticText", "Click me", &[]),
            with_prop(node("5", Some("1"), "link", "Docs", &[]), "url", json!("http://x/docs")),
            text_input,
            with_prop(node("7", Some("1"), "checkbox", "Agree", &[]), "checked", json!("true")),
            ignored,
            node("10", Some("9"), "link", "No href", &[]),
            node("11", Some("1"), "list", "", &["12", "14"]),
            node("12", Some("11"), "listitem", "", &["13"]),
            node("13", Some("12"), "StaticText", "First item", &[]),
            node("14", Some("11"), "listitem", "", &["15", "16"]),
            node("15", Some("14"), "StaticText", "Second", &[]),
            node("16", Some("14"), "combobox", "Color", &["17"]),
            node("17", Some("16"), "MenuListPopup", "", &["18", "19"]),
            node("18", Some("17"), "option", "Red", &[]),
            node("19", Some("17"), "option", "Green", &[]),
        ]
    }

    #[test]
    fn renders_outline_with_refs() {
        let outline = render_ax_tree(&tree());
        let (text, omitted) = budget_join(&outline.lines, 10_000);
        assert_eq!(omitted, 0);
        let expected = "\
- heading \"Welcome\" [level=1]
- text: Hello big world.
- button \"Click me\" [e1]
- link \"Docs\" [e2] href=http://x/docs
- textbox \"Name\" [e3] value=\"Ada\"
- checkbox \"Agree\" [e4] checked=true
- link \"No href\" [e5]
- list:
  - listitem: First item
  - listitem:
    - text: Second
    - combobox \"Color\" [e6] options=[\"Red\", \"Green\"]
";
        assert_eq!(text, expected);
        assert_eq!(outline.refs["e1"].target, RefTarget::Backend(104));
        assert_eq!(outline.refs["e1"].label, "button \"Click me\"");
        assert_eq!(outline.refs.len(), 6);
        // The link without a url property is queued for href lookup.
        assert_eq!(outline.missing_href, vec![(6, 110)]);
    }

    #[test]
    fn budget_truncates_whole_lines() {
        let outline = render_ax_tree(&tree());
        let (text, omitted) = budget_join(&outline.lines, 60);
        assert!(text.len() <= 60);
        assert!(text.ends_with('\n'));
        assert!(omitted > 0);
        assert_eq!(text.lines().count() + omitted, outline.lines.len());
    }

    #[test]
    fn dom_walk_rendering() {
        let items = json!([
            {"k": "heading", "level": 1, "name": "Title"},
            {"k": "text", "t": "Some"}, {"k": "text", "t": "words"}, {"k": "break"},
            {"k": "node", "ref": "e1", "role": "button", "name": "Go"},
            {"k": "node", "ref": "e2", "role": "combobox", "name": "Size", "value": "M", "options": ["S", "M"]},
            {"k": "node", "ref": "e3", "role": "checkbox", "name": "Ok", "checked": false},
            {"k": "img", "name": "Logo"}
        ]);
        let outline = render_dom_walk(items.as_array().unwrap());
        let (text, _) = budget_join(&outline.lines, 10_000);
        assert_eq!(
            text,
            "- heading \"Title\" [level=1]\n- text: Some words\n- button \"Go\" [e1]\n- combobox \"Size\" [e2] value=\"M\" \
             options=[\"S\", \"M\"]\n- checkbox \"Ok\" [e3] checked=false\n- img \"Logo\"\n"
        );
        assert_eq!(outline.refs["e2"].target, RefTarget::Selector("[data-odex-ref=\"e2\"]".into()));
    }
}

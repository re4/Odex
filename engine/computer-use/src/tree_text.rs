//! Compact, token-budgeted text rendering of UI trees (so text-only models can act on them).

use crate::UiNode;

/// Longest value/name snippet shown per line.
const MAX_SNIPPET: usize = 80;
/// Deepest indentation rendered (deeper nodes keep this indent).
const MAX_INDENT: u32 = 24;

fn snippet(s: &str, max: usize) -> String {
    let flat: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.trim();
    if flat.chars().count() <= max {
        flat.replace('"', "'")
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{}…", cut.replace('"', "'"))
    }
}

/// One line for a node: `  [e12] Button "Save" #saveBtn value="…" (focused, toggle=on) @10,20 80x24`.
pub fn node_line(n: &UiNode) -> String {
    let mut line = "  ".repeat(n.depth.min(MAX_INDENT) as usize);
    line.push_str(&format!("[{}] {}", n.id, n.role));
    if !n.name.trim().is_empty() {
        line.push_str(&format!(" \"{}\"", snippet(&n.name, MAX_SNIPPET)));
    }
    if let Some(aid) = n.automation_id.as_deref().filter(|a| !a.is_empty() && *a != n.name) {
        line.push_str(&format!(" #{}", snippet(aid, 40)));
    }
    if let Some(v) = n.value.as_deref().filter(|v| !v.is_empty() && !n.is_password) {
        line.push_str(&format!(" value=\"{}\"", snippet(v, MAX_SNIPPET)));
    }
    let mut flags: Vec<String> = Vec::new();
    if !n.enabled {
        flags.push("disabled".into());
    }
    if n.focused {
        flags.push("focused".into());
    }
    if n.offscreen {
        flags.push("offscreen".into());
    }
    if n.is_password {
        flags.push("password".into());
    }
    if let Some(t) = &n.toggle_state {
        flags.push(format!("toggle={t}"));
    }
    match n.expanded {
        Some(true) => flags.push("expanded".into()),
        Some(false) => flags.push("collapsed".into()),
        None => {}
    }
    if n.selected == Some(true) {
        flags.push("selected".into());
    }
    if !flags.is_empty() {
        line.push_str(&format!(" ({})", flags.join(", ")));
    }
    let b = &n.bounds;
    if b.width > 0.0 && b.height > 0.0 {
        line.push_str(&format!(" @{},{} {}x{}", b.x.round(), b.y.round(), b.width.round(), b.height.round()));
    }
    line
}

/// Render nodes one per line within `max_chars` characters. When the budget runs out the remaining nodes are
/// replaced by a note and `true` is returned.
pub fn render_tree_text(nodes: &[UiNode], max_chars: usize) -> (String, bool) {
    let mut out = String::new();
    let mut used = 0usize;
    for (i, n) in nodes.iter().enumerate() {
        let line = node_line(n);
        let line_len = line.chars().count() + 1;
        let remaining_after = nodes.len() - i - 1;
        // Reserve room for the truncation note unless this is the last node.
        let reserve = if remaining_after > 0 { truncation_note(remaining_after + 1).chars().count() } else { 0 };
        if used + line_len + reserve > max_chars {
            let note = truncation_note(nodes.len() - i);
            out.push_str(&note);
            return (out, true);
        }
        out.push_str(&line);
        out.push('\n');
        used += line_len;
    }
    if out.ends_with('\n') {
        out.pop();
    }
    (out, false)
}

fn truncation_note(remaining: usize) -> String {
    format!("… truncated: {remaining} more elements not shown (use a filter, a smaller depth or a larger max_chars)")
}

/// Lowercase and fold typographic quotes (`Don’t` → `don't`) so models can type plain ASCII queries.
fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{02BC}' | '\u{2032}' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            '\u{00A0}' => ' ',
            c => c,
        })
        .collect::<String>()
        .to_lowercase()
}

/// Case-insensitive substring match on name, role, automation id and class name (typographic quotes fold to
/// ASCII).
pub fn matches_query(n: &UiNode, query: &str) -> bool {
    let query = fold(query);
    if query.is_empty() {
        return true;
    }
    let hit = |s: &str| fold(s).contains(&query);
    hit(&n.name)
        || hit(&n.role)
        || n.automation_id.as_deref().is_some_and(hit)
        || n.class_name.as_deref().is_some_and(hit)
}

/// Given each node's parent index (nodes in DFS order) and which nodes matched, mark the matches and all their
/// ancestors.
pub fn select_with_ancestors(parents: &[Option<usize>], matched: &[bool]) -> Vec<bool> {
    let mut keep = vec![false; parents.len()];
    for (i, &m) in matched.iter().enumerate() {
        if !m {
            continue;
        }
        let mut cur = Some(i);
        while let Some(c) = cur {
            if keep[c] {
                break;
            }
            keep[c] = true;
            cur = parents.get(c).copied().flatten();
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rect;

    fn node(id: &str, role: &str, name: &str, depth: u32) -> UiNode {
        UiNode {
            id: id.into(),
            role: role.into(),
            name: name.into(),
            value: None,
            automation_id: None,
            class_name: None,
            bounds: Rect { x: 10.0, y: 20.0, width: 80.0, height: 24.0 },
            enabled: true,
            focused: false,
            offscreen: false,
            is_password: false,
            toggle_state: None,
            expanded: None,
            selected: None,
            depth,
            patterns: vec![],
        }
    }

    #[test]
    fn line_format() {
        let mut n = node("e12", "Button", "Save", 1);
        assert_eq!(node_line(&n), "  [e12] Button \"Save\" @10,20 80x24");
        n.enabled = false;
        n.focused = true;
        n.toggle_state = Some("on".into());
        n.automation_id = Some("saveBtn".into());
        n.bounds = Rect::default();
        assert_eq!(node_line(&n), "  [e12] Button \"Save\" #saveBtn (disabled, focused, toggle=on)");
        let mut p = node("e3", "Edit", "Password", 0);
        p.is_password = true;
        p.value = Some("secret".into());
        assert!(!node_line(&p).contains("secret"));
        assert!(node_line(&p).contains("password"));
        let mut v = node("e4", "Document", "", 2);
        v.value = Some(format!("line1\nline2 \"quoted\" {}", "x".repeat(200)));
        let l = node_line(&v);
        assert!(l.starts_with("    [e4] Document value=\"line1 line2 'quoted'"), "{l}");
        assert!(l.contains('…'));
    }

    #[test]
    fn budget_truncation() {
        let nodes: Vec<UiNode> = (0..100).map(|i| node(&format!("e{i}"), "ListItem", "Item", 1)).collect();
        let (full, truncated) = render_tree_text(&nodes, usize::MAX);
        assert!(!truncated);
        assert_eq!(full.lines().count(), 100);

        let (text, truncated) = render_tree_text(&nodes, 1000);
        assert!(truncated);
        assert!(text.chars().count() <= 1000, "{}", text.chars().count());
        assert!(text.lines().last().unwrap().starts_with("… truncated:"));
        let shown = text.lines().filter(|l| l.contains("[e")).count();
        assert!(shown > 5 && shown < 100);
        assert!(text.contains(&format!("{} more elements", 100 - shown)));

        // Exactly-fitting text is not truncated.
        let one = vec![node("e1", "Button", "OK", 0)];
        let line_len = node_line(&one[0]).chars().count();
        assert_eq!(render_tree_text(&one, line_len + 1), (node_line(&one[0]), false));
        // A tiny budget yields just the note.
        let (tiny, t) = render_tree_text(&nodes, 5);
        assert!(t);
        assert!(tiny.starts_with("… truncated: 100 more"));
    }

    #[test]
    fn query_matching() {
        let mut n = node("e1", "Button", "Don\u{2019}t Save", 0);
        n.automation_id = Some("SecondaryButton".into());
        n.class_name = Some("Button".into());
        assert!(matches_query(&n, "don't save"));
        assert!(matches_query(&n, "DON'T"));
        assert!(matches_query(&n, "Don\u{2019}t save"));
        assert!(matches_query(&n, "button"));
        assert!(matches_query(&n, "secondary"));
        assert!(matches_query(&n, ""));
        assert!(!matches_query(&n, "cancel"));
    }

    #[test]
    fn ancestors_are_kept() {
        // 0 root; 1 child of 0; 2 child of 1; 3 child of 0; 4 child of 3
        let parents = vec![None, Some(0), Some(1), Some(0), Some(3)];
        let matched = vec![false, false, true, false, false];
        assert_eq!(select_with_ancestors(&parents, &matched), vec![true, true, true, false, false]);
        let matched = vec![false, false, true, false, true];
        assert_eq!(select_with_ancestors(&parents, &matched), vec![true, true, true, true, true]);
        assert_eq!(select_with_ancestors(&parents, &[false; 5]), vec![false; 5]);
    }
}

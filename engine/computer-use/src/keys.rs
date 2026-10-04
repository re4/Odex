//! Platform-neutral parsing of key combos such as `ctrl+shift+s`, `alt+f4` or `ctrl+a ctrl+c`.

use std::fmt;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    /// Windows / Super key.
    Win,
    /// `cmd`/`command`: Ctrl on Windows and Linux, Command on macOS (so prompts written for a Mac still work).
    Cmd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedKey {
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    Space,
    CapsLock,
    NumLock,
    ScrollLock,
    PrintScreen,
    Pause,
    ContextMenu,
    VolumeUp,
    VolumeDown,
    VolumeMute,
    MediaPlayPause,
    MediaNext,
    MediaPrev,
    BrowserBack,
    BrowserForward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Named(NamedKey),
    /// A character key (letters are lowercase).
    Char(char),
    /// F1..=F24.
    Function(u8),
}

/// One chord: modifiers held while `key` is pressed. `key == None` presses just the modifiers (e.g. `win`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCombo {
    pub modifiers: Vec<Modifier>,
    pub key: Option<Key>,
}

impl KeyCombo {
    fn has_command_modifier(&self) -> bool {
        self.modifiers.iter().any(|m| matches!(m, Modifier::Ctrl | Modifier::Alt | Modifier::Win | Modifier::Cmd))
    }

    /// Whether this combo would enter text (a printable key without Ctrl/Alt/Win, or a paste shortcut).
    /// Such combos are refused while a password field has focus.
    pub fn enters_text(&self) -> bool {
        let printable = matches!(self.key, Some(Key::Char(_)) | Some(Key::Named(NamedKey::Space)));
        if printable && !self.has_command_modifier() {
            return true;
        }
        let ctrl_like = self.modifiers.iter().any(|m| matches!(m, Modifier::Ctrl | Modifier::Cmd));
        let paste = ctrl_like && self.key == Some(Key::Char('v'));
        let shift_insert = self.modifiers.contains(&Modifier::Shift) && self.key == Some(Key::Named(NamedKey::Insert));
        paste || shift_insert
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = self
            .modifiers
            .iter()
            .map(|m| match m {
                Modifier::Ctrl => "ctrl",
                Modifier::Alt => "alt",
                Modifier::Shift => "shift",
                Modifier::Win => "win",
                Modifier::Cmd => "cmd",
            })
            .map(str::to_string)
            .collect();
        match self.key {
            Some(Key::Char(c)) => parts.push(c.to_string()),
            Some(Key::Function(n)) => parts.push(format!("f{n}")),
            Some(Key::Named(k)) => parts.push(format!("{k:?}").to_lowercase()),
            None => {}
        }
        f.write_str(&parts.join("+"))
    }
}

/// Maximum number of combos in one `keyboard_keys` call.
pub const MAX_COMBOS: usize = 64;

/// Parse `ctrl+shift+s`, `Enter`, `alt+f4`, `win+r`, `ctrl++`, or several combos separated by whitespace
/// (`ctrl+a ctrl+c`). Spaces around `+` are tolerated (`ctrl + s`).
pub fn parse_key_combos(input: &str) -> Result<Vec<KeyCombo>> {
    let groups = group_tokens(input);
    if groups.is_empty() {
        return Err(Error::Other("empty key combo".into()));
    }
    if groups.len() > MAX_COMBOS {
        return Err(Error::Other(format!("too many key combos ({} > {MAX_COMBOS})", groups.len())));
    }
    groups.iter().map(|g| parse_one(g)).collect()
}

/// Split on whitespace, re-joining pieces around a `+` joiner: `ctrl + s` -> `ctrl+s`.
fn group_tokens(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pending_join = false;
    for tok in input.split_whitespace() {
        match out.last_mut() {
            Some(last) if pending_join || tok.starts_with('+') => last.push_str(tok),
            _ => out.push(tok.to_string()),
        }
        let last = out.last().map(String::as_str).unwrap_or("");
        pending_join = last.len() > 1 && last.ends_with('+') && !last.ends_with("++");
    }
    out
}

fn parse_one(combo: &str) -> Result<KeyCombo> {
    let tokens: Vec<&str> = if combo == "+" {
        vec!["+"]
    } else if let Some(rest) = combo.strip_suffix("++") {
        let mut t: Vec<&str> = if rest.is_empty() { Vec::new() } else { rest.split('+').collect() };
        t.push("+");
        t
    } else {
        combo.split('+').collect()
    };
    let mut modifiers = Vec::new();
    let mut key = None;
    for (i, raw) in tokens.iter().enumerate() {
        if raw.is_empty() {
            return Err(Error::Other(format!("invalid key combo `{combo}`")));
        }
        let is_last = i + 1 == tokens.len();
        if let Some(m) = parse_modifier(raw) {
            if !modifiers.contains(&m) {
                modifiers.push(m);
            }
            continue;
        }
        if !is_last {
            return Err(Error::Other(format!("`{raw}` is not a modifier in `{combo}` (use ctrl, alt, shift, win)")));
        }
        key = Some(parse_key(raw).ok_or_else(|| Error::Other(format!("unknown key `{raw}` in `{combo}`")))?);
    }
    Ok(KeyCombo { modifiers, key })
}

fn parse_modifier(token: &str) -> Option<Modifier> {
    Some(match token.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "ctl" | "lctrl" | "rctrl" => Modifier::Ctrl,
        "alt" | "option" | "opt" | "lalt" | "ralt" | "altgr" => Modifier::Alt,
        "shift" | "lshift" | "rshift" => Modifier::Shift,
        "win" | "windows" | "super" | "meta" | "lwin" | "rwin" => Modifier::Win,
        "cmd" | "command" => Modifier::Cmd,
        _ => return None,
    })
}

fn parse_key(token: &str) -> Option<Key> {
    let mut chars = token.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(Key::Char(c.to_lowercase().next().unwrap_or(c)));
    }
    let t: String = token.to_lowercase().chars().filter(|c| !matches!(c, '_' | '-' | ' ')).collect();
    if let Some(n) = t.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        if (1..=24).contains(&n) {
            return Some(Key::Function(n));
        }
    }
    use NamedKey::*;
    let named = match t.as_str() {
        "enter" | "return" | "ret" => Enter,
        "tab" => Tab,
        "esc" | "escape" => Escape,
        "backspace" | "back" | "bksp" | "bs" => Backspace,
        "delete" | "del" => Delete,
        "insert" | "ins" => Insert,
        "home" => Home,
        "end" => End,
        "pageup" | "pgup" | "prior" => PageUp,
        "pagedown" | "pgdn" | "pgdown" | "next" => PageDown,
        "up" | "arrowup" | "uparrow" => Up,
        "down" | "arrowdown" | "downarrow" => Down,
        "left" | "arrowleft" | "leftarrow" => Left,
        "right" | "arrowright" | "rightarrow" => Right,
        "space" | "spacebar" => Space,
        "capslock" | "caps" => CapsLock,
        "numlock" => NumLock,
        "scrolllock" => ScrollLock,
        "printscreen" | "prtsc" | "prtscn" | "print" | "snapshot" => PrintScreen,
        "pause" | "break" => Pause,
        "menu" | "apps" | "contextmenu" | "application" => ContextMenu,
        "volumeup" => VolumeUp,
        "volumedown" => VolumeDown,
        "volumemute" | "mute" => VolumeMute,
        "playpause" | "mediaplaypause" | "play" => MediaPlayPause,
        "nexttrack" | "medianext" => MediaNext,
        "prevtrack" | "previoustrack" | "mediaprev" | "mediaprevious" => MediaPrev,
        "browserback" => BrowserBack,
        "browserforward" => BrowserForward,
        "plus" => return Some(Key::Char('+')),
        "minus" | "dash" => return Some(Key::Char('-')),
        "comma" => return Some(Key::Char(',')),
        "period" | "dot" => return Some(Key::Char('.')),
        "slash" => return Some(Key::Char('/')),
        "backslash" => return Some(Key::Char('\\')),
        "semicolon" => return Some(Key::Char(';')),
        "quote" | "apostrophe" => return Some(Key::Char('\'')),
        "backtick" | "grave" => return Some(Key::Char('`')),
        "equals" | "equal" => return Some(Key::Char('=')),
        _ => return None,
    };
    Some(Key::Named(named))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(s: &str) -> KeyCombo {
        let mut v = parse_key_combos(s).unwrap();
        assert_eq!(v.len(), 1, "{s}");
        v.remove(0)
    }

    #[test]
    fn parses_basic_combos() {
        assert_eq!(one("ctrl+s"), KeyCombo { modifiers: vec![Modifier::Ctrl], key: Some(Key::Char('s')) });
        assert_eq!(
            one("Ctrl+Shift+S"),
            KeyCombo { modifiers: vec![Modifier::Ctrl, Modifier::Shift], key: Some(Key::Char('s')) }
        );
        assert_eq!(one("alt+f4"), KeyCombo { modifiers: vec![Modifier::Alt], key: Some(Key::Function(4)) });
        assert_eq!(one("enter"), KeyCombo { modifiers: vec![], key: Some(Key::Named(NamedKey::Enter)) });
        assert_eq!(one("Return").key, Some(Key::Named(NamedKey::Enter)));
        assert_eq!(one("win+r"), KeyCombo { modifiers: vec![Modifier::Win], key: Some(Key::Char('r')) });
        assert_eq!(one("cmd+c").modifiers, vec![Modifier::Cmd]);
        assert_eq!(one("page_down").key, Some(Key::Named(NamedKey::PageDown)));
        assert_eq!(one("PgUp").key, Some(Key::Named(NamedKey::PageUp)));
        assert_eq!(one("f12").key, Some(Key::Function(12)));
        assert_eq!(one("F24").key, Some(Key::Function(24)));
        assert_eq!(one("shift+tab").modifiers, vec![Modifier::Shift]);
        assert_eq!(one("escape").key, Some(Key::Named(NamedKey::Escape)));
    }

    #[test]
    fn parses_plus_and_modifier_only() {
        assert_eq!(one("ctrl++"), KeyCombo { modifiers: vec![Modifier::Ctrl], key: Some(Key::Char('+')) });
        assert_eq!(one("+"), KeyCombo { modifiers: vec![], key: Some(Key::Char('+')) });
        assert_eq!(one("ctrl+plus").key, Some(Key::Char('+')));
        assert_eq!(one("win"), KeyCombo { modifiers: vec![Modifier::Win], key: None });
        assert_eq!(one("ctrl+shift"), KeyCombo { modifiers: vec![Modifier::Ctrl, Modifier::Shift], key: None });
        assert_eq!(one("ctrl+-").key, Some(Key::Char('-')));
    }

    #[test]
    fn parses_sequences_and_spaces() {
        let v = parse_key_combos("ctrl+a  ctrl+c").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].key, Some(Key::Char('c')));
        assert_eq!(parse_key_combos("ctrl + s").unwrap(), vec![one("ctrl+s")]);
        assert_eq!(parse_key_combos(" ctrl +shift+ s ").unwrap(), vec![one("ctrl+shift+s")]);
        assert_eq!(parse_key_combos("tab tab enter").unwrap().len(), 3);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_key_combos("").is_err());
        assert!(parse_key_combos("   ").is_err());
        assert!(parse_key_combos("ctrl+banana").is_err());
        assert!(parse_key_combos("s+ctrl").is_err());
        assert!(parse_key_combos("ctrl++s").is_err());
        assert!(parse_key_combos("f25").is_err());
        assert!(parse_key_combos(&"a ".repeat(MAX_COMBOS + 1)).is_err());
    }

    #[test]
    fn text_entering_combos() {
        assert!(one("a").enters_text());
        assert!(one("shift+a").enters_text());
        assert!(one("space").enters_text());
        assert!(one("ctrl+v").enters_text());
        assert!(one("cmd+v").enters_text());
        assert!(one("shift+insert").enters_text());
        assert!(!one("ctrl+a").enters_text());
        assert!(!one("enter").enters_text());
        assert!(!one("tab").enters_text());
        assert!(!one("alt+f4").enters_text());
        assert!(!one("win").enters_text());
    }

    #[test]
    fn display_round_trips() {
        assert_eq!(one("Ctrl+Shift+S").to_string(), "ctrl+shift+s");
        assert_eq!(one("alt+F4").to_string(), "alt+f4");
        assert_eq!(one("pagedown").to_string(), "pagedown");
    }
}

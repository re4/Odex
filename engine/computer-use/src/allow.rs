//! Per-app allowlist matching and detection of windows the agent must never touch.

use crate::WindowInfo;

/// True when the window's executable is in `allowed_apps` (see [`app_matches`]).
pub fn is_allowed(window: &WindowInfo, allowed_apps: &[String]) -> bool {
    app_matches(&window.app, allowed_apps)
}

/// Case-insensitive executable-name match. Entries may be a file name (`notepad.exe`), a bare stem
/// (`notepad`, which also matches `notepad.exe`) or a full path (only its file name is compared).
pub fn app_matches(app: &str, allowed_apps: &[String]) -> bool {
    let app = file_name(app).to_ascii_lowercase();
    if app.is_empty() {
        return false;
    }
    let app_stem = app.strip_suffix(".exe").unwrap_or(&app);
    allowed_apps.iter().any(|entry| {
        let entry = file_name(entry.trim()).to_ascii_lowercase();
        if entry.is_empty() {
            return false;
        }
        entry == app || (!entry.contains('.') && entry == app_stem)
    })
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Processes that host UAC, credential, lock-screen or sign-in UI.
const SENSITIVE_PROCESSES: &[&str] = &[
    "consent.exe",
    "credentialuibroker.exe",
    "logonui.exe",
    "lockapp.exe",
    "winlogon.exe",
    "credwiz.exe",
    "useraccountcontrolsettings.exe",
    "useraccountbroker.exe",
];

/// Window classes used by credential / secure prompts.
const SENSITIVE_CLASSES: &[&str] = &[
    "credential dialog xaml host",
    "$$$secure uap dummy window class for interim dialog",
    "windows.ui.core.corewindow:credentialui",
];

/// Exact (case-insensitive) titles of security prompts.
const SENSITIVE_TITLES: &[&str] = &["windows security", "user account control", "credential manager ui host"];

/// Why a window must not be touched by the agent, if it is a UAC / credential / sign-in prompt.
/// `process` is the executable file name, `class` the window class, `title` the window title.
pub fn sensitive_reason(process: &str, class: &str, title: &str) -> Option<String> {
    let process_l = file_name(process).to_ascii_lowercase();
    if SENSITIVE_PROCESSES.contains(&process_l.as_str()) {
        return Some(format!("{process} (UAC, credential or sign-in prompt)"));
    }
    let class_l = class.to_ascii_lowercase();
    if SENSITIVE_CLASSES.contains(&class_l.as_str()) {
        return Some(format!("credential prompt window ({class})"));
    }
    let title_l = title.trim().to_lowercase();
    if SENSITIVE_TITLES.contains(&title_l.as_str()) {
        return Some(format!("\"{}\" prompt", title.trim()));
    }
    None
}

/// Quote one argument for a Windows command line (the rules `CommandLineToArgvW` / the MSVC CRT parse).
pub fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rect;

    fn win(app: &str) -> WindowInfo {
        WindowInfo {
            handle: "1".into(),
            title: "t".into(),
            app: app.into(),
            pid: 1,
            bounds: Rect::default(),
            minimized: false,
            focused: false,
            allowed: false,
        }
    }

    #[test]
    fn allowlist_matching() {
        let allowed = vec!["notepad.exe".to_string(), "Code".to_string(), r"C:\Tools\paint.EXE".to_string()];
        assert!(is_allowed(&win("Notepad.exe"), &allowed));
        assert!(is_allowed(&win("NOTEPAD.EXE"), &allowed));
        assert!(is_allowed(&win("code.exe"), &allowed));
        assert!(is_allowed(&win("Code.exe"), &allowed));
        assert!(is_allowed(&win("paint.exe"), &allowed));
        assert!(!is_allowed(&win("notepad++.exe"), &allowed));
        assert!(!is_allowed(&win("calc.exe"), &allowed));
        assert!(!is_allowed(&win(""), &allowed));
        assert!(!is_allowed(&win("notepad.exe"), &[]));
        assert!(!is_allowed(&win("notepad.exe"), &["".to_string(), "  ".to_string()]));
        // A stem with a dot must match exactly.
        assert!(!app_matches("foo.bar.exe", &["foo.bar".to_string()]));
        assert!(app_matches(r"C:\Windows\System32\notepad.exe", &["notepad".to_string()]));
    }

    #[test]
    fn sensitive_windows() {
        assert!(sensitive_reason("consent.exe", "", "").is_some());
        assert!(sensitive_reason("CredentialUIBroker.exe", "x", "y").is_some());
        assert!(sensitive_reason(r"C:\Windows\System32\LogonUI.exe", "", "").is_some());
        assert!(sensitive_reason("foo.exe", "Credential Dialog Xaml Host", "").is_some());
        assert!(sensitive_reason("foo.exe", "", "Windows Security").is_some());
        assert!(sensitive_reason("notepad.exe", "Notepad", "Untitled - Notepad").is_none());
    }

    #[test]
    fn windows_arg_quoting() {
        assert_eq!(quote_windows_arg("plain"), "plain");
        assert_eq!(quote_windows_arg(""), "\"\"");
        assert_eq!(quote_windows_arg("two words"), "\"two words\"");
        assert_eq!(quote_windows_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_windows_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_windows_arg(r"C:\no_space\"), r"C:\no_space\");
    }
}

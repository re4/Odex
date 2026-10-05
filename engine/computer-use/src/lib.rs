//! Odex computer use: let an agent see and operate desktop applications.
//!
//! Windows is the first platform (UI Automation, `PrintWindow`/`BitBlt` capture and `SendInput`). macOS
//! (AX) and Linux (AT-SPI) expose the same API but every action currently fails with [`Error::Unsupported`].
//!
//! Design notes:
//! * Everything is synchronous. The engine calls these functions from `spawn_blocking`.
//! * UI Automation runs on one dedicated MTA worker thread that owns all COM objects and the element-id
//!   cache, so callers may use any thread (including STA or uninitialised ones).
//! * Coordinates are physical pixels in virtual-screen space (the primary monitor's top-left is `(0, 0)`, other
//!   monitors can be negative). The process opts into `PER_MONITOR_AWARE_V2` so Win32 and UIA agree on that.
//! * Safety: a process-wide [`kill_switch`] is checked first by every action, typing into password fields is
//!   refused, and nothing acts while a UAC / credential prompt or the secure desktop is up. The per-app
//!   allowlist is enforced by the engine with [`is_allowed`] (use [`window_at`] to find the app under a mouse
//!   target and [`expect_foreground`] before keyboard input).
//! * Prefer the non-intrusive paths (background capture, [`ui_tree`] + [`ui_action`]); [`mouse`] and the
//!   keyboard functions move the real cursor / need the target in the foreground.

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use odex_protocol::{Appshot, ComputerUseStatus, CoordinateSpace, Rect, WindowInfo};

mod allow;
mod geometry;
mod keys;
mod tree_text;

#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as platform;

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
use unsupported as platform;

pub use allow::{app_matches, is_allowed, quote_windows_arg, sensitive_reason};
pub use geometry::{fit_within, map_point, rect_center};
pub use keys::{parse_key_combos, Key, KeyCombo, Modifier, NamedKey};
pub use tree_text::{matches_query, node_line, render_tree_text, select_with_ancestors};

/// Errors returned by computer-use actions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("computer use is not supported on this platform yet")]
    Unsupported,
    #[error("the computer-use kill switch is engaged; all actions are blocked until the user releases it")]
    KillSwitchEngaged,
    #[error("refusing to type into a password field; ask the user to enter it themselves")]
    PasswordField,
    #[error("a secure prompt is active ({0}); ask the user to complete it themselves")]
    SecureDesktop(String),
    #[error("not allowed: {0}")]
    NotAllowed(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("windows error: {0}")]
    Win(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Process-wide emergency stop. The engine engages it (e.g. on the user's hotkey); every action checks it first.
pub mod kill_switch {
    use std::sync::atomic::{AtomicBool, Ordering};

    static ENGAGED: AtomicBool = AtomicBool::new(false);

    /// Block every computer-use action until [`release`] is called.
    pub fn engage() {
        ENGAGED.store(true, Ordering::SeqCst);
    }

    pub fn release() {
        ENGAGED.store(false, Ordering::SeqCst);
    }

    pub fn is_engaged() -> bool {
        ENGAGED.load(Ordering::SeqCst)
    }

    /// `Err(KillSwitchEngaged)` when engaged. Long-running actions also call this between steps.
    pub fn check() -> crate::Result<()> {
        if is_engaged() {
            Err(crate::Error::KillSwitchEngaged)
        } else {
            Ok(())
        }
    }
}

/// What to capture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureTarget {
    /// The whole virtual screen (all monitors).
    Screen,
    /// One monitor; index into [`monitors`] (0 is the primary monitor).
    Monitor(usize),
    /// A top-level window by HWND. Works for occluded/background windows; minimized windows must be restored.
    Window(isize),
    /// A region in physical virtual-screen pixels.
    Region(Rect),
}

/// A PNG screenshot plus the mapping back to physical screen coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Screenshot {
    pub png: Vec<u8>,
    /// Size of the PNG (after downscaling).
    pub width: u32,
    pub height: u32,
    /// Physical pixels per screenshot pixel.
    pub scale_x: f64,
    pub scale_y: f64,
    /// Physical virtual-screen coordinate of the screenshot's top-left pixel.
    pub origin_x: i32,
    pub origin_y: i32,
    /// The captured window, for window captures.
    pub window: Option<WindowInfo>,
    /// How the image was obtained when it matters (e.g. a fallback that may include occluding windows).
    pub note: Option<String>,
}

impl Screenshot {
    /// `data:image/png;base64,...`
    pub fn data_url(&self) -> String {
        use base64::Engine as _;
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&self.png))
    }

    /// Captured area in physical pixels.
    pub fn physical_size(&self) -> (u32, u32) {
        ((self.width as f64 * self.scale_x).round() as u32, (self.height as f64 * self.scale_y).round() as u32)
    }
}

/// A display monitor in physical virtual-screen pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub index: usize,
    pub bounds: Rect,
    pub work_area: Rect,
    pub primary: bool,
    /// Effective DPI / 96 (1.5 = 150%).
    pub scale: f64,
}

/// One UI Automation element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiNode {
    /// `e12`. Stable per window: the same element keeps its id across `ui_tree`/`find_elements` calls and an id
    /// never refers to a different element.
    pub id: String,
    /// Control type, e.g. `Button`, `Edit`, `Document`.
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub automation_id: Option<String>,
    pub class_name: Option<String>,
    /// Physical virtual-screen pixels.
    pub bounds: Rect,
    pub enabled: bool,
    pub focused: bool,
    pub offscreen: bool,
    pub is_password: bool,
    /// `on`, `off` or `indeterminate`.
    pub toggle_state: Option<String>,
    pub expanded: Option<bool>,
    pub selected: Option<bool>,
    /// 0 for the window itself.
    pub depth: u32,
    /// Supported patterns: `invoke`, `value`, `toggle`, `expandCollapse`, `selectionItem`, `scrollItem`, ...
    pub patterns: Vec<String>,
}

/// A token-budgeted UI Automation tree for one window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiTree {
    pub window: WindowInfo,
    pub nodes: Vec<UiNode>,
    /// One indented line per node, e.g. `  [e12] Button "Save" @10,20 80x24`; truncated to the budget with a note.
    pub text: String,
    pub truncated: bool,
}

/// Non-intrusive UI Automation actions (they don't move the mouse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiActionKind {
    Invoke,
    Focus,
    SetValue(String),
    Toggle,
    Expand,
    Collapse,
    Select,
    ScrollIntoView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// Real mouse input at physical virtual-screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Move,
    Click {
        button: MouseButton,
        double: bool,
    },
    /// Press at the start point, move to `(to_x, to_y)`, release.
    Drag {
        to_x: i32,
        to_y: i32,
    },
    /// Wheel notches at the point; positive `dy` scrolls down, positive `dx` scrolls right.
    Scroll {
        dx: i32,
        dy: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowOp {
    Focus,
    /// Move the visible frame's top-left to physical `(x, y)`.
    Move {
        x: i32,
        y: i32,
    },
    /// Resize the visible frame (physical pixels).
    Resize {
        width: i32,
        height: i32,
    },
    Minimize,
    Maximize,
    Restore,
    /// Politely ask the window to close (`WM_CLOSE`); the app may show a "save changes?" prompt.
    Close,
}

/// Which window [`wait_for_window`] waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowMatch {
    Pid(u32),
    /// Executable name (`notepad.exe` or `notepad`), case-insensitive.
    App(String),
}

/// Opt the process into per-monitor DPI awareness (v2). Idempotent; every entry point also calls it.
pub fn init_dpi_awareness() {
    platform::init_dpi_awareness()
}

/// Visible top-level windows with titles (cloaked and tool windows excluded), front to back.
/// `allowed` is computed from `allowed_apps`. Not blocked by the kill switch (metadata only).
pub fn list_windows(allowed_apps: &[String]) -> Result<Vec<WindowInfo>> {
    platform::list_windows(allowed_apps)
}

/// The foreground window (`allowed` is always false here; use [`is_allowed`]).
pub fn foreground_window() -> Option<WindowInfo> {
    platform::foreground_window()
}

/// Metadata for one window (`allowed` computed from `allowed_apps`).
pub fn window_info(hwnd: isize, allowed_apps: &[String]) -> Result<WindowInfo> {
    platform::window_info(hwnd, allowed_apps)
}

/// The top-level window under a physical virtual-screen point (use it to enforce the app allowlist before
/// mouse input). `allowed` is computed from `allowed_apps`.
pub fn window_at(x: i32, y: i32, allowed_apps: &[String]) -> Result<Option<WindowInfo>> {
    platform::window_at(x, y, allowed_apps)
}

/// Monitors, primary first, then left-to-right.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    platform::monitors()
}

/// Capture `target` and downscale so the longest edge is at most `max_edge_px` (0 = no limit).
pub fn screenshot(target: CaptureTarget, max_edge_px: u32) -> Result<Screenshot> {
    kill_switch::check()?;
    platform::screenshot(target, max_edge_px)
}

/// UI Automation tree of a window. `filter` keeps nodes whose name/role/automation id/class contains it
/// (case-insensitive) plus their ancestors. The text rendering is capped at `max_chars`.
pub fn ui_tree(hwnd: isize, max_depth: u32, filter: Option<&str>, max_chars: usize) -> Result<UiTree> {
    kill_switch::check()?;
    platform::ui_tree(hwnd, max_depth, filter, max_chars)
}

/// Elements of a window matching `query` (same matching as the `ui_tree` filter, no ancestors). Ids are shared
/// with `ui_tree`.
pub fn find_elements(hwnd: isize, query: &str) -> Result<Vec<UiNode>> {
    kill_switch::check()?;
    platform::find_elements(hwnd, query)
}

/// Run a UI Automation pattern on an element returned by `ui_tree`/`find_elements` for that window.
pub fn ui_action(hwnd: isize, element_id: &str, action: UiActionKind) -> Result<String> {
    kill_switch::check()?;
    platform::ui_action(hwnd, element_id, action)
}

/// Real mouse input via `SendInput` at physical virtual-screen coordinates.
pub fn mouse(action: MouseAction, x: i32, y: i32) -> Result<()> {
    kill_switch::check()?;
    platform::mouse(action, x, y)
}

/// Background-friendly click: posts `WM_*BUTTON*` messages to the deepest child window of `hwnd` under the
/// physical point without moving the cursor or activating the window. Works for many classic Win32
/// controls; XAML/WinUI, Chromium/Electron and DirectX apps usually ignore posted mouse messages, so fall back
/// to [`ui_action`] or [`mouse`].
pub fn post_click(hwnd: isize, x: i32, y: i32, button: MouseButton, double: bool) -> Result<()> {
    kill_switch::check()?;
    platform::post_click(hwnd, x, y, button, double)
}

/// Type text into the focused control: one paced Unicode key press per character (layout, Caps Lock, dead keys
/// and IMEs don't matter), Enter/Tab as real keys. Refused for password fields, also when Enter/Tab moves focus
/// into one part-way. Roughly 15 ms per character; prefer `ui_action(SetValue)` or the clipboard for long text.
pub fn keyboard_type(text: &str) -> Result<()> {
    kill_switch::check()?;
    platform::keyboard_type(text)
}

/// Press key combos like `ctrl+s`, `alt+f4`, `enter`, `win+r`; several may be separated by spaces
/// (`ctrl+a ctrl+c`). Printable keys and paste shortcuts are refused while a password field has focus.
pub fn keyboard_keys(combo: &str) -> Result<()> {
    kill_switch::check()?;
    let combos = parse_key_combos(combo)?;
    platform::keyboard_keys(&combos)
}

pub fn window_op(hwnd: isize, op: WindowOp) -> Result<()> {
    kill_switch::check()?;
    platform::window_op(hwnd, op)
}

/// `Ok` if `hwnd` (or a window it owns) is the foreground window. Call before real keyboard input to make
/// sure keystrokes reach the intended app.
pub fn expect_foreground(hwnd: isize) -> Result<()> {
    platform::expect_foreground(hwnd)
}

/// Start an app: an executable name on PATH/App Paths (`notepad`), a full path, or a shell target such as
/// `shell:AppsFolder\<AUMID>` or `ms-settings:`. Returns the pid (0 when the shell didn't report one).
pub fn launch(app: &str, args: &[String]) -> Result<u32> {
    kill_switch::check()?;
    platform::launch(app, args)
}

/// Poll until a matching top-level window exists.
pub fn wait_for_window(target: &WindowMatch, timeout: Duration) -> Result<WindowInfo> {
    kill_switch::check()?;
    platform::wait_for_window(target, timeout)
}

/// Launch and wait for a *new* top-level window from it (by pid, or by executable name for apps whose launcher
/// process hands off to another process, like the Store Notepad).
pub fn launch_and_wait(app: &str, args: &[String], timeout: Duration) -> Result<WindowInfo> {
    kill_switch::check()?;
    platform::launch_and_wait(app, args, timeout)
}

/// Unicode text on the clipboard ("" when it holds no text).
pub fn clipboard_get() -> Result<String> {
    kill_switch::check()?;
    platform::clipboard_get()
}

pub fn clipboard_set(text: &str) -> Result<()> {
    kill_switch::check()?;
    platform::clipboard_set(text)
}

/// `Err(SecureDesktop)` if the secure desktop (UAC, lock screen) is active or the foreground window is a
/// UAC/credential prompt. Input actions call this themselves.
pub fn security_check() -> Result<()> {
    platform::security_check()
}

/// Screenshot of a window (default: the foreground window) plus, optionally, its compact UI tree.
pub fn appshot(hwnd: Option<isize>, include_ui_tree: bool, max_edge_px: u32) -> Result<Appshot> {
    kill_switch::check()?;
    let hwnd = match hwnd {
        Some(h) => h,
        None => {
            let fg = foreground_window().ok_or_else(|| Error::NotFound("no foreground window".into()))?;
            fg.handle.parse::<isize>().map_err(|_| Error::Other("bad window handle".into()))?
        }
    };
    let shot = screenshot(CaptureTarget::Window(hwnd), max_edge_px)?;
    let window = match shot.window.clone() {
        Some(w) => w,
        None => window_info(hwnd, &[])?,
    };
    let ui_tree = if include_ui_tree { ui_tree(hwnd, 30, None, APPSHOT_TREE_CHARS).ok().map(|t| t.text) } else { None };
    Ok(Appshot {
        title: window.title,
        app: window.app,
        image_url: shot.data_url(),
        width: shot.width,
        height: shot.height,
        ui_tree,
    })
}

/// Character budget for the UI tree attached to an appshot.
pub const APPSHOT_TREE_CHARS: usize = 8_000;

/// Status for the settings UI.
pub fn status(enabled: bool, allowed_apps: &[String]) -> ComputerUseStatus {
    let supported = cfg!(windows);
    let mut notes = Vec::new();
    if !supported {
        notes.push("Computer use is implemented for Windows first; this platform is not supported yet.".to_string());
    } else {
        notes.push(
            "Windows: UI Automation for trees/actions, PrintWindow for background window capture, SendInput \
             for real input."
                .to_string(),
        );
        notes.push(
            "Elevated (administrator) windows cannot be controlled from a non-elevated engine (UIPI).".to_string(),
        );
        if let Err(e) = security_check() {
            notes.push(e.to_string());
        }
    }
    if kill_switch::is_engaged() {
        notes.push("Kill switch engaged: all actions are blocked.".to_string());
    }
    ComputerUseStatus {
        supported,
        enabled,
        platform: std::env::consts::OS.to_string(),
        allowed_apps: allowed_apps.to_vec(),
        active_thread_id: None,
        killed: kill_switch::is_engaged(),
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_switch_blocks_actions() {
        kill_switch::engage();
        assert!(kill_switch::is_engaged());
        assert_eq!(kill_switch::check(), Err(Error::KillSwitchEngaged));
        assert_eq!(screenshot(CaptureTarget::Screen, 100).unwrap_err(), Error::KillSwitchEngaged);
        assert_eq!(keyboard_keys("f24").unwrap_err(), Error::KillSwitchEngaged);
        assert_eq!(ui_action(1, "e1", UiActionKind::Invoke).unwrap_err(), Error::KillSwitchEngaged);
        assert_eq!(clipboard_get().unwrap_err(), Error::KillSwitchEngaged);
        assert!(status(true, &[]).killed);
        kill_switch::release();
        assert!(!kill_switch::is_engaged());
        assert_eq!(kill_switch::check(), Ok(()));
    }

    #[test]
    fn data_url_prefix() {
        let shot = Screenshot {
            png: vec![1, 2, 3],
            width: 10,
            height: 5,
            scale_x: 2.0,
            scale_y: 2.0,
            origin_x: 0,
            origin_y: 0,
            window: None,
            note: None,
        };
        assert_eq!(shot.data_url(), "data:image/png;base64,AQID");
        assert_eq!(shot.physical_size(), (20, 10));
    }

    #[test]
    fn status_reports_platform() {
        let s = status(false, &["notepad.exe".to_string()]);
        assert_eq!(s.supported, cfg!(windows));
        assert_eq!(s.allowed_apps, vec!["notepad.exe".to_string()]);
        assert!(!s.platform.is_empty());
    }
}

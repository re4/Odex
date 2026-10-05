//! macOS (AX) and Linux (AT-SPI) backends are not implemented yet; every action reports `Unsupported`.

use std::time::Duration;

use crate::{
    CaptureTarget, Error, KeyCombo, MonitorInfo, MouseAction, MouseButton, Result, Screenshot, UiActionKind, UiNode,
    UiTree, WindowInfo, WindowMatch, WindowOp,
};

pub fn init_dpi_awareness() {}

pub fn list_windows(_allowed_apps: &[String]) -> Result<Vec<WindowInfo>> {
    Err(Error::Unsupported)
}

pub fn foreground_window() -> Option<WindowInfo> {
    None
}

pub fn window_info(_hwnd: isize, _allowed_apps: &[String]) -> Result<WindowInfo> {
    Err(Error::Unsupported)
}

pub fn window_at(_x: i32, _y: i32, _allowed_apps: &[String]) -> Result<Option<WindowInfo>> {
    Err(Error::Unsupported)
}

pub fn monitors() -> Result<Vec<MonitorInfo>> {
    Err(Error::Unsupported)
}

pub fn screenshot(_target: CaptureTarget, _max_edge_px: u32) -> Result<Screenshot> {
    Err(Error::Unsupported)
}

pub fn ui_tree(_hwnd: isize, _max_depth: u32, _filter: Option<&str>, _max_chars: usize) -> Result<UiTree> {
    Err(Error::Unsupported)
}

pub fn find_elements(_hwnd: isize, _query: &str) -> Result<Vec<UiNode>> {
    Err(Error::Unsupported)
}

pub fn ui_action(_hwnd: isize, _element_id: &str, _action: UiActionKind) -> Result<String> {
    Err(Error::Unsupported)
}

pub fn mouse(_action: MouseAction, _x: i32, _y: i32) -> Result<()> {
    Err(Error::Unsupported)
}

pub fn post_click(_hwnd: isize, _x: i32, _y: i32, _button: MouseButton, _double: bool) -> Result<()> {
    Err(Error::Unsupported)
}

pub fn keyboard_type(_text: &str) -> Result<()> {
    Err(Error::Unsupported)
}

pub fn keyboard_keys(_combos: &[KeyCombo]) -> Result<()> {
    Err(Error::Unsupported)
}

pub fn window_op(_hwnd: isize, _op: WindowOp) -> Result<()> {
    Err(Error::Unsupported)
}

pub fn expect_foreground(_hwnd: isize) -> Result<()> {
    Err(Error::Unsupported)
}

pub fn launch(_app: &str, _args: &[String]) -> Result<u32> {
    Err(Error::Unsupported)
}

pub fn wait_for_window(_target: &WindowMatch, _timeout: Duration) -> Result<WindowInfo> {
    Err(Error::Unsupported)
}

pub fn launch_and_wait(_app: &str, _args: &[String], _timeout: Duration) -> Result<WindowInfo> {
    Err(Error::Unsupported)
}

pub fn clipboard_get() -> Result<String> {
    Err(Error::Unsupported)
}

pub fn clipboard_set(_text: &str) -> Result<()> {
    Err(Error::Unsupported)
}

pub fn security_check() -> Result<()> {
    Err(Error::Unsupported)
}

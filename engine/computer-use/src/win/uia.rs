//! UI Automation: trees, element search and pattern-based actions.
//!
//! All UIA COM objects live on one dedicated MTA worker thread. Public functions send closures to it and wait
//! (with a timeout, so a hung app can't wedge the engine; a timed-out worker is abandoned and replaced). The
//! worker keeps an element-id cache per window keyed by UIA runtime id: the same element keeps its id across
//! `ui_tree` / `find_elements` calls and an id never refers to a different element. Elements that disappeared
//! fail with `NotFound`. The cache is dropped when the window closes or grows past `MAX_CACHED`.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{Interface, BSTR, HSTRING, VARIANT};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData,
};
use windows::Win32::UI::Accessibility::*;

use super::window::{info_for, init_dpi_awareness, require_window};
use super::{rect_from, security, to_hwnd};
use crate::{matches_query, render_tree_text, select_with_ancestors, Error, Result, UiActionKind, UiNode, UiTree};

/// Deepest tree level walked.
const MAX_DEPTH: u32 = 64;
/// Most elements walked per call (protects against huge or cyclic trees).
const NODE_CAP: usize = 5_000;
/// Time budget for one walk.
const WALK_BUDGET: Duration = Duration::from_secs(20);
/// Longest value kept per node.
const MAX_VALUE: usize = 4_096;
/// How long a caller waits for the worker.
const CALL_TIMEOUT: Duration = Duration::from_secs(45);
/// Cached elements per window before the id cache is reset.
const MAX_CACHED: usize = 20_000;
/// `UIA_E_ELEMENTNOTAVAILABLE`
const E_ELEMENT_NOT_AVAILABLE: u32 = 0x8004_0201;

type Job = Box<dyn FnOnce(&mut std::result::Result<Uia, String>) + Send + 'static>;

static WORKER: Mutex<Option<Sender<Job>>> = Mutex::new(None);

fn spawn_worker() -> Result<Sender<Job>> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("odex-uia".into())
        .spawn(move || {
            init_dpi_awareness();
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let mut state = Uia::new().map_err(|e| e.to_string());
            while let Ok(job) = rx.recv() {
                job(&mut state);
            }
        })
        .map_err(|e| Error::Other(format!("could not start the UI Automation thread: {e}")))?;
    Ok(tx)
}

fn reset_worker() {
    *WORKER.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Run `f` on the UIA worker thread.
fn run<R: Send + 'static>(f: impl FnOnce(&mut Uia) -> Result<R> + Send + 'static) -> Result<R> {
    let (tx, rx) = mpsc::channel::<Result<R>>();
    let job: Job = Box::new(move |state| {
        let r = match state {
            Ok(uia) => f(uia),
            Err(e) => Err(Error::Win(format!("UI Automation is unavailable: {e}"))),
        };
        let _ = tx.send(r);
    });
    {
        let mut guard = WORKER.lock().unwrap_or_else(|p| p.into_inner());
        let sender = match guard.take() {
            Some(s) => s,
            None => spawn_worker()?,
        };
        match sender.send(job) {
            Ok(()) => *guard = Some(sender),
            Err(mpsc::SendError(job)) => {
                // The worker died (a panic); start a fresh one.
                let fresh = spawn_worker()?;
                fresh.send(job).map_err(|_| Error::Other("the UI Automation thread is unavailable".into()))?;
                *guard = Some(fresh);
            }
        }
    }
    match rx.recv_timeout(CALL_TIMEOUT) {
        Ok(r) => r,
        Err(RecvTimeoutError::Timeout) => {
            reset_worker();
            Err(Error::Other("UI Automation timed out (the app may be hung); try again or use a screenshot".into()))
        }
        Err(RecvTimeoutError::Disconnected) => {
            reset_worker();
            Err(Error::Other("the UI Automation thread stopped unexpectedly".into()))
        }
    }
}

/// Element ids for one window.
#[derive(Default)]
struct ElementCache {
    elements: HashMap<String, IUIAutomationElement>,
    by_runtime_id: HashMap<Vec<i32>, String>,
    next: u32,
}

impl ElementCache {
    fn register(&mut self, e: &IUIAutomationElement) -> String {
        let rid = runtime_id(e);
        if let Some(id) = rid.as_ref().and_then(|r| self.by_runtime_id.get(r)) {
            let id = id.clone();
            self.elements.insert(id.clone(), e.clone());
            return id;
        }
        self.next += 1;
        let id = format!("e{}", self.next);
        self.elements.insert(id.clone(), e.clone());
        if let Some(rid) = rid {
            self.by_runtime_id.insert(rid, id.clone());
        }
        id
    }
}

struct Walked {
    elem: IUIAutomationElement,
    parent: Option<usize>,
    node: UiNode,
}

struct Uia {
    auto: IUIAutomation,
    walker: IUIAutomationTreeWalker,
    request: IUIAutomationCacheRequest,
    windows: HashMap<isize, ElementCache>,
}

const BASE_PROPERTIES: &[UIA_PROPERTY_ID] = &[
    UIA_ControlTypePropertyId,
    UIA_NamePropertyId,
    UIA_AutomationIdPropertyId,
    UIA_ClassNamePropertyId,
    UIA_BoundingRectanglePropertyId,
    UIA_IsEnabledPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_IsPasswordPropertyId,
    UIA_ValueValuePropertyId,
    UIA_ToggleToggleStatePropertyId,
    UIA_ExpandCollapseExpandCollapseStatePropertyId,
    UIA_SelectionItemIsSelectedPropertyId,
];

/// Pattern availability properties and the names reported in `UiNode::patterns`.
const PATTERNS: &[(UIA_PROPERTY_ID, &str)] = &[
    (UIA_IsInvokePatternAvailablePropertyId, "invoke"),
    (UIA_IsValuePatternAvailablePropertyId, "value"),
    (UIA_IsTogglePatternAvailablePropertyId, "toggle"),
    (UIA_IsExpandCollapsePatternAvailablePropertyId, "expandCollapse"),
    (UIA_IsSelectionItemPatternAvailablePropertyId, "selectionItem"),
    (UIA_IsScrollItemPatternAvailablePropertyId, "scrollItem"),
    (UIA_IsScrollPatternAvailablePropertyId, "scroll"),
    (UIA_IsRangeValuePatternAvailablePropertyId, "rangeValue"),
    (UIA_IsTextPatternAvailablePropertyId, "text"),
    (UIA_IsWindowPatternAvailablePropertyId, "window"),
    (UIA_IsLegacyIAccessiblePatternAvailablePropertyId, "legacy"),
];

impl Uia {
    fn new() -> Result<Self> {
        unsafe {
            let auto: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
                .or_else(|_| CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER))?;
            if let Ok(a2) = auto.cast::<IUIAutomation2>() {
                let _ = a2.SetConnectionTimeout(5_000);
                let _ = a2.SetTransactionTimeout(10_000);
            }
            let walker = auto.ControlViewWalker()?;
            let request = auto.CreateCacheRequest()?;
            for p in BASE_PROPERTIES.iter().chain(PATTERNS.iter().map(|(p, _)| p)) {
                request.AddProperty(*p)?;
            }
            Ok(Uia { auto, walker, request, windows: HashMap::new() })
        }
    }

    fn root(&self, hwnd: isize) -> Result<IUIAutomationElement> {
        unsafe { self.auto.ElementFromHandleBuildCache(to_hwnd(hwnd), &self.request) }.map_err(Error::from)
    }

    fn walk(&self, root: &IUIAutomationElement, max_depth: u32) -> Vec<Walked> {
        let deadline = Instant::now() + WALK_BUDGET;
        let mut out = vec![Walked { elem: root.clone(), parent: None, node: read_node(root, 0) }];
        self.walk_children(0, max_depth, deadline, &mut out);
        out
    }

    fn walk_children(&self, idx: usize, max_depth: u32, deadline: Instant, out: &mut Vec<Walked>) {
        let depth = out[idx].node.depth;
        if depth >= max_depth || out.len() >= NODE_CAP || Instant::now() >= deadline {
            return;
        }
        let parent = out[idx].elem.clone();
        let mut child = unsafe { self.walker.GetFirstChildElementBuildCache(&parent, &self.request) }.ok();
        while let Some(c) = child {
            if out.len() >= NODE_CAP || Instant::now() >= deadline {
                return;
            }
            out.push(Walked { elem: c.clone(), parent: Some(idx), node: read_node(&c, depth + 1) });
            let ci = out.len() - 1;
            self.walk_children(ci, max_depth, deadline, out);
            child = unsafe { self.walker.GetNextSiblingElementBuildCache(&c, &self.request) }.ok();
        }
    }

    /// Forget windows that no longer exist.
    fn prune(&mut self) {
        self.windows.retain(|&h, _| require_window(h).is_ok());
    }

    fn tree(&mut self, hwnd: isize, max_depth: u32, filter: Option<String>) -> Result<Vec<UiNode>> {
        let root = self.root(hwnd)?;
        let walked = self.walk(&root, max_depth);
        let keep = match filter.as_deref() {
            Some(q) => {
                let parents: Vec<Option<usize>> = walked.iter().map(|w| w.parent).collect();
                let matched: Vec<bool> = walked.iter().map(|w| matches_query(&w.node, q)).collect();
                select_with_ancestors(&parents, &matched)
            }
            None => vec![true; walked.len()],
        };
        self.prune();
        let cache = self.windows.entry(hwnd).or_default();
        if cache.elements.len() > MAX_CACHED {
            *cache = ElementCache::default();
        }
        let mut nodes = Vec::new();
        for (w, keep) in walked.into_iter().zip(keep) {
            if !keep {
                continue;
            }
            let mut node = w.node;
            node.id = cache.register(&w.elem);
            fill_text_value(&w.elem, &mut node);
            nodes.push(node);
        }
        Ok(nodes)
    }

    fn find(&mut self, hwnd: isize, query: String) -> Result<Vec<UiNode>> {
        let root = self.root(hwnd)?;
        let walked = self.walk(&root, MAX_DEPTH);
        self.prune();
        let cache = self.windows.entry(hwnd).or_default();
        if cache.elements.len() > MAX_CACHED {
            *cache = ElementCache::default();
        }
        let mut nodes = Vec::new();
        for w in walked {
            if !matches_query(&w.node, &query) {
                continue;
            }
            let mut node = w.node;
            node.id = cache.register(&w.elem);
            fill_text_value(&w.elem, &mut node);
            nodes.push(node);
        }
        Ok(nodes)
    }

    fn element(&self, hwnd: isize, id: &str) -> Result<IUIAutomationElement> {
        self.windows.get(&hwnd).and_then(|c| c.elements.get(id)).cloned().ok_or_else(|| {
            Error::NotFound(format!(
                "element {id} in window {hwnd}; call ui_tree (or find_elements) for this window first"
            ))
        })
    }

    fn action(&self, hwnd: isize, id: &str, action: UiActionKind) -> Result<String> {
        let e = self.element(hwnd, id)?;
        let err = |x: windows::core::Error| uia_error(id, x);
        let label = unsafe {
            let role = control_type_name(e.CurrentControlType().map_err(err)?.0);
            let name = e.CurrentName().map(|n| n.to_string()).unwrap_or_default();
            if name.is_empty() {
                format!("[{id}] {role}")
            } else {
                format!("[{id}] {role} \"{name}\"")
            }
        };
        unsafe {
            match action {
                UiActionKind::Invoke => {
                    if let Some(p) = pattern::<IUIAutomationInvokePattern>(&e, UIA_InvokePatternId) {
                        p.Invoke().map_err(err)?;
                        return Ok(format!("Invoked {label}"));
                    }
                    if let Some(p) = pattern::<IUIAutomationTogglePattern>(&e, UIA_TogglePatternId) {
                        p.Toggle().map_err(err)?;
                        return Ok(format!("Toggled {label} (it has no Invoke pattern)"));
                    }
                    if let Some(p) = pattern::<IUIAutomationSelectionItemPattern>(&e, UIA_SelectionItemPatternId) {
                        p.Select().map_err(err)?;
                        return Ok(format!("Selected {label} (it has no Invoke pattern)"));
                    }
                    if let Some(p) = pattern::<IUIAutomationExpandCollapsePattern>(&e, UIA_ExpandCollapsePatternId) {
                        let state = p.CurrentExpandCollapseState().map_err(err)?;
                        if state == ExpandCollapseState_Collapsed {
                            p.Expand().map_err(err)?;
                            return Ok(format!("Expanded {label} (it has no Invoke pattern)"));
                        }
                        p.Collapse().map_err(err)?;
                        return Ok(format!("Collapsed {label} (it has no Invoke pattern)"));
                    }
                    if let Some(p) =
                        pattern::<IUIAutomationLegacyIAccessiblePattern>(&e, UIA_LegacyIAccessiblePatternId)
                    {
                        p.DoDefaultAction().map_err(err)?;
                        return Ok(format!("Performed the default action of {label}"));
                    }
                    Err(Error::Other(format!(
                        "{label} supports no invoke-like pattern; click the center of its bounds with the mouse instead"
                    )))
                }
                UiActionKind::Focus => {
                    e.SetFocus().map_err(err)?;
                    Ok(format!("Focused {label}"))
                }
                UiActionKind::SetValue(value) => {
                    if e.CurrentIsPassword().map_err(err)?.as_bool() {
                        return Err(Error::PasswordField);
                    }
                    if let Some(p) = pattern::<IUIAutomationValuePattern>(&e, UIA_ValuePatternId) {
                        if p.CurrentIsReadOnly().map_err(err)?.as_bool() {
                            return Err(Error::Other(format!("{label} is read-only")));
                        }
                        p.SetValue(&BSTR::from(value.as_str())).map_err(err)?;
                        return Ok(format!("Set the value of {label} ({} characters)", value.chars().count()));
                    }
                    if let Some(p) = pattern::<IUIAutomationRangeValuePattern>(&e, UIA_RangeValuePatternId) {
                        if let Ok(v) = value.trim().parse::<f64>() {
                            p.SetValue(v).map_err(err)?;
                            return Ok(format!("Set {label} to {v}"));
                        }
                    }
                    if let Some(p) =
                        pattern::<IUIAutomationLegacyIAccessiblePattern>(&e, UIA_LegacyIAccessiblePatternId)
                    {
                        if p.SetValue(&HSTRING::from(value.as_str())).is_ok() {
                            return Ok(format!("Set the value of {label} via IAccessible"));
                        }
                    }
                    Err(Error::Other(format!("{label} has no Value pattern; focus it and use keyboard typing instead")))
                }
                UiActionKind::Toggle => {
                    let p = pattern::<IUIAutomationTogglePattern>(&e, UIA_TogglePatternId)
                        .ok_or_else(|| Error::Other(format!("{label} cannot be toggled (no Toggle pattern)")))?;
                    p.Toggle().map_err(err)?;
                    let state = match p.CurrentToggleState().map(|s| s.0) {
                        Ok(0) => "off",
                        Ok(1) => "on",
                        Ok(_) => "indeterminate",
                        Err(_) => "unknown",
                    };
                    Ok(format!("Toggled {label}; it is now {state}"))
                }
                UiActionKind::Expand | UiActionKind::Collapse => {
                    let p = pattern::<IUIAutomationExpandCollapsePattern>(&e, UIA_ExpandCollapsePatternId)
                        .ok_or_else(|| Error::Other(format!("{label} cannot expand/collapse")))?;
                    if action == UiActionKind::Expand {
                        p.Expand().map_err(err)?;
                        Ok(format!("Expanded {label}"))
                    } else {
                        p.Collapse().map_err(err)?;
                        Ok(format!("Collapsed {label}"))
                    }
                }
                UiActionKind::Select => {
                    if let Some(p) = pattern::<IUIAutomationSelectionItemPattern>(&e, UIA_SelectionItemPatternId) {
                        p.Select().map_err(err)?;
                        return Ok(format!("Selected {label}"));
                    }
                    if let Some(p) =
                        pattern::<IUIAutomationLegacyIAccessiblePattern>(&e, UIA_LegacyIAccessiblePatternId)
                    {
                        // SELFLAG_TAKEFOCUS | SELFLAG_TAKESELECTION
                        p.Select(0x1 | 0x2).map_err(err)?;
                        return Ok(format!("Selected {label} via IAccessible"));
                    }
                    Err(Error::Other(format!("{label} cannot be selected (no SelectionItem pattern)")))
                }
                UiActionKind::ScrollIntoView => {
                    let p = pattern::<IUIAutomationScrollItemPattern>(&e, UIA_ScrollItemPatternId)
                        .ok_or_else(|| Error::Other(format!("{label} has no ScrollItem pattern")))?;
                    p.ScrollIntoView().map_err(err)?;
                    Ok(format!("Scrolled {label} into view"))
                }
            }
        }
    }
}

fn uia_error(id: &str, e: windows::core::Error) -> Error {
    if e.code().0 as u32 == E_ELEMENT_NOT_AVAILABLE {
        Error::NotFound(format!("element {id} is no longer available (the UI changed); call ui_tree again"))
    } else {
        Error::from(e)
    }
}

unsafe fn pattern<T: Interface>(e: &IUIAutomationElement, id: UIA_PATTERN_ID) -> Option<T> {
    e.GetCurrentPatternAs::<T>(id).ok()
}

fn runtime_id(e: &IUIAutomationElement) -> Option<Vec<i32>> {
    unsafe {
        let sa = e.GetRuntimeId().ok()?;
        if sa.is_null() {
            return None;
        }
        let mut out = None;
        if let (Ok(lb), Ok(ub)) = (SafeArrayGetLBound(sa, 1), SafeArrayGetUBound(sa, 1)) {
            let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
            if SafeArrayAccessData(sa, &mut data).is_ok() {
                let n = (ub - lb + 1).max(0) as usize;
                if !data.is_null() {
                    out = Some(std::slice::from_raw_parts(data as *const i32, n).to_vec());
                }
                let _ = SafeArrayUnaccessData(sa);
            }
        }
        let _ = SafeArrayDestroy(sa);
        out
    }
}

fn vt(v: &VARIANT) -> u16 {
    unsafe { v.as_raw().Anonymous.Anonymous.vt }
}

const VT_I4: u16 = 3;
const VT_BSTR: u16 = 8;
const VT_BOOL: u16 = 11;

fn cached(e: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<VARIANT> {
    unsafe { e.GetCachedPropertyValue(id) }.ok()
}

fn cached_bool(e: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<bool> {
    let v = cached(e, id)?;
    (vt(&v) == VT_BOOL).then(|| bool::try_from(&v).ok()).flatten()
}

fn cached_i32(e: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<i32> {
    let v = cached(e, id)?;
    (vt(&v) == VT_I4).then(|| i32::try_from(&v).ok()).flatten()
}

fn cached_string(e: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<String> {
    let v = cached(e, id)?;
    (vt(&v) == VT_BSTR).then(|| BSTR::try_from(&v).ok().map(|b| b.to_string())).flatten()
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        s.chars().take(max).collect()
    }
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

/// Build a node from the element's cached properties (no cross-process calls).
fn read_node(e: &IUIAutomationElement, depth: u32) -> UiNode {
    unsafe {
        let control_type = e.CachedControlType().map(|c| c.0).unwrap_or(0);
        let is_password = e.CachedIsPassword().map(|b| b.as_bool()).unwrap_or(false);
        let patterns: Vec<String> =
            PATTERNS.iter().filter(|(p, _)| cached_bool(e, *p).unwrap_or(false)).map(|(_, n)| n.to_string()).collect();
        let has = |name: &str| patterns.iter().any(|p| p == name);
        let value = if !is_password && has("value") {
            cached_string(e, UIA_ValueValuePropertyId).map(|v| truncate_chars(v, MAX_VALUE))
        } else {
            None
        };
        let toggle_state = if has("toggle") {
            cached_i32(e, UIA_ToggleToggleStatePropertyId).map(|s| {
                match s {
                    0 => "off",
                    1 => "on",
                    _ => "indeterminate",
                }
                .to_string()
            })
        } else {
            None
        };
        let expanded = if has("expandCollapse") {
            cached_i32(e, UIA_ExpandCollapseExpandCollapseStatePropertyId).and_then(|s| match s {
                0 => Some(false),
                1 | 2 => Some(true),
                _ => None, // leaf node
            })
        } else {
            None
        };
        let selected = if has("selectionItem") { cached_bool(e, UIA_SelectionItemIsSelectedPropertyId) } else { None };
        UiNode {
            id: String::new(),
            role: control_type_name(control_type).to_string(),
            name: e.CachedName().map(|b| b.to_string()).unwrap_or_default(),
            value,
            automation_id: non_empty(e.CachedAutomationId().ok().map(|b| b.to_string())),
            class_name: non_empty(e.CachedClassName().ok().map(|b| b.to_string())),
            bounds: e.CachedBoundingRectangle().map(rect_from).unwrap_or_default(),
            enabled: e.CachedIsEnabled().map(|b| b.as_bool()).unwrap_or(true),
            focused: e.CachedHasKeyboardFocus().map(|b| b.as_bool()).unwrap_or(false),
            offscreen: e.CachedIsOffscreen().map(|b| b.as_bool()).unwrap_or(false),
            is_password,
            toggle_state,
            expanded,
            selected,
            depth,
            patterns,
        }
    }
}

/// Documents/edits without a Value pattern: read their text through the Text pattern.
fn fill_text_value(e: &IUIAutomationElement, node: &mut UiNode) {
    if node.value.is_some() || node.is_password || !node.patterns.iter().any(|p| p == "text") {
        return;
    }
    if node.role != "Document" && node.role != "Edit" {
        return;
    }
    unsafe {
        if let Some(p) = pattern::<IUIAutomationTextPattern>(e, UIA_TextPatternId) {
            if let Ok(text) = p.DocumentRange().and_then(|r| r.GetText(MAX_VALUE as i32)) {
                node.value = Some(text.to_string());
            }
        }
    }
}

fn control_type_name(id: i32) -> &'static str {
    const NAMES: [&str; 41] = [
        "Button",
        "Calendar",
        "CheckBox",
        "ComboBox",
        "Edit",
        "Hyperlink",
        "Image",
        "ListItem",
        "List",
        "Menu",
        "MenuBar",
        "MenuItem",
        "ProgressBar",
        "RadioButton",
        "ScrollBar",
        "Slider",
        "Spinner",
        "StatusBar",
        "Tab",
        "TabItem",
        "Text",
        "ToolBar",
        "ToolTip",
        "Tree",
        "TreeItem",
        "Custom",
        "Group",
        "Thumb",
        "DataGrid",
        "DataItem",
        "Document",
        "SplitButton",
        "Window",
        "Pane",
        "Header",
        "HeaderItem",
        "Table",
        "TitleBar",
        "Separator",
        "SemanticZoom",
        "AppBar",
    ];
    usize::try_from(id - 50_000).ok().and_then(|i| NAMES.get(i)).copied().unwrap_or("Unknown")
}

pub fn ui_tree(hwnd: isize, max_depth: u32, filter: Option<&str>, max_chars: usize) -> Result<UiTree> {
    init_dpi_awareness();
    let h = require_window(hwnd)?;
    let window = info_for(h, &[]);
    let filter = filter.map(|f| f.trim().to_lowercase()).filter(|f| !f.is_empty());
    let depth = max_depth.min(MAX_DEPTH);
    let nodes = run(move |u| u.tree(hwnd, depth, filter))?;
    let (text, truncated) = render_tree_text(&nodes, max_chars);
    Ok(UiTree { window, nodes, text, truncated })
}

pub fn find_elements(hwnd: isize, query: &str) -> Result<Vec<UiNode>> {
    init_dpi_awareness();
    require_window(hwnd)?;
    let query = query.trim().to_lowercase();
    run(move |u| u.find(hwnd, query))
}

pub fn ui_action(hwnd: isize, element_id: &str, action: UiActionKind) -> Result<String> {
    init_dpi_awareness();
    let h = require_window(hwnd)?;
    security::security_check()?;
    security::check_target(h)?;
    let id = element_id.trim().trim_start_matches('[').trim_end_matches(']').to_string();
    run(move |u| u.action(hwnd, &id, action))
}

/// Whether the element with keyboard focus is a password field. No focused element counts as "no".
pub(crate) fn focused_is_password() -> Result<bool> {
    run(|u| unsafe {
        match u.auto.GetFocusedElement() {
            Ok(e) => Ok(e.CurrentIsPassword()?.as_bool()),
            Err(_) => Ok(false),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::control_type_name;

    #[test]
    fn control_type_names() {
        assert_eq!(control_type_name(50000), "Button");
        assert_eq!(control_type_name(50030), "Document");
        assert_eq!(control_type_name(50032), "Window");
        assert_eq!(control_type_name(50040), "AppBar");
        assert_eq!(control_type_name(50041), "Unknown");
        assert_eq!(control_type_name(0), "Unknown");
    }
}

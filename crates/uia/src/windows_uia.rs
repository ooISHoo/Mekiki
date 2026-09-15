//! Windows: element search via UI Automation.
//!
//! # What makes this fast
//!
//! UIA calls cross a process boundary. Written naively, "take one element →
//! read its name → read its rectangle" is three round trips, so a thousand
//! elements means three thousand round trips and takes seconds.
//!
//! Four things avoid that.
//!
//! 1. **A cache request** ([`IUIAutomationCacheRequest`]) fetches the properties
//!    we need in bulk as part of the search. One round trip covers it.
//! 2. **`AutomationElementMode_None`** stops it materialising element
//!    references. Clicking happens by coordinate here, so they are not needed.
//! 3. **Partitioning by top-level window.** Throwing `TreeScope_Descendants` at
//!    the desktop root targets every element of every window. Instead the
//!    windows are enumerated first and only those overlapping the search area
//!    are descended into.
//! 4. **Not using UIA to enumerate the windows.** This was the biggest trap.
//!    `FindAll(TreeScope_Children)` on the root element covers every top-level
//!    window including the invisible shell ones, and **measured 3.3 seconds**
//!    (while scanning all the contents together took 0.9). Taking only the
//!    visible windows with `EnumWindows` and entering UIA through
//!    `ElementFromHandle` brings that under 1ms.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    AutomationElementMode_Full, AutomationElementMode_None, CUIAutomation, IUIAutomation,
    IUIAutomationCacheRequest, IUIAutomationCondition, IUIAutomationElement,
    IUIAutomationTextPattern, IUIAutomationValuePattern, TreeScope_Subtree,
    UIA_AutomationIdPropertyId, UIA_BoundingRectanglePropertyId, UIA_ButtonControlTypeId,
    UIA_CONTROLTYPE_ID, UIA_CheckBoxControlTypeId, UIA_ClassNamePropertyId,
    UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId, UIA_EditControlTypeId,
    UIA_HyperlinkControlTypeId, UIA_ImageControlTypeId, UIA_IsEnabledPropertyId,
    UIA_IsOffscreenPropertyId, UIA_IsPasswordPropertyId, UIA_ListControlTypeId,
    UIA_ListItemControlTypeId, UIA_MenuItemControlTypeId, UIA_NamePropertyId,
    UIA_RadioButtonControlTypeId, UIA_TabControlTypeId, UIA_TabItemControlTypeId,
    UIA_TextControlTypeId, UIA_TextPatternId, UIA_TreeControlTypeId, UIA_TreeItemControlTypeId,
    UIA_ValuePatternId, UIA_WindowControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetWindow, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    IsIconic, IsWindowVisible,
};
use windows::core::{BOOL, BSTR};

use crate::{Bounds, ControlType, Element, ElementFinder, ElementValue, Query, UiaError};

fn os_err(e: windows::core::Error) -> UiaError {
    UiaError::Os(format!("{e}"))
}

/// A top-level window to be scanned.
struct TopLevel {
    hwnd: HWND,
    title: String,
    bounds: Bounds,
}

pub struct WindowsUia {
    automation: IUIAutomation,
    /// For scanning the contents (no element references needed).
    element_cache: IUIAutomationCacheRequest,
    /// Full element references plus ValuePattern, used only by read_value.
    value_cache: IUIAutomationCacheRequest,
}

impl WindowsUia {
    pub fn new() -> Result<Self, UiaError> {
        // MTA is recommended for UIA clients. In an STA it can deadlock against
        // the target's UI thread.
        //
        // Both "already initialised" (S_FALSE) and "initialised in a different
        // mode" (RPC_E_CHANGED_MODE) are accepted, because a host such as Tauri
        // may have taken COM first.
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_err() && hr != windows::Win32::Foundation::RPC_E_CHANGED_MODE {
                return Err(UiaError::ComInit(format!("{hr:?}")));
            }
        }

        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .map_err(os_err)?;

        let element_cache = build_cache(&automation, AutomationElementMode_None, false)?;
        let value_cache = build_cache(&automation, AutomationElementMode_Full, true)?;

        Ok(Self {
            automation,
            element_cache,
            value_cache,
        })
    }

    /// Scan inside one window.
    fn collect_in_window(
        &self,
        window: &IUIAutomationElement,
        condition: &IUIAutomationCondition,
        query: &Query,
        out: &mut Vec<Element>,
        values: bool,
    ) -> Result<(), UiaError> {
        // Subtree is "self plus descendants". This rather than Descendants, so
        // the window itself can be picked up with `ui:type=window`.
        let cache = if values {
            &self.value_cache
        } else {
            &self.element_cache
        };
        let array = unsafe { window.FindAllBuildCache(TreeScope_Subtree, condition, cache) }
            .map_err(os_err)?;

        let count = unsafe { array.Length() }.map_err(os_err)?;

        for i in 0..count {
            if out.len() >= query.limit {
                log::debug!("UIA: stopping, hit the limit of {}", query.limit);
                return Ok(());
            }
            let Ok(element) = (unsafe { array.GetElement(i) }) else {
                continue;
            };
            if let Some(e) = self.read_element(&element, query, values) {
                out.push(e);
            }
        }
        Ok(())
    }

    /// Read the cached properties into an [`Element`]. `None` if it does not
    /// meet the conditions.
    ///
    /// Every read here comes **from the cache**, so nothing crosses the process
    /// boundary.
    fn read_element(
        &self,
        element: &IUIAutomationElement,
        query: &Query,
        values: bool,
    ) -> Option<Element> {
        let rect = unsafe { element.CachedBoundingRectangle() }.ok()?;
        let bounds = to_bounds(rect);
        if bounds.is_empty() {
            // An element without a rectangle (a logical grouping, say) cannot be clicked.
            return None;
        }
        if let Some(w) = query.within
            && !bounds.intersects(w)
        {
            return None;
        }

        let offscreen = unsafe { element.CachedIsOffscreen() }
            .map(|b| b.as_bool())
            .unwrap_or(false);
        if offscreen && !query.include_offscreen {
            return None;
        }

        let automation_id = unsafe { element.CachedAutomationId() }
            .map(|s| s.to_string())
            .unwrap_or_default();
        if let Some(want) = &query.automation_id
            && &automation_id != want
        {
            return None;
        }

        let is_password = values
            && unsafe { element.CachedIsPassword() }
                .map(|b| b.as_bool())
                .unwrap_or(false);
        let (value, value_source) = if values && !is_password {
            let via_value = unsafe {
                element.GetCachedPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            }
            .ok()
            .and_then(|pattern| unsafe { pattern.CachedValue() }.ok())
            .map(|s| s.to_string());
            if via_value.is_some() {
                (via_value, Some("value"))
            } else {
                let via_text = (|| {
                    let pattern = unsafe {
                        element.GetCachedPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                    }
                    .ok()?;
                    let range = unsafe { pattern.DocumentRange() }.ok()?;
                    unsafe { range.GetText(-1) }.ok().map(|s| s.to_string())
                })();
                let source = via_text.as_ref().map(|_| "text");
                (via_text, source)
            }
        } else {
            (None, None)
        };

        Some(Element {
            name: unsafe { element.CachedName() }
                .map(|s| s.to_string())
                .unwrap_or_default(),
            automation_id,
            class_name: unsafe { element.CachedClassName() }
                .map(|s| s.to_string())
                .unwrap_or_default(),
            control_type: unsafe { element.CachedControlType() }
                .map(from_control_type_id)
                .unwrap_or(ControlType::Other(0)),
            bounds,
            enabled: unsafe { element.CachedIsEnabled() }
                .map(|b| b.as_bool())
                .unwrap_or(true),
            offscreen,
            runtime_id: runtime_id(element),
            value,
            value_source,
            is_password,
        })
    }

    /// Build the conditions that UIA itself filters on.
    ///
    /// This filtering happens **in the target process**, so the number of
    /// elements that come back shrinks. Far faster than receiving them on the
    /// Rust side and discarding them.
    ///
    /// Only conditions decidable by exact match can be pushed down. `name` is
    /// evaluated by substring and edit distance (to stay on the same scale as
    /// OCR), so it cannot go here.
    fn condition_for(&self, query: &Query) -> Result<IUIAutomationCondition, UiaError> {
        let mut conditions: Vec<IUIAutomationCondition> = Vec::new();

        if let Some(t) = query.control_type {
            let value = VARIANT::from(to_control_type_id(t).0);
            conditions.push(
                unsafe {
                    self.automation
                        .CreatePropertyCondition(UIA_ControlTypePropertyId, &value)
                }
                .map_err(os_err)?,
            );
        }

        // AutomationId is an exact match. It is the identifier the application
        // assigned, so when it can be specified it is both the fastest and the
        // most stable option.
        if let Some(id) = &query.automation_id {
            let value = VARIANT::from(BSTR::from(id.as_str()));
            conditions.push(
                unsafe {
                    self.automation
                        .CreatePropertyCondition(UIA_AutomationIdPropertyId, &value)
                }
                .map_err(os_err)?,
            );
        }

        match conditions.len() {
            0 => unsafe { self.automation.CreateTrueCondition() }.map_err(os_err),
            1 => Ok(conditions.remove(0)),
            _ => unsafe {
                self.automation
                    .CreateAndCondition(&conditions[0], &conditions[1])
            }
            .map_err(os_err),
        }
    }
}

/// Enumerate visible top-level windows in Z order (frontmost first).
///
/// Nearly the same rules as the equivalent in `mekiki-capture`, redeclared here
/// because this one has to return the `HWND` (to pass to `ElementFromHandle`).
///
/// **The point is that it does not use the UIA tree.** See the note at the top
/// of the module.
fn enumerate_windows() -> Result<Vec<TopLevel>, UiaError> {
    /// The value an `EnumWindows` callback returns to keep enumerating.
    const CONTINUE: BOOL = BOOL(1);

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: a &mut Vec<TopLevel> is passed through lparam below.
        let out = unsafe { &mut *(lparam.0 as *mut Vec<TopLevel>) };

        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return CONTINUE;
            }
            // The title is only used for diagnostics.
            //
            // A "skip windows without a title" condition was added once and
            // then **removed**. The Windows 11 taskbar (`Shell_TrayWnd`) has an
            // empty title, and the moment that condition went in the Start
            // button became impossible to find. Dropdown lists are untitled too.
            let title = window_title(hwnd);

            if is_phantom(hwnd) {
                log::trace!("UIA: skipping, it is cloaked: {title}");
                return CONTINUE;
            }

            let Some(bounds) = window_bounds(hwnd) else {
                return CONTINUE;
            };

            out.push(TopLevel {
                hwnd,
                title,
                bounds,
            });
        }

        CONTINUE
    }

    let mut out: Vec<TopLevel> = Vec::new();
    unsafe { EnumWindows(Some(enum_proc), LPARAM(&raw mut out as isize)) }
        .map_err(|e| UiaError::Os(format!("EnumWindows failed: {e}")))?;
    Ok(out)
}

/// Whether a window looks visible but is not actually something to operate on.
///
/// `IsWindowVisible` alone is not enough. What was found sitting on the desktop
/// in practice were windows **cloaked by DWM**: ones on another virtual desktop,
/// suspended UWP apps, and shell parts such as `Shell Handwriting Canvas`. They
/// come back at full screen size, so leaving them in only eats scan time.
///
/// # Do not exclude tool windows
///
/// `WS_EX_TOOLWINDOW` was added as an exclusion once and then **removed**. That
/// extended style only means "does not appear on the taskbar", and it is set on
/// **the taskbar itself** (`Shell_TrayWnd`) and on dropdown lists. In practice,
/// the moment it was excluded the Start button became impossible to find.
fn is_phantom(hwnd: HWND) -> bool {
    // DWMWA_CLOAKED: anything other than 0 means cloaked.
    let mut cloaked: u32 = 0;
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        )
    }
    .is_ok();

    ok && cloaked != 0
}

/// The window title, empty when absent. Only used in diagnostic logs.
fn window_title(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if copied <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..copied as usize])
}

/// The window's visible rectangle.
///
/// `GetWindowRect` includes the invisible border used for the drop shadow, so
/// the DWM extended frame bounds take priority. The same call as in
/// `mekiki-capture`.
fn window_bounds(hwnd: HWND) -> Option<Bounds> {
    let mut rect = RECT::default();

    let dwm_ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .is_ok();

    if !dwm_ok {
        unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    }

    let bounds = to_bounds(rect);
    (!bounds.is_empty()).then_some(bounds)
}

/// Build a cache request registering the properties to fetch.
///
/// Everything listed here comes back in a single round trip alongside the
/// search. Conversely, reading a property that is not listed here later costs
/// one round trip per element.
fn build_cache(
    automation: &IUIAutomation,
    mode: windows::Win32::UI::Accessibility::AutomationElementMode,
    values: bool,
) -> Result<IUIAutomationCacheRequest, UiaError> {
    let cache = unsafe { automation.CreateCacheRequest() }.map_err(os_err)?;
    for property in [
        UIA_NamePropertyId,
        UIA_AutomationIdPropertyId,
        UIA_ClassNamePropertyId,
        UIA_ControlTypePropertyId,
        UIA_BoundingRectanglePropertyId,
        UIA_IsEnabledPropertyId,
        UIA_IsOffscreenPropertyId,
    ] {
        unsafe { cache.AddProperty(property) }.map_err(os_err)?;
    }
    if values {
        unsafe { cache.AddProperty(UIA_IsPasswordPropertyId) }.map_err(os_err)?;
        unsafe { cache.AddPattern(UIA_ValuePatternId) }.map_err(os_err)?;
        unsafe { cache.AddPattern(UIA_TextPatternId) }.map_err(os_err)?;
    }
    unsafe { cache.SetAutomationElementMode(mode) }.map_err(os_err)?;
    Ok(cache)
}

fn to_bounds(rect: RECT) -> Bounds {
    Bounds::new(
        rect.left,
        rect.top,
        (rect.right - rect.left).max(0) as u32,
        (rect.bottom - rect.top).max(0) as u32,
    )
}

/// The control type mapping table.
///
/// Both directions of the conversion are derived from this single table.
/// Writing two `match` arms invites fixing only one of them and having them
/// drift apart.
const CONTROL_TYPES: &[(ControlType, UIA_CONTROLTYPE_ID)] = &[
    (ControlType::Button, UIA_ButtonControlTypeId),
    (ControlType::CheckBox, UIA_CheckBoxControlTypeId),
    (ControlType::ComboBox, UIA_ComboBoxControlTypeId),
    (ControlType::Edit, UIA_EditControlTypeId),
    (ControlType::Hyperlink, UIA_HyperlinkControlTypeId),
    (ControlType::Image, UIA_ImageControlTypeId),
    (ControlType::ListItem, UIA_ListItemControlTypeId),
    (ControlType::List, UIA_ListControlTypeId),
    (ControlType::MenuItem, UIA_MenuItemControlTypeId),
    (ControlType::RadioButton, UIA_RadioButtonControlTypeId),
    (ControlType::Tab, UIA_TabControlTypeId),
    (ControlType::TabItem, UIA_TabItemControlTypeId),
    (ControlType::Text, UIA_TextControlTypeId),
    (ControlType::Tree, UIA_TreeControlTypeId),
    (ControlType::TreeItem, UIA_TreeItemControlTypeId),
    (ControlType::Window, UIA_WindowControlTypeId),
];

fn to_control_type_id(t: ControlType) -> UIA_CONTROLTYPE_ID {
    if let ControlType::Other(id) = t {
        return UIA_CONTROLTYPE_ID(id);
    }
    CONTROL_TYPES
        .iter()
        .find(|(kind, _)| *kind == t)
        .map(|(_, id)| *id)
        .unwrap_or(UIA_CONTROLTYPE_ID(0))
}

fn from_control_type_id(id: UIA_CONTROLTYPE_ID) -> ControlType {
    CONTROL_TYPES
        .iter()
        .find(|(_, known)| known.0 == id.0)
        .map(|(kind, _)| *kind)
        .unwrap_or(ControlType::Other(id.0))
}

impl ElementFinder for WindowsUia {
    fn find(&mut self, query: &Query) -> Result<Vec<Element>, UiaError> {
        self.find_impl(query, false)
    }

    fn find_values(&mut self, query: &Query) -> Result<Vec<Element>, UiaError> {
        self.find_impl(query, true)
    }

    fn read_value(&mut self, element: &Element) -> Result<ElementValue, UiaError> {
        let point = POINT {
            x: element.bounds.x + element.bounds.width as i32 / 2,
            y: element.bounds.y + element.bounds.height as i32 / 2,
        };
        let target = unsafe { self.automation.ElementFromPoint(point) }.map_err(os_err)?;
        // ElementFromPoint is deliberately used only after a scoped, unique
        // enumeration.  Revalidate the identity here: another window may have
        // covered the point, or the UI may have changed between enumeration
        // and this cross-process call.  Failing closed avoids returning a
        // value from an unrelated application.
        let current_name = unsafe { target.CurrentName() }
            .map(|value| value.to_string())
            .map_err(os_err)?;
        let current_type = unsafe { target.CurrentControlType() }
            .map(from_control_type_id)
            .map_err(os_err)?;
        let current_bounds = unsafe { target.CurrentBoundingRectangle() }
            .map(to_bounds)
            .map_err(os_err)?;
        if current_name != element.name
            || current_type != element.control_type
            || current_bounds != element.bounds
        {
            return Err(UiaError::Os(
                "selected element was obscured or changed before its value was read".into(),
            ));
        }
        let is_password = unsafe { target.CurrentIsPassword() }
            .map(|value| value.as_bool())
            .unwrap_or(false);
        if is_password {
            return Ok(ElementValue {
                value: None,
                source: None,
                is_password: true,
            });
        }
        let via_value =
            unsafe { target.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
                .ok()
                .and_then(|pattern| unsafe { pattern.CurrentValue() }.ok())
                .map(|value| value.to_string());
        if let Some(value) = via_value {
            return Ok(ElementValue {
                value: Some(value),
                source: Some("value"),
                is_password: false,
            });
        }
        let via_text = (|| {
            let pattern = unsafe {
                target.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            }
            .ok()?;
            let range = unsafe { pattern.DocumentRange() }.ok()?;
            unsafe { range.GetText(-1) }
                .ok()
                .map(|value| value.to_string())
        })();
        Ok(ElementValue {
            source: via_text.as_ref().map(|_| "text"),
            value: via_text,
            is_password: false,
        })
    }

    fn backend_name(&self) -> &'static str {
        "windows/uiautomation"
    }
}

impl WindowsUia {
    fn find_impl(&self, query: &Query, values: bool) -> Result<Vec<Element>, UiaError> {
        let condition = self.condition_for(query)?;

        // Only windows overlapping the search area, in Z order (front first).
        // Front first because when an element with the same name also exists
        // behind, the front one should win: that is what a person would click.
        //
        // An early exit of "once a front window covers the search area, ignore
        // what is behind" was tried and **dropped**. If a full-screen invisible
        // shell window (measured: 'Shell Handwriting Canvas') or a transparent
        // overlay comes first, the search ends right there. Missing results for
        // the sake of speed is not worth it at this layer.
        let mut windows = enumerate_windows()?;
        let total = windows.len();
        if let Some(root) = query.root {
            let root = HWND(root as *mut core::ffi::c_void);
            windows.retain(|win| {
                win.hwnd == root || unsafe { GetWindow(win.hwnd, GW_OWNER) }.ok() == Some(root)
            });
            if !windows.iter().any(|win| win.hwnd == root)
                && let Some(bounds) = window_bounds(root)
            {
                windows.insert(
                    0,
                    TopLevel {
                        hwnd: root,
                        title: window_title(root),
                        bounds,
                    },
                );
            }
        }
        if let Some(w) = query.within {
            windows.retain(|win| win.bounds.intersects(w));
        }

        let mut out = Vec::new();
        let mut scanned = 0usize;
        for window in &windows {
            let element = match unsafe { self.automation.ElementFromHandle(window.hwnd) } {
                Ok(e) => e,
                Err(e) => {
                    log::debug!(
                        "UIA: cannot get the element for '{}' (skipping): {e}",
                        window.title
                    );
                    continue;
                }
            };

            let before = out.len();
            let t0 = std::time::Instant::now();

            // Failing to scan one window must not abandon the whole search.
            // A single unresponsive application should not kill it.
            if let Err(e) = self.collect_in_window(&element, &condition, query, &mut out, values) {
                log::debug!(
                    "UIA: failed to scan '{}' (ignoring and continuing): {e}",
                    window.title
                );
            }
            scanned += 1;

            // Make it possible to name the slow window. UIA timings vary by
            // three orders of magnitude depending on the target application, so
            // a report of "it is slow" is unactionable without the culprit.
            log::debug!(
                "UIA: [{:>7.1}ms] {:>4} hits  {}",
                t0.elapsed().as_secs_f64() * 1000.0,
                out.len() - before,
                window.title
            );

            if out.len() >= query.limit {
                break;
            }
        }

        // Providers sometimes surface the same logical element through both
        // the document tree and an owned popup. Runtime IDs are not available
        // in the fast `AutomationElementMode_None` path, so use the documented
        // stable fallback key.
        let mut seen = std::collections::HashSet::new();
        out.retain(|e| {
            // RuntimeId is the primary identity, but some legacy providers
            // (observed: OpenOffice SAL) return one shared ID for distinct
            // descendants. Pair it with the stable fallback material so a
            // broken provider cannot collapse an entire form into one item.
            seen.insert((
                e.runtime_id.clone(),
                query.root,
                e.automation_id.clone(),
                e.class_name.clone(),
                e.control_type.as_str(),
                e.bounds.x,
                e.bounds.y,
                e.bounds.width,
                e.bounds.height,
                e.name.clone(),
            ))
        });

        log::debug!(
            "UIA: {} hits total ({scanned}/{total} windows, conditions {})",
            out.len(),
            query.describe()
        );
        Ok(out)
    }
}

/// Copy and release the SAFEARRAY returned by UIA. Providers are permitted to
/// omit RuntimeId; callers then use the documented composite fallback key.
fn runtime_id(element: &IUIAutomationElement) -> Option<Vec<i32>> {
    let array = unsafe { element.GetRuntimeId() }.ok()?;
    if array.is_null() {
        return None;
    }
    let result = (|| {
        let lower = unsafe { SafeArrayGetLBound(array, 1) }.ok()?;
        let upper = unsafe { SafeArrayGetUBound(array, 1) }.ok()?;
        let mut values = Vec::with_capacity((upper - lower + 1).max(0) as usize);
        for index in lower..=upper {
            let mut value = 0i32;
            unsafe { SafeArrayGetElement(array, &index, (&raw mut value).cast()) }.ok()?;
            values.push(value);
        }
        Some(values)
    })();
    let _ = unsafe { SafeArrayDestroy(array) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Types in the table must survive a round trip. This is where keeping the
    /// mapping in one place pays off.
    #[test]
    fn control_type_round_trips_through_uia_ids() {
        for (kind, _) in CONTROL_TYPES {
            assert_eq!(
                from_control_type_id(to_control_type_id(*kind)),
                *kind,
                "the round trip for {kind} is broken"
            );
        }
    }

    /// Types not in the table pass through as numbers. UIA can gain new types,
    /// so unknown ones are not swallowed.
    #[test]
    fn unknown_control_type_passes_through() {
        let unknown = UIA_CONTROLTYPE_ID(50999);
        assert_eq!(from_control_type_id(unknown), ControlType::Other(50999));
        assert_eq!(to_control_type_id(ControlType::Other(50999)).0, 50999);
    }

    #[test]
    fn empty_rect_becomes_empty_bounds() {
        let r = RECT {
            left: 10,
            top: 10,
            right: 10,
            bottom: 40,
        };
        assert!(to_bounds(r).is_empty());
    }

    /// UIA can return negative or inverted rectangles for off-screen elements.
    /// Width and height must not come out negative.
    #[test]
    fn inverted_rect_does_not_underflow() {
        let r = RECT {
            left: 100,
            top: 100,
            right: 20,
            bottom: 20,
        };
        let b = to_bounds(r);
        assert_eq!((b.width, b.height), (0, 0));
    }
}

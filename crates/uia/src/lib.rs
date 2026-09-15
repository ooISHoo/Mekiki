//! Finding UI elements through the accessibility API.
//!
//! Phase 4-4 of the development plan. The implementation is Windows UI Automation.
//!
//! # Why this is used alongside image matching
//!
//! Image matching **breaks when the appearance changes**. DPI scaling, a dark
//! theme, font settings and OS version differences all stop a template from
//! matching. The accessibility API looks at the UI tree rather than the pixels,
//! so none of that affects it.
//!
//! On the other hand there is plenty this API **cannot** reach.
//!
//! - applications that draw their own UI (games, some Electron apps, the
//!   contents of a remote desktop session)
//! - regions drawn as a canvas or an image
//! - older applications that expose no accessibility information
//!
//! Neither approach is sufficient alone, so Mekiki uses both.
//! **Which one is tried first is up to the script author** (`Target::or` in
//! `mekiki-core`). The order is not fixed because the speed difference is large:
//! UIA takes 350–800ms against roughly 40ms for image matching, so always
//! putting UIA first would make even the cases that work 10x slower. See
//! [Windows capture and UIA notes](../../../docs/maintenance/windows-capture-and-uia.md).
//!
//! Controlling the fallback is `mekiki-core`'s responsibility; this crate is
//! only responsible for "search via UIA".
//!
//! The OS-dependent part is split the same way as in `mekiki-capture` /
//! `mekiki-ocr`: the [`ElementFinder`] trait is the boundary and the
//! implementation stays inside `windows_*.rs`.

use std::fmt;

#[cfg(windows)]
mod windows_uia;

/// A rectangle in screen coordinates.
///
/// The same shape as `mekiki-capture`'s `Rect`, redeclared here to avoid adding
/// a dependency. The caller (`mekiki-core`) converts between them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Bounds {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(self) -> i32 {
        self.x + self.width as i32
    }

    pub fn bottom(self) -> i32 {
        self.y + self.height as i32
    }

    pub fn intersects(self, other: Bounds) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// The kind of an element.
///
/// UIA has about 40 ControlTypes, but only a few are ones you would want to
/// name in an RPA script. Mirroring all of them would only enlarge the
/// vocabulary, so the common ones get names and the rest pass through as
/// numbers via [`ControlType::Other`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ControlType {
    Button,
    CheckBox,
    ComboBox,
    Edit,
    Hyperlink,
    Image,
    ListItem,
    List,
    MenuItem,
    RadioButton,
    Tab,
    TabItem,
    Text,
    Tree,
    TreeItem,
    Window,
    Other(i32),
}

impl ControlType {
    /// The name written in a script — the `button` in `ui:type=button`.
    ///
    /// Case and the `-` / `_` separators are ignored.
    pub fn parse(s: &str) -> Option<Self> {
        let key: String = s
            .chars()
            .filter(|c| *c != '-' && *c != '_')
            .flat_map(|c| c.to_lowercase())
            .collect();
        Some(match key.as_str() {
            "button" => Self::Button,
            "checkbox" => Self::CheckBox,
            "combobox" | "dropdown" => Self::ComboBox,
            "edit" | "input" | "textbox" => Self::Edit,
            "hyperlink" | "link" => Self::Hyperlink,
            "image" => Self::Image,
            "listitem" => Self::ListItem,
            "list" => Self::List,
            "menuitem" => Self::MenuItem,
            "radiobutton" | "radio" => Self::RadioButton,
            "tab" => Self::Tab,
            "tabitem" => Self::TabItem,
            "text" | "label" => Self::Text,
            "tree" => Self::Tree,
            "treeitem" => Self::TreeItem,
            "window" => Self::Window,
            _ => return None,
        })
    }

    /// The canonical name, spelled as `ui:type=` expects.
    ///
    /// The inverse of [`Self::parse`] for the primary spelling: parsing this
    /// back yields the same variant. Used when listing elements so an agent can
    /// copy the type straight into a locator.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Button => "button",
            Self::CheckBox => "checkbox",
            Self::ComboBox => "combobox",
            Self::Edit => "edit",
            Self::Hyperlink => "hyperlink",
            Self::Image => "image",
            Self::ListItem => "listitem",
            Self::List => "list",
            Self::MenuItem => "menuitem",
            Self::RadioButton => "radiobutton",
            Self::Tab => "tab",
            Self::TabItem => "tabitem",
            Self::Text => "text",
            Self::Tree => "tree",
            Self::TreeItem => "treeitem",
            Self::Window => "window",
            Self::Other(_) => "other",
        }
    }

    /// The list of writable type names, for error messages.
    pub const NAMES: &'static str = "button / checkbox / combobox / edit / hyperlink / image / \
         list / listitem / menuitem / radiobutton / tab / tabitem / text / tree / treeitem / window";
}

impl fmt::Display for ControlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Button => "button",
            Self::CheckBox => "checkbox",
            Self::ComboBox => "combobox",
            Self::Edit => "edit",
            Self::Hyperlink => "hyperlink",
            Self::Image => "image",
            Self::ListItem => "listitem",
            Self::List => "list",
            Self::MenuItem => "menuitem",
            Self::RadioButton => "radiobutton",
            Self::Tab => "tab",
            Self::TabItem => "tabitem",
            Self::Text => "text",
            Self::Tree => "tree",
            Self::TreeItem => "treeitem",
            Self::Window => "window",
            Self::Other(id) => return write!(f, "type#{id}"),
        };
        f.write_str(s)
    }
}

/// A found element.
///
/// **It does not hold the element itself (the COM reference).** Clicking goes
/// through `SendInput` against a coordinate, so all that is needed is the
/// rectangle and the strings that identify it. Not holding references is what
/// makes the search dramatically faster (see the note on [`ElementFinder`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    /// The name visible on screen, such as a button label.
    pub name: String,
    /// The identifier the application assigned. More stable than the name when present.
    pub automation_id: String,
    pub class_name: String,
    pub control_type: ControlType,
    /// The rectangle in screen (virtual desktop) coordinates.
    pub bounds: Bounds,
    pub enabled: bool,
    /// Off screen, or scrolled out of view.
    pub offscreen: bool,
    /// Provider-stable UIA RuntimeId when the provider exposes one.
    pub runtime_id: Option<Vec<i32>>,
    /// Current ValuePattern value. Populated only by the explicit value-reading
    /// path; ordinary tree enumeration deliberately leaves this absent.
    pub value: Option<String>,
    pub value_source: Option<&'static str>,
    /// UIA marks password controls so callers can redact them.
    pub is_password: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementValue {
    pub value: Option<String>,
    pub source: Option<&'static str>,
    pub is_password: bool,
}

/// Search conditions.
///
/// `name` / `automation_id` / `control_type` are all optional; only what you
/// specify becomes a condition.
#[derive(Clone, Debug, Default)]
pub struct Query {
    /// The display name. Matching is done by the caller (`mekiki-core`), so
    /// this is **not used for filtering** here. It is passed along as material
    /// for cases where the engine can pre-filter on it.
    pub name: Option<String>,
    /// Filters by exact match.
    pub automation_id: Option<String>,
    pub control_type: Option<ControlType>,
    /// Return only elements overlapping this rectangle.
    ///
    /// **Do not omit this.** Scanning the whole desktop unconditionally can
    /// take seconds depending on how many windows are open.
    pub within: Option<Bounds>,
    /// Native top-level HWND to scan. When set, only this window and popups it
    /// owns are searched, even if another application's window overlaps it.
    pub root: Option<isize>,
    /// Whether to include off-screen elements. Excluded by default.
    pub include_offscreen: bool,
    /// Upper bound on results. A safety valve against a runaway search.
    pub limit: usize,
}

impl Query {
    /// The default limit, comfortably above the number of actionable elements
    /// on one screen.
    pub const DEFAULT_LIMIT: usize = 2000;

    pub fn new() -> Self {
        Self {
            limit: Self::DEFAULT_LIMIT,
            ..Default::default()
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_automation_id(mut self, id: impl Into<String>) -> Self {
        self.automation_id = Some(id.into());
        self
    }

    pub fn with_control_type(mut self, t: ControlType) -> Self {
        self.control_type = Some(t);
        self
    }

    pub fn within(mut self, bounds: Bounds) -> Self {
        self.within = Some(bounds);
        self
    }

    pub fn rooted_at(mut self, hwnd: isize) -> Self {
        self.root = Some(hwnd);
        self
    }

    /// A human-readable description, used in error messages.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(n) = &self.name {
            parts.push(format!("name={n}"));
        }
        if let Some(i) = &self.automation_id {
            parts.push(format!("id={i}"));
        }
        if let Some(t) = &self.control_type {
            parts.push(format!("type={t}"));
        }
        if parts.is_empty() {
            "(no conditions)".to_string()
        } else {
            parts.join(",")
        }
    }
}

#[derive(Debug)]
pub enum UiaError {
    Unsupported(&'static str),
    /// COM initialisation failed.
    ComInit(String),
    Os(String),
}

impl fmt::Display for UiaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(p) => write!(f, "accessibility search is not implemented for {p}"),
            Self::ComInit(e) => write!(f, "COM initialisation failed: {e}"),
            Self::Os(e) => write!(f, "UI Automation failed: {e}"),
        }
    }
}

impl std::error::Error for UiaError {}

/// A backend for finding UI elements.
///
/// # About threads
///
/// A UI Automation client is tied to a COM apartment. Implementations of this
/// trait assume they are **used on the thread that created them** and therefore
/// do not require `Send`. `mekiki-core`'s execution engine is single threaded,
/// so that is enough.
///
/// # About speed
///
/// UIA calls **cross a process boundary**. Walking elements one at a time and
/// reading properties can cost several milliseconds each. An implementation
/// must always use a cache request (fetching in bulk) and narrow the scan with
/// [`Query::within`].
pub trait ElementFinder {
    fn find(&mut self, query: &Query) -> Result<Vec<Element>, UiaError>;

    /// Find elements and read their ValuePattern. Kept separate so `ui_tree`
    /// cannot accidentally disclose field contents.
    fn find_values(&mut self, query: &Query) -> Result<Vec<Element>, UiaError> {
        self.find(query)
    }

    /// Read a value only after the caller has narrowed enumeration to one
    /// element. This prevents a broad query from fetching every form value.
    fn read_value(&mut self, element: &Element) -> Result<ElementValue, UiaError> {
        Ok(ElementValue {
            value: if element.is_password {
                None
            } else {
                element.value.clone()
            },
            source: if element.is_password {
                None
            } else {
                element.value_source
            },
            is_password: element.is_password,
        })
    }

    fn backend_name(&self) -> &'static str;
}

/// Open a search backend usable in this environment.
pub fn open() -> Result<Box<dyn ElementFinder>, UiaError> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows_uia::WindowsUia::new()?))
    }
    #[cfg(not(windows))]
    {
        // This would be AT-SPI (Linux) / AXUIElement (macOS). Out of scope for now.
        Err(UiaError::Unsupported("this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_type_names_are_case_and_separator_insensitive() {
        assert_eq!(ControlType::parse("Button"), Some(ControlType::Button));
        assert_eq!(ControlType::parse("CHECK_BOX"), Some(ControlType::CheckBox));
        assert_eq!(ControlType::parse("check-box"), Some(ControlType::CheckBox));
        assert_eq!(ControlType::parse("listitem"), Some(ControlType::ListItem));
        assert_eq!(ControlType::parse("something"), None);
    }

    /// Aliases fold to the same type. Whatever word the author reaches for first
    /// should work.
    #[test]
    fn control_type_aliases() {
        assert_eq!(ControlType::parse("link"), Some(ControlType::Hyperlink));
        assert_eq!(ControlType::parse("input"), Some(ControlType::Edit));
        assert_eq!(ControlType::parse("textbox"), Some(ControlType::Edit));
        assert_eq!(ControlType::parse("radio"), Some(ControlType::RadioButton));
    }

    #[test]
    fn control_type_display_round_trips() {
        for t in [
            ControlType::Button,
            ControlType::CheckBox,
            ControlType::Edit,
            ControlType::Window,
        ] {
            assert_eq!(ControlType::parse(&t.to_string()), Some(t));
        }
        assert_eq!(ControlType::Other(50042).to_string(), "type#50042");
    }

    #[test]
    fn bounds_intersection() {
        let a = Bounds::new(0, 0, 100, 100);
        assert!(a.intersects(Bounds::new(50, 50, 100, 100)));
        assert!(
            !a.intersects(Bounds::new(100, 0, 10, 10)),
            "touching is not overlapping"
        );
        assert!(!a.intersects(Bounds::new(200, 200, 10, 10)));
    }

    #[test]
    fn query_describe_lists_given_conditions() {
        let q = Query::new()
            .with_name("保存")
            .with_control_type(ControlType::Button);
        let d = q.describe();
        assert!(d.contains("name=保存"), "{d}");
        assert!(d.contains("type=button"), "{d}");
        assert_eq!(Query::new().describe(), "(no conditions)");
    }
}

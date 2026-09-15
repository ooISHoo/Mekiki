//! Turning a locator string into something the engine can act on, **without
//! going through Rhai**.
//!
//! # Why this is separate from `api.rs`
//!
//! `api::ScriptTarget::build` does the same job, but everything it touches is
//! wrapped in `Rc<RefCell<Runtime>>` because Rhai values have to be
//! `Clone + 'static`. A caller that is not a script — the MCP server, a test,
//! any embedding — should not have to build that machinery just to resolve
//! `"ocr:Save"`.
//!
//! **Both paths must agree.** A locator that works in a `.rhai` script and the
//! same locator handed to an MCP tool have to mean the same thing, or agents
//! will write scripts that behave differently from the calls they tested with.
//! So the parsing lives in [`crate::locator`] and the interpretation lives
//! here, and `api.rs` calls into this module rather than keeping its own copy.

use mekiki_core::{ControlType, Mekiki, Needle, Region, Target, TextPattern, UiPattern};

use crate::assets::AssetStore;
use crate::locator::{self, Locator, UiQuery};

/// What a locator turned out to denote.
///
/// Not every locator is something to search for. `point:` and `region:` name a
/// place directly, and no amount of looking at the screen changes where they
/// are — so they cannot become a [`Target`], and callers have to handle them.
pub enum Resolved {
    /// Something to search for.
    Target(Box<Target>),
    /// A fixed point in screen coordinates.
    Point(i32, i32),
    /// A fixed rectangle. The click point is its centre.
    Rect(Region),
}

impl Resolved {
    /// The rectangle this denotes right now, searching if it has to.
    ///
    /// A point becomes a 1x1 rectangle, which keeps callers that only want
    /// "where do I click" from having to match on the variant.
    pub fn rect_now(&self, mekiki: &mut Mekiki) -> Result<mekiki_core::Rect, String> {
        match self {
            Self::Target(t) => Ok(mekiki.on(t).resolve().map_err(|e| e.to_string())?.rect),
            Self::Point(x, y) => Ok(mekiki_core::Rect::new(*x, *y, 1, 1)),
            Self::Rect(r) => Ok(r.rect),
        }
    }
}

/// Interpret a locator string against a search area.
///
/// `assets` is needed because an image locator has to be loaded and turned into
/// a pyramid before it can be searched for; the store caches that.
pub fn resolve(
    mekiki: &Mekiki,
    assets: &mut AssetStore,
    scope: Region,
    locator_str: &str,
) -> Result<Resolved, String> {
    let parsed = locator::parse(locator_str).map_err(|e| e.to_string())?;

    Ok(match parsed {
        Locator::Image(reference) => {
            let pattern = assets
                .pattern(mekiki, &reference)
                .map_err(|e| e.to_string())?;
            Resolved::Target(Box::new(mekiki.target(scope, &pattern)))
        }
        Locator::Point(x, y) => Resolved::Point(x, y),
        Locator::Offset(dx, dy) => {
            // Relative to where the cursor is **now**. Resolving it later would
            // measure from wherever the pointer had drifted to since.
            let (x, y) = mekiki.mouse_position().map_err(|e| e.to_string())?;
            Resolved::Point(x + dx, y + dy)
        }
        Locator::Region(x, y, w, h) => Resolved::Rect(Region::new(
            mekiki_core::Rect::new(x, y, w, h),
            scope.display,
        )),
        Locator::Window(query) => {
            Resolved::Rect(mekiki.window_by(&query).map_err(|e| e.to_string())?)
        }
        Locator::Ocr(query) => Resolved::Target(Box::new(mekiki.text_target(scope, query))),
        Locator::Ui(query) => Resolved::Target(Box::new(
            mekiki.ui_target(scope, ui_pattern(mekiki, &query)),
        )),
    })
}

/// Move a parsed `ui:` locator into a [`UiPattern`].
///
/// The control type name was already validated by [`locator::parse`], so it
/// cannot fail here.
pub fn ui_pattern(mekiki: &Mekiki, query: &UiQuery) -> UiPattern {
    let mut pattern = mekiki.ui_pattern();
    if let Some(name) = &query.name {
        pattern = pattern.with_name(name);
    }
    if let Some(id) = &query.automation_id {
        pattern = pattern.with_automation_id(id);
    }
    if let Some(t) = query.control_type.as_deref().and_then(ControlType::parse) {
        pattern = pattern.with_control_type(t);
    }
    pattern
}

/// Interpret a search-area string.
///
/// `None` means the whole primary screen. Otherwise only `window:` and
/// `region:` make sense — a scope is an area, and the other locator kinds name
/// something to look for inside one.
pub fn scope_region(mekiki: &Mekiki, spec: Option<&str>) -> Result<Region, String> {
    let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        return mekiki.primary_screen().map_err(|e| e.to_string());
    };

    match locator::parse(spec).map_err(|e| e.to_string())? {
        Locator::Window(query) => mekiki.window_by(&query).map_err(|e| e.to_string()),
        Locator::Region(x, y, w, h) => Ok(mekiki.region(mekiki_core::Rect::new(x, y, w, h))),
        _ => Err(format!(
            "'{spec}' is not a search area. Use window:<title|exe=...> or region:<x>,<y>,<w>,<h>"
        )),
    }
}

/// Build a needle from a locator, for use as an anchor or an `or()` alternative.
///
/// Fails for the locators that name a place rather than something to look for.
pub fn needle(
    mekiki: &Mekiki,
    assets: &mut AssetStore,
    locator_str: &str,
) -> Result<Needle, String> {
    match locator::parse(locator_str).map_err(|e| e.to_string())? {
        Locator::Image(reference) => Ok(Needle::Image(
            assets
                .pattern(mekiki, &reference)
                .map_err(|e| e.to_string())?,
        )),
        Locator::Ocr(query) => Ok(Needle::Text(TextPattern::new(
            query,
            mekiki.settings.min_similarity,
        ))),
        Locator::Ui(query) => Ok(Needle::Ui(ui_pattern(mekiki, &query))),
        _ => Err(format!(
            "'{locator_str}' names a place rather than something to search for, \
             so it cannot be used here"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scope has to be an area. Handing it something to search for is a
    /// mistake worth naming, not something to silently accept.
    #[test]
    fn scope_rejects_non_area_locators() {
        // No engine is needed: parsing rejects these before anything is touched.
        for spec in ["ocr:Save", "image:ok.png", "point:10,20"] {
            let parsed = locator::parse(spec).unwrap();
            assert!(
                !matches!(parsed, Locator::Window(_) | Locator::Region(..)),
                "{spec} should not be usable as a scope"
            );
        }
    }

    #[test]
    fn area_locators_parse_as_areas() {
        assert!(matches!(
            locator::parse("window:Notepad").unwrap(),
            Locator::Window(_)
        ));
        assert!(matches!(
            locator::parse("region:0,0,100,50").unwrap(),
            Locator::Region(0, 0, 100, 50)
        ));
    }
}

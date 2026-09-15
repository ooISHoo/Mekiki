//! Rust representations of values exposed to Rhai.
//!
//! Rhai values must be `Clone + 'static`, so the script-side types hold an
//! `Rc<RefCell<Runtime>>`. Borrows must not cross a return to Rhai because a
//! nested call could otherwise panic. The scripting contract is documented in
//! [`crate::rhai_api`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use mekiki_core::{Match, Mekiki, Pattern, Region, Target};
use rhai::{Dynamic, EvalAltResult};

use crate::assets::AssetStore;
use crate::keys;
use crate::locator::{self, Locator};
// Shared with the non-Rhai path so both agree on what a `ui:` locator means.
use crate::resolve::ui_pattern;

type Result<T> = std::result::Result<T, Box<EvalAltResult>>;

/// State shared by all values created during one script host session.
pub struct Runtime {
    /// The desktop automation engine.
    pub mekiki: Mekiki,
    /// Image lookup and content-addressed storage.
    pub assets: AssetStore,
    /// Under `set_recheck(false)`, reuse the coordinates for the same locator
    /// and area.
    hits: HashMap<String, Match>,
}

impl Runtime {
    /// Combine a Mekiki engine and asset store into script runtime state.
    pub fn new(mekiki: Mekiki, assets: AssetStore) -> Self {
        Self {
            mekiki,
            assets,
            hits: HashMap::new(),
        }
    }
}

pub(crate) type Shared = Rc<RefCell<Runtime>>;

fn err(msg: impl std::fmt::Display) -> Box<EvalAltResult> {
    msg.to_string().into()
}

// ---------------------------------------------------------------------------
// Region
// ---------------------------------------------------------------------------

/// The Rust value registered in Rhai as `Region`.
///
/// See [`crate::rhai_api#region`] for its script-visible properties and methods.
#[derive(Clone)]
pub struct ScriptRegion {
    pub(crate) rt: Shared,
    pub(crate) region: Region,
    /// Present only when built by `window(...)`. Actions keep using this HWND
    /// instead of resolving a Z-order-sensitive query again.
    pub(crate) window: Option<isize>,
}

impl ScriptRegion {
    pub(crate) fn plain(rt: Shared, region: Region) -> Self {
        Self {
            rt,
            region,
            window: None,
        }
    }

    pub(crate) fn of_window(rt: Shared, region: Region) -> Self {
        let window = region.window;
        Self { rt, region, window }
    }

    pub(crate) fn derived(&self, region: Region) -> Self {
        Self {
            rt: self.rt.clone(),
            region,
            window: None,
        }
    }

    pub(crate) fn target(&self, locator: &str) -> Result<ScriptTarget> {
        ScriptTarget::build(self.rt.clone(), self.region, locator)
    }

    pub(crate) fn to_display(&self) -> String {
        format!("Region({})", self.region.rect)
    }
}

// ---------------------------------------------------------------------------
// Target
// ---------------------------------------------------------------------------

/// The contents of each kind of locator.
///
/// Non-image locators (coordinates, rectangles) are handled as the same
/// `Target`, so from the script's point of view everything is uniformly
/// "something clickable".
#[derive(Clone)]
pub(crate) enum Kind {
    Pattern(Box<Target>),
    /// Absolute coordinates.
    Point(i32, i32),
    /// Relative to the current mouse position, evaluated at resolution time.
    Offset(i32, i32),
    /// A rectangle. The click point is its centre.
    Rect(mekiki_core::Region),
}

/// The Rust value registered in Rhai as a lazy `Target`.
///
/// See [`crate::rhai_api#target`] for its script-visible methods.
#[derive(Clone)]
pub struct ScriptTarget {
    pub(crate) rt: Shared,
    pub(crate) kind: Kind,
    /// The original locator string, for error messages.
    pub(crate) source: String,
    /// `None` follows the engine setting (`set_recheck`).
    recheck: Option<bool>,
}

impl ScriptTarget {
    pub(crate) fn build(rt: Shared, scope: Region, locator_str: &str) -> Result<Self> {
        let parsed = locator::parse(locator_str).map_err(err)?;

        let kind = match parsed {
            Locator::Image(reference) => {
                let pattern = load_pattern(&rt, &reference)?;
                let target = rt.borrow().mekiki.target(scope, &pattern);
                Kind::Pattern(Box::new(target))
            }
            Locator::Point(x, y) => Kind::Point(x, y),
            Locator::Offset(dx, dy) => Kind::Offset(dx, dy),
            Locator::Region(x, y, w, h) => {
                let rect =
                    mekiki_core::model::Region::new(mekiki_capture_rect(x, y, w, h), scope.display);
                Kind::Rect(rect)
            }
            Locator::Window(query) => {
                let region = rt.borrow().mekiki.window_by(&query).map_err(err)?;
                Kind::Rect(region)
            }
            Locator::Ocr(query) => {
                let target = rt.borrow().mekiki.text_target(scope, query);
                Kind::Pattern(Box::new(target))
            }
            Locator::Ui(query) => {
                let engine = rt.borrow();
                let target = engine
                    .mekiki
                    .ui_target(scope, ui_pattern(&engine.mekiki, &query));
                drop(engine);
                Kind::Pattern(Box::new(target))
            }
        };

        Ok(Self {
            rt,
            kind,
            source: locator_str.to_string(),
            recheck: None,
        })
    }

    pub(crate) fn or_locator(&self, locator_str: &str) -> Result<Self> {
        let scope = match &self.kind {
            Kind::Pattern(t) => t.region(),
            _ => return Err(self.pattern_only("or()")),
        };

        // Build the alternative along the same path, so that locator syntax
        // cannot diverge between the primary and the alternative.
        let alternative = ScriptTarget::build(self.rt.clone(), scope, locator_str)?;
        let Kind::Pattern(alt_target) = alternative.kind else {
            return Err(err(format!(
                "'{locator_str}' is a locator that involves no search, so it cannot be an or() alternative"
            )));
        };

        let Kind::Pattern(base) = self.kind.clone() else {
            unreachable!("checked above");
        };

        Ok(Self {
            rt: self.rt.clone(),
            kind: Kind::Pattern(Box::new(base.or(alt_target.needle().clone()))),
            source: format!("{} → {locator_str}", self.source),
            recheck: self.recheck,
        })
    }

    fn pattern_only(&self, what: &str) -> Box<EvalAltResult> {
        err(format!(
            "'{}' is not an image locator, so {what} is unavailable",
            self.source
        ))
    }

    pub(crate) fn map_pattern(
        &self,
        what: &str,
        f: impl FnOnce(Target) -> Target,
    ) -> Result<ScriptTarget> {
        match &self.kind {
            Kind::Pattern(t) => Ok(Self {
                rt: self.rt.clone(),
                kind: Kind::Pattern(Box::new(f((**t).clone()))),
                source: self.source.clone(),
                recheck: self.recheck,
            }),
            _ => Err(self.pattern_only(what)),
        }
    }

    pub(crate) fn with_anchor(
        &self,
        what: &str,
        anchor_locator: &str,
        distance: i64,
        f: impl FnOnce(Target, mekiki_core::Needle, u32) -> Target,
    ) -> Result<ScriptTarget> {
        let anchor = match locator::parse(anchor_locator).map_err(err)? {
            Locator::Image(reference) => {
                mekiki_core::Needle::Image(load_pattern(&self.rt, &reference)?)
            }
            // Text can be an anchor too. "To the right of the label 'Name:'"
            // reads more naturally than capturing that label as an image.
            Locator::Ocr(query) => mekiki_core::Needle::Text(mekiki_core::TextPattern::new(
                query,
                default_similarity(&self.rt),
            )),
            _ => {
                return Err(err(format!(
                    "an anchor needs an image or text locator ('{anchor_locator}' is neither)"
                )));
            }
        };
        let distance = distance.clamp(0, u32::MAX as i64) as u32;
        self.map_pattern(what, |t| f(t, anchor, distance))
    }

    pub(crate) fn with_recheck(&self, enabled: bool) -> Self {
        let mut t = self.clone();
        t.recheck = Some(enabled);
        t
    }

    fn should_recheck(&self) -> bool {
        self.recheck
            .unwrap_or_else(|| self.rt.borrow().mekiki.settings.recheck)
    }

    fn cache_key(&self) -> String {
        match &self.kind {
            Kind::Pattern(t) => {
                let r = t.region().rect;
                format!("{}@{}+{}+{}x{}", self.source, r.x, r.y, r.width, r.height)
            }
            _ => self.source.clone(),
        }
    }

    fn remember(&self, m: &Match) {
        self.rt
            .borrow_mut()
            .hits
            .insert(self.cache_key(), m.clone());
    }

    fn forget(&self) {
        self.rt.borrow_mut().hits.remove(&self.cache_key());
    }

    pub(crate) fn resolve_point(&self) -> Result<(i32, i32)> {
        match &self.kind {
            Kind::Pattern(_) => Ok(self.resolve_for_action(true)?.inner.target()),
            Kind::Point(x, y) => Ok((*x, *y)),
            Kind::Offset(dx, dy) => {
                let rt = self.rt.borrow();
                let (x, y) = rt.mekiki.mouse_position().map_err(err)?;
                Ok((x + dx, y + dy))
            }
            Kind::Rect(r) => Ok(r.center()),
        }
    }

    /// For actions. With `recheck` off and a previous position recorded, that
    /// position is reused.
    ///
    /// `consume` true discards the position once used. Clicks pass true;
    /// `hover` passes false, because the click that follows should use the same
    /// position. Without discarding, the next loop iteration would press the old
    /// position even after the menu order changed.
    pub(crate) fn resolve_for_action(&self, consume: bool) -> Result<ScriptMatch> {
        if !self.should_recheck() {
            let key = self.cache_key();
            let hit = self.rt.borrow().hits.get(&key).cloned();
            if let Some(inner) = hit {
                if consume {
                    self.forget();
                }
                return Ok(ScriptMatch {
                    rt: self.rt.clone(),
                    inner,
                });
            }
        }
        let m = self.resolve_match()?;
        if consume {
            self.forget();
        }
        Ok(m)
    }

    pub(crate) fn resolve_match(&self) -> Result<ScriptMatch> {
        match &self.kind {
            Kind::Pattern(t) => {
                let mut rt = self.rt.borrow_mut();
                let m = rt.mekiki.on(t).resolve().map_err(from_core)?;
                drop(rt);
                self.remember(&m);
                Ok(ScriptMatch {
                    rt: self.rt.clone(),
                    inner: m,
                })
            }
            _ => {
                let (x, y) = self.resolve_point()?;
                let rect = match &self.kind {
                    Kind::Rect(r) => r.rect,
                    _ => mekiki_capture_rect(x, y, 1, 1),
                };
                Ok(ScriptMatch {
                    rt: self.rt.clone(),
                    inner: Match {
                        rect,
                        display: 0,
                        score: 1.0,
                        target_offset: (0, 0),
                    },
                })
            }
        }
    }

    pub(crate) fn as_target(&self) -> Result<Target> {
        match &self.kind {
            Kind::Pattern(t) => Ok((**t).clone()),
            _ => Err(self.pattern_only("this operation")),
        }
    }

    pub(crate) fn to_display(&self) -> String {
        match &self.kind {
            Kind::Pattern(t) => format!("Target({})", t.describe()),
            Kind::Point(x, y) => format!("Target(point:{x},{y})"),
            Kind::Offset(dx, dy) => format!("Target(offset:{dx},{dy})"),
            Kind::Rect(r) => format!("Target({})", r.rect),
        }
    }
}

// ---------------------------------------------------------------------------
// Match
// ---------------------------------------------------------------------------

/// The Rust value registered in Rhai as `Match`.
///
/// See [`crate::rhai_api#match`] for snapshot semantics and available members.
#[derive(Clone)]
pub struct ScriptMatch {
    pub(crate) rt: Shared,
    pub(crate) inner: Match,
}

impl ScriptMatch {
    pub(crate) fn to_display(&self) -> String {
        format!("Match({} score={:.4})", self.inner.rect, self.inner.score)
    }
}

// ---------------------------------------------------------------------------
// Expect
// ---------------------------------------------------------------------------

/// The Rust value registered in Rhai as `Expect`.
///
/// See [`crate::rhai_api#expect`] for its assertion methods.
#[derive(Clone)]
pub struct ScriptExpect {
    pub(crate) target: ScriptTarget,
}

/// The Rust value registered in Rhai as `WindowExpect`.
///
/// See [`crate::rhai_api#windowexpect`] for its assertion methods.
#[derive(Clone)]
pub struct ScriptWindowExpect {
    pub(crate) rt: Shared,
    pub(crate) query: mekiki_core::WindowQuery,
    pub(crate) source: String,
}

impl ScriptWindowExpect {
    pub(crate) fn new(rt: Shared, source: String, query: mekiki_core::WindowQuery) -> Self {
        Self { rt, query, source }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_similarity(rt: &Shared) -> f32 {
    rt.borrow().mekiki.settings.min_similarity
}

fn load_pattern(rt: &Shared, reference: &str) -> Result<Pattern> {
    // Resolving an asset needs a reference to Mekiki, but this stays inside a
    // single borrow_mut so no borrow spans another.
    let mut guard = rt.borrow_mut();
    let Runtime { mekiki, assets, .. } = &mut *guard;
    assets.pattern(mekiki, reference).map_err(err)
}

fn mekiki_capture_rect(x: i32, y: i32, w: u32, h: u32) -> mekiki_core::Rect {
    mekiki_core::Rect::new(x, y, w, h)
}

pub(crate) fn millis(v: i64) -> Duration {
    Duration::from_millis(v.max(0) as u64)
}

pub(crate) fn optional_millis(v: i64) -> Option<Duration> {
    if v <= 0 { None } else { Some(millis(v)) }
}

pub(crate) fn parse_combo(
    spec: &str,
) -> Result<(mekiki_core::InputKey, mekiki_core::InputModifiers)> {
    keys::parse_combo(spec).map_err(err)
}

pub(crate) fn to_dynamic_array(items: Vec<ScriptMatch>) -> Dynamic {
    let arr: rhai::Array = items.into_iter().map(Dynamic::from).collect();
    Dynamic::from(arr)
}

pub(crate) fn runtime_error(msg: impl std::fmt::Display) -> Box<EvalAltResult> {
    err(msg)
}

pub(crate) fn from_core(e: mekiki_core::Error) -> Box<EvalAltResult> {
    match e {
        mekiki_core::Error::Interrupted => terminated(),
        mekiki_core::Error::Capture(mekiki_core::CaptureError::ActivateFailed(_)) => err(format!(
            "{e} (Windows can refuse this when another app owns the foreground; retrying often works)"
        )),
        other => err(other),
    }
}

/// The Rhai error representing "stopped".
///
/// An interruption is **not a failure**, so giving it the shape of an ordinary
/// runtime error would make the IDE show it in red. Matching Rhai's own
/// `ErrorTerminated`, used for script termination, lets the layer above
/// distinguish it in one place ([`crate::ScriptHost::is_interrupted`]).
pub(crate) fn terminated() -> Box<EvalAltResult> {
    EvalAltResult::ErrorTerminated(Dynamic::UNIT, rhai::Position::NONE).into()
}

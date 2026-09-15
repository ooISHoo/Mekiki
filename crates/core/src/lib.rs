//! The Mekiki core engine.
//!
//! Phases 1 and 2 of the development plan. It ties capture, matching and input
//! injection together so that "screenshot, find, click" is complete within the
//! Rust API alone.
//!
//! The API design is described in the [Rhai API architecture](../../../docs/architecture/rhai-api.md). Two
//! ideas are central.
//!
//! - **[`Target`] is evaluated lazily.** It is a description of how to search,
//!   not a search result, and every action re-captures and re-searches.
//! - **Every action waits automatically**, until the target is found and until
//!   the screen has stopped moving.
//!
//! # Usage
//!
//! ```ignore
//! let mut mekiki = Mekiki::new()?;
//! let win = mekiki.window("Notepad")?;
//! let pattern = mekiki.pattern_from_file("ok_button.png")?;
//! let ok = mekiki.target(win, &pattern).similar(0.85);
//!
//! mekiki.on(&ok).click()?;
//! mekiki.expect(&ok).to_vanish(None)?;
//! ```
//!
//! # On the shape of the API
//!
//! SikuliX offers the method chain `region.find(pattern).click()`, but here
//! everything goes explicitly through [`Mekiki`]. The capture session, the GPU
//! device and the input backend are all mutable shared resources, and hiding
//! them behind a method chain in Rust would propagate `Rc<RefCell<_>>`
//! throughout.
//!
//! The scripting layer (Rhai) can keep a handle to the engine inside its
//! objects, so there you can write `t.click()`.

pub mod act;
pub mod artifacts;
pub mod expect;
pub mod interrupt;
pub mod model;
pub mod pyramid;
pub mod search;
pub mod target;

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mekiki_capture::ScreenCapture;
use mekiki_input::{InputError, InputInjector, Key, Modifiers, MouseButton};
use mekiki_matching::Image;
use mekiki_ocr::{OcrError, TextRecognizer};
use mekiki_overlay::{Overlay, OverlayError};
use mekiki_uia::{ElementFinder, UiaError};

pub use act::{Act, Actionability, ChangeDetection, TextLine, UiItem, UiValue};
pub use artifacts::FailureArtifacts;
pub use expect::{AssertionFailed, Expect};
pub use interrupt::{Interrupt, RunState};
pub use mekiki_capture::{
    CaptureError, DisplayInfo, Frame, Rect, WindowInfo, WindowQuery, window_icon_bgra,
};
pub use mekiki_input::{Key as InputKey, Modifiers as InputModifiers, MouseButton as Button};
pub use mekiki_overlay::Color as HighlightColor;
/// UI element kinds, used when interpreting `ui:type=button`.
pub use mekiki_uia::ControlType;
/// The accessibility search backend, so a caller can supply its own.
pub use mekiki_uia::{Bounds as UiBounds, Element as UiElement, Query as UiQuery};
pub use model::{Match, Pattern, Region, TextPattern, UiPattern};
pub use search::SearchParams;
pub use target::{Direction, MatchOrder, Needle, Selection, Target};

/// The engine version. Matches the workspace `version` in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Runtime tuning values.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The default similarity threshold.
    pub min_similarity: f32,
    /// The default timeout for the automatic wait.
    pub auto_wait_timeout: Duration,
    /// The polling interval while waiting.
    pub wait_scan_interval: Duration,
    /// The conditions to meet before an action.
    pub actionability: Actionability,
    /// Whether to search again on the second and later actions against the
    /// same Target.
    ///
    /// `true` by default. A game's software cursor sitting on top changes the
    /// appearance, so a `right_click()` after a `hover()` misses when it
    /// searches again. Scripts turn it off with `set_recheck(false)`.
    pub recheck: bool,
    /// Settings for skipping the search while waiting when the screen has not
    /// changed.
    pub change_detection: ChangeDetection,
    /// The default ordering for multiple matches.
    pub match_order: MatchOrder,
    /// Where failure artifacts are written. `None` disables them.
    pub artifact_dir: Option<PathBuf>,
    /// The maximum number of candidates recorded in an artifact.
    pub artifact_candidates: usize,
    /// The interval between press and release for a click.
    ///
    /// At 0, some applications drop the click.
    pub click_hold: Duration,
    /// The interval between the first and second click of a double click.
    ///
    /// It must be shorter than the OS double-click time (500ms by default).
    pub double_click_interval: Duration,
    /// The wait between moving the mouse and pressing a button.
    pub move_settle: Duration,
    /// How many characters `type_text` sends per call. 0 sends the lot at once.
    ///
    /// **Defaults to 1, because what a dropped-character application needs is a
    /// gap between each character, not smaller calls.** Windows 11 Notepad was
    /// measured: splitting the call changed nothing, but a per-character pause
    /// reduced it (`type_interval` 30ms gave 7/7 correct across two test rounds,
    /// while a later 20ms round gave only 2/5), and
    /// a pause only lands between characters when each is its own call. So the
    /// two defaults work together — one character per call, a pause after each.
    ///
    /// The cost of splitting is that the guarantee "events in one `SendInput`
    /// call are not interleaved with input from another process" now only holds
    /// **within** a chunk. For an automation tool that is a fair trade: the user
    /// is not supposed to be typing at the same time, and silently losing
    /// characters is far worse than the theoretical interleaving.
    ///
    /// Chunks are split on character boundaries, never inside a surrogate pair.
    pub type_chunk: usize,
    /// The pause before **every** chunk and injected keystroke, the first one
    /// included. Defaults to 30ms.
    ///
    /// Before the first, not only between: the character sent right after a
    /// key press is exactly the one Windows 11's Notepad was measured to drop
    /// (round 3: four of five lines damaged with a between-only interval, the
    /// first character of each burst replaced by the one after it).
    ///
    /// **Correctness first.** A robust receiver does not need it and a script
    /// that wants speed drops it with `set_type_interval(0)`, but a fresh script
    /// typing into an unknown application should get the value that was measured
    /// to work rather than the one that silently corrupts text.
    ///
    /// The floor is the Windows timer's resolution, so values below ~15ms are
    /// unreliable; 20ms still failed in a later 5-run sample, while 30ms held
    /// for the current samples. A long string
    /// costs one interval per character, which is the price of not losing any.
    pub type_interval: Duration,
    /// Cursor movement speed (pixels per second). 0 jumps straight there.
    ///
    /// The duration is distance divided by speed, capped by
    /// [`Self::move_max_duration`]. The path eases with OutQuartic, so this
    /// value is the average speed.
    pub move_speed: f64,
    /// The cap on a single movement, so crossing the screen does not take
    /// several seconds.
    pub move_max_duration: Duration,
    /// Search tuning values.
    pub search: SearchParams,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            min_similarity: 0.7,
            auto_wait_timeout: Duration::from_secs(3),
            wait_scan_interval: Duration::from_millis(333),
            actionability: Actionability::default(),
            recheck: true,
            change_detection: ChangeDetection::default(),
            match_order: MatchOrder::ReadingOrder,
            artifact_dir: Some(PathBuf::from("mekiki-artifacts")),
            artifact_candidates: 5,
            click_hold: Duration::from_millis(20),
            double_click_interval: Duration::from_millis(60),
            move_settle: Duration::from_millis(30),
            // One character per call, a measured pause before each: the pair that
            // keeps Windows 11 Notepad from dropping characters. See the field
            // docs. A script opts into speed with set_type_interval(0).
            type_chunk: 1,
            type_interval: Duration::from_millis(30),
            move_speed: 1200.0,
            move_max_duration: Duration::from_secs(2),
            search: SearchParams::default(),
        }
    }
}

/// The pattern was not found.
#[derive(Debug)]
pub struct FindFailed {
    /// The result of [`Target::describe`].
    pub pattern: String,
    pub region: Rect,
    /// The best score seen during the search. Useful for tuning the threshold.
    pub best_score: Option<f32>,
    pub required: f32,
    pub waited: Duration,
    /// The anchor's name, when it was the anchor that was not found.
    ///
    /// Mistaking a bad target for a bad anchor sends you to fix the wrong thing.
    pub anchor_missing: Option<String>,
    /// It was found, but never stopped moving.
    pub unstable: bool,
    /// How many candidates were dropped by the anchor-relative geometry.
    pub filtered_out: usize,
    pub artifacts: Option<FailureArtifacts>,
}

impl fmt::Display for FindFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(anchor) = &self.anchor_missing {
            return write!(
                f,
                "cannot search for '{}' because the anchor '{anchor}' was not found in {} (waited {:.1}s)",
                self.pattern,
                self.region,
                self.waited.as_secs_f32()
            );
        }

        write!(
            f,
            "'{}' not found in {} (required {:.2}",
            self.pattern, self.region, self.required
        )?;
        match self.best_score {
            Some(s) => write!(f, " / best {s:.4}")?,
            None => write!(f, " / no candidate")?,
        }
        write!(f, " / waited {:.1}s)", self.waited.as_secs_f32())?;

        if self.unstable {
            write!(
                f,
                "\n  found, but the screen kept moving. force() drops the wait"
            )?;
        }
        if self.filtered_out > 0 {
            write!(
                f,
                "\n  {} were excluded by their position relative to the anchor. The distance may be too tight",
                self.filtered_out
            )?;
        }
        if let Some(a) = &self.artifacts {
            write!(f, "\n  diagnostic files: {a}")?;
        }
        Ok(())
    }
}

impl std::error::Error for FindFailed {}

/// The failure details are boxed.
///
/// `FindFailed` carries diagnostics (artifact paths and so on) and is large;
/// putting it directly in a `Result` would fatten the success path's return
/// value too. Searching is the hottest path, so it is kept tight here.
#[derive(Debug)]
pub enum Error {
    Capture(CaptureError),
    Input(InputError),
    Overlay(OverlayError),
    Ocr(OcrError),
    Uia(UiaError),
    NotFound(Box<FindFailed>),
    Assertion(Box<AssertionFailed>),
    /// Loading an image failed.
    Image(String),
    /// The template has no texture (per-pixel standard deviation below one
    /// quantization step).
    ///
    /// ZMD is built on the assumption that no uniform template exists (one
    /// whose template-side energy in the denominator is 0). The matcher's own
    /// defence merely returns an all-zero map, which tells the user nothing
    /// about the cause, so this is an explicit error at load time.
    FlatPattern(String),
    /// A stop was requested from outside. This is not a failure, so callers
    /// should treat it as "stopped" rather than as an error.
    Interrupted,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capture(e) => write!(f, "{e}"),
            Self::Input(e) => write!(f, "{e}"),
            Self::Overlay(e) => write!(f, "{e}"),
            Self::Ocr(e) => write!(f, "{e}"),
            Self::Uia(e) => write!(f, "{e}"),
            Self::NotFound(e) => write!(f, "{e}"),
            Self::Assertion(e) => write!(f, "{e}"),
            Self::Image(e) => write!(f, "cannot read the image: {e}"),
            Self::FlatPattern(name) => write!(
                f,
                "the template has no texture: {name} (a near-blank image cannot be searched for)"
            ),
            Self::Interrupted => write!(f, "stopped"),
        }
    }
}

impl std::error::Error for Error {}

impl From<CaptureError> for Error {
    fn from(e: CaptureError) -> Self {
        Self::Capture(e)
    }
}

impl From<InputError> for Error {
    fn from(e: InputError) -> Self {
        Self::Input(e)
    }
}

impl From<OverlayError> for Error {
    fn from(e: OverlayError) -> Self {
        Self::Overlay(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The core engine.
pub struct Mekiki {
    pub(crate) capture: Box<dyn ScreenCapture>,
    pub(crate) input: Box<dyn InputInjector>,
    pub(crate) matcher: search::Matcher,
    /// The overlay is not created until `highlight()` is called. There is no
    /// reason to register a window class every run purely for debugging.
    overlay: Option<Box<dyn Overlay>>,
    /// The OCR engine is likewise deferred until `ocr:` is used, so scripts
    /// that only do image matching do not pay the initialisation cost.
    pub(crate) ocr: Option<Box<dyn TextRecognizer>>,
    /// Accessibility search is deferred until `ui:` is used. It initialises
    /// COM, so there is no reason to charge scripts that never use it.
    pub(crate) uia: Option<Box<dyn ElementFinder>>,
    /// Stop and pause requests from outside. Nothing touches it by default, so
    /// it has no effect on a run.
    pub(crate) interrupt: Interrupt,
    pub settings: Settings,
}

impl Mekiki {
    pub fn new() -> Result<Self> {
        Self::with_settings(Settings::default())
    }

    /// Construct with substituted backends.
    ///
    /// For tests and for embedding. Passing a capture implementation that
    /// returns a synthetic screen lets the whole search pipeline be verified
    /// without real hardware or a display.
    pub fn with_backends(
        capture: Box<dyn ScreenCapture>,
        input: Box<dyn InputInjector>,
        matcher: search::Matcher,
        settings: Settings,
    ) -> Self {
        Self {
            capture,
            input,
            matcher,
            overlay: None,
            ocr: None,
            uia: None,
            interrupt: Interrupt::new(),
            settings,
        }
    }

    pub fn with_settings(settings: Settings) -> Result<Self> {
        let capture = mekiki_capture::open()?;
        let input = mekiki_input::open()?;
        let matcher = search::Matcher::best_available();

        log::info!(
            "Mekiki core: capture={} input={} matching={}",
            capture.backend_name(),
            input.backend_name(),
            matcher.backend_name()
        );

        Ok(Self {
            capture,
            input,
            matcher,
            overlay: None,
            ocr: None,
            uia: None,
            interrupt: Interrupt::new(),
            settings,
        })
    }

    // -----------------------------------------------------------------------
    // Regions
    // -----------------------------------------------------------------------

    pub fn displays(&self) -> &[DisplayInfo] {
        self.capture.displays()
    }

    /// A [`Region`] covering a whole display.
    pub fn screen(&self, index: usize) -> Result<Region> {
        let d = self
            .capture
            .displays()
            .get(index)
            .ok_or(CaptureError::NoSuchDisplay(index))?;
        Ok(Region::new(d.bounds, index))
    }

    /// The primary display, or the first one if there is none.
    pub fn primary_screen(&self) -> Result<Region> {
        let displays = self.capture.displays();
        let d = displays
            .iter()
            .find(|d| d.is_primary)
            .or_else(|| displays.first())
            .ok_or(CaptureError::NoDisplays)?;
        Ok(Region::new(d.bounds, d.index))
    }

    /// Turn an arbitrary rectangle into a [`Region`].
    pub fn region(&self, rect: Rect) -> Region {
        let display = self
            .capture
            .displays()
            .iter()
            .find(|d| d.bounds.intersect(&rect).is_some())
            .map(|d| d.index)
            .unwrap_or(0);
        Region::new(rect, display)
    }

    /// Enumerate visible top-level windows in Z order, frontmost first.
    pub fn window_list(&self) -> Result<Vec<WindowInfo>> {
        Ok(mekiki_capture::windows()?)
    }

    /// Look up a window by a substring of its title and turn its rectangle into
    /// a [`Region`].
    ///
    /// Narrowing the search to one window is faster and **stops the same image
    /// in a background window from being matched**. The latter matters when
    /// several copies of the same application are open.
    pub fn window(&self, title_contains: &str) -> Result<Region> {
        let w = mekiki_capture::find_window(title_contains)?;
        Ok(self.region(w.bounds).with_window(w.hwnd))
    }

    /// Look up a window by exact title.
    pub fn window_exact(&self, title: &str) -> Result<Region> {
        let w = mekiki_capture::find_window_exact(title)?;
        Ok(self.region(w.bounds).with_window(w.hwnd))
    }

    /// Look up a window by conditions.
    ///
    /// A title changes with the file being edited, so it can be combined with
    /// the executable name or the window class. The reasoning is in
    /// [window locator design](../../../docs/architecture/window-locators.md).
    pub fn window_by(&self, query: &WindowQuery) -> Result<Region> {
        let w = mekiki_capture::find_window_by(query)?;
        Ok(self.region(w.bounds).with_window(w.hwnd))
    }

    /// Bring the window matching the conditions to the front. Does not restore
    /// a minimised one.
    pub fn activate_window(&self, query: &WindowQuery) -> Result<()> {
        Ok(mekiki_capture::activate_window(query)?)
    }

    /// Bring a window captured by an existing [`Region`] to the front without
    /// resolving its query again.
    pub fn activate_window_handle(&self, hwnd: isize) -> Result<()> {
        Ok(mekiki_capture::activate_window_handle(hwnd)?)
    }

    // -----------------------------------------------------------------------
    // Patterns and targets
    // -----------------------------------------------------------------------

    /// Build a pattern from an image file.
    ///
    /// An image with no texture (per-pixel standard deviation below one
    /// quantization step, 1/255) yields [`Error::FlatPattern`].
    pub fn pattern_from_file(&self, path: impl AsRef<Path>) -> Result<Pattern> {
        let path = path.as_ref();
        let img = image::open(path)
            .map_err(|e| Error::Image(format!("{}: {e}", path.display())))?
            .to_luma8();
        let (w, h) = img.dimensions();
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());

        // A load-time template check, not a runtime guard — ZMD has no runtime
        // guard. The test is deterministic in f64.
        let n = img.as_raw().len() as f64;
        let mean = img.as_raw().iter().map(|&b| f64::from(b)).sum::<f64>() / n;
        let var = img
            .as_raw()
            .iter()
            .map(|&b| (f64::from(b) - mean).powi(2))
            .sum::<f64>()
            / n;
        // Pixel values are 0..=255, so scale by 1/255 before comparing.
        if var.sqrt() / 255.0 < mekiki_matching::MIN_TEMPLATE_STDDEV {
            return Err(Error::FlatPattern(name));
        }

        Ok(self.pattern_from_luma8(img.as_raw(), w, h, name))
    }

    /// Build a pattern from 8-bit greyscale bytes.
    pub fn pattern_from_luma8(
        &self,
        bytes: &[u8],
        width: u32,
        height: u32,
        name: impl Into<String>,
    ) -> Pattern {
        let img = Image::from_luma8(bytes, width, height);
        self.build_pattern(img, name)
    }

    /// Build a pattern from an already-captured frame.
    pub fn pattern_from_frame(&self, frame: &Frame, name: impl Into<String>) -> Pattern {
        let img = Image::new(frame.to_luma_f32(), frame.width, frame.height);
        self.build_pattern(img, name)
    }

    fn build_pattern(&self, img: Image<'_>, name: impl Into<String>) -> Pattern {
        Pattern::from_image(
            img,
            name,
            self.settings.min_similarity,
            self.settings.search.max_levels,
            self.settings.search.min_level_size,
        )
    }

    /// Create a target that searches for an image. **Nothing is searched for
    /// at this point.**
    pub fn target(&self, region: Region, pattern: &Pattern) -> Target {
        Target::new(region, Needle::Image(pattern.clone()))
    }

    /// Create a target that searches for text on screen.
    ///
    /// The threshold is on the same scale as for images (`0.0..=1.0`). OCR
    /// variation (spacing, full-width versus half-width, long-vowel marks) is
    /// absorbed before comparison.
    pub fn text_target(&self, region: Region, query: impl Into<String>) -> Target {
        Target::new(
            region,
            Needle::Text(TextPattern::new(query, self.settings.min_similarity)),
        )
    }

    /// Replace the flag that carries stop and pause requests.
    ///
    /// The caller (the IDE worker or the CLI) keeps a clone and calls
    /// [`Interrupt::stop`] and friends from another thread.
    pub fn set_interrupt(&mut self, interrupt: Interrupt) {
        self.interrupt = interrupt;
    }

    /// A clone of the flag currently in use.
    pub fn interrupt(&self) -> Interrupt {
        self.interrupt.clone()
    }

    /// Supply the accessibility backend instead of opening the platform one.
    ///
    /// For tests and for embedding, in the same spirit as
    /// [`Mekiki::with_backends`]. `ui:` is otherwise the one search path with no
    /// way to substitute a fake, which makes it the one path that cannot be
    /// tested without a live desktop.
    pub fn set_element_finder(&mut self, finder: Box<dyn ElementFinder>) {
        self.uia = Some(finder);
    }

    /// Release any held mouse button or modifier key.
    ///
    /// Cleanup after an interruption. Stopping with something held down drags
    /// the user's own input down with it, so this runs on both stop and pause.
    pub fn release_input(&mut self) -> Result<()> {
        self.input.release_all()?;
        Ok(())
    }

    /// Create a target that searches via the accessibility API (Phase 4-4).
    ///
    /// The conditions are assembled with the [`UiPattern`] builder. Specifying
    /// none of them would match every element on screen, so that is rejected at
    /// resolution time.
    pub fn ui_target(&self, region: Region, ui: UiPattern) -> Target {
        Target::new(region, Needle::Ui(ui))
    }

    /// An empty pattern for [`Mekiki::ui_target`], with only the threshold
    /// taken from the settings.
    pub fn ui_pattern(&self) -> UiPattern {
        UiPattern::new(self.settings.min_similarity)
    }

    /// Whether OCR is available and, if so, in which languages.
    pub fn ocr_languages() -> std::result::Result<Vec<String>, OcrError> {
        mekiki_ocr::available_languages()
    }

    /// Act on a target.
    pub fn on<'a>(&'a mut self, target: &'a Target) -> Act<'a> {
        Act {
            engine: self,
            target,
        }
    }

    /// Assert with retries.
    pub fn expect(&mut self, target: &Target) -> Expect<'_> {
        Expect {
            engine: self,
            target: target.clone(),
        }
    }

    // -----------------------------------------------------------------------
    // Capture
    // -----------------------------------------------------------------------

    pub fn capture_region(&mut self, region: Region) -> Result<Frame> {
        Ok(self.capture.capture_rect(region.rect)?)
    }

    pub fn capture_display(&mut self, index: usize) -> Result<Frame> {
        Ok(self.capture.capture(index)?)
    }

    // -----------------------------------------------------------------------
    // The classic search API
    //
    // Going through a Target is safer, since the coordinates never go stale,
    // but this is kept for the case of "search once and give me the coordinates".
    // -----------------------------------------------------------------------

    /// Wait for actionability and return one match. Shorthand for
    /// [`Act::resolve`].
    pub fn find(&mut self, region: Region, pattern: &Pattern) -> Result<Match> {
        let t = self.target(region, pattern);
        self.wait_actionable(&t)
    }

    /// Return every match meeting the conditions; empty if there are none.
    pub fn find_all(&mut self, region: Region, pattern: &Pattern) -> Result<Vec<Match>> {
        let t = self.target(region, pattern);
        Ok(self.scan_target_all(&t)?.0)
    }

    /// Wait until it appears.
    pub fn wait(
        &mut self,
        region: Region,
        pattern: &Pattern,
        timeout: Option<Duration>,
    ) -> Result<Match> {
        let mut t = self.target(region, pattern);
        if let Some(d) = timeout {
            t = t.timeout(d);
        }
        self.wait_actionable(&t)
    }

    /// Wait until it disappears. `true` once it does.
    pub fn wait_vanish(
        &mut self,
        region: Region,
        pattern: &Pattern,
        timeout: Option<Duration>,
    ) -> Result<bool> {
        let t = self.target(region, pattern);
        self.on(&t).wait_vanish(timeout)
    }

    /// Just check whether it is there.
    pub fn exists(
        &mut self,
        region: Region,
        pattern: &Pattern,
        timeout: Option<Duration>,
    ) -> Result<Option<Match>> {
        match self.wait(region, pattern, timeout) {
            Ok(m) => Ok(Some(m)),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    // -----------------------------------------------------------------------
    // Input (the low-level API that takes coordinates directly)
    // -----------------------------------------------------------------------

    pub fn mouse_position(&self) -> Result<(i32, i32)> {
        Ok(self.input.cursor_position()?)
    }

    /// Move the cursor only. No activation, no waiting.
    ///
    /// If a stop has been requested it does not jump to the target but returns
    /// [`Error::Interrupted`] where it is.
    pub fn move_to(&mut self, target: (i32, i32)) -> Result<()> {
        if self.interrupt.is_stopping() {
            return Err(Error::Interrupted);
        }

        let (sx, sy) = self.input.cursor_position()?;
        let dx = (target.0 - sx) as f64;
        let dy = (target.1 - sy) as f64;
        let distance = (dx * dx + dy * dy).sqrt();
        let duration = travel_duration(
            distance,
            self.settings.move_speed,
            self.settings.move_max_duration,
        );

        if duration.is_zero() {
            self.input.mouse_move(target.0, target.1)?;
            return Ok(());
        }

        let started = Instant::now();
        let mut paused_total = Duration::ZERO;
        loop {
            if self.interrupt.is_stopping() {
                return Err(Error::Interrupted);
            }
            paused_total += self.interrupt.wait_while_paused();
            if self.interrupt.is_stopping() {
                return Err(Error::Interrupted);
            }

            let elapsed = started.elapsed().saturating_sub(paused_total);
            let t = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
            let e = out_quartic(t);
            let nx = sx + (dx as f32 * e).round() as i32;
            let ny = sy + (dy as f32 * e).round() as i32;
            self.input.mouse_move(nx, ny)?;

            if t >= 1.0 {
                break;
            }
            let remaining = duration.saturating_sub(elapsed);
            std::thread::sleep(Duration::from_millis(10).min(remaining));
        }

        if self.input.cursor_position()? != target {
            self.input.mouse_move(target.0, target.1)?;
        }
        Ok(())
    }

    pub fn hover(&mut self, target: (i32, i32)) -> Result<()> {
        self.move_to(target)?;
        self.input.activate_at(target.0, target.1)?;
        std::thread::sleep(self.settings.move_settle);
        Ok(())
    }

    pub fn click(&mut self, target: (i32, i32)) -> Result<()> {
        self.click_button(target, MouseButton::Left)
    }

    pub fn right_click(&mut self, target: (i32, i32)) -> Result<()> {
        self.click_button(target, MouseButton::Right)
    }

    pub fn click_button(&mut self, target: (i32, i32), button: MouseButton) -> Result<()> {
        self.hover(target)?;
        self.input.mouse_down(button)?;
        std::thread::sleep(self.settings.click_hold);
        self.input.mouse_up(button)?;
        Ok(())
    }

    pub fn double_click(&mut self, target: (i32, i32)) -> Result<()> {
        self.hover(target)?;
        for i in 0..2 {
            self.input.mouse_down(MouseButton::Left)?;
            std::thread::sleep(self.settings.click_hold);
            self.input.mouse_up(MouseButton::Left)?;
            if i == 0 {
                std::thread::sleep(self.settings.double_click_interval);
            }
        }
        Ok(())
    }

    /// Drag and drop.
    ///
    /// `hover` over the start, then [`move_to`] the end with the button held.
    /// The button is always released, even on a failure or a stop part-way.
    pub fn drag_drop(&mut self, from: (i32, i32), to: (i32, i32)) -> Result<()> {
        self.hover(from)?;
        self.input.mouse_down(MouseButton::Left)?;
        std::thread::sleep(self.settings.click_hold);

        let moved = self.move_to(to);
        if moved.is_err() {
            let _ = self.input.mouse_up(MouseButton::Left);
            return moved;
        }

        std::thread::sleep(self.settings.click_hold);
        self.input.mouse_up(MouseButton::Left)?;
        Ok(())
    }

    pub fn scroll(&mut self, horizontal: i32, vertical: i32) -> Result<()> {
        Ok(self.input.scroll(horizontal, vertical)?)
    }

    /// Type a string, in chunks of [`Settings::type_chunk`] characters.
    ///
    /// Splitting is not cosmetic: some applications drop characters from a long
    /// burst even though `SendInput` reported every event as accepted. See
    /// [`Settings::type_chunk`] for what was measured and what the split costs.
    ///
    /// Chunk boundaries fall between characters, so a surrogate pair is never
    /// cut in half.
    ///
    /// **Line breaks and tabs are keystrokes, not characters.** `\r\n`, `\n`
    /// and `\r` are each typed as one Enter press and `\t` as a Tab press;
    /// injected as Unicode code units they do nothing in most editors, and
    /// silently running the lines together was the worst of the outcomes.
    ///
    /// [`Settings::type_interval`] is applied before **every** send, including
    /// the first: the character typed right after a key press is exactly the
    /// one Windows 11's Notepad was measured to drop, and an interval that
    /// only runs between chunks leaves that first character unprotected.
    pub fn type_text(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        let mut segment = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' | '\n' => {
                    if c == '\r' && chars.peek() == Some(&'\n') {
                        chars.next(); // one CRLF is one Enter, not two
                    }
                    self.type_segment(&segment)?;
                    segment.clear();
                    self.key_press(Key::Enter, Modifiers::NONE)?;
                }
                '\t' => {
                    self.type_segment(&segment)?;
                    segment.clear();
                    self.key_press(Key::Tab, Modifiers::NONE)?;
                }
                c => segment.push(c),
            }
        }
        self.type_segment(&segment)
    }

    /// Type one run of plain characters, chunked, pausing before every send.
    fn type_segment(&mut self, segment: &str) -> Result<()> {
        if segment.is_empty() {
            return Ok(());
        }

        let chunk = self.settings.type_chunk;
        if chunk == 0 {
            self.pause_before_send()?;
            return Ok(self.input.type_text(segment)?);
        }

        let chars: Vec<char> = segment.chars().collect();
        for group in chars.chunks(chunk) {
            self.pause_before_send()?;
            let part: String = group.iter().collect();
            self.input.type_text(&part)?;
        }
        Ok(())
    }

    /// The gap in front of every injected keystroke or chunk.
    ///
    /// Checks the stop flag first: a stop must not leave half a string typed
    /// and then keep going.
    fn pause_before_send(&mut self) -> Result<()> {
        if self.interrupt.is_stopping() {
            return Err(Error::Interrupted);
        }
        let interval = self.settings.type_interval;
        if !interval.is_zero() {
            std::thread::sleep(interval);
        }
        Ok(())
    }

    pub fn key_press(&mut self, key: Key, modifiers: Modifiers) -> Result<()> {
        self.pause_before_send()?;
        Ok(self.input.key_press(key, modifiers)?)
    }

    /// Read the clipboard text. An empty or non-text clipboard is `""`.
    pub fn clipboard_text(&self) -> Result<String> {
        Ok(mekiki_input::clipboard_text()?)
    }

    /// Replace the clipboard contents with text.
    pub fn set_clipboard_text(&self, text: &str) -> Result<()> {
        Ok(mekiki_input::set_clipboard_text(text)?)
    }

    // -----------------------------------------------------------------------
    // Debugging
    // -----------------------------------------------------------------------

    /// Outline a rectangle on screen.
    ///
    /// **Blocks for `duration`.** Intended for debugging only.
    pub fn highlight_rect(&mut self, rect: Rect, duration: Duration) -> Result<()> {
        self.highlight_rect_colored(rect, duration, HighlightColor::RED)
    }

    pub fn highlight_rect_colored(
        &mut self,
        rect: Rect,
        duration: Duration,
        color: HighlightColor,
    ) -> Result<()> {
        if self.overlay.is_none() {
            self.overlay = Some(mekiki_overlay::open()?);
        }
        let overlay = self.overlay.as_mut().expect("just created above");
        let style = mekiki_overlay::Style {
            color,
            duration,
            ..Default::default()
        };
        overlay.show(rect.x, rect.y, rect.width, rect.height, style)?;
        Ok(())
    }

    /// For inspecting matches: draw several outlines with their scores on the
    /// desktop.
    ///
    /// The call does not block; the marks disappear after `duration`. Passing
    /// nothing clears them.
    pub fn show_match_marks(
        &mut self,
        hits: impl IntoIterator<Item = (Rect, f32)>,
        duration: Duration,
    ) -> Result<()> {
        if self.overlay.is_none() {
            self.overlay = Some(mekiki_overlay::open()?);
        }
        let marks: Vec<mekiki_overlay::Mark> = hits
            .into_iter()
            .filter(|(r, _)| r.width > 0 && r.height > 0)
            .map(|(r, score)| mekiki_overlay::Mark {
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
                label: Some(mekiki_overlay::format_score(score)),
            })
            .collect();
        let overlay = self.overlay.as_mut().expect("just created above");
        if marks.is_empty() {
            overlay.hide_marks()?;
            return Ok(());
        }
        let style = mekiki_overlay::Style {
            color: HighlightColor::RED,
            duration,
            ..Default::default()
        };
        overlay.show_marks(&marks, style)?;
        Ok(())
    }

    /// Clear the marks drawn by [`show_match_marks`] immediately.
    pub fn hide_match_marks(&mut self) -> Result<()> {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.hide_marks()?;
        }
        Ok(())
    }

    pub fn matcher_backend(&self) -> &'static str {
        self.matcher.backend_name()
    }

    /// The capture backend's state. Used to narrow things down when it is
    /// slower than expected.
    pub fn capture_diagnostics(&self) -> String {
        self.capture.diagnostics()
    }

    // -----------------------------------------------------------------------
    // Failure artifacts
    // -----------------------------------------------------------------------

    /// Write out diagnostic material for a failure. Failures here are
    /// swallowed: replacing the real error with a diagnostics error would
    /// defeat the purpose.
    pub(crate) fn write_failure_artifacts(&mut self, target: &Target) -> Option<FailureArtifacts> {
        let dir = self.settings.artifact_dir.clone()?;
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!(
                "cannot create the artifact directory {}: {e}",
                dir.display()
            );
            return None;
        }

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let stem = format!("{}-{stamp}", artifacts::sanitize(&target.describe()));

        let frame = match self.capture.capture_rect(target.region.rect) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("failed to capture for the artifact: {e}");
                return None;
            }
        };

        let screen = dir.join(format!("{stem}-screen.png"));
        if let Err(e) = artifacts::save_frame(&frame, &screen) {
            log::warn!("cannot write {}: {e}", screen.display());
            return None;
        }

        // Take a full-size score map. This cost is only paid on failure, so a
        // brute-force pass is fine.
        //
        // A text target has no notion of a score map, so only the capture is
        // kept and no heat map is produced.
        let Some(pattern) = target.pattern().cloned() else {
            return Some(FailureArtifacts {
                screen,
                heatmap: None,
                annotated: None,
                candidates: None,
            });
        };

        let haystack = Image::new(frame.to_luma_f32(), frame.width, frame.height);
        let scores = self.matcher.score_map(&haystack, pattern.level(0));

        let heatmap = scores.as_ref().and_then(|map| {
            let path = dir.join(format!("{stem}-heatmap.png"));
            match artifacts::save_heatmap(map, &path) {
                Ok(()) => Some(path),
                Err(e) => {
                    log::warn!("cannot write the heat map: {e}");
                    None
                }
            }
        });

        // Collect the top candidates ignoring the threshold, so it is visible
        // whether the search just fell short or was nowhere close.
        let candidates: Vec<Match> = scores
            .as_ref()
            .map(|map| {
                let opts = mekiki_matching::NmsOptions::similar(-1.0)
                    .with_max_results(self.settings.artifact_candidates);
                mekiki_matching::find_matches(map, (pattern.width(), pattern.height()), &opts)
                    .into_iter()
                    .map(|m| Match {
                        rect: Rect::new(
                            frame.origin.0 + m.x as i32,
                            frame.origin.1 + m.y as i32,
                            m.width,
                            m.height,
                        ),
                        display: target.region().display,
                        score: m.score,
                        target_offset: (0, 0),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let annotated = if candidates.is_empty() {
            None
        } else {
            let path = dir.join(format!("{stem}-annotated.png"));
            match artifacts::save_annotated(&frame, &candidates, &path) {
                Ok(()) => Some(path),
                Err(e) => {
                    log::warn!("cannot write the annotated image: {e}");
                    None
                }
            }
        };

        let list = {
            let path = dir.join(format!("{stem}-candidates.txt"));
            match artifacts::save_candidates(&candidates, pattern.similarity(), &path) {
                Ok(()) => Some(path),
                Err(e) => {
                    log::warn!("cannot write the candidate list: {e}");
                    None
                }
            }
        };

        Some(FailureArtifacts {
            screen,
            heatmap,
            annotated,
            candidates: list,
        })
    }
}

/// Compute the travel duration from distance and speed.
///
/// 0 (an instant jump) when `speed <= 0` or the distance is 0. Clipped at the
/// maximum.
pub fn travel_duration(distance: f64, speed: f64, max: Duration) -> Duration {
    if speed <= 0.0 || distance <= 0.0 || !speed.is_finite() || !distance.is_finite() {
        return Duration::ZERO;
    }
    let secs = distance / speed;
    if secs <= 0.0 || !secs.is_finite() {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(secs).min(max)
}

fn out_quartic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(4)
}

/// Wait a fixed time without performing an action. Used by the scripting layer.
pub fn sleep(duration: Duration) {
    std::thread::sleep(duration);
}

/// Enumerate visible top-level windows.
///
/// Usable without constructing a [`Mekiki`], for things like listing them from
/// the CLI.
pub fn window_list() -> Result<Vec<WindowInfo>> {
    Ok(mekiki_capture::windows()?)
}

/// A helper for measuring elapsed time.
pub fn since(start: Instant) -> Duration {
    start.elapsed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings() {
        let s = Settings::default();
        assert_eq!(s.min_similarity, 0.7);
        assert_eq!(s.auto_wait_timeout, Duration::from_secs(3));
        // The double-click interval must be shorter than the OS double-click
        // time (500ms by default).
        assert!(s.double_click_interval < Duration::from_millis(500));
        // The stability check is on by default.
        assert!(s.actionability.stable_frames > 0);
        // Re-searching per action is on by default.
        assert!(s.recheck);
        // The default ordering is reading order.
        assert_eq!(s.match_order, MatchOrder::ReadingOrder);
        // Artifacts are on by default.
        assert!(s.artifact_dir.is_some());
        assert_eq!(s.move_speed, 1200.0);
        assert_eq!(s.move_max_duration, Duration::from_secs(2));
    }

    #[test]
    fn travel_duration_is_distance_over_speed() {
        let max = Duration::from_secs(2);
        assert_eq!(travel_duration(1200.0, 1200.0, max), Duration::from_secs(1));
        assert_eq!(
            travel_duration(600.0, 1200.0, max),
            Duration::from_millis(500)
        );
        assert_eq!(travel_duration(6000.0, 1200.0, max), max);
        assert_eq!(travel_duration(100.0, 0.0, max), Duration::ZERO);
        assert_eq!(travel_duration(0.0, 1200.0, max), Duration::ZERO);
    }

    #[test]
    fn out_quartic_starts_fast_and_ends_at_one() {
        assert!((out_quartic(0.0) - 0.0).abs() < 1e-6);
        assert!((out_quartic(1.0) - 1.0).abs() < 1e-6);
        assert!(out_quartic(0.25) > 0.25);
    }

    #[test]
    fn find_failed_message_includes_scores() {
        let f = FindFailed {
            pattern: "ok.png".into(),
            region: Rect::new(0, 0, 100, 100),
            best_score: Some(0.61),
            required: 0.7,
            waited: Duration::from_millis(3000),
            anchor_missing: None,
            unstable: false,
            filtered_out: 0,
            artifacts: None,
        };
        let msg = f.to_string();
        assert!(msg.contains("ok.png"), "{msg}");
        assert!(msg.contains("0.61"), "{msg}");
        assert!(msg.contains("0.70"), "{msg}");
    }

    /// A failure caused by the anchor is reported distinctly from one caused by
    /// the target. Confusing them sends you to fix the wrong thing.
    #[test]
    fn anchor_failure_is_reported_distinctly() {
        let f = FindFailed {
            pattern: "field.png".into(),
            region: Rect::new(0, 0, 100, 100),
            best_score: None,
            required: 0.7,
            waited: Duration::from_millis(3000),
            anchor_missing: Some("label.png".into()),
            unstable: false,
            filtered_out: 0,
            artifacts: None,
        };
        let msg = f.to_string();
        assert!(msg.contains("anchor"), "{msg}");
        assert!(msg.contains("label.png"), "{msg}");
    }

    #[test]
    fn unstable_failure_suggests_force() {
        let f = FindFailed {
            pattern: "banner.png".into(),
            region: Rect::new(0, 0, 100, 100),
            best_score: Some(0.99),
            required: 0.7,
            waited: Duration::from_millis(3000),
            anchor_missing: None,
            unstable: true,
            filtered_out: 0,
            artifacts: None,
        };
        assert!(f.to_string().contains("force()"), "{f}");
    }

    #[test]
    fn filtered_out_hints_at_distance() {
        let f = FindFailed {
            pattern: "field.png".into(),
            region: Rect::new(0, 0, 100, 100),
            best_score: Some(0.95),
            required: 0.7,
            waited: Duration::from_millis(100),
            anchor_missing: None,
            unstable: false,
            filtered_out: 3,
            artifacts: None,
        };
        let msg = f.to_string();
        assert!(msg.contains("3 were excluded"), "{msg}");
    }
}

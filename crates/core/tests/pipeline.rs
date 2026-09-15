//! Integration tests for the whole search pipeline.
//!
//! Substituting fakes for capture and input verifies the end-to-end path —
//! `Target`, scan, anchor filtering, sorting, selection, automatic wait —
//! without real hardware or a display.
//!
//! Some bugs only show up here. Unit tests for the individual pieces check the
//! anchor geometry and the reading-order sort separately, but **which of the
//! two wins when both are used at once** only appears end to end.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mekiki_capture::{CaptureError, DisplayInfo, Frame, Rect, ScreenCapture};
use mekiki_core::search::Matcher;
use mekiki_core::{MatchOrder, Mekiki, Pattern, Region, Settings};
use mekiki_input::{InputError, InputInjector, Key, MouseButton};

// ---------------------------------------------------------------------------
// Fake backends
// ---------------------------------------------------------------------------

/// A capture that returns a prepared sequence of frames in order.
///
/// Once it reaches the last frame it keeps returning that one, which reproduces
/// "a moving screen that eventually settles".
struct ScriptedCapture {
    frames: Vec<Vec<u8>>,
    width: u32,
    height: u32,
    calls: Arc<Mutex<usize>>,
    displays: Vec<DisplayInfo>,
}

impl ScriptedCapture {
    fn new(width: u32, height: u32, frames: Vec<Vec<u8>>) -> Self {
        Self {
            frames,
            width,
            height,
            calls: Arc::new(Mutex::new(0)),
            displays: vec![DisplayInfo {
                index: 0,
                name: "fake".into(),
                bounds: Rect::new(0, 0, width, height),
                is_primary: true,
            }],
        }
    }

    fn full_frame(&self) -> &[u8] {
        let mut calls = self.calls.lock().unwrap();
        let idx = (*calls).min(self.frames.len() - 1);
        *calls += 1;
        &self.frames[idx]
    }
}

impl ScreenCapture for ScriptedCapture {
    fn displays(&self) -> &[DisplayInfo] {
        &self.displays
    }

    fn capture(&mut self, _display: usize) -> Result<Frame, CaptureError> {
        let bgra = self.full_frame().to_vec();
        Ok(Frame {
            width: self.width,
            height: self.height,
            origin: (0, 0),
            bgra,
        })
    }

    fn backend_name(&self) -> &'static str {
        "fake/scripted"
    }
}

/// A capture that always returns the same frame and **counts the captures**.
///
/// Used to see whether change detection (Phase 4-2) is skipping searches. There
/// is one capture per search, so "skipped" does not mean "fewer captures than
/// polls": the capture count is unchanged and **only the search** goes away.
/// Counting the searches would mean counting the calls that involve template
/// matching separately, but rather than peer inside, this measures elapsed time.
struct CountingCapture {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
    displays: Vec<DisplayInfo>,
    captures: Arc<Mutex<usize>>,
}

impl ScreenCapture for CountingCapture {
    fn displays(&self) -> &[DisplayInfo] {
        &self.displays
    }

    fn capture(&mut self, _display: usize) -> Result<Frame, CaptureError> {
        *self.captures.lock().unwrap() += 1;
        Ok(Frame {
            width: self.width,
            height: self.height,
            origin: (0, 0),
            bgra: self.bgra.clone(),
        })
    }

    fn backend_name(&self) -> &'static str {
        "fake/counting"
    }
}

/// An input backend that only records the operations it was asked to perform.
#[derive(Default)]
struct RecordingInput {
    log: Arc<Mutex<Vec<String>>>,
    position: (i32, i32),
}

impl InputInjector for RecordingInput {
    fn mouse_move(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        self.position = (x, y);
        self.log.lock().unwrap().push(format!("move({x},{y})"));
        Ok(())
    }
    fn activate_at(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        self.log.lock().unwrap().push(format!("activate({x},{y})"));
        Ok(())
    }
    fn mouse_down(&mut self, b: MouseButton) -> Result<(), InputError> {
        self.log.lock().unwrap().push(format!("down({b:?})"));
        Ok(())
    }
    fn mouse_up(&mut self, b: MouseButton) -> Result<(), InputError> {
        self.log.lock().unwrap().push(format!("up({b:?})"));
        Ok(())
    }
    fn scroll(&mut self, h: i32, v: i32) -> Result<(), InputError> {
        self.log.lock().unwrap().push(format!("scroll({h},{v})"));
        Ok(())
    }
    fn key_down(&mut self, k: Key) -> Result<(), InputError> {
        self.log.lock().unwrap().push(format!("key_down({k:?})"));
        Ok(())
    }
    fn key_up(&mut self, k: Key) -> Result<(), InputError> {
        self.log.lock().unwrap().push(format!("key_up({k:?})"));
        Ok(())
    }
    fn type_text(&mut self, text: &str) -> Result<(), InputError> {
        self.log.lock().unwrap().push(format!("type({text})"));
        Ok(())
    }
    fn cursor_position(&self) -> Result<(i32, i32), InputError> {
        Ok(self.position)
    }
    fn backend_name(&self) -> &'static str {
        "fake/recording"
    }
}

// ---------------------------------------------------------------------------
// Synthetic screens
// ---------------------------------------------------------------------------

struct Canvas {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        // A uniform image is useless as a template (it matches nothing), so lay
        // down a faint texture.
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let v = (((x * 7 + y * 13) % 23) + 200) as u8;
                let i = ((y * width + x) * 4) as usize;
                bgra[i] = v;
                bgra[i + 1] = v;
                bgra[i + 2] = v;
                bgra[i + 3] = 255;
            }
        }
        Self {
            width,
            height,
            bgra,
        }
    }

    /// Stamp a tile with a deterministic pattern. A different `seed` looks
    /// different.
    fn stamp(&mut self, x: u32, y: u32, size: u32, seed: u32) {
        for dy in 0..size {
            for dx in 0..size {
                let px = x + dx;
                let py = y + dy;
                if px >= self.width || py >= self.height {
                    continue;
                }
                let v = ((dx * 37 + dy * 61 + seed * 97) % 200) as u8;
                let i = ((py * self.width + px) * 4) as usize;
                self.bgra[i] = v;
                self.bgra[i + 1] = v.wrapping_add(20);
                self.bgra[i + 2] = v.wrapping_add(40);
                self.bgra[i + 3] = 255;
            }
        }
    }

    /// Build a pattern that looks like the tile.
    fn tile_pattern(mekiki: &Mekiki, size: u32, seed: u32, name: &str) -> Pattern {
        let mut c = Canvas::new(size, size);
        c.stamp(0, 0, size, seed);
        let luma: Vec<u8> = c
            .bgra
            .chunks_exact(4)
            .map(|p| {
                (0.114 * p[0] as f32 + 0.587 * p[1] as f32 + 0.299 * p[2] as f32).round() as u8
            })
            .collect();
        mekiki.pattern_from_luma8(&luma, size, size, name)
    }
}

fn engine(frames: Vec<Canvas>) -> Mekiki {
    engine_with(frames, Settings::default())
}

fn engine_with(frames: Vec<Canvas>, mut settings: Settings) -> Mekiki {
    let width = frames[0].width;
    let height = frames[0].height;
    // The synthetic screens are small, so paying the GPU initialisation cost
    // makes no sense.
    settings.artifact_dir = None;
    // Tighten the waits to keep the tests fast.
    settings.wait_scan_interval = Duration::from_millis(5);
    settings.actionability.stable_interval = Duration::from_millis(1);
    settings.move_settle = Duration::from_millis(0);
    settings.move_speed = 0.0;
    settings.click_hold = Duration::from_millis(0);

    let capture = ScriptedCapture::new(width, height, frames.into_iter().map(|c| c.bgra).collect());
    Mekiki::with_backends(
        Box::new(capture),
        Box::new(RecordingInput::default()),
        Matcher::cpu(),
        settings,
    )
}

fn screen(m: &Mekiki) -> Region {
    m.primary_screen().unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn target_resolves_planted_tile() {
    let mut c = Canvas::new(320, 240);
    c.stamp(100, 60, 40, 1);
    let mut m = engine(vec![c]);

    let p = Canvas::tile_pattern(&m, 40, 1, "tile1").similar(0.95);
    let s = screen(&m);
    let t = m.target(s, &p);

    let found = m.on(&t).resolve().unwrap();
    assert_eq!((found.rect.x, found.rect.y), (100, 60));
    assert!(found.score > 0.99, "{}", found.score);
}

#[test]
fn missing_target_reports_best_score() {
    let c = Canvas::new(200, 200);
    let mut m = engine(vec![c]);

    let p = Canvas::tile_pattern(&m, 32, 9, "absent").similar(0.95);
    let s = screen(&m);
    let t = m.target(s, &p).timeout(Duration::from_millis(30));

    let err = m.on(&t).resolve().unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("absent"), "{msg}");
    assert!(msg.contains("0.95"), "{msg}");
}

/// When the primary misses, fall through to the next needle in the order
/// written (the Phase 4-4 hybrid).
///
/// In production the primary would be `ui:`, but what is being checked here is
/// **the fall-through mechanism**, so this is built from images. Not requiring
/// the accessibility API makes the result the same in every environment.
#[test]
fn fallback_needle_is_used_when_the_primary_misses() {
    let mut c = Canvas::new(320, 240);
    c.stamp(140, 90, 40, 7);
    let mut m = engine(vec![c]);

    let absent = Canvas::tile_pattern(&m, 40, 3, "absent.png").similar(0.95);
    let present = Canvas::tile_pattern(&m, 40, 7, "present.png").similar(0.95);
    let s = screen(&m);

    let t = m
        .target(s, &absent)
        .or(mekiki_core::target::Needle::Image(present));

    let found = m.on(&t).resolve().unwrap();
    assert_eq!(
        (found.rect.x, found.rect.y),
        (140, 90),
        "the alternative needle did not find it"
    );
}

/// When the primary hits, the alternative is not used. Confirms the order
/// matters.
#[test]
fn fallback_is_not_used_when_the_primary_hits() {
    let mut c = Canvas::new(320, 240);
    c.stamp(40, 40, 40, 4);
    c.stamp(200, 150, 40, 8);
    let mut m = engine(vec![c]);

    let first = Canvas::tile_pattern(&m, 40, 4, "first.png").similar(0.95);
    let second = Canvas::tile_pattern(&m, 40, 8, "second.png").similar(0.95);
    let s = screen(&m);

    let t = m
        .target(s, &first)
        .or(mekiki_core::target::Needle::Image(second));

    let found = m.on(&t).resolve().unwrap();
    assert_eq!((found.rect.x, found.rect.y), (40, 40));
}

/// When both miss, the message lists every route that was tried. Without
/// knowing what was attempted there is no way to fix it.
#[test]
fn failure_message_lists_every_alternative() {
    let c = Canvas::new(200, 200);
    let mut m = engine(vec![c]);

    let a = Canvas::tile_pattern(&m, 32, 5, "primary.png").similar(0.95);
    let b = Canvas::tile_pattern(&m, 32, 9, "backup.png").similar(0.95);
    let s = screen(&m);

    let t = m
        .target(s, &a)
        .or(mekiki_core::target::Needle::Image(b))
        .timeout(Duration::from_millis(30));

    let msg = m.on(&t).resolve().unwrap_err().to_string();
    assert!(msg.contains("primary.png"), "{msg}");
    assert!(msg.contains("backup.png"), "{msg}");
}

/// Reading order is top to bottom, left to right — not score order.
#[test]
fn reading_order_drives_nth() {
    let mut c = Canvas::new(400, 300);
    // Deliberately place them in an order that differs from reading order.
    c.stamp(250, 40, 30, 5); // reading order 1 (top row, right)
    c.stamp(60, 35, 30, 5); // reading order 0 (top row, left)
    c.stamp(150, 200, 30, 5); // reading order 2 (bottom row)
    let mut m = engine(vec![c]);

    let p = Canvas::tile_pattern(&m, 30, 5, "row").similar(0.9);
    let s = screen(&m);

    let all = m.on(&m.target(s, &p).clone()).resolve_all().unwrap();
    assert_eq!(all.len(), 3, "{all:?}");
    let xs: Vec<i32> = all.iter().map(|m| m.rect.x).collect();
    assert_eq!(xs, vec![60, 250, 150], "not in reading order");

    let second = m.on(&m.target(s, &p).nth(1).clone()).resolve().unwrap();
    assert_eq!((second.rect.x, second.rect.y), (250, 40));
}

/// Asking for score order overrides reading order.
#[test]
fn score_order_can_be_requested() {
    let mut c = Canvas::new(400, 300);
    c.stamp(60, 35, 30, 5);
    c.stamp(250, 40, 30, 5);
    let mut m = engine(vec![c]);

    let p = Canvas::tile_pattern(&m, 30, 5, "row").similar(0.9);
    let s = screen(&m);
    let t = m.target(s, &p).order(MatchOrder::Score).first();

    // Both score close to 1.0 so the order cannot be checked, but that the
    // setting takes effect can be.
    let hit = m.on(&t).resolve().unwrap();
    assert!(hit.score > 0.99);
}

/// Anchor-relative: with two identical-looking tiles, only the one to the
/// right of the anchor is chosen.
#[test]
fn anchor_disambiguates_identical_tiles() {
    let mut c = Canvas::new(500, 300);
    c.stamp(50, 100, 30, 7); // target A (left of the anchor)
    c.stamp(150, 100, 30, 3); // the anchor
    c.stamp(250, 100, 30, 7); // target B (right of the anchor)
    let mut m = engine(vec![c]);

    let field = Canvas::tile_pattern(&m, 30, 7, "field").similar(0.9);
    let label = Canvas::tile_pattern(&m, 30, 3, "label").similar(0.9);
    let s = screen(&m);

    let right = m.target(s, &field).right_of(&label, 200);
    let hit = m.on(&right).resolve().unwrap();
    assert_eq!(
        hit.rect.x, 250,
        "the one right of the anchor was not chosen"
    );

    let left = m.target(s, &field).left_of(&label, 200);
    let hit = m.on(&left).resolve().unwrap();
    assert_eq!(hit.rect.x, 50, "the one left of the anchor was not chosen");
}

/// A failure to find the anchor is distinguished from a failure to find the
/// target. Confusing them sends you to fix the wrong thing.
#[test]
fn missing_anchor_is_reported_as_such() {
    let mut c = Canvas::new(300, 200);
    c.stamp(100, 80, 30, 7);
    let mut m = engine(vec![c]);

    let field = Canvas::tile_pattern(&m, 30, 7, "field").similar(0.9);
    let absent_anchor = Canvas::tile_pattern(&m, 30, 42, "anchor").similar(0.95);
    let s = screen(&m);

    let t = m
        .target(s, &field)
        .right_of(&absent_anchor, 200)
        .timeout(Duration::from_millis(30));

    let msg = m.on(&t).resolve().unwrap_err().to_string();
    assert!(msg.contains("anchor"), "{msg}");
    assert!(msg.contains("anchor"), "{msg}");
}

/// Timing out against a static screen must not degrade the diagnosis to "no
/// candidate".
///
/// Change detection (Phase 4-2) skips the search on iterations where the screen
/// has not changed. Timing out on one of those leaves that iteration with no
/// progress recorded, and without carrying the most recent search result
/// forward the reason that was known gets lost.
///
/// This was hit for real: parallelising `center_input` made the search fast
/// enough to reach a second iteration within the time limit, and
/// `missing_anchor_is_reported_as_such` promptly failed.
#[test]
fn diagnosis_survives_a_static_screen_timeout() {
    let mut c = Canvas::new(300, 200);
    c.stamp(100, 80, 30, 7);
    // The last frame repeats, so passing a single one guarantees "unchanged"
    // from the second iteration onwards.
    let mut m = engine(vec![c]);

    let field = Canvas::tile_pattern(&m, 30, 7, "field").similar(0.9);
    let absent_anchor = Canvas::tile_pattern(&m, 30, 42, "anchor").similar(0.95);
    let s = screen(&m);

    // Allow time for many iterations.
    let t = m
        .target(s, &field)
        .right_of(&absent_anchor, 200)
        .timeout(Duration::from_millis(300));

    let msg = m.on(&t).resolve().unwrap_err().to_string();
    assert!(
        msg.contains("anchor"),
        "the reason vanished when waiting out a static screen: {msg}"
    );
}

/// Too short a distance drops every candidate, and the message says so.
#[test]
fn too_short_distance_reports_filtered_candidates() {
    let mut c = Canvas::new(500, 300);
    c.stamp(150, 100, 30, 3); // the anchor
    c.stamp(400, 100, 30, 7); // a target that is too far away
    let mut m = engine(vec![c]);

    let field = Canvas::tile_pattern(&m, 30, 7, "field").similar(0.9);
    let label = Canvas::tile_pattern(&m, 30, 3, "label").similar(0.9);
    let s = screen(&m);

    let t = m
        .target(s, &field)
        .right_of(&label, 50) // actually 220px apart
        .timeout(Duration::from_millis(30));

    let msg = m.on(&t).resolve().unwrap_err().to_string();
    assert!(msg.contains("excluded"), "{msg}");
}

/// The automatic wait does not grab while the screen is moving; it returns once
/// things have settled.
#[test]
fn unstable_screen_is_waited_out() {
    // Three frames: the target drifts into place and settles on the third.
    let mut f1 = Canvas::new(300, 200);
    f1.stamp(100, 50, 30, 7);
    let mut f2 = Canvas::new(300, 200);
    f2.stamp(104, 50, 30, 7);
    let mut f3 = Canvas::new(300, 200);
    f3.stamp(110, 50, 30, 7);

    let mut m = engine(vec![f1, f2, f3]);
    let p = Canvas::tile_pattern(&m, 30, 7, "moving").similar(0.9);
    let s = screen(&m);
    let t = m.target(s, &p).timeout(Duration::from_millis(500));

    let hit = m.on(&t).resolve().unwrap();
    // The last, settled position should come back.
    assert_eq!(hit.rect.x, 110, "grabbed it while it was still moving");
}

/// force() skips the stability check.
#[test]
fn force_skips_stability_check() {
    let mut f1 = Canvas::new(300, 200);
    f1.stamp(100, 50, 30, 7);
    let mut f2 = Canvas::new(300, 200);
    f2.stamp(104, 50, 30, 7);
    let mut f3 = Canvas::new(300, 200);
    f3.stamp(110, 50, 30, 7);

    let mut m = engine(vec![f1, f2, f3]);
    let p = Canvas::tile_pattern(&m, 30, 7, "moving").similar(0.9);
    let s = screen(&m);
    let t = m.target(s, &p).force();

    // Grabs the first frame as-is.
    let hit = m.on(&t).resolve().unwrap();
    assert_eq!(hit.rect.x, 100);
}

#[test]
fn expect_to_have_count_matches_planted_tiles() {
    let mut c = Canvas::new(400, 400);
    c.stamp(20, 20, 30, 5);
    c.stamp(200, 20, 30, 5);
    c.stamp(20, 200, 30, 5);
    let mut m = engine(vec![c]);

    let p = Canvas::tile_pattern(&m, 30, 5, "row").similar(0.9);
    let s = screen(&m);
    let t = m.target(s, &p);

    let hits = m
        .expect(&t)
        .to_have_count(3, Some(Duration::from_millis(50)))
        .unwrap();
    assert_eq!(hits.len(), 3);

    let err = m
        .expect(&t)
        .to_have_count(5, Some(Duration::from_millis(30)))
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("should have 5 matches but has 3"), "{msg}");
}

#[test]
fn expect_to_vanish_succeeds_for_absent_pattern() {
    let c = Canvas::new(200, 200);
    let mut m = engine(vec![c]);

    let p = Canvas::tile_pattern(&m, 30, 88, "never").similar(0.95);
    let s = screen(&m);
    let t = m.target(s, &p);

    m.expect(&t)
        .to_vanish(Some(Duration::from_millis(50)))
        .unwrap();
}

#[test]
fn expect_to_vanish_fails_while_present() {
    let mut c = Canvas::new(300, 200);
    c.stamp(100, 50, 30, 7);
    let mut m = engine(vec![c]);

    let p = Canvas::tile_pattern(&m, 30, 7, "stays").similar(0.9);
    let s = screen(&m);
    let t = m.target(s, &p);

    let msg = m
        .expect(&t)
        .to_vanish(Some(Duration::from_millis(30)))
        .unwrap_err()
        .to_string();
    assert!(msg.contains("did not disappear"), "{msg}");
}

/// Change detection skips searching while the screen does not move (Phase 4-2).
///
/// Waiting for a pattern that is not present on a static screen runs a full
/// search on every poll without change detection, and only once with it.
/// **Elapsed time** is used because it is the most direct way to observe from
/// the outside whether the search was skipped.
#[test]
fn change_detection_skips_searching_on_a_static_screen() {
    // A reasonably large screen and pattern, so the search is expensive.
    let canvas = Canvas::new(600, 500);

    let elapsed = |enabled: bool| -> Duration {
        let captures = Arc::new(Mutex::new(0usize));
        let capture = CountingCapture {
            width: canvas.width,
            height: canvas.height,
            bgra: canvas.bgra.clone(),
            displays: vec![DisplayInfo {
                index: 0,
                name: "fake".into(),
                bounds: Rect::new(0, 0, canvas.width, canvas.height),
                is_primary: true,
            }],
            captures: captures.clone(),
        };

        let settings = Settings {
            artifact_dir: None,
            wait_scan_interval: Duration::from_millis(1),
            auto_wait_timeout: Duration::from_millis(300),
            change_detection: if enabled {
                mekiki_core::ChangeDetection {
                    // The insurance rescan gets in the way here, so push it out.
                    force_rescan_every: 1000,
                    ..Default::default()
                }
            } else {
                mekiki_core::ChangeDetection::disabled()
            },
            ..Default::default()
        };

        let mut m = Mekiki::with_backends(
            Box::new(capture),
            Box::new(RecordingInput::default()),
            Matcher::cpu(),
            settings,
        );

        // A pattern that is not on screen, so it searches until the timeout.
        let p = Canvas::tile_pattern(&m, 48, 77, "absent").similar(0.95);
        let s = m.primary_screen().unwrap();
        let t = m.target(s, &p);

        let started = std::time::Instant::now();
        let _ = m.on(&t).resolve();
        started.elapsed()
    };

    let without = elapsed(false);
    let with = elapsed(true);

    // Both run until the 300ms timeout, so the total time is not where the
    // difference shows: it is in how many searches happen inside that timeout.
    // With change detection working, there is one search and the rest is waiting.
    //
    // A single search costs tens of milliseconds here, so with it disabled the
    // run overshoots the timeout substantially — it cannot exit until the last
    // search finishes.
    assert!(
        with <= without,
        "enabling change detection made it slower: {with:?} vs {without:?}"
    );
}

/// Change detection does not stop a search from happening once things change.
#[test]
fn change_detection_still_finds_things_when_the_screen_changes() {
    let mut f1 = Canvas::new(400, 300);
    f1.stamp(10, 10, 30, 3); // the target is absent
    let mut f2 = Canvas::new(400, 300);
    f2.stamp(10, 10, 30, 3);
    f2.stamp(200, 150, 40, 9); // appears part-way through

    let mut m = engine(vec![f1, f2]);
    let p = Canvas::tile_pattern(&m, 40, 9, "appears").similar(0.9);
    let s = screen(&m);
    let t = m.target(s, &p).timeout(Duration::from_millis(500));

    let found = m.on(&t).resolve().unwrap();
    assert_eq!((found.rect.x, found.rect.y), (200, 150));
}

/// Change detection can be turned off.
#[test]
fn change_detection_can_be_disabled() {
    assert!(!mekiki_core::ChangeDetection::disabled().enabled);
    assert!(
        Settings::default().change_detection.enabled,
        "it is enabled by default"
    );
}

#[test]
fn click_targets_match_center_with_offset() {
    let mut c = Canvas::new(300, 200);
    c.stamp(100, 50, 40, 7);
    let mut m = engine(vec![c]);

    let p = Canvas::tile_pattern(&m, 40, 7, "btn").similar(0.9);
    let s = screen(&m);
    let t = m.target(s, &p).offset(5, -5);

    let hit = m.on(&t).click().unwrap();
    // Centre (120, 70) plus the offset (5, -5).
    assert_eq!(hit.target(), (125, 65));
    assert_eq!(m.mouse_position().unwrap(), (125, 65));
}

/// A template with no texture is rejected at load time.
///
/// The ZMD denominator can only reach 0 for a uniform template. The matcher's
/// own defence merely returns an all-zero map, which never shows the user why
/// nothing was found, so this is brought forward to an explicit load-time error.
#[test]
fn flat_pattern_file_is_rejected_at_load() {
    let m = engine(vec![Canvas::new(64, 64)]);
    let dir = std::env::temp_dir().join("mekiki_flat_pattern_test");
    std::fs::create_dir_all(&dir).unwrap();

    // Completely blank.
    let flat = dir.join("flat.png");
    image::GrayImage::from_pixel(32, 32, image::Luma([200u8]))
        .save(&flat)
        .unwrap();
    let err = m.pattern_from_file(&flat).unwrap_err();
    assert!(
        matches!(err, mekiki_core::Error::FlatPattern(_)),
        "should be FlatPattern: {err}"
    );

    // Even a one-quantization-step checkerboard only reaches a per-pixel std of
    // 0.5/255, which is still not enough.
    let faint = dir.join("faint.png");
    let img = image::GrayImage::from_fn(32, 32, |x, y| image::Luma([200 + ((x + y) % 2) as u8]));
    img.save(&faint).unwrap();
    assert!(
        m.pattern_from_file(&faint).is_err(),
        "texture at the level of quantization noise should be rejected"
    );

    // A clear pattern passes.
    let ok = dir.join("ok.png");
    let img =
        image::GrayImage::from_fn(32, 32, |x, y| image::Luma([((x * 7 + y * 13) % 200) as u8]));
    img.save(&ok).unwrap();
    assert!(m.pattern_from_file(&ok).is_ok());
}

#[test]
fn hover_activates_but_move_to_does_not() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let input = RecordingInput {
        log: log.clone(),
        position: (0, 0),
    };
    let mut settings = Settings::default();
    settings.artifact_dir = None;
    settings.move_speed = 0.0;
    settings.move_settle = Duration::ZERO;
    let mut m = Mekiki::with_backends(
        Box::new(ScriptedCapture::new(64, 64, vec![vec![0; 64 * 64 * 4]])),
        Box::new(input),
        Matcher::cpu(),
        settings,
    );

    m.move_to((10, 10)).unwrap();
    m.hover((20, 20)).unwrap();

    let entries = log.lock().unwrap().clone();
    assert!(
        !entries.iter().any(|e| e == "activate(10,10)"),
        "{entries:?}"
    );
    assert!(
        entries.contains(&"activate(20,20)".to_string()),
        "{entries:?}"
    );
}

#[test]
fn drag_drop_holds_button_across_move_to() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let input = RecordingInput {
        log: log.clone(),
        position: (0, 0),
    };
    let mut settings = Settings::default();
    settings.artifact_dir = None;
    settings.move_speed = 0.0;
    settings.move_settle = Duration::ZERO;
    settings.click_hold = Duration::ZERO;
    let mut m = Mekiki::with_backends(
        Box::new(ScriptedCapture::new(64, 64, vec![vec![0; 64 * 64 * 4]])),
        Box::new(input),
        Matcher::cpu(),
        settings,
    );

    m.drag_drop((10, 10), (40, 30)).unwrap();
    let entries = log.lock().unwrap().clone();
    let down = entries.iter().position(|e| e == "down(Left)").unwrap();
    let dest = entries.iter().position(|e| e == "move(40,30)").unwrap();
    let up = entries.iter().position(|e| e == "up(Left)").unwrap();
    assert!(down < dest && dest < up, "{entries:?}");
}

// ---------------------------------------------------------------------------
// Typing
//
// `type_text` splits a string across several backend calls because some
// applications drop characters from one long burst — measured on Windows 11
// Notepad, where everything after the first space collapsed into the final
// character. The backend is not at fault, so the split lives here.
// ---------------------------------------------------------------------------

/// Build an engine whose input backend just records what it was asked to do.
fn typing_engine(settings: Settings) -> (Mekiki, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let input = RecordingInput {
        log: log.clone(),
        position: (0, 0),
    };
    let mut settings = settings;
    settings.artifact_dir = None;
    // The default interval is a real 20ms per character; these tests only check
    // what reaches the backend, so drop it to keep them fast.
    settings.type_interval = Duration::ZERO;
    let m = Mekiki::with_backends(
        Box::new(ScriptedCapture::new(64, 64, vec![vec![0; 64 * 64 * 4]])),
        Box::new(input),
        Matcher::cpu(),
        settings,
    );
    (m, log)
}

/// Everything typed has to arrive, in order, whatever the chunk size.
///
/// The split is an implementation detail; losing or reordering a character
/// because of it would be far worse than the problem it solves.
#[test]
fn chunked_typing_delivers_the_whole_string_in_order() {
    for chunk in [0, 1, 3, 8, 1000] {
        let mut settings = Settings::default();
        settings.type_chunk = chunk;
        let (mut m, log) = typing_engine(settings);

        m.type_text("abcdefg hijklmn").unwrap();

        let typed: String = log
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| e.strip_prefix("type(").and_then(|e| e.strip_suffix(")")))
            .collect();
        assert_eq!(typed, "abcdefg hijklmn", "chunk={chunk}");
    }
}

/// The chunk size has to actually reach the backend.
///
/// Without this the setting could quietly do nothing and the Notepad case would
/// come back with no test failing.
#[test]
fn the_chunk_size_decides_how_many_calls_the_backend_sees() {
    let mut settings = Settings::default();
    settings.type_chunk = 4;
    let (mut m, log) = typing_engine(settings);

    m.type_text("0123456789").unwrap();

    let calls: Vec<String> = log.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec!["type(0123)", "type(4567)", "type(89)"],
        "10 characters at 4 per call should be 3 calls"
    );
}

/// 0 means "send it all at once", which is what the backend used to do.
#[test]
fn a_chunk_of_zero_sends_one_call() {
    let mut settings = Settings::default();
    settings.type_chunk = 0;
    let (mut m, log) = typing_engine(settings);

    m.type_text("0123456789").unwrap();
    assert_eq!(log.lock().unwrap().clone(), vec!["type(0123456789)"]);
}

/// A chunk boundary must never fall inside a surrogate pair.
///
/// Chunking by UTF-16 code unit rather than by character would split an emoji
/// into two halves, and each half is meaningless on its own.
#[test]
fn chunking_never_splits_a_surrogate_pair() {
    let mut settings = Settings::default();
    settings.type_chunk = 1;
    let (mut m, log) = typing_engine(settings);

    // Each of these is one char but two UTF-16 code units.
    m.type_text("😀😁😂").unwrap();

    let calls: Vec<String> = log.lock().unwrap().clone();
    assert_eq!(calls, vec!["type(😀)", "type(😁)", "type(😂)"]);
    for call in &calls {
        let inner = call
            .strip_prefix("type(")
            .unwrap()
            .strip_suffix(")")
            .unwrap();
        assert_eq!(inner.chars().count(), 1, "{call} is not a whole character");
    }
}

/// A stop must not leave half a string typed and then carry on.
#[test]
fn stopping_halts_typing_partway() {
    let mut settings = Settings::default();
    settings.type_chunk = 1;
    let (mut m, log) = typing_engine(settings);

    let interrupt = mekiki_core::Interrupt::new();
    m.set_interrupt(interrupt.clone());
    interrupt.stop();

    let result = m.type_text("abcdef");
    assert!(
        matches!(result, Err(mekiki_core::Error::Interrupted)),
        "{result:?}"
    );
    assert!(
        log.lock().unwrap().is_empty(),
        "nothing should have been typed after a stop"
    );
}

#[test]
fn typing_an_empty_string_does_nothing() {
    let (mut m, log) = typing_engine(Settings::default());
    m.type_text("").unwrap();
    assert!(log.lock().unwrap().is_empty());
}

/// A line break is a keystroke, not a character.
///
/// Injected as a Unicode code unit, `\n` does nothing in most editors — round 3
/// watched "A\nB" arrive as "AB" with no error. Every newline spelling becomes
/// one Enter press, and CRLF is one, not two.
#[test]
fn newlines_are_typed_as_enter_presses() {
    let mut settings = Settings::default();
    settings.type_chunk = 0; // one call per segment keeps the log readable
    let (mut m, log) = typing_engine(settings);

    m.type_text("ab\ncd\r\nef\rgh").unwrap();

    let calls: Vec<String> = log.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            "type(ab)",
            "key_down(Enter)",
            "key_up(Enter)",
            "type(cd)",
            "key_down(Enter)",
            "key_up(Enter)",
            "type(ef)",
            "key_down(Enter)",
            "key_up(Enter)",
            "type(gh)",
        ],
        "\\n, \\r\\n and \\r must each be exactly one Enter"
    );
}

/// A tab is a keystroke too: fields treat it as focus movement, editors as
/// indentation, and neither happens for an injected U+0009.
#[test]
fn tabs_are_typed_as_tab_presses() {
    let mut settings = Settings::default();
    settings.type_chunk = 0;
    let (mut m, log) = typing_engine(settings);

    m.type_text("a\tb").unwrap();

    let calls: Vec<String> = log.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec!["type(a)", "key_down(Tab)", "key_up(Tab)", "type(b)"]
    );
}

/// Text that is nothing but line breaks still presses Enter and types nothing.
#[test]
fn a_lone_newline_presses_enter_without_typing() {
    let mut settings = Settings::default();
    settings.type_chunk = 0;
    let (mut m, log) = typing_engine(settings);

    m.type_text("\n").unwrap();

    let calls: Vec<String> = log.lock().unwrap().clone();
    assert_eq!(calls, vec!["key_down(Enter)", "key_up(Enter)"]);
}

/// The defaults are the correctness-first pair, not the fast one.
///
/// Round 2 found the old default (chunk 8, interval 0) still corrupted Notepad.
/// If these drift back, a fresh script typing into an unknown application loses
/// characters again with no test failing.
#[test]
fn the_default_typing_settings_are_per_character_with_a_pause() {
    let s = Settings::default();
    assert_eq!(s.type_chunk, 1, "each character must be its own call");
    assert!(
        s.type_interval >= Duration::from_millis(15),
        "the pause must clear the Windows timer resolution, got {:?}",
        s.type_interval
    );
}

// ---------------------------------------------------------------------------
// The accessibility path without a capture
//
// `ui:` reads the UI tree, never pixels. It used to take a captured frame all
// the same, purely to learn the search area, which meant a broken capture took
// it down too — exactly when it would have been the way out. The first agent
// test round hit that: capture failed on every call and `ui:` went with it,
// leaving the server able to type but blind.
// ---------------------------------------------------------------------------

/// A capture backend that always fails, standing in for a display that cannot
/// be duplicated.
struct DeadCapture {
    displays: Vec<DisplayInfo>,
}

impl DeadCapture {
    fn new() -> Self {
        Self {
            displays: vec![DisplayInfo {
                index: 0,
                name: "dead".into(),
                bounds: Rect::new(0, 0, 800, 600),
                is_primary: true,
            }],
        }
    }
}

impl ScreenCapture for DeadCapture {
    fn displays(&self) -> &[DisplayInfo] {
        &self.displays
    }

    fn capture(&mut self, _display: usize) -> Result<Frame, CaptureError> {
        Err(CaptureError::SessionLost(
            "the capture session was lost (test)".into(),
        ))
    }

    fn backend_name(&self) -> &'static str {
        "fake/dead"
    }
}

/// An accessibility backend returning a fixed set of elements.
struct FakeUia {
    elements: Vec<mekiki_core::UiElement>,
}

impl mekiki_uia::ElementFinder for FakeUia {
    fn find(
        &mut self,
        query: &mekiki_core::UiQuery,
    ) -> Result<Vec<mekiki_core::UiElement>, mekiki_uia::UiaError> {
        // Mirror the real backend closely enough to be worth trusting: filter on
        // what it filters on, and leave name matching to the caller.
        Ok(self
            .elements
            .iter()
            .filter(|e| {
                query
                    .automation_id
                    .as_ref()
                    .is_none_or(|id| *id == e.automation_id)
                    && query.control_type.is_none_or(|t| t == e.control_type)
            })
            .cloned()
            .collect())
    }

    fn backend_name(&self) -> &'static str {
        "fake/uia"
    }
}

fn ui_element(
    name: &str,
    control_type: mekiki_core::ControlType,
    x: i32,
    y: i32,
) -> mekiki_core::UiElement {
    mekiki_core::UiElement {
        name: name.into(),
        automation_id: String::new(),
        class_name: String::new(),
        control_type,
        bounds: mekiki_core::UiBounds::new(x, y, 80, 24),
        enabled: true,
        offscreen: false,
        runtime_id: None,
        value: None,
        value_source: None,
        is_password: false,
    }
}

/// Build an engine whose capture is dead but whose accessibility API works.
fn blind_engine(elements: Vec<mekiki_core::UiElement>) -> Mekiki {
    let mut settings = Settings::default();
    settings.artifact_dir = None;
    settings.wait_scan_interval = Duration::from_millis(5);
    settings.auto_wait_timeout = Duration::from_millis(100);

    let mut m = Mekiki::with_backends(
        Box::new(DeadCapture::new()),
        Box::new(RecordingInput::default()),
        Matcher::cpu(),
        settings,
    );
    m.set_element_finder(Box::new(FakeUia { elements }));
    m
}

/// **The point of the change.** A `ui:` target resolves with no capture at all.
#[test]
fn a_ui_target_resolves_when_capture_is_dead() {
    use mekiki_core::ControlType;

    let mut m = blind_engine(vec![
        ui_element("Save", ControlType::Button, 100, 200),
        ui_element("Cancel", ControlType::Button, 200, 200),
    ]);
    let screen = m.primary_screen().unwrap();
    let pattern = m.ui_pattern().with_name("Save");
    let target = m.ui_target(screen, pattern);

    let found = m
        .on(&target)
        .resolve()
        .expect("a ui: target must not need a capture");
    assert_eq!((found.rect.x, found.rect.y), (100, 200));
}

/// The visible label matches an accessible name that carries a mnemonic.
///
/// On Japanese Windows the Save button is named 保存(S). Before the fix,
/// ui:name=保存 scored 2/4 = 0.5 against it and fell under the threshold, so
/// writing the label you see always missed.
#[test]
fn a_ui_name_matches_through_the_mnemonic() {
    use mekiki_core::ControlType;

    let mut m = blind_engine(vec![ui_element("保存(S)", ControlType::Button, 100, 200)]);
    let screen = m.primary_screen().unwrap();

    // The natural spelling, without the accelerator.
    let target = m.ui_target(screen, m.ui_pattern().with_name("保存"));
    let found = m
        .on(&target)
        .resolve()
        .expect("the visible label must match through the mnemonic");
    assert_eq!((found.rect.x, found.rect.y), (100, 200));

    // The full accessible name still matches too.
    let full = m.ui_target(screen, m.ui_pattern().with_name("保存(S)"));
    assert!(m.on(&full).resolve().is_ok());
}

/// list_ui enumerates without a capture, and reports usable type names.
#[test]
fn list_ui_enumerates_with_no_capture() {
    use mekiki_core::ControlType;

    let mut m = blind_engine(vec![
        ui_element("保存(S)", ControlType::Button, 100, 200),
        ui_element("Name", ControlType::Edit, 10, 100),
    ]);
    let screen = m.primary_screen().unwrap();

    let all = m
        .list_ui(screen, None)
        .expect("list_ui must not need a capture");
    assert_eq!(all.len(), 2);

    let buttons = m.list_ui(screen, Some("button")).unwrap();
    assert_eq!(buttons.len(), 1);
    assert_eq!(buttons[0].name, "保存(S)");
    // The type is spelled the way ui:type= expects, so it can be copied straight in.
    assert_eq!(buttons[0].control_type, "button");

    // A bad filter is an error, not a silent empty list.
    assert!(m.list_ui(screen, Some("nonsense")).is_err());
}

#[test]
fn read_ui_value_requires_one_match_and_redacts_passwords() {
    use mekiki_core::ControlType;

    let mut name = ui_element("Name", ControlType::Edit, 10, 100);
    name.automation_id = "name".into();
    name.value = Some("Alice".into());
    name.value_source = Some("value");
    let mut password = ui_element("Password", ControlType::Edit, 10, 140);
    password.automation_id = "password".into();
    password.value = Some("must-not-escape".into());
    password.value_source = Some("value");
    password.is_password = true;
    let mut m = blind_engine(vec![name, password]);
    let screen = m.primary_screen().unwrap();

    let value = m
        .read_ui_value(screen, &m.ui_pattern().with_automation_id("name"))
        .unwrap();
    assert_eq!(value.value.as_deref(), Some("Alice"));
    assert_eq!(value.source, Some("value"));

    let secret = m
        .read_ui_value(screen, &m.ui_pattern().with_automation_id("password"))
        .unwrap();
    assert!(secret.is_password);
    assert_eq!(secret.value, None);
    assert_eq!(secret.source, None);

    assert!(
        m.read_ui_value(screen, &m.ui_pattern().with_control_type(ControlType::Edit))
            .is_err(),
        "an ambiguous locator must not choose a field"
    );
}

/// A target that does read pixels still reports the capture failure.
///
/// The point is to free `ui:` from the capture, not to hide a dead capture from
/// everything else.
#[test]
fn an_image_target_still_fails_when_capture_is_dead() {
    let mut m = blind_engine(vec![]);
    let screen = m.primary_screen().unwrap();

    let luma: Vec<u8> = (0..32u32 * 32).map(|i| (i % 251) as u8).collect();
    let pattern = m.pattern_from_luma8(&luma, 32, 32, "probe");
    let target = m.target(screen, &pattern);

    let err = m.on(&target).resolve().unwrap_err();
    assert!(
        matches!(err, mekiki_core::Error::Capture(_)),
        "an image target must surface the capture failure: {err}"
    );
}

/// One image fallback makes the whole target need pixels again.
///
/// `is_ui_only` demands *every* needle be `ui:` on purpose: taking the
/// capture-free path for a target that might fall back to an image would skip
/// the fallback silently.
#[test]
fn a_ui_target_with_an_image_fallback_is_not_capture_free() {
    use mekiki_core::{ControlType, Needle};

    let mut m = blind_engine(vec![ui_element("Save", ControlType::Button, 10, 20)]);
    let screen = m.primary_screen().unwrap();

    let luma: Vec<u8> = (0..32u32 * 32).map(|i| (i % 251) as u8).collect();
    let fallback = m.pattern_from_luma8(&luma, 32, 32, "fallback");
    let pattern = m.ui_pattern().with_name("Save");
    let target = m.ui_target(screen, pattern).or(Needle::Image(fallback));

    assert!(!target.is_ui_only());
    let err = m.on(&target).resolve().unwrap_err();
    assert!(
        matches!(err, mekiki_core::Error::Capture(_)),
        "a target with an image fallback must still capture: {err}"
    );
}

/// A `ui:` anchor keeps the capture-free path; anything else gives it up.
#[test]
fn only_a_ui_anchor_keeps_the_target_capture_free() {
    use mekiki_core::{ControlType, Needle, TextPattern};

    let mut m = blind_engine(vec![
        ui_element("Name", ControlType::Text, 10, 100),
        ui_element("Save", ControlType::Button, 200, 100),
    ]);
    let screen = m.primary_screen().unwrap();
    let pattern = m.ui_pattern().with_name("Save");
    let anchor = Needle::Ui(m.ui_pattern().with_name("Name"));

    let with_ui_anchor = m.ui_target(screen, pattern.clone()).related_to(
        mekiki_core::Direction::RightOf,
        anchor,
        400,
    );
    assert!(with_ui_anchor.is_ui_only());
    let resolved = m.on(&with_ui_anchor).resolve();
    assert!(
        resolved.is_ok(),
        "a ui: anchor must not need a capture: {resolved:?}"
    );

    let with_text_anchor = m.ui_target(screen, pattern).related_to(
        mekiki_core::Direction::RightOf,
        Needle::Text(TextPattern::new("Name", 0.8)),
        400,
    );
    assert!(
        !with_text_anchor.is_ui_only(),
        "an ocr: anchor needs pixels, so the target is not capture free"
    );
}

#[test]
fn move_to_stop_does_not_snap_to_target() {
    let input = RecordingInput::default();
    let mut settings = Settings::default();
    settings.artifact_dir = None;
    settings.move_speed = 400.0;
    settings.move_max_duration = Duration::from_secs(5);
    settings.move_settle = Duration::ZERO;
    let mut m = Mekiki::with_backends(
        Box::new(ScriptedCapture::new(64, 64, vec![vec![0; 64 * 64 * 4]])),
        Box::new(input),
        Matcher::cpu(),
        settings,
    );
    let interrupt = mekiki_core::Interrupt::new();
    m.set_interrupt(interrupt.clone());

    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        interrupt.stop();
    });

    let err = m.move_to((800, 0)).unwrap_err();
    assert!(matches!(err, mekiki_core::Error::Interrupted), "{err}");
    let (x, _) = m.mouse_position().unwrap();
    assert!(x < 800, "must not jump to the target: x={x}");
    assert!(x > 0, "must have moved at least a little: x={x}");
    stopper.join().unwrap();
}

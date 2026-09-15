//! Integration tests for the scripting layer.
//!
//! Capture and input are replaced with fakes, and verification happens by
//! **actually running Rhai scripts** against a synthetic screen — end to end
//! from locator parsing to an action firing.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mekiki_capture::{CaptureError, DisplayInfo, Frame, Rect, ScreenCapture};
use mekiki_core::search::Matcher;
use mekiki_core::{Mekiki, Settings};
use mekiki_input::{InputError, InputInjector, Key, MouseButton};
use mekiki_scripting::assets::AssetStore;
use mekiki_scripting::{ScriptHost, api::Runtime};

// ---------------------------------------------------------------------------
// Fake backends
// ---------------------------------------------------------------------------

struct FakeCapture {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
    displays: Vec<DisplayInfo>,
}

impl ScreenCapture for FakeCapture {
    fn displays(&self) -> &[DisplayInfo] {
        &self.displays
    }
    fn capture(&mut self, _d: usize) -> Result<Frame, CaptureError> {
        Ok(Frame {
            width: self.width,
            height: self.height,
            origin: (0, 0),
            bgra: self.bgra.clone(),
        })
    }
    fn backend_name(&self) -> &'static str {
        "fake"
    }
}

#[derive(Clone, Default)]
struct InputLog(Arc<Mutex<Vec<String>>>);

impl InputLog {
    fn entries(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
    fn push(&self, s: String) {
        self.0.lock().unwrap().push(s);
    }
}

struct FakeInput {
    log: InputLog,
    position: (i32, i32),
}

impl InputInjector for FakeInput {
    fn mouse_move(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        self.position = (x, y);
        self.log.push(format!("move({x},{y})"));
        Ok(())
    }
    fn activate_at(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        self.log.push(format!("activate({x},{y})"));
        Ok(())
    }
    fn mouse_down(&mut self, b: MouseButton) -> Result<(), InputError> {
        self.log.push(format!("down({b:?})"));
        Ok(())
    }
    fn mouse_up(&mut self, b: MouseButton) -> Result<(), InputError> {
        self.log.push(format!("up({b:?})"));
        Ok(())
    }
    fn scroll(&mut self, h: i32, v: i32) -> Result<(), InputError> {
        self.log.push(format!("scroll({h},{v})"));
        Ok(())
    }
    fn key_down(&mut self, k: Key) -> Result<(), InputError> {
        self.log.push(format!("keydown({k:?})"));
        Ok(())
    }
    fn key_up(&mut self, k: Key) -> Result<(), InputError> {
        self.log.push(format!("keyup({k:?})"));
        Ok(())
    }
    fn type_text(&mut self, t: &str) -> Result<(), InputError> {
        self.log.push(format!("type({t})"));
        Ok(())
    }
    fn cursor_position(&self) -> Result<(i32, i32), InputError> {
        Ok(self.position)
    }
    fn backend_name(&self) -> &'static str {
        "fake"
    }
}

// ---------------------------------------------------------------------------
// Synthetic screens and assets
// ---------------------------------------------------------------------------

struct Canvas {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
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

    fn stamp(&mut self, x: u32, y: u32, size: u32, seed: u32) {
        for dy in 0..size {
            for dx in 0..size {
                let (px, py) = (x + dx, y + dy);
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

    fn tile_png(size: u32, seed: u32) -> Vec<u8> {
        let mut c = Canvas::new(size, size);
        c.stamp(0, 0, size, seed);
        let rgba: Vec<u8> = c
            .bgra
            .chunks_exact(4)
            .flat_map(|p| [p[2], p[1], p[0], 255])
            .collect();
        let img = image::RgbaImage::from_raw(size, size, rgba).unwrap();
        let mut bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }
}

struct Fixture {
    host: ScriptHost,
    log: InputLog,
    #[allow(dead_code)]
    dir: std::path::PathBuf,
}

fn fixture(canvas: Canvas, tiles: &[(&str, u32, u32)]) -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "mekiki-script-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    for (name, size, seed) in tiles {
        std::fs::write(dir.join(name), Canvas::tile_png(*size, *seed)).unwrap();
    }

    let log = InputLog::default();
    let capture = FakeCapture {
        width: canvas.width,
        height: canvas.height,
        bgra: canvas.bgra,
        displays: vec![DisplayInfo {
            index: 0,
            name: "fake".into(),
            bounds: Rect::new(0, 0, canvas.width, canvas.height),
            is_primary: true,
        }],
    };
    let input = FakeInput {
        log: log.clone(),
        position: (0, 0),
    };

    // Tighten the waits to keep the tests fast, and write no artifacts.
    let settings = Settings {
        artifact_dir: None,
        wait_scan_interval: Duration::from_millis(5),
        move_settle: Duration::from_millis(0),
        move_speed: 0.0,
        click_hold: Duration::from_millis(0),
        double_click_interval: Duration::from_millis(0),
        auto_wait_timeout: Duration::from_millis(200),
        actionability: mekiki_core::Actionability {
            stable_interval: Duration::from_millis(1),
            ..Default::default()
        },
        ..Default::default()
    };

    let mekiki =
        Mekiki::with_backends(Box::new(capture), Box::new(input), Matcher::cpu(), settings);
    let host = ScriptHost::from_runtime(Runtime::new(mekiki, AssetStore::new(&dir)));

    Fixture { host, log, dir }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn registered_rhai_api_exposes_reflection_metadata() {
    let fixture = fixture(Canvas::new(8, 8), &[]);
    let actual: std::collections::BTreeSet<_> = fixture
        .host
        .engine()
        .gen_fn_signatures(false)
        .iter()
        .map(|signature| reflected_shape(signature))
        .collect();
    let catalog_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("ide/src/mekiki-api.generated.json");
    let catalog: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(catalog_path).expect("generated API catalog is readable"),
    )
    .expect("generated API catalog is valid JSON");

    let expected: std::collections::BTreeSet<_> = catalog["items"]
        .as_array()
        .expect("catalog items")
        .iter()
        .map(catalog_shape)
        .collect();
    assert_eq!(
        actual, expected,
        "Rhai registration/catalog signature drift"
    );
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ApiShape {
    name: String,
    owner: String,
    params: Vec<String>,
    returns: String,
}

fn reflected_shape(signature: &str) -> ApiShape {
    let (call, raw_return) = signature
        .split_once(" -> ")
        .map_or((signature, "unit"), |parts| parts);
    let (raw_name, raw_params) = call.split_once('(').expect("reflected function call");
    let mut params: Vec<String> = raw_params
        .trim_end_matches(')')
        .split(", ")
        .filter(|value| !value.is_empty())
        .map(|value| public_type(value.split_once(": ").map_or(value, |(_, ty)| ty)))
        .collect();
    let mut owner = String::new();
    if params
        .first()
        .is_some_and(|value| value.starts_with("&mut "))
    {
        owner = params.remove(0).trim_start_matches("&mut ").to_string();
    }
    ApiShape {
        name: raw_name.trim_start_matches("get$").to_string(),
        owner,
        params,
        returns: public_type(raw_return),
    }
}

fn catalog_shape(item: &serde_json::Value) -> ApiShape {
    let mut owner = item["owner"].as_str().unwrap_or_default().to_string();
    let mut params: Vec<String> = item["params"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|param| param["type"].as_str().unwrap().to_string())
        .collect();
    // One Rhai registration supports both expect(target) and target.expect().
    if item["id"] == "global.expect.target" {
        owner = params.remove(0);
    }
    ApiShape {
        name: item["name"].as_str().unwrap().to_string(),
        owner,
        params,
        returns: item["returns"].as_str().unwrap().to_string(),
    }
}

fn public_type(raw: &str) -> String {
    let raw = raw
        .strip_prefix("core::result::Result<")
        .and_then(|value| value.split(',').next())
        .unwrap_or(raw);
    if let Some(inner) = raw.strip_prefix("&mut ") {
        return format!("&mut {}", public_type(inner));
    }
    for (needle, public) in [
        ("ScriptWindowExpect", "WindowExpect"),
        ("ScriptRegion", "Region"),
        ("ScriptTarget", "Target"),
        ("ScriptMatch", "Match"),
        ("ScriptExpect", "Expect"),
        ("i64", "int"),
        ("f64", "float"),
        ("bool", "bool"),
        ("string", "string"),
        ("Vec", "array<string>"),
        ("Dynamic", "array<Match>"),
        ("()", "unit"),
    ] {
        if raw.contains(needle) {
            return public.to_string();
        }
    }
    raw.to_string()
}

/// A long `sleep` can be stopped part-way.
///
/// `on_progress` only fires between instructions, so sleeping in one go would
/// make it unstoppable. This confirms the implementation waits in slices.
#[test]
fn stop_interrupts_a_long_sleep() {
    let mut f = fixture(Canvas::new(80, 60), &[]);
    let interrupt = mekiki_core::Interrupt::new();
    f.host.set_interrupt(interrupt.clone());

    let stopper = interrupt.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        stopper.stop();
    });

    let started = Instant::now();
    let err = f.host.run("sleep(10000);").unwrap_err();
    let elapsed = started.elapsed();

    assert!(
        ScriptHost::is_interrupted(&err),
        "it was not treated as a stop: {err}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "it took too long to stop: {elapsed:?}"
    );
}

/// Stopping releases any held input.
///
/// **Without this, depending on the state at the moment of the stop, a mouse
/// button or modifier key would be left held down on the user's desktop.**
#[test]
fn stopping_releases_held_input() {
    let mut f = fixture(Canvas::new(80, 60), &[]);
    let interrupt = mekiki_core::Interrupt::new();
    f.host.set_interrupt(interrupt.clone());

    let stopper = interrupt.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        stopper.stop();
    });

    let _ = f.host.run("sleep(10000);");

    let log = f.log.entries().join("\n");
    assert!(
        log.contains("up(Left)"),
        "no mouse button release appears:\n{log}"
    );
}

/// Pausing halts progress, and resuming continues.
#[test]
fn pause_holds_execution_until_resumed() {
    let mut f = fixture(Canvas::new(80, 60), &[]);
    let interrupt = mekiki_core::Interrupt::new();
    f.host.set_interrupt(interrupt.clone());

    interrupt.pause();

    let resumer = interrupt.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        resumer.resume();
    });

    let started = Instant::now();
    f.host.run("sleep(1);").unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(90),
        "the pause did not halt it: {elapsed:?}"
    );
}

/// Paused time does not count against the automatic wait's timeout.
///
/// If it did, **a search would time out purely because it was paused for
/// debugging**.
#[test]
fn paused_time_is_not_counted_towards_the_timeout() {
    let mut c = Canvas::new(160, 120);
    c.stamp(40, 30, 24, 3);
    let mut f = fixture(c, &[("tile.png", 24, 3)]);

    let interrupt = mekiki_core::Interrupt::new();
    f.host.set_interrupt(interrupt.clone());

    // The fixture sets the automatic wait to 200ms. Pause for longer than that.
    interrupt.pause();
    let resumer = interrupt.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(320));
        resumer.resume();
    });

    // If paused time were counted, this would come back as "not found".
    f.host
        .run(r#"target("tile.png").similar(0.9).click();"#)
        .expect("paused time is being counted against the timeout");
}

#[test]
fn click_via_image_locator() {
    let mut c = Canvas::new(320, 240);
    c.stamp(100, 60, 40, 1);
    let f = fixture(c, &[("ok.png", 40, 1)]);

    f.host
        .run(r#"target("image:ok.png").similar(0.95).click();"#)
        .unwrap();

    // It should click the centre, (120, 80).
    let log = f.log.entries();
    assert!(log.contains(&"move(120,80)".to_string()), "{log:?}");
    assert!(log.contains(&"down(Left)".to_string()), "{log:?}");
    assert!(log.contains(&"up(Left)".to_string()), "{log:?}");
}

#[test]
fn right_click_via_image_locator() {
    let mut c = Canvas::new(320, 240);
    c.stamp(100, 60, 40, 1);
    let f = fixture(c, &[("ok.png", 40, 1)]);

    f.host
        .run(r#"target("image:ok.png").similar(0.95).right_click();"#)
        .unwrap();

    let log = f.log.entries();
    assert!(log.contains(&"move(120,80)".to_string()), "{log:?}");
    assert!(log.contains(&"down(Right)".to_string()), "{log:?}");
    assert!(log.contains(&"up(Right)".to_string()), "{log:?}");
}

#[test]
fn match_right_click_uses_resolved_point() {
    let mut c = Canvas::new(320, 240);
    c.stamp(100, 60, 40, 1);
    let f = fixture(c, &[("ok.png", 40, 1)]);

    f.host
        .run(
            r#"
            let m = target("image:ok.png").similar(0.95).resolve();
            m.right_click();
        "#,
        )
        .unwrap();

    let log = f.log.entries();
    assert!(log.contains(&"down(Right)".to_string()), "{log:?}");
    assert!(log.contains(&"up(Right)".to_string()), "{log:?}");
}

#[test]
fn recheck_off_reuses_coordinates_after_hover() {
    let mut c = Canvas::new(320, 240);
    c.stamp(100, 60, 40, 1);
    let f = fixture(c, &[("ok.png", 40, 1)]);

    f.host
        .run(
            r#"
            set_recheck(false);
            let t = target("image:ok.png").similar(0.95);
            t.hover();
            t.right_click();
        "#,
        )
        .unwrap();

    let log = f.log.entries();
    assert!(log.contains(&"move(120,80)".to_string()), "{log:?}");
    assert!(log.contains(&"down(Right)".to_string()), "{log:?}");
}

/// Omitting the prefix means image:.
#[test]
fn bare_locator_defaults_to_image() {
    let mut c = Canvas::new(320, 240);
    c.stamp(100, 60, 40, 1);
    let f = fixture(c, &[("ok.png", 40, 1)]);

    let x = f
        .host
        .eval::<i64>(r#"target("ok.png").similar(0.95).resolve().x"#)
        .unwrap();
    assert_eq!(x, 100);
}

#[test]
fn global_right_click_uses_current_mouse_position() {
    let f = fixture(Canvas::new(200, 200), &[]);
    f.host
        .run(
            r#"
            target("point:40,50").hover();
            right_click();
        "#,
        )
        .unwrap();
    let log = f.log.entries();
    assert!(log.contains(&"down(Right)".to_string()), "{log:?}");
    assert!(log.contains(&"up(Right)".to_string()), "{log:?}");
}

#[test]
fn point_locator_clicks_absolute_coordinates() {
    let f = fixture(Canvas::new(200, 200), &[]);
    f.host.run(r#"target("point:42,84").click();"#).unwrap();
    assert!(f.log.entries().contains(&"move(42,84)".to_string()));
}

#[test]
fn activate_rejects_plain_region() {
    let f = fixture(Canvas::new(200, 200), &[]);
    let err = f
        .host
        .run(r#"region(0, 0, 10, 10).activate();"#)
        .unwrap_err();
    assert!(err.to_string().contains("window"), "{err}");
}

#[test]
fn window_wait_api_handles_an_absent_window_without_sleep_polling() {
    let f = fixture(Canvas::new(200, 200), &[]);
    let missing = "__mekiki_window_that_must_not_exist_5f4c__";
    let found = f
        .host
        .eval::<bool>(&format!(r#"window_exists("{missing}")"#))
        .unwrap();
    assert!(!found);
    f.host
        .run(&format!(r#"expect_window("{missing}").to_vanish(20);"#))
        .unwrap();
    let error = f
        .host
        .run(&format!(r#"expect_window("{missing}").to_appear(20);"#))
        .unwrap_err();
    assert!(error.to_string().contains("did not appear"), "{error}");
}

#[test]
fn window_only_region_input_rejects_a_plain_region() {
    let f = fixture(Canvas::new(200, 200), &[]);
    let press = f
        .host
        .run(r#"region(0, 0, 10, 10).press("ctrl+s");"#)
        .unwrap_err();
    assert!(press.to_string().contains("window"), "{press}");
    let typing = f
        .host
        .run(r#"region(0, 0, 10, 10).type_text("x");"#)
        .unwrap_err();
    assert!(typing.to_string().contains("window"), "{typing}");
}

#[test]
fn global_mouse_move_is_relative_to_cursor() {
    let f = fixture(Canvas::new(200, 200), &[]);
    f.host
        .run(
            r#"
            target("point:100,100").hover();
            mouse_move(10, -20);
        "#,
        )
        .unwrap();
    let log = f.log.entries();
    assert!(log.contains(&"move(110,80)".to_string()), "{log:?}");
}

#[test]
fn global_mouse_move_does_not_activate() {
    let f = fixture(Canvas::new(200, 200), &[]);
    f.host
        .run(
            r#"
            target("point:100,100").hover();
            mouse_move(10, -20);
        "#,
        )
        .unwrap();
    let log = f.log.entries();
    assert!(log.contains(&"activate(100,100)".to_string()), "{log:?}");
    assert!(!log.iter().any(|e| e == "activate(110,80)"), "{log:?}");
}

#[test]
fn set_move_speed_zero_is_instant() {
    let f = fixture(Canvas::new(200, 200), &[]);
    f.host
        .run(
            r#"
            set_move_speed(0);
            target("point:30,40").hover();
        "#,
        )
        .unwrap();
    assert!(f.log.entries().contains(&"move(30,40)".to_string()));
}

#[test]
fn offset_locator_is_relative_to_cursor() {
    let f = fixture(Canvas::new(200, 200), &[]);
    f.host
        .run(
            r#"
            target("point:100,100").hover();
            target("offset:10,-20").click();
        "#,
        )
        .unwrap();
    let log = f.log.entries();
    assert!(log.contains(&"move(110,80)".to_string()), "{log:?}");
}

#[test]
fn region_locator_clicks_center() {
    let f = fixture(Canvas::new(400, 400), &[]);
    f.host
        .run(r#"target("region:100,100,80,40").click();"#)
        .unwrap();
    assert!(f.log.entries().contains(&"move(140,120)".to_string()));
}

/// `ocr:` is accepted as a locator (implemented in Phase 4-1).
///
/// Actual recognition depends on the OS OCR engine, so against a fake capture it
/// ends in "no text found". What is checked here is **that it parses and builds
/// into a text target**.
#[test]
fn ocr_locator_builds_a_text_target() {
    let f = fixture(Canvas::new(200, 200), &[]);
    let found = f
        .host
        .eval::<bool>(r#"target("ocr:ログイン").exists()"#)
        .unwrap();
    assert!(!found, "the synthetic screen should hold no text");
}

/// A malformed `ocr:` is rejected.
#[test]
fn empty_ocr_query_is_rejected() {
    let f = fixture(Canvas::new(200, 200), &[]);
    assert!(f.host.run(r#"target("ocr:").click();"#).is_err());
}

#[test]
fn missing_image_lists_searched_paths() {
    let f = fixture(Canvas::new(200, 200), &[]);
    let err = f
        .host
        .run(r#"target("image:nope.png").click();"#)
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("nope.png"), "{msg}");
    assert!(msg.contains("Searched"), "{msg}");
}

/// Using a pattern-only operation on a non-image locator fails with a reason.
#[test]
fn pattern_only_builders_reject_coordinate_locators() {
    let f = fixture(Canvas::new(200, 200), &[]);
    let err = f
        .host
        .run(r#"target("point:10,10").similar(0.9);"#)
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("not an image locator"), "{msg}");
}

#[test]
fn find_all_returns_reading_order() {
    let mut c = Canvas::new(400, 300);
    c.stamp(250, 40, 30, 5);
    c.stamp(60, 35, 30, 5);
    c.stamp(150, 200, 30, 5);
    let f = fixture(c, &[("row.png", 30, 5)]);

    let xs = f
        .host
        .eval::<rhai::Array>(
            r#"
            let hits = find_all("row.png");
            let xs = [];
            for h in hits { xs.push(h.x); }
            xs
        "#,
        )
        .unwrap();
    let xs: Vec<i64> = xs.into_iter().map(|d| d.cast::<i64>()).collect();
    assert_eq!(xs, vec![60, 250, 150], "not in reading order");
}

#[test]
fn nth_selects_in_reading_order() {
    let mut c = Canvas::new(400, 300);
    c.stamp(250, 40, 30, 5);
    c.stamp(60, 35, 30, 5);
    let f = fixture(c, &[("row.png", 30, 5)]);

    let x = f
        .host
        .eval::<i64>(r#"target("row.png").nth(1).resolve().x"#)
        .unwrap();
    assert_eq!(x, 250);
}

#[test]
fn anchor_relative_disambiguates() {
    let mut c = Canvas::new(500, 300);
    c.stamp(50, 100, 30, 7);
    c.stamp(150, 100, 30, 3);
    c.stamp(250, 100, 30, 7);
    let f = fixture(c, &[("field.png", 30, 7), ("label.png", 30, 3)]);

    let x = f
        .host
        .eval::<i64>(r#"target("field.png").right_of("label.png", 200).resolve().x"#)
        .unwrap();
    assert_eq!(x, 250);

    let x = f
        .host
        .eval::<i64>(r#"target("field.png").left_of("label.png", 200).resolve().x"#)
        .unwrap();
    assert_eq!(x, 50);
}

#[test]
fn expect_to_have_count() {
    let mut c = Canvas::new(400, 400);
    c.stamp(20, 20, 30, 5);
    c.stamp(200, 20, 30, 5);
    c.stamp(20, 200, 30, 5);
    let f = fixture(c, &[("row.png", 30, 5)]);

    f.host
        .run(r#"expect("row.png").to_have_count(3, 100);"#)
        .unwrap();

    let err = f
        .host
        .run(r#"expect("row.png").to_have_count(5, 50);"#)
        .unwrap_err();
    assert!(
        err.to_string().contains("should have 5 matches but has 3"),
        "{err}"
    );
}

#[test]
fn expect_to_vanish() {
    let mut c = Canvas::new(300, 200);
    c.stamp(100, 50, 30, 7);
    let f = fixture(c, &[("here.png", 30, 7), ("gone.png", 30, 55)]);

    // Something not on screen has already vanished.
    //
    // The threshold is explicit because the default 0.7 can false-positive
    // against the synthetic background. expect() also accepts a Target.
    f.host
        .run(r#"expect(target("gone.png").similar(0.95)).to_vanish(100);"#)
        .unwrap();

    let err = f
        .host
        .run(r#"expect(target("here.png").similar(0.95)).to_vanish(50);"#)
        .unwrap_err();
    assert!(err.to_string().contains("did not disappear"), "{err}");
}

#[test]
fn typing_clicks_then_types() {
    let mut c = Canvas::new(300, 200);
    c.stamp(100, 50, 30, 7);
    let f = fixture(c, &[("field.png", 30, 7)]);

    f.host
        .run(r#"target("field.png").type_text("山田太郎");"#)
        .unwrap();

    let log = f.log.entries();
    assert!(log.contains(&"move(115,65)".to_string()), "{log:?}");
    assert!(log.contains(&"down(Left)".to_string()), "{log:?}");
    // type_text now sends one character per call by default (so an application
    // that drops fast bursts still gets every character), so the text arrives
    // in pieces rather than one type(山田太郎). Reassemble to check it all lands.
    let typed: String = log
        .iter()
        .filter_map(|e| e.strip_prefix("type(").and_then(|e| e.strip_suffix(")")))
        .collect();
    assert_eq!(typed, "山田太郎", "{log:?}");
}

#[test]
fn key_combo_is_parsed_and_sent() {
    let f = fixture(Canvas::new(200, 200), &[]);
    f.host.run(r#"press("ctrl+s");"#).unwrap();

    let log = f.log.entries();
    assert!(log.iter().any(|e| e.contains("keydown(Ctrl)")), "{log:?}");
    assert!(
        log.iter().any(|e| e.contains("keydown(Raw(83))")),
        "{log:?}"
    );
    assert!(log.iter().any(|e| e.contains("keyup(Ctrl)")), "{log:?}");
}

#[test]
fn exists_does_not_throw_for_absent_pattern() {
    let f = fixture(Canvas::new(200, 200), &[("gone.png", 30, 55)]);
    let found = f
        .host
        .eval::<bool>(r#"target("gone.png").similar(0.95).exists()"#)
        .unwrap();
    assert!(!found);
}

#[test]
fn region_scoping_limits_the_search() {
    let mut c = Canvas::new(400, 300);
    c.stamp(20, 20, 30, 5); // outside the area
    c.stamp(250, 200, 30, 5); // inside the area
    let f = fixture(c, &[("row.png", 30, 5)]);

    let x = f
        .host
        .eval::<i64>(
            r#"
            let r = region(200, 150, 180, 140);
            r.target("row.png").resolve().x
        "#,
        )
        .unwrap();
    assert_eq!(x, 250, "something outside the scope was picked up");
}

#[test]
fn match_exposes_geometry_and_score() {
    let mut c = Canvas::new(300, 200);
    c.stamp(100, 50, 40, 7);
    let f = fixture(c, &[("btn.png", 40, 7)]);

    let out = f
        .host
        .eval::<rhai::Map>(
            r#"
            let m = target("btn.png").resolve();
            #{ x: m.x, y: m.y, w: m.width, h: m.height, cx: m.center_x, cy: m.center_y }
        "#,
        )
        .unwrap();

    assert_eq!(out["x"].clone().cast::<i64>(), 100);
    assert_eq!(out["y"].clone().cast::<i64>(), 50);
    assert_eq!(out["w"].clone().cast::<i64>(), 40);
    assert_eq!(out["cx"].clone().cast::<i64>(), 120);
    assert_eq!(out["cy"].clone().cast::<i64>(), 70);
}

/// A failure stops the script, and the error carries diagnostics.
#[test]
fn failure_stops_the_script_with_diagnostics() {
    let f = fixture(Canvas::new(300, 200), &[("gone.png", 30, 55)]);
    let err = f
        .host
        .run(
            r#"
            target("gone.png").similar(0.95).timeout(30).click();
            target("point:1,1").click();
        "#,
        )
        .unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("gone.png"), "{msg}");
    // It must not have reached the second line.
    assert!(
        !f.log.entries().contains(&"move(1,1)".to_string()),
        "execution continued after the failure"
    );
}

/// An image asset can be referenced by content address.
#[test]
fn content_addressed_reference_works() {
    let mut c = Canvas::new(320, 240);
    c.stamp(100, 60, 40, 1);
    let f = fixture(c, &[]);

    // Import into the store, then look it up by that reference.
    let png = Canvas::tile_png(40, 1);
    let reference = {
        let rt = f.host.runtime().borrow();
        rt.assets.import_bytes(&png).unwrap()
    };
    assert!(reference.starts_with("sha256:"), "{reference}");

    let script = format!(r#"target("image:{reference}").similar(0.95).resolve().x"#);
    let x = f.host.eval::<i64>(&script).unwrap();
    assert_eq!(x, 100);
}

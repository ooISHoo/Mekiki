//! Rhai integration and script execution for Mekiki.
//!
//! ```rhai
//! let win = expect_window("exe=notepad.exe").to_appear(10_000);
//! win.target("ui:name=Save,type=button").click();
//! expect(win.target("ocr:Saved")).to_appear(5_000);
//! ```
//!
//! See [`rhai_api`] for the complete scripting API. Rust applications use
//! [`ScriptHost`] to configure and execute scripts.

pub mod api;
pub mod assets;
pub mod keys;
pub mod locator;
pub mod resolve;
pub mod rhai_api;
pub mod scan;
#[cfg(feature = "debugging")]
pub mod step;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mekiki_core::{Interrupt, Mekiki, Settings};
use rhai::{Array, Dynamic, Engine, EvalAltResult};

use api::{
    Kind, Runtime, ScriptExpect, ScriptMatch, ScriptRegion, ScriptTarget, ScriptWindowExpect,
    Shared,
};

type RhaiResult<T> = std::result::Result<T, Box<EvalAltResult>>;

/// A configured Rhai engine and its Mekiki runtime.
///
/// The host owns the engine used to compile and execute scripts. Image paths are
/// resolved relative to the base directory supplied at construction time. See
/// [`rhai_api`] for the API available inside a script.
pub struct ScriptHost {
    engine: Engine,
    runtime: Shared,
}

impl ScriptHost {
    /// Build an environment based at the script's directory.
    ///
    /// Uses [`Settings::default`] and resolves relative image assets from
    /// `base_dir`.
    pub fn new(base_dir: impl AsRef<Path>) -> Result<Self, mekiki_core::Error> {
        Self::with_settings(base_dir, Settings::default())
    }

    /// Build an environment with explicit Mekiki settings.
    ///
    /// The settings become the initial values observed by the Rhai setting
    /// functions. Relative image assets are resolved from `base_dir`.
    pub fn with_settings(
        base_dir: impl AsRef<Path>,
        settings: Settings,
    ) -> Result<Self, mekiki_core::Error> {
        let mekiki = Mekiki::with_settings(settings)?;
        let assets = assets::AssetStore::new(base_dir.as_ref());
        Ok(Self::from_runtime(Runtime::new(mekiki, assets)))
    }

    /// Build from an existing runtime.
    ///
    /// This is useful when a caller needs to supply a prepared Mekiki engine or
    /// asset store, including test doubles.
    pub fn from_runtime(runtime: Runtime) -> Self {
        let runtime = Rc::new(RefCell::new(runtime));
        let mut engine = Engine::new();
        register(&mut engine, &runtime);
        Self { engine, runtime }
    }

    /// Borrow the configured Rhai engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Collect the output of `print` and `debug` from a script.
    ///
    /// By default it goes to stdout. Use this to route it to the IDE's output
    /// pane.
    pub fn capture_output(&mut self, sink: Rc<RefCell<Vec<String>>>) {
        let print_sink = sink.clone();
        self.engine.on_print(move |text| {
            print_sink.borrow_mut().push(text.to_string());
        });
        self.engine.on_debug(move |text, source, pos| {
            let where_ = source.unwrap_or("script");
            sink.borrow_mut()
                .push(format!("[debug] {where_}:{pos}: {text}"));
        });
    }

    /// Borrow the shared runtime used by registered Rhai functions.
    ///
    /// The returned handle uses `Rc<RefCell<_>>`; a conflicting borrow during
    /// script execution will panic.
    pub fn runtime(&self) -> &Shared {
        &self.runtime
    }

    /// Start accepting stop and pause requests.
    ///
    /// # Where it takes effect
    ///
    /// Rhai's `on_progress` fires **between instructions**. A single instruction
    /// can be long, so the core's wait loop, `move_to` and the `sleep` below all
    /// check the same flag. Stopping mid-move does not jump to the target.
    pub fn set_interrupt(&mut self, interrupt: Interrupt) {
        self.runtime
            .borrow_mut()
            .mekiki
            .set_interrupt(interrupt.clone());

        self.engine.on_progress(move |_ops| {
            // Pause is handled here. It blocks the calling thread, which is the
            // script's execution thread, so execution simply stops.
            interrupt.wait_while_paused();
            // Returning Some makes Rhai exit with ErrorTerminated.
            interrupt.is_stopping().then_some(Dynamic::UNIT)
        });
    }

    /// Enable step execution and line highlighting.
    ///
    /// # Built on a volatile API
    ///
    /// This is Rhai's `Engine::register_debugger`. Rhai itself declares it
    /// **not deprecated, but volatile and subject to change**, and marks it
    /// `#[deprecated]` to say so — hence the `#[allow(deprecated)]`. To make a
    /// break visible, `tests/step.rs` pins the behaviour.
    ///
    /// # Why the callback fires at every node
    ///
    /// Returning `DebuggerCommand::Continue` moves Rhai's debugger state to
    /// `CONTINUE`, after which **the callback stops being called**. That would
    /// make line highlighting impossible, so this always returns `StepInto` to
    /// keep it firing.
    ///
    /// The cost has been measured (`examples/script_bench.rs`). A pure
    /// computation loop runs about 20% slower, but for real operations where one
    /// instruction takes 1ms, 300 of them cost only +0.5ms.
    #[cfg(feature = "debugging")]
    pub fn set_debugger(&mut self, step: step::StepControl, interrupt: Interrupt) {
        use rhai::debugger::DebuggerCommand;

        #[allow(deprecated)]
        self.engine.register_debugger(
            |_engine, debugger| debugger,
            move |_ctx, _event, _node, _source, pos| {
                let line = pos.line().map(|l| l as u32);
                // A stop request breaks the wait; without that there is no way
                // to stop.
                step.visit(line, &|| interrupt.is_stopping());
                Ok(DebuggerCommand::StepInto)
            },
        );
    }

    /// Run a script string.
    pub fn run(&self, script: &str) -> RhaiResult<()> {
        self.finish(self.engine.run(script))
    }

    /// Evaluate a script and return its result.
    pub fn eval<T: Clone + 'static>(&self, script: &str) -> RhaiResult<T> {
        self.finish(self.engine.eval::<T>(script))
    }

    /// Run a file.
    pub fn run_file(&self, path: impl AsRef<Path>) -> RhaiResult<()> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path)
            .map_err(|e| -> Box<EvalAltResult> { format!("{}: {e}", path.display()).into() })?;
        self.finish(self.engine.run(&source))
    }

    /// Cleanup after a run.
    ///
    /// **Release any held input regardless of success.** That covers stopping
    /// part-way, and also exiting on a failure that could have happened during a
    /// `drag_to`. An "up" must not be sent for a button that was never pressed;
    /// some games treat that as a click at the current position.
    fn finish<T>(&self, result: RhaiResult<T>) -> RhaiResult<T> {
        if let Err(e) = self.runtime.borrow_mut().mekiki.release_input() {
            log::warn!("failed to release input: {e}");
        }
        result
    }

    /// Whether it ended in an interruption.
    ///
    /// A stop is **not a failure**, so callers must distinguish it from an error
    /// display.
    pub fn is_interrupted(error: &EvalAltResult) -> bool {
        matches!(error, EvalAltResult::ErrorTerminated(..))
    }

    /// Take the core engine back after a run.
    ///
    /// The IDE uses the same capture session for match inspection and for
    /// running scripts. DXGI Desktop Duplication allows one per output, so
    /// rebuilding the engine on every run makes the second attempt fail with
    /// `0x80070057`.
    ///
    /// `None` if a Rhai closure still holds a reference, in which case the
    /// caller rebuilds it.
    pub fn into_mekiki(self) -> Option<Mekiki> {
        let ScriptHost { engine, runtime } = self;
        drop(engine);
        Rc::try_unwrap(runtime)
            .ok()
            .map(|cell| cell.into_inner().mekiki)
    }
}

/// Register the Mekiki API with a Rhai engine.
fn register(engine: &mut Engine, rt: &Shared) {
    engine
        .register_type_with_name::<ScriptRegion>("Region")
        .register_type_with_name::<ScriptTarget>("Target")
        .register_type_with_name::<ScriptMatch>("Match")
        .register_type_with_name::<ScriptExpect>("Expect")
        .register_type_with_name::<ScriptWindowExpect>("WindowExpect");

    register_globals(engine, rt);
    register_region(engine);
    register_target_builders(engine);
    register_target_actions(engine);
    register_match(engine);
    register_expect(engine);
    register_window_expect(engine);
}

// ---------------------------------------------------------------------------
// Global functions
// ---------------------------------------------------------------------------

fn register_globals(engine: &mut Engine, rt: &Shared) {
    let r = rt.clone();
    engine.register_fn("screen", move || -> RhaiResult<ScriptRegion> {
        let region = r
            .borrow()
            .mekiki
            .primary_screen()
            .map_err(api::runtime_error)?;
        Ok(ScriptRegion::plain(r.clone(), region))
    });

    let r = rt.clone();
    engine.register_fn("screen", move |index: i64| -> RhaiResult<ScriptRegion> {
        let region = r
            .borrow()
            .mekiki
            .screen(index.max(0) as usize)
            .map_err(api::runtime_error)?;
        Ok(ScriptRegion::plain(r.clone(), region))
    });

    let r = rt.clone();
    engine.register_fn("screen_count", move || -> i64 {
        r.borrow().mekiki.displays().len() as i64
    });

    let r = rt.clone();
    engine.register_fn(
        "region",
        move |x: i64, y: i64, w: i64, h: i64| -> RhaiResult<ScriptRegion> {
            if w <= 0 || h <= 0 {
                return Err(api::runtime_error(
                    "region width and height must be at least 1",
                ));
            }
            let rect = mekiki_core::Rect::new(x as i32, y as i32, w as u32, h as u32);
            let region = r.borrow().mekiki.region(rect);
            Ok(ScriptRegion::plain(r.clone(), region))
        },
    );

    let r = rt.clone();
    engine.register_fn("window", move |spec: &str| -> RhaiResult<ScriptRegion> {
        let query = locator::parse_window_spec(spec).map_err(api::runtime_error)?;
        let region = r
            .borrow()
            .mekiki
            .window_by(&query)
            .map_err(api::runtime_error)?;
        Ok(ScriptRegion::of_window(r.clone(), region))
    });

    let r = rt.clone();
    engine.register_fn("window_titles", move || -> RhaiResult<Array> {
        let list = r
            .borrow()
            .mekiki
            .window_list()
            .map_err(api::runtime_error)?;
        Ok(list.into_iter().map(|w| Dynamic::from(w.title)).collect())
    });

    let r = rt.clone();
    engine.register_fn("window_exists", move |spec: &str| -> RhaiResult<bool> {
        let query = locator::parse_window_spec(spec).map_err(api::runtime_error)?;
        match r.borrow().mekiki.window_by(&query) {
            Ok(_) => Ok(true),
            Err(mekiki_core::Error::Capture(mekiki_core::CaptureError::NoSuchWindow(_))) => {
                Ok(false)
            }
            Err(error) => Err(api::from_core(error)),
        }
    });

    let r = rt.clone();
    engine.register_fn(
        "expect_window",
        move |spec: &str| -> RhaiResult<ScriptWindowExpect> {
            let query = locator::parse_window_spec(spec).map_err(api::runtime_error)?;
            Ok(ScriptWindowExpect::new(r.clone(), spec.to_string(), query))
        },
    );

    let r = rt.clone();
    engine.register_fn("target", move |locator: &str| -> RhaiResult<ScriptTarget> {
        let region = r
            .borrow()
            .mekiki
            .primary_screen()
            .map_err(api::runtime_error)?;
        ScriptTarget::build(r.clone(), region, locator)
    });

    let r = rt.clone();
    engine.register_fn("find", move |locator: &str| -> RhaiResult<ScriptMatch> {
        let region = r
            .borrow()
            .mekiki
            .primary_screen()
            .map_err(api::runtime_error)?;
        let t = ScriptTarget::build(r.clone(), region, locator)?;
        t.resolve_match()
    });

    let r = rt.clone();
    engine.register_fn("find_all", move |locator: &str| -> RhaiResult<Dynamic> {
        let region = r
            .borrow()
            .mekiki
            .primary_screen()
            .map_err(api::runtime_error)?;
        let t = ScriptTarget::build(r.clone(), region, locator)?;
        find_all_impl(&t)
    });

    let r = rt.clone();
    engine.register_fn("expect", move |locator: &str| -> RhaiResult<ScriptExpect> {
        let region = r
            .borrow()
            .mekiki
            .primary_screen()
            .map_err(api::runtime_error)?;
        Ok(ScriptExpect {
            target: ScriptTarget::build(r.clone(), region, locator)?,
        })
    });

    let r = rt.clone();
    engine.register_fn("mouse_x", move || -> RhaiResult<i64> {
        Ok(r.borrow()
            .mekiki
            .mouse_position()
            .map_err(api::runtime_error)?
            .0 as i64)
    });

    let r = rt.clone();
    engine.register_fn("mouse_y", move || -> RhaiResult<i64> {
        Ok(r.borrow()
            .mekiki
            .mouse_position()
            .map_err(api::runtime_error)?
            .1 as i64)
    });

    let r = rt.clone();
    engine.register_fn("type_text", move |text: &str| -> RhaiResult<()> {
        r.borrow_mut()
            .mekiki
            .type_text(text)
            .map_err(api::runtime_error)
    });

    let r = rt.clone();
    engine.register_fn("press", move |spec: &str| -> RhaiResult<()> {
        let (key, mods) = api::parse_combo(spec)?;
        r.borrow_mut()
            .mekiki
            .key_press(key, mods)
            .map_err(api::runtime_error)
    });

    let r = rt.clone();
    engine.register_fn("scroll", move |h: i64, v: i64| -> RhaiResult<()> {
        r.borrow_mut()
            .mekiki
            .scroll(h as i32, v as i32)
            .map_err(api::runtime_error)
    });

    let r = rt.clone();
    engine.register_fn("click", move || -> RhaiResult<()> {
        let mut rt = r.borrow_mut();
        let pos = rt.mekiki.mouse_position().map_err(api::runtime_error)?;
        rt.mekiki.click(pos).map_err(api::from_core)
    });

    let r = rt.clone();
    engine.register_fn("right_click", move || -> RhaiResult<()> {
        let mut rt = r.borrow_mut();
        let pos = rt.mekiki.mouse_position().map_err(api::runtime_error)?;
        rt.mekiki.right_click(pos).map_err(api::from_core)
    });

    let r = rt.clone();
    engine.register_fn("middle_click", move || -> RhaiResult<()> {
        let mut rt = r.borrow_mut();
        let pos = rt.mekiki.mouse_position().map_err(api::runtime_error)?;
        rt.mekiki
            .click_button(pos, mekiki_core::Button::Middle)
            .map_err(api::from_core)
    });

    let r = rt.clone();
    engine.register_fn("mouse_move", move |dx: i64, dy: i64| -> RhaiResult<()> {
        let mut rt = r.borrow_mut();
        let (x, y) = rt.mekiki.mouse_position().map_err(api::from_core)?;
        rt.mekiki
            .move_to((x + dx as i32, y + dy as i32))
            .map_err(api::from_core)
    });

    // `sleep` waits in slices.
    //
    // Sleeping in one go would make `sleep(60000)` unstoppable: `on_progress`
    // only fires between instructions and never inside this one.
    let r = rt.clone();
    engine.register_fn("sleep", move |ms: i64| -> RhaiResult<()> {
        const STEP: Duration = Duration::from_millis(50);

        let interrupt = r.borrow().mekiki.interrupt();
        let mut deadline = Instant::now() + api::millis(ms);

        loop {
            // Return as an interruption. Exiting silently here would count as a
            // normal finish when `sleep` was the last statement, showing
            // "completed" for a run that was stopped.
            if interrupt.is_stopping() {
                return Err(api::terminated());
            }
            // Push the deadline back while paused, so paused time does not
            // count against the wait — the same treatment as the core wait loop.
            deadline += interrupt.wait_while_paused();

            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(());
            };
            mekiki_core::sleep(left.min(STEP));
        }
    });

    let r = rt.clone();
    engine.register_fn("clipboard", move || -> RhaiResult<String> {
        r.borrow().mekiki.clipboard_text().map_err(api::from_core)
    });

    let r = rt.clone();
    engine.register_fn("set_clipboard", move |text: &str| -> RhaiResult<()> {
        r.borrow()
            .mekiki
            .set_clipboard_text(text)
            .map_err(api::from_core)
    });

    let r = rt.clone();
    engine.register_fn("set_similarity", move |v: f64| {
        r.borrow_mut().mekiki.settings.min_similarity = v as f32;
    });

    let r = rt.clone();
    engine.register_fn("set_timeout", move |ms: i64| {
        r.borrow_mut().mekiki.settings.auto_wait_timeout = api::millis(ms);
    });

    let r = rt.clone();
    engine.register_fn("set_auto_wait", move |enabled: bool| {
        let mut guard = r.borrow_mut();
        guard.mekiki.settings.actionability.stable_frames = if enabled { 1 } else { 0 };
    });

    let r = rt.clone();
    engine.register_fn("set_recheck", move |enabled: bool| {
        r.borrow_mut().mekiki.settings.recheck = enabled;
    });

    let r = rt.clone();
    engine.register_fn("set_type_chunk", move |n: i64| {
        r.borrow_mut().mekiki.settings.type_chunk = n.max(0) as usize;
    });

    let r = rt.clone();
    engine.register_fn("set_type_interval", move |ms: i64| {
        r.borrow_mut().mekiki.settings.type_interval = api::millis(ms);
    });

    let r = rt.clone();
    engine.register_fn("set_click_hold", move |ms: i64| {
        r.borrow_mut().mekiki.settings.click_hold = api::millis(ms);
    });

    let r = rt.clone();
    engine.register_fn("set_double_click_interval", move |ms: i64| {
        r.borrow_mut().mekiki.settings.double_click_interval = api::millis(ms);
    });

    let r = rt.clone();
    engine.register_fn("set_move_settle", move |ms: i64| {
        r.borrow_mut().mekiki.settings.move_settle = api::millis(ms);
    });

    let r = rt.clone();
    engine.register_fn("set_move_speed", move |v: i64| {
        r.borrow_mut().mekiki.settings.move_speed = v.max(0) as f64;
    });
    let r = rt.clone();
    engine.register_fn("set_move_speed", move |v: f64| {
        r.borrow_mut().mekiki.settings.move_speed = v.max(0.0);
    });
}

// ---------------------------------------------------------------------------
// Region
// ---------------------------------------------------------------------------

fn register_region(engine: &mut Engine) {
    engine
        .register_fn("to_string", |r: &mut ScriptRegion| r.to_display())
        .register_fn("to_debug", |r: &mut ScriptRegion| r.to_display())
        .register_get("x", |r: &mut ScriptRegion| r.region.rect.x as i64)
        .register_get("y", |r: &mut ScriptRegion| r.region.rect.y as i64)
        .register_get("width", |r: &mut ScriptRegion| r.region.rect.width as i64)
        .register_get("height", |r: &mut ScriptRegion| r.region.rect.height as i64);

    engine.register_fn(
        "target",
        |r: &mut ScriptRegion, locator: &str| -> RhaiResult<ScriptTarget> { r.target(locator) },
    );

    engine.register_fn(
        "find",
        |r: &mut ScriptRegion, locator: &str| -> RhaiResult<ScriptMatch> {
            let t = r.target(locator)?;
            t.resolve_match()
        },
    );

    engine.register_fn(
        "find_all",
        |r: &mut ScriptRegion, locator: &str| -> RhaiResult<Dynamic> {
            find_all_impl(&r.target(locator)?)
        },
    );

    engine.register_fn("read_text", |r: &mut ScriptRegion| -> RhaiResult<Array> {
        let lines =
            r.rt.borrow_mut()
                .mekiki
                .read_text(r.region)
                .map_err(api::from_core)?;
        Ok(lines.into_iter().map(|l| Dynamic::from(l.text)).collect())
    });

    engine.register_fn("list_ui", |r: &mut ScriptRegion| -> RhaiResult<Array> {
        list_ui_impl(r, None)
    });
    engine.register_fn(
        "list_ui",
        |r: &mut ScriptRegion, control_type: &str| -> RhaiResult<Array> {
            list_ui_impl(r, Some(control_type))
        },
    );

    engine.register_fn(
        "read_value",
        |r: &mut ScriptRegion, spec: &str| -> RhaiResult<String> {
            let parsed = locator::parse(spec).map_err(api::runtime_error)?;
            let locator::Locator::Ui(query) = parsed else {
                return Err(api::runtime_error("read_value() needs a ui: locator"));
            };
            let mut rt = r.rt.borrow_mut();
            let pattern = resolve::ui_pattern(&rt.mekiki, &query);
            let value = rt
                .mekiki
                .read_ui_value(r.region, &pattern)
                .map_err(api::from_core)?;
            if value.is_password {
                return Err(api::runtime_error(
                    "the element is a password field; its value is redacted",
                ));
            }
            value
                .value
                .ok_or_else(|| api::runtime_error("the element has no readable value"))
        },
    );

    engine.register_fn(
        "save",
        |r: &mut ScriptRegion, path: &str| -> RhaiResult<()> {
            let frame =
                r.rt.borrow_mut()
                    .mekiki
                    .capture_region(r.region)
                    .map_err(api::from_core)?;
            mekiki_core::artifacts::save_frame(&frame, Path::new(path))
                .map_err(|e| api::runtime_error(format!("cannot save '{path}': {e}")))
        },
    );

    engine.register_fn(
        "expect",
        |r: &mut ScriptRegion, locator: &str| -> RhaiResult<ScriptExpect> {
            Ok(ScriptExpect {
                target: r.target(locator)?,
            })
        },
    );

    engine.register_fn("activate", |r: &mut ScriptRegion| -> RhaiResult<()> {
        let Some(hwnd) = r.window else {
            return Err(api::runtime_error(
                "activate() only works on an area built by window(...)",
            ));
        };
        r.rt.borrow()
            .mekiki
            .activate_window_handle(hwnd)
            .map_err(api::from_core)
    });

    engine.register_fn(
        "press",
        |r: &mut ScriptRegion, spec: &str| -> RhaiResult<()> {
            let Some(hwnd) = r.window else {
                return Err(api::runtime_error(
                    "press() only works on an area built by window(...)",
                ));
            };
            let (key, modifiers) = api::parse_combo(spec)?;
            let mut rt = r.rt.borrow_mut();
            rt.mekiki
                .activate_window_handle(hwnd)
                .map_err(api::from_core)?;
            rt.mekiki.key_press(key, modifiers).map_err(api::from_core)
        },
    );

    engine.register_fn(
        "type_text",
        |r: &mut ScriptRegion, text: &str| -> RhaiResult<()> {
            let Some(hwnd) = r.window else {
                return Err(api::runtime_error(
                    "type_text() only works on an area built by window(...)",
                ));
            };
            let mut rt = r.rt.borrow_mut();
            rt.mekiki
                .activate_window_handle(hwnd)
                .map_err(api::from_core)?;
            rt.mekiki.type_text(text).map_err(api::from_core)
        },
    );

    engine
        .register_fn("grow", |r: &mut ScriptRegion, by: i64| {
            r.derived(r.region.grow(by as i32))
        })
        .register_fn("offset", |r: &mut ScriptRegion, dx: i64, dy: i64| {
            r.derived(r.region.offset(dx as i32, dy as i32))
        })
        .register_fn("above", |r: &mut ScriptRegion, h: i64| {
            r.derived(r.region.above(h.max(0) as u32))
        })
        .register_fn("below", |r: &mut ScriptRegion, h: i64| {
            r.derived(r.region.below(h.max(0) as u32))
        })
        .register_fn("left", |r: &mut ScriptRegion, w: i64| {
            r.derived(r.region.left(w.max(0) as u32))
        })
        .register_fn("right", |r: &mut ScriptRegion, w: i64| {
            r.derived(r.region.right(w.max(0) as u32))
        });
}

// ---------------------------------------------------------------------------
// Target: builders
// ---------------------------------------------------------------------------

fn register_target_builders(engine: &mut Engine) {
    engine
        .register_fn("to_string", |t: &mut ScriptTarget| t.to_display())
        .register_fn("to_debug", |t: &mut ScriptTarget| t.to_display());

    engine.register_fn(
        "similar",
        |t: &mut ScriptTarget, v: f64| -> RhaiResult<ScriptTarget> {
            t.map_pattern("similar()", |x| x.similar(v as f32))
        },
    );

    engine.register_fn(
        "offset",
        |t: &mut ScriptTarget, dx: i64, dy: i64| -> RhaiResult<ScriptTarget> {
            t.map_pattern("offset()", |x| x.offset(dx as i32, dy as i32))
        },
    );

    engine.register_fn(
        "timeout",
        |t: &mut ScriptTarget, ms: i64| -> RhaiResult<ScriptTarget> {
            t.map_pattern("timeout()", |x| x.timeout(api::millis(ms)))
        },
    );

    engine.register_fn(
        "force",
        |t: &mut ScriptTarget| -> RhaiResult<ScriptTarget> {
            t.map_pattern("force()", |x| x.force())
        },
    );

    engine.register_fn(
        "recheck",
        |t: &mut ScriptTarget, enabled: bool| -> RhaiResult<ScriptTarget> {
            Ok(t.with_recheck(enabled))
        },
    );

    engine.register_fn(
        "nth",
        |t: &mut ScriptTarget, n: i64| -> RhaiResult<ScriptTarget> {
            t.map_pattern("nth()", |x| x.nth(n.max(0) as usize))
        },
    );

    engine.register_fn(
        "first",
        |t: &mut ScriptTarget| -> RhaiResult<ScriptTarget> {
            t.map_pattern("first()", |x| x.first())
        },
    );

    engine.register_fn("last", |t: &mut ScriptTarget| -> RhaiResult<ScriptTarget> {
        t.map_pattern("last()", |x| x.last())
    });

    engine.register_fn("best", |t: &mut ScriptTarget| -> RhaiResult<ScriptTarget> {
        t.map_pattern("best()", |x| x.best())
    });

    engine.register_fn(
        "in_region",
        |t: &mut ScriptTarget, r: ScriptRegion| -> RhaiResult<ScriptTarget> {
            t.map_pattern("in_region()", |x| x.in_region(r.region))
        },
    );

    engine.register_fn(
        "or",
        |t: &mut ScriptTarget, locator: &str| -> RhaiResult<ScriptTarget> { t.or_locator(locator) },
    );

    engine.register_fn(
        "right_of",
        |t: &mut ScriptTarget, anchor: &str, d: i64| -> RhaiResult<ScriptTarget> {
            t.with_anchor("right_of()", anchor, d, |x, a, dist| {
                x.related_to(mekiki_core::Direction::RightOf, a, dist)
            })
        },
    );
    engine.register_fn(
        "left_of",
        |t: &mut ScriptTarget, anchor: &str, d: i64| -> RhaiResult<ScriptTarget> {
            t.with_anchor("left_of()", anchor, d, |x, a, dist| {
                x.related_to(mekiki_core::Direction::LeftOf, a, dist)
            })
        },
    );
    engine.register_fn(
        "above",
        |t: &mut ScriptTarget, anchor: &str, d: i64| -> RhaiResult<ScriptTarget> {
            t.with_anchor("above()", anchor, d, |x, a, dist| {
                x.related_to(mekiki_core::Direction::Above, a, dist)
            })
        },
    );
    engine.register_fn(
        "below",
        |t: &mut ScriptTarget, anchor: &str, d: i64| -> RhaiResult<ScriptTarget> {
            t.with_anchor("below()", anchor, d, |x, a, dist| {
                x.related_to(mekiki_core::Direction::Below, a, dist)
            })
        },
    );
    engine.register_fn(
        "near",
        |t: &mut ScriptTarget, anchor: &str, d: i64| -> RhaiResult<ScriptTarget> {
            t.with_anchor("near()", anchor, d, |x, a, dist| {
                x.related_to(mekiki_core::Direction::Near, a, dist)
            })
        },
    );
}

// ---------------------------------------------------------------------------
// Target: actions
// ---------------------------------------------------------------------------

fn register_target_actions(engine: &mut Engine) {
    engine.register_fn("click", |t: &mut ScriptTarget| -> RhaiResult<ScriptMatch> {
        let m = t.resolve_for_action(true)?;
        let point = m.inner.target();
        t.rt.borrow_mut()
            .mekiki
            .click(point)
            .map_err(api::from_core)?;
        Ok(m)
    });

    engine.register_fn(
        "right_click",
        |t: &mut ScriptTarget| -> RhaiResult<ScriptMatch> {
            let m = t.resolve_for_action(true)?;
            let point = m.inner.target();
            t.rt.borrow_mut()
                .mekiki
                .right_click(point)
                .map_err(api::from_core)?;
            Ok(m)
        },
    );

    engine.register_fn(
        "double_click",
        |t: &mut ScriptTarget| -> RhaiResult<ScriptMatch> {
            let m = t.resolve_for_action(true)?;
            let point = m.inner.target();
            t.rt.borrow_mut()
                .mekiki
                .double_click(point)
                .map_err(api::from_core)?;
            Ok(m)
        },
    );

    engine.register_fn(
        "middle_click",
        |t: &mut ScriptTarget| -> RhaiResult<ScriptMatch> {
            let m = t.resolve_for_action(true)?;
            let point = m.inner.target();
            t.rt.borrow_mut()
                .mekiki
                .click_button(point, mekiki_core::Button::Middle)
                .map_err(api::from_core)?;
            Ok(m)
        },
    );

    engine.register_fn("hover", |t: &mut ScriptTarget| -> RhaiResult<ScriptMatch> {
        let m = t.resolve_for_action(false)?;
        let point = m.inner.target();
        t.rt.borrow_mut()
            .mekiki
            .hover(point)
            .map_err(api::from_core)?;
        Ok(m)
    });

    engine.register_fn(
        "type_text",
        |t: &mut ScriptTarget, text: &str| -> RhaiResult<ScriptMatch> { type_into(t, text) },
    );

    engine.register_fn(
        "press",
        |t: &mut ScriptTarget, spec: &str| -> RhaiResult<ScriptMatch> {
            let (key, mods) = api::parse_combo(spec)?;
            let m = t.resolve_for_action(true)?;
            let point = m.inner.target();
            let mut rt = t.rt.borrow_mut();
            rt.mekiki.hover(point).map_err(api::from_core)?;
            rt.mekiki.key_press(key, mods).map_err(api::from_core)?;
            Ok(m)
        },
    );

    engine.register_fn(
        "scroll",
        |t: &mut ScriptTarget, h: i64, v: i64| -> RhaiResult<ScriptMatch> {
            let m = t.resolve_for_action(true)?;
            let point = m.inner.target();
            let mut rt = t.rt.borrow_mut();
            rt.mekiki.hover(point).map_err(api::from_core)?;
            rt.mekiki
                .scroll(h as i32, v as i32)
                .map_err(api::from_core)?;
            Ok(m)
        },
    );

    engine.register_fn(
        "drag_to",
        |t: &mut ScriptTarget, dest: ScriptTarget| -> RhaiResult<()> {
            // Resolve both ends first. Grabbing and then searching for the
            // destination would mean searching with the button held down.
            let from = t.resolve_point()?;
            let to = dest.resolve_point()?;
            t.rt.borrow_mut()
                .mekiki
                .drag_drop(from, to)
                .map_err(api::from_core)
        },
    );

    engine.register_fn(
        "resolve",
        |t: &mut ScriptTarget| -> RhaiResult<ScriptMatch> { t.resolve_match() },
    );

    engine.register_fn("exists", |t: &mut ScriptTarget| -> RhaiResult<bool> {
        match &t.kind {
            Kind::Pattern(inner) => {
                let mut rt = t.rt.borrow_mut();
                rt.mekiki.on(inner).exists().map_err(api::runtime_error)
            }
            // Coordinates and rectangles always exist.
            _ => Ok(true),
        }
    });

    engine.register_fn(
        "wait_vanish",
        |t: &mut ScriptTarget, ms: i64| -> RhaiResult<bool> {
            let inner = t.as_target()?;
            let mut rt = t.rt.borrow_mut();
            rt.mekiki
                .on(&inner)
                .wait_vanish(api::optional_millis(ms))
                .map_err(api::runtime_error)
        },
    );

    engine.register_fn(
        "highlight",
        |t: &mut ScriptTarget, ms: i64| -> RhaiResult<ScriptMatch> {
            let m = t.resolve_for_action(false)?;
            let rect = m.inner.rect;
            t.rt.borrow_mut()
                .mekiki
                .highlight_rect(rect, api::millis(ms))
                .map_err(api::runtime_error)?;
            Ok(m)
        },
    );
}

fn type_into(t: &mut ScriptTarget, text: &str) -> RhaiResult<ScriptMatch> {
    let m = t.resolve_for_action(true)?;
    let point = m.inner.target();
    let mut rt = t.rt.borrow_mut();
    rt.mekiki.click(point).map_err(api::from_core)?;
    rt.mekiki.type_text(text).map_err(api::from_core)?;
    Ok(m)
}

fn find_all_impl(t: &ScriptTarget) -> RhaiResult<Dynamic> {
    let inner = t.as_target()?;
    let matches = {
        let mut rt = t.rt.borrow_mut();
        rt.mekiki.on(&inner).resolve_all()
    };

    // Finding none is an empty array, not an error — the same as SikuliX's findAll.
    let matches = match matches {
        Ok(v) => v,
        Err(mekiki_core::Error::NotFound(_)) => Vec::new(),
        Err(e) => return Err(api::runtime_error(e)),
    };

    Ok(api::to_dynamic_array(
        matches
            .into_iter()
            .map(|m| ScriptMatch {
                rt: t.rt.clone(),
                inner: m,
            })
            .collect(),
    ))
}

fn list_ui_impl(r: &mut ScriptRegion, control_type: Option<&str>) -> RhaiResult<Array> {
    let items =
        r.rt.borrow_mut()
            .mekiki
            .list_ui(r.region, control_type)
            .map_err(api::from_core)?;
    Ok(items
        .into_iter()
        .map(|i| Dynamic::from(ui_locator_of(&i)))
        .collect())
}

/// Format one enumerated element as a ready-made `ui:` locator, so discovery
/// output can be pasted straight into `target(...)`.
fn ui_locator_of(item: &mekiki_core::UiItem) -> String {
    let escape = |s: &str| s.replace('\\', "\\\\").replace(',', "\\,");
    let mut parts = Vec::new();
    if !item.name.is_empty() {
        parts.push(format!("name={}", escape(&item.name)));
    }
    if !item.automation_id.is_empty() {
        parts.push(format!("id={}", escape(&item.automation_id)));
    }
    parts.push(format!("type={}", item.control_type));
    format!("ui:{}", parts.join(","))
}

// ---------------------------------------------------------------------------
// Match
// ---------------------------------------------------------------------------

fn register_match(engine: &mut Engine) {
    engine
        .register_fn("to_string", |m: &mut ScriptMatch| m.to_display())
        .register_fn("to_debug", |m: &mut ScriptMatch| m.to_display())
        .register_get("x", |m: &mut ScriptMatch| m.inner.rect.x as i64)
        .register_get("y", |m: &mut ScriptMatch| m.inner.rect.y as i64)
        .register_get("width", |m: &mut ScriptMatch| m.inner.rect.width as i64)
        .register_get("height", |m: &mut ScriptMatch| m.inner.rect.height as i64)
        .register_get("score", |m: &mut ScriptMatch| m.inner.score as f64)
        .register_get("center_x", |m: &mut ScriptMatch| m.inner.center().0 as i64)
        .register_get("center_y", |m: &mut ScriptMatch| m.inner.center().1 as i64);

    engine.register_fn("click", |m: &mut ScriptMatch| -> RhaiResult<()> {
        let point = m.inner.target();
        m.rt.borrow_mut()
            .mekiki
            .click(point)
            .map_err(api::from_core)
    });

    engine.register_fn("right_click", |m: &mut ScriptMatch| -> RhaiResult<()> {
        let point = m.inner.target();
        m.rt.borrow_mut()
            .mekiki
            .right_click(point)
            .map_err(api::from_core)
    });

    engine.register_fn("double_click", |m: &mut ScriptMatch| -> RhaiResult<()> {
        let point = m.inner.target();
        m.rt.borrow_mut()
            .mekiki
            .double_click(point)
            .map_err(api::from_core)
    });

    engine.register_fn("middle_click", |m: &mut ScriptMatch| -> RhaiResult<()> {
        let point = m.inner.target();
        m.rt.borrow_mut()
            .mekiki
            .click_button(point, mekiki_core::Button::Middle)
            .map_err(api::from_core)
    });

    engine.register_fn("hover", |m: &mut ScriptMatch| -> RhaiResult<()> {
        let point = m.inner.target();
        m.rt.borrow_mut()
            .mekiki
            .hover(point)
            .map_err(api::from_core)
    });

    engine.register_fn(
        "highlight",
        |m: &mut ScriptMatch, ms: i64| -> RhaiResult<()> {
            let rect = m.inner.rect;
            m.rt.borrow_mut()
                .mekiki
                .highlight_rect(rect, api::millis(ms))
                .map_err(api::runtime_error)
        },
    );

    engine.register_fn("region", |m: &mut ScriptMatch| {
        ScriptRegion::plain(m.rt.clone(), m.inner.region())
    });
}

// ---------------------------------------------------------------------------
// Expect
// ---------------------------------------------------------------------------

fn register_expect(engine: &mut Engine) {
    engine.register_fn("expect", |t: &mut ScriptTarget| ScriptExpect {
        target: t.clone(),
    });

    engine.register_fn(
        "to_appear",
        |e: &mut ScriptExpect, ms: i64| -> RhaiResult<ScriptMatch> {
            let inner = e.target.as_target()?;
            let rt = e.target.rt.clone();
            let m = {
                let mut guard = rt.borrow_mut();
                guard
                    .mekiki
                    .expect(&inner)
                    .to_appear(api::optional_millis(ms))
                    .map_err(api::runtime_error)?
            };
            Ok(ScriptMatch { rt, inner: m })
        },
    );

    engine.register_fn(
        "to_vanish",
        |e: &mut ScriptExpect, ms: i64| -> RhaiResult<()> {
            let inner = e.target.as_target()?;
            let mut guard = e.target.rt.borrow_mut();
            guard
                .mekiki
                .expect(&inner)
                .to_vanish(api::optional_millis(ms))
                .map_err(api::runtime_error)
        },
    );

    engine.register_fn(
        "to_have_count",
        |e: &mut ScriptExpect, n: i64, ms: i64| -> RhaiResult<Dynamic> {
            let inner = e.target.as_target()?;
            let rt = e.target.rt.clone();
            let matches = {
                let mut guard = rt.borrow_mut();
                guard
                    .mekiki
                    .expect(&inner)
                    .to_have_count(n.max(0) as usize, api::optional_millis(ms))
                    .map_err(api::runtime_error)?
            };
            Ok(api::to_dynamic_array(
                matches
                    .into_iter()
                    .map(|m| ScriptMatch {
                        rt: rt.clone(),
                        inner: m,
                    })
                    .collect(),
            ))
        },
    );

    engine.register_fn(
        "to_have_count_at_least",
        |e: &mut ScriptExpect, n: i64, ms: i64| -> RhaiResult<Dynamic> {
            let inner = e.target.as_target()?;
            let rt = e.target.rt.clone();
            let matches = {
                let mut guard = rt.borrow_mut();
                guard
                    .mekiki
                    .expect(&inner)
                    .to_have_count_at_least(n.max(0) as usize, api::optional_millis(ms))
                    .map_err(api::runtime_error)?
            };
            Ok(api::to_dynamic_array(
                matches
                    .into_iter()
                    .map(|m| ScriptMatch {
                        rt: rt.clone(),
                        inner: m,
                    })
                    .collect(),
            ))
        },
    );

    engine.register_fn(
        "to_have_count_at_most",
        |e: &mut ScriptExpect, n: i64, ms: i64| -> RhaiResult<Dynamic> {
            let inner = e.target.as_target()?;
            let rt = e.target.rt.clone();
            let matches = {
                let mut guard = rt.borrow_mut();
                guard
                    .mekiki
                    .expect(&inner)
                    .to_have_count_at_most(n.max(0) as usize, api::optional_millis(ms))
                    .map_err(api::runtime_error)?
            };
            Ok(api::to_dynamic_array(
                matches
                    .into_iter()
                    .map(|m| ScriptMatch {
                        rt: rt.clone(),
                        inner: m,
                    })
                    .collect(),
            ))
        },
    );
}

fn register_window_expect(engine: &mut Engine) {
    engine.register_fn(
        "to_appear",
        |e: &mut ScriptWindowExpect, ms: i64| -> RhaiResult<ScriptRegion> {
            let limit = api::optional_millis(ms)
                .unwrap_or_else(|| e.rt.borrow().mekiki.settings.auto_wait_timeout);
            let interval = e.rt.borrow().mekiki.settings.wait_scan_interval;
            let interrupt = e.rt.borrow().mekiki.interrupt();
            let mut deadline = Instant::now() + limit;
            loop {
                match e.rt.borrow().mekiki.window_by(&e.query) {
                    Ok(region) => {
                        return Ok(ScriptRegion::of_window(e.rt.clone(), region));
                    }
                    Err(mekiki_core::Error::Capture(mekiki_core::CaptureError::NoSuchWindow(
                        _,
                    ))) => {}
                    Err(error) => return Err(api::from_core(error)),
                }
                if interrupt.is_stopping() {
                    return Err(api::terminated());
                }
                deadline += interrupt.wait_while_paused();
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    return Err(api::runtime_error(format!(
                        "window '{}' did not appear after {:.1}s",
                        e.source,
                        limit.as_secs_f32()
                    )));
                };
                mekiki_core::sleep(left.min(interval));
            }
        },
    );

    engine.register_fn(
        "to_vanish",
        |e: &mut ScriptWindowExpect, ms: i64| -> RhaiResult<()> {
            let limit = api::optional_millis(ms)
                .unwrap_or_else(|| e.rt.borrow().mekiki.settings.auto_wait_timeout);
            let interval = e.rt.borrow().mekiki.settings.wait_scan_interval;
            let interrupt = e.rt.borrow().mekiki.interrupt();
            let mut deadline = Instant::now() + limit;
            loop {
                match e.rt.borrow().mekiki.window_by(&e.query) {
                    Err(mekiki_core::Error::Capture(mekiki_core::CaptureError::NoSuchWindow(
                        _,
                    ))) => return Ok(()),
                    Ok(_) => {}
                    Err(error) => return Err(api::from_core(error)),
                }
                if interrupt.is_stopping() {
                    return Err(api::terminated());
                }
                deadline += interrupt.wait_while_paused();
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    return Err(api::runtime_error(format!(
                        "window '{}' did not disappear after {:.1}s",
                        e.source,
                        limit.as_secs_f32()
                    )));
                };
                mekiki_core::sleep(left.min(interval));
            }
        },
    );
}

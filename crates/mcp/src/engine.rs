//! The worker thread that owns the engine.
//!
//! Copied in shape from the IDE's `ide/src-tauri/src/engine.rs`, for the same
//! two reasons.
//!
//! 1. **The engine is not `Send`.** The scripting layer holds `Rc<RefCell<_>>`,
//!    so it cannot cross threads. One thread owns it and everything else talks
//!    to that thread over a channel.
//! 2. **State is worth keeping.** The GPU device, the capture session and the
//!    pattern cache are expensive to build. An agent calling `find` twenty
//!    times in a row must not pay for that twenty times.
//!
//! Jobs run **strictly one at a time**. That is not a limitation to work
//! around: the desktop is a single shared resource, and two overlapping clicks
//! would be a race against the user's own hands.
//!
//! # Stop does not go through the queue
//!
//! While a script runs, the worker does not come back to read the queue, so a
//! `stop` job would sit behind the very thing it is meant to interrupt.
//! [`Interrupt`] is a shared flag instead, and [`EngineHandle::stop`] sets it
//! from whatever thread the request arrived on.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mekiki_core::{
    Interrupt, Match, Mekiki, Rect, Settings, TextLine, UiItem, UiValue, WindowInfo, WindowQuery,
};
use mekiki_scripting::assets::AssetStore;
use mekiki_scripting::locator::{self, Locator};
use mekiki_scripting::resolve::{self, Resolved};
use mekiki_scripting::{ScriptHost, api::Runtime};

/// What the worker is asked to do. The reply channel comes with the job.
enum Job {
    ListWindows(Sender<Result<Vec<WindowInfo>, String>>),
    WaitWindow {
        query: WindowQuery,
        vanish: bool,
        timeout: Duration,
        reply: Sender<Result<WindowWaitResult, String>>,
    },
    Capture {
        scope: Option<String>,
        reply: Sender<Result<CapturedFrame, String>>,
    },
    ReadText {
        scope: Option<String>,
        reply: Sender<Result<Vec<TextLine>, String>>,
    },
    ListUi {
        scope: Option<String>,
        control_type: Option<String>,
        reply: Sender<Result<Vec<UiItem>, String>>,
    },
    ReadValue {
        locator: String,
        scope: String,
        reply: Sender<Result<UiValue, String>>,
    },
    Find {
        locator: String,
        scope: Option<String>,
        similar: Option<f32>,
        timeout_ms: Option<u64>,
        reply: Sender<Result<Vec<Match>, String>>,
    },
    Act {
        locator: String,
        scope: Option<String>,
        action: Action,
        reply: Sender<Result<Match, String>>,
    },
    /// Input aimed at a window, or at whatever has focus; nothing is searched for.
    RawInput {
        input: RawInput,
        /// A window to bring to the front first. `None` leaves focus alone.
        scope: Option<String>,
        reply: Sender<Result<(), String>>,
    },
    CheckScript {
        source: String,
        reply: Sender<Result<(), String>>,
    },
    RunScript {
        source: String,
        /// **The handle's flag, carried in deliberately.**
        ///
        /// The engine builds its own `Interrupt` at construction, and reaching
        /// for that one instead is the mistake this field exists to prevent:
        /// the watchdog, the `stop` tool and the hotkey all raise the handle's,
        /// so a script wired to any other flag cannot be stopped at all.
        interrupt: Interrupt,
        reply: Sender<RunOutcome>,
    },
    CaptureAsset {
        rect: Rect,
        reply: Sender<Result<CapturedAsset, String>>,
    },
}

/// What to do once a locator has been resolved.
#[derive(Clone, Copy, Debug)]
pub enum Action {
    Click { right: bool, double: bool },
    Hover,
}

/// Input aimed at a window rather than at a located target.
#[derive(Clone, Debug)]
pub enum RawInput {
    TypeText(String),
    Press(String),
    Scroll { h: i32, v: i32 },
}

/// A captured frame, still in BGRA.
pub struct CapturedFrame {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Top-left in screen coordinates, so the agent can map back.
    pub origin: (i32, i32),
}

#[derive(Clone, Debug)]
pub struct LastCapture {
    pub timestamp_ms: u64,
    pub elapsed_ms: u64,
    pub width: u32,
    pub height: u32,
    pub origin: (i32, i32),
    pub backend: String,
}

#[derive(Clone, Debug)]
pub struct WindowWaitResult {
    pub matched: bool,
    pub window: Option<WindowInfo>,
    pub elapsed_ms: u64,
}

/// A region imported into the content-addressed store.
pub struct CapturedAsset {
    /// What to write in a script: `sha256:...`.
    pub reference: String,
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// How a script run ended.
#[derive(Default)]
pub struct RunOutcome {
    pub ok: bool,
    pub output: Vec<String>,
    pub error: Option<String>,
    /// Stopped on request — by the hotkey, by `stop`, or by the timeout.
    /// **Not a failure**, and callers must not report it as one.
    pub interrupted: bool,
    /// The timeout fired. Implies `interrupted`.
    pub timed_out: bool,
    pub elapsed_ms: u64,
}

/// The way in to the worker.
pub struct EngineHandle {
    tx: Mutex<Sender<Job>>,
    /// Shared with the running script. **Deliberately outside the queue** — see
    /// the module docs.
    interrupt: Interrupt,
    /// True while a script is running.
    ///
    /// The queue alone would make a concurrent `click` *wait* rather than fail,
    /// and it would then fire whenever the script happened to end — against a
    /// screen that had moved on. Failing fast says what is actually going on.
    /// The host may issue tool calls concurrently, so this is a real case, not
    /// a hypothetical one.
    running: AtomicBool,
    /// Start time of the current script, for the status tool.
    running_since: Mutex<Option<Instant>>,
    /// Capture diagnostics published by the worker after engine construction.
    capture_diagnostics: Arc<Mutex<String>>,
    last_capture: Arc<Mutex<Option<LastCapture>>>,
    base: PathBuf,
}

impl EngineHandle {
    /// Start the worker. `base` is where scripts and assets live.
    ///
    /// Engine construction happens on the worker thread and is allowed to fail:
    /// the server still starts, so the agent gets a diagnosable error from the
    /// first tool call instead of a process that vanished at startup.
    pub fn spawn(base: PathBuf) -> Self {
        let (tx, rx) = channel::<Job>();
        let worker_base = base.clone();

        // One flag for the whole server. Everything that can stop something —
        // the watchdog, the `stop` tool, the hotkey — raises this one, and
        // everything that can be stopped watches it.
        let interrupt = Interrupt::new();
        let worker_interrupt = interrupt.clone();
        let capture_diagnostics = Arc::new(Mutex::new("initialising".to_string()));
        let worker_diagnostics = capture_diagnostics.clone();
        let last_capture = Arc::new(Mutex::new(None));
        let worker_last_capture = last_capture.clone();

        std::thread::Builder::new()
            .name("mekiki-mcp-engine".into())
            .spawn(move || {
                let mut engine = match Mekiki::with_settings(mcp_settings(&worker_base)) {
                    Ok(mut m) => {
                        // Replace the engine's own flag with the shared one, so
                        // that a long `find` or `click` is interruptible too —
                        // not just a running script.
                        m.set_interrupt(worker_interrupt.clone());
                        let diagnostics = m.capture_diagnostics();
                        if let Ok(mut state) = worker_diagnostics.lock() {
                            *state = diagnostics.clone();
                        }
                        log::info!("engine ready: {diagnostics}");
                        Some(m)
                    }
                    Err(e) => {
                        if let Ok(mut state) = worker_diagnostics.lock() {
                            *state = format!("unavailable: {e}");
                        }
                        log::error!("cannot initialise the engine: {e}");
                        None
                    }
                };
                let mut assets = AssetStore::new(&worker_base);

                while let Ok(job) = rx.recv() {
                    handle(
                        &mut engine,
                        &mut assets,
                        &worker_base,
                        &worker_diagnostics,
                        &worker_last_capture,
                        job,
                    );
                }

                // Leaving a button held down outlives this process and lands on
                // the user's desktop, so release on the way out.
                if let Some(m) = engine.as_mut()
                    && let Err(e) = m.release_input()
                {
                    log::warn!("failed to release input while shutting down: {e}");
                }
            })
            .expect("cannot start the engine worker");

        Self {
            tx: Mutex::new(tx),
            interrupt,
            running: AtomicBool::new(false),
            running_since: Mutex::new(None),
            capture_diagnostics,
            last_capture,
            base,
        }
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// The flag the emergency-stop hotkey raises.
    pub fn interrupt(&self) -> Interrupt {
        self.interrupt.clone()
    }

    /// Interrupt whatever is running. Does not wait for it to finish.
    pub fn stop(&self) {
        self.interrupt.stop();
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn elapsed_ms(&self) -> Option<u64> {
        self.running_since
            .lock()
            .ok()
            .and_then(|started| started.as_ref().map(|t| t.elapsed().as_millis() as u64))
    }

    pub fn capture_diagnostics(&self) -> String {
        self.capture_diagnostics
            .lock()
            .map(|value| value.clone())
            .unwrap_or_else(|_| "unavailable: diagnostics lock poisoned".to_string())
    }

    pub fn last_capture(&self) -> Option<LastCapture> {
        self.last_capture
            .lock()
            .ok()
            .and_then(|value| value.clone())
    }

    /// Refuse work that would land while a script is running.
    ///
    /// `stop` never calls this: interrupting the run is the one thing that has
    /// to work while it is in progress.
    fn reject_if_running(&self) -> Result<(), String> {
        if self.running.load(Ordering::SeqCst) {
            return Err(
                "busy: a script is running. Wait for run_script to return, or call stop \
                 to interrupt it."
                    .to_string(),
            );
        }
        Ok(())
    }

    fn send<T>(&self, make: impl FnOnce(Sender<T>) -> Job) -> Result<T, String> {
        self.reject_if_running()?;

        // Clear a stop left over from the last run.
        //
        // Now that the engine watches the shared flag, a `stop` raised while
        // nothing was running would otherwise poison every later call: the very
        // next `find` would return `Interrupted` for no visible reason. Getting
        // here means nothing is running (`reject_if_running` passed), so there
        // is nothing left for that stop to cancel.
        self.interrupt.reset();
        self.send_unchecked(make)
    }

    fn send_unchecked<T>(&self, make: impl FnOnce(Sender<T>) -> Job) -> Result<T, String> {
        let (reply_tx, reply_rx) = channel::<T>();
        self.tx
            .lock()
            .map_err(|_| "the channel to the engine worker is poisoned".to_string())?
            .send(make(reply_tx))
            .map_err(|_| "the engine worker has stopped".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "the engine worker did not reply".to_string())
    }

    pub fn list_windows(&self) -> Result<Vec<WindowInfo>, String> {
        let windows = classify_desktop_error(self.send(Job::ListWindows)?)?;
        if windows.is_empty() && !crate::desktop::input_desktop_accessible().0 {
            return Err(
                "desktop_access_denied: the process cannot open the interactive input desktop"
                    .to_string(),
            );
        }
        Ok(windows)
    }

    pub fn list_windows_filtered(&self, query: WindowQuery) -> Result<Vec<WindowInfo>, String> {
        let mut windows = self.list_windows()?;
        windows.retain(|window| query.matches(window));
        Ok(windows)
    }

    pub fn wait_window(
        &self,
        query: WindowQuery,
        vanish: bool,
        timeout: Duration,
    ) -> Result<WindowWaitResult, String> {
        self.send(|reply| Job::WaitWindow {
            query,
            vanish,
            timeout,
            reply,
        })?
    }

    pub fn capture(&self, scope: Option<String>) -> Result<CapturedFrame, String> {
        classify_desktop_error(self.send(|reply| Job::Capture { scope, reply })?)
    }

    pub fn read_text(&self, scope: Option<String>) -> Result<Vec<TextLine>, String> {
        classify_desktop_error(self.send(|reply| Job::ReadText { scope, reply })?)
    }

    pub fn list_ui(
        &self,
        scope: Option<String>,
        control_type: Option<String>,
    ) -> Result<Vec<UiItem>, String> {
        self.send(|reply| Job::ListUi {
            scope,
            control_type,
            reply,
        })?
    }

    pub fn read_value(&self, locator: String, scope: String) -> Result<UiValue, String> {
        self.send(|reply| Job::ReadValue {
            locator,
            scope,
            reply,
        })?
    }

    pub fn find(
        &self,
        locator: String,
        scope: Option<String>,
        similar: Option<f32>,
        timeout_ms: Option<u64>,
    ) -> Result<Vec<Match>, String> {
        self.send(|reply| Job::Find {
            locator,
            scope,
            similar,
            timeout_ms,
            reply,
        })?
    }

    pub fn act(
        &self,
        locator: String,
        scope: Option<String>,
        action: Action,
    ) -> Result<Match, String> {
        self.send(|reply| Job::Act {
            locator,
            scope,
            action,
            reply,
        })?
    }

    pub fn raw_input(&self, input: RawInput, scope: Option<String>) -> Result<(), String> {
        self.send(|reply| Job::RawInput {
            input,
            scope,
            reply,
        })?
    }

    pub fn check_script(&self, source: String) -> Result<(), String> {
        self.send(|reply| Job::CheckScript { source, reply })?
    }

    pub fn capture_asset(&self, rect: Rect) -> Result<CapturedAsset, String> {
        self.send(|reply| Job::CaptureAsset { rect, reply })?
    }

    /// Run a script under a deadline.
    ///
    /// The timeout is enforced from **this** side, by a watchdog thread that
    /// raises the interrupt. The worker cannot time itself out: it is inside
    /// the script.
    pub fn run_script(&self, source: String, timeout: Duration) -> Result<RunOutcome, String> {
        // Claim the engine before anything else can. `swap` rather than a
        // load-then-store so two concurrent run_script calls cannot both think
        // they won.
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(
                "busy: a script is already running. Call stop to interrupt it.".to_string(),
            );
        }
        if let Ok(mut started) = self.running_since.lock() {
            *started = Some(Instant::now());
        }
        let _release = ReleaseOnDrop {
            running: &self.running,
            running_since: &self.running_since,
        };

        // Never inherit a stop from a previous run.
        self.interrupt.reset();

        let watchdog_interrupt = self.interrupt.clone();
        let (done_tx, done_rx) = channel::<()>();
        let watchdog = std::thread::spawn(move || {
            // Waking on the channel means the run finished first, so no stop is
            // raised. Only a genuine timeout gets here.
            if done_rx.recv_timeout(timeout).is_err() {
                log::warn!("run_script exceeded {timeout:?}, stopping it");
                watchdog_interrupt.stop();
                return true;
            }
            false
        });

        // Past the flag, so `send`'s own busy check would reject this call.
        let interrupt = self.interrupt.clone();
        let mut outcome = self.send_unchecked(|reply| Job::RunScript {
            source,
            interrupt,
            reply,
        })?;

        let _ = done_tx.send(());
        outcome.timed_out = watchdog.join().unwrap_or(false);
        if outcome.timed_out {
            outcome.interrupted = true;
        }
        Ok(outcome)
    }
}

/// Clears the running flag however the run ends, including on an early return.
struct ReleaseOnDrop<'a> {
    running: &'a AtomicBool,
    running_since: &'a Mutex<Option<Instant>>,
}

impl Drop for ReleaseOnDrop<'_> {
    fn drop(&mut self) {
        if let Ok(mut started) = self.running_since.lock() {
            *started = None;
        }
        self.running.store(false, Ordering::SeqCst);
    }
}

fn classify_desktop_error<T>(result: Result<T, String>) -> Result<T, String> {
    result.map_err(|message| {
        let lower = message.to_ascii_lowercase();
        if lower.contains("0x80070005")
            || lower.contains("access is denied")
            || message.contains("アクセスが拒否")
        {
            format!("desktop_access_denied: {message}")
        } else {
            message
        }
    })
}

/// Defaults for a server driven by an agent.
///
/// Failure artifacts land under the base directory rather than a temp folder.
/// They are the agent's evidence for its next attempt, and an agent that cannot
/// find them writes the same broken script again.
fn mcp_settings(base: &Path) -> Settings {
    Settings {
        artifact_dir: Some(base.join(".mekiki").join("artifacts")),
        ..Default::default()
    }
}

fn handle(
    engine: &mut Option<Mekiki>,
    assets: &mut AssetStore,
    base: &Path,
    capture_diagnostics: &Arc<Mutex<String>>,
    last_capture: &Arc<Mutex<Option<LastCapture>>>,
    job: Job,
) {
    match job {
        Job::ListWindows(reply) => {
            let _ = reply.send(mekiki_core::window_list().map_err(|e| e.to_string()));
        }
        Job::WaitWindow {
            query,
            vanish,
            timeout,
            reply,
        } => {
            let _ = reply.send(wait_for_window(&query, vanish, timeout));
        }
        Job::Capture { scope, reply } => {
            let started = Instant::now();
            let result = with_engine(engine, |m| capture(m, scope.as_deref()));
            if let Ok(frame) = &result {
                // capture_diagnostics is dynamic: a DXGI failure can switch one
                // display to GDI during this very capture. Refresh it after the
                // frame, rather than reusing the startup capability string.
                let actual_backend = engine
                    .as_ref()
                    .map(Mekiki::capture_diagnostics)
                    .unwrap_or_else(|| "unknown".into());
                if let Ok(mut diagnostics) = capture_diagnostics.lock() {
                    *diagnostics = actual_backend.clone();
                }
                if let Ok(mut state) = last_capture.lock() {
                    *state = Some(LastCapture {
                        timestamp_ms: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                        width: frame.width,
                        height: frame.height,
                        origin: frame.origin,
                        backend: actual_backend,
                    });
                }
            }
            let _ = reply.send(result);
        }
        Job::ReadText { scope, reply } => {
            let _ = reply.send(with_engine(engine, |m| {
                let region = resolve::scope_region(m, scope.as_deref())?;
                m.read_text(region).map_err(|e| e.to_string())
            }));
        }
        Job::ListUi {
            scope,
            control_type,
            reply,
        } => {
            let _ = reply.send(with_engine(engine, |m| {
                let region = resolve::scope_region(m, scope.as_deref())?;
                m.list_ui(region, control_type.as_deref())
                    .map_err(|e| e.to_string())
            }));
        }
        Job::ReadValue {
            locator,
            scope,
            reply,
        } => {
            let _ = reply.send(with_engine(engine, |m| {
                let region = resolve::scope_region(m, Some(&scope))?;
                let Locator::Ui(query) = locator::parse(&locator).map_err(|e| e.to_string())?
                else {
                    return Err("read_value accepts only a ui: locator".to_string());
                };
                let pattern = resolve::ui_pattern(m, &query);
                m.read_ui_value(region, &pattern).map_err(|e| e.to_string())
            }));
        }
        Job::Find {
            locator,
            scope,
            similar,
            timeout_ms,
            reply,
        } => {
            let _ = reply.send(with_engine(engine, |m| {
                find(m, assets, &locator, scope.as_deref(), similar, timeout_ms)
            }));
        }
        Job::Act {
            locator,
            scope,
            action,
            reply,
        } => {
            let _ = reply.send(with_engine(engine, |m| {
                act(m, assets, &locator, scope.as_deref(), action)
            }));
        }
        Job::RawInput {
            input,
            scope,
            reply,
        } => {
            let _ = reply.send(with_engine(engine, |m| {
                raw_input(m, &input, scope.as_deref())
            }));
        }
        Job::CheckScript { source, reply } => {
            let _ = reply.send(check_script(&source));
        }
        Job::RunScript {
            source,
            interrupt,
            reply,
        } => {
            let _ = reply.send(run_script(engine, base, &source, interrupt));
        }
        Job::CaptureAsset { rect, reply } => {
            let _ = reply.send(with_engine(engine, |m| capture_asset(m, assets, rect)));
        }
    }
}

fn with_engine<T>(
    engine: &mut Option<Mekiki>,
    f: impl FnOnce(&mut Mekiki) -> Result<T, String>,
) -> Result<T, String> {
    match engine {
        Some(m) => f(m),
        None => Err(
            "the engine could not be initialised (a GPU or screen capture problem). \
             Check the server's stderr log."
                .to_string(),
        ),
    }
}

fn capture(mekiki: &mut Mekiki, scope: Option<&str>) -> Result<CapturedFrame, String> {
    let region = resolve::scope_region(mekiki, scope)?;
    let frame = mekiki.capture_region(region).map_err(|e| e.to_string())?;
    Ok(CapturedFrame {
        bgra: frame.bgra,
        width: frame.width,
        height: frame.height,
        origin: frame.origin,
    })
}

/// Search without acting.
///
/// **Finding nothing is not an error.** `resolve_all` reports a miss as
/// `FindFailed` because a script that cannot find its button should stop; an
/// agent asking "is this here" wants an empty list and a chance to try a
/// different locator.
fn find(
    mekiki: &mut Mekiki,
    assets: &mut AssetStore,
    locator: &str,
    scope: Option<&str>,
    similar: Option<f32>,
    timeout_ms: Option<u64>,
) -> Result<Vec<Match>, String> {
    // A window locator denotes something whose existence can change. Resolve
    // it inside the timeout loop instead of resolving once up front (which
    // used to make `find(window:...)` fail immediately before timeout applied).
    if let Locator::Window(query) = locator::parse(locator).map_err(|e| e.to_string())? {
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(0));
        let result = wait_for_window(&query, false, timeout)?;
        return Ok(result
            .window
            .map(|window| vec![point_match(mekiki, window.bounds)])
            .unwrap_or_default());
    }
    let region = resolve::scope_region(mekiki, scope)?;
    let resolved = resolve::resolve(mekiki, assets, region, locator)?;

    match resolved {
        Resolved::Target(target) => {
            let mut target = *target;
            if let Some(s) = similar {
                target = target.similar(s);
            }
            // Zero by default: one scan, no waiting. An agent exploring wants an
            // answer now, and can ask for a wait when it means to.
            target = target.timeout(Duration::from_millis(timeout_ms.unwrap_or(0)));

            match mekiki.on(&target).resolve_all() {
                Ok(matches) => Ok(matches),
                Err(mekiki_core::Error::NotFound(_)) => Ok(Vec::new()),
                Err(e) => Err(e.to_string()),
            }
        }
        // A place does not need finding; report it as found where it is.
        Resolved::Point(x, y) => Ok(vec![point_match(mekiki, Rect::new(x, y, 1, 1))]),
        Resolved::Rect(r) => Ok(vec![point_match(mekiki, r.rect)]),
    }
}

/// The one window polling primitive used by MCP find, wait_window and launch.
fn wait_for_window(
    query: &WindowQuery,
    vanish: bool,
    timeout: Duration,
) -> Result<WindowWaitResult, String> {
    let started = Instant::now();
    loop {
        let found = mekiki_core::window_list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|window| query.matches(window))
            .nth(query.index);
        if (!vanish && found.is_some()) || (vanish && found.is_none()) {
            return Ok(WindowWaitResult {
                matched: true,
                window: found,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        }
        if started.elapsed() >= timeout {
            return Ok(WindowWaitResult {
                matched: false,
                window: found,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Present a fixed place as a match, so callers get one shape back.
fn point_match(mekiki: &Mekiki, rect: Rect) -> Match {
    Match {
        rect,
        display: mekiki.region(rect).display,
        score: 1.0,
        target_offset: (0, 0),
    }
}

fn act(
    mekiki: &mut Mekiki,
    assets: &mut AssetStore,
    locator: &str,
    scope: Option<&str>,
    action: Action,
) -> Result<Match, String> {
    let region = resolve::scope_region(mekiki, scope)?;
    let resolved = resolve::resolve(mekiki, assets, region, locator)?;

    match resolved {
        Resolved::Target(target) => {
            let target = *target;
            let act = mekiki.on(&target);
            match action {
                Action::Click {
                    right: false,
                    double: false,
                } => act.click(),
                Action::Click {
                    right: true,
                    double: false,
                } => act.right_click(),
                Action::Click { double: true, .. } => act.double_click(),
                Action::Hover => act.hover(),
            }
            .map_err(|e| e.to_string())
        }
        Resolved::Point(x, y) => act_at(mekiki, Rect::new(x, y, 1, 1), action),
        Resolved::Rect(r) => act_at(mekiki, r.rect, action),
    }
}

/// Act on a fixed place, bypassing the search but not the input path.
fn act_at(mekiki: &mut Mekiki, rect: Rect, action: Action) -> Result<Match, String> {
    let m = point_match(mekiki, rect);
    let point = m.target();
    match action {
        Action::Click {
            right: false,
            double: false,
        } => mekiki.click(point),
        Action::Click {
            right: true,
            double: false,
        } => mekiki.right_click(point),
        Action::Click { double: true, .. } => mekiki.double_click(point),
        Action::Hover => mekiki.hover(point),
    }
    .map_err(|e| e.to_string())?;
    Ok(m)
}

/// Send input that is not aimed at a located target.
///
/// With a `scope`, the window is brought to the front first. Without one the
/// input goes wherever focus already is — which is how, during the first agent
/// test, text meant for one application landed in another. Naming the window is
/// the difference between "type this" and "type this *there*".
///
/// Since the third round, `None` no longer means "the agent forgot": the
/// server requires `scope` on every raw-input call and only passes `None` when
/// the agent wrote `focused` explicitly (`MekikiServer::raw_target`).
///
/// Only a window can be a scope here. A `region:` says where to look, not what
/// has the keyboard, so accepting one would suggest a targeting that does not
/// exist.
fn raw_input(mekiki: &mut Mekiki, input: &RawInput, scope: Option<&str>) -> Result<(), String> {
    if let Some(spec) = scope.map(str::trim).filter(|s| !s.is_empty()) {
        let Locator::Window(query) = locator::parse(spec).map_err(|e| e.to_string())? else {
            return Err(format!(
                "'{spec}' cannot receive input. Use window:<title> or window:exe=<name>; \
                 a region does not have keyboard focus"
            ));
        };
        // The engine's error already reads "cannot bring '...' to the front",
        // so pass it through rather than wrapping it in the same phrase again.
        // A hint about the usual cause is worth adding — but only on the
        // failure it explains: Windows refuses SetForegroundWindow to a
        // process that neither owns the foreground nor recently injected
        // input, and it is intermittent. Glued onto "no window matches",
        // "retrying often works" sends an agent retrying a window that will
        // never exist; a modal-dialog refusal already names its own fix.
        mekiki.activate_window(&query).map_err(|e| {
            if matches!(
                &e,
                mekiki_core::Error::Capture(mekiki_core::CaptureError::ActivateFailed(_))
            ) {
                format!(
                    "{e} (Windows can refuse this when another app owns the foreground; \
                     retrying often works)"
                )
            } else {
                e.to_string()
            }
        })?;
    }

    match input {
        RawInput::TypeText(text) => mekiki.type_text(text).map_err(|e| e.to_string()),
        RawInput::Press(combo) => {
            let (key, modifiers) = mekiki_scripting::keys::parse_combo(combo)?;
            mekiki.key_press(key, modifiers).map_err(|e| e.to_string())
        }
        RawInput::Scroll { h, v } => mekiki.scroll(*h, *v).map_err(|e| e.to_string()),
    }
}

/// Compile without running.
///
/// Compilation needs no engine, which is what makes this cheap enough to be the
/// inner loop of writing a script.
fn check_script(source: &str) -> Result<(), String> {
    rhai::Engine::new()
        .compile(source)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn capture_asset(
    mekiki: &mut Mekiki,
    assets: &mut AssetStore,
    rect: Rect,
) -> Result<CapturedAsset, String> {
    let region = mekiki.region(rect);
    let frame = mekiki.capture_region(region).map_err(|e| e.to_string())?;

    let png = crate::imaging::frame_to_png(&frame.bgra, frame.width, frame.height)?;
    let reference = assets.import_bytes(&png).map_err(|e| e.to_string())?;

    // Importing invalidates nothing by itself, but the pattern cache is keyed by
    // path, and a re-captured asset under the same reference must not serve the
    // old pyramid.
    assets.clear_cache();

    Ok(CapturedAsset {
        reference: format!("image:{reference}"),
        png,
        width: frame.width,
        height: frame.height,
    })
}

/// Run a script, taking the engine out and putting it back.
///
/// The engine has to move into the `ScriptHost` because the scripting layer
/// owns it for the duration. If the host cannot give it back — a Rhai closure
/// still holds a reference — the slot is left empty and the next call rebuilds
/// it rather than failing forever.
///
/// **No deadline is enforced here.** This function is inside the script, so it
/// cannot time itself out; [`EngineHandle::run_script`] watches the clock from
/// outside and raises the interrupt.
///
/// `interrupt` **must** be the handle's flag, handed down through the job. It
/// is what makes the timeout, the `stop` tool and the emergency hotkey real:
/// reaching for `mekiki.interrupt()` here instead wires the script to a flag
/// nobody raises, and a runaway script then cannot be stopped by anything short
/// of killing the process.
fn run_script(
    engine: &mut Option<Mekiki>,
    base: &Path,
    source: &str,
    interrupt: Interrupt,
) -> RunOutcome {
    let started = Instant::now();

    let Some(mekiki) = engine.take() else {
        return RunOutcome {
            ok: false,
            error: Some(
                "the engine could not be initialised (a GPU or screen capture problem)".to_string(),
            ),
            ..Default::default()
        };
    };

    let mut host = ScriptHost::from_runtime(Runtime::new(mekiki, AssetStore::new(base)));
    // This installs the flag in two places at once: Rhai's `on_progress`, which
    // catches a loop that never touches the engine, and the engine itself, which
    // catches a long wait inside one call.
    host.set_interrupt(interrupt.clone());

    // `print` is how a script talks back. Collect it rather than letting it
    // reach stdout, which carries JSON-RPC and nothing else.
    let collected = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    host.capture_output(collected.clone());

    let result = host.run(source);
    let output = collected.borrow().clone();
    *engine = host.into_mekiki();

    if engine.is_none() {
        log::warn!(
            "the engine was not returned after the run; it will be rebuilt on the next call"
        );
    }

    // Interruption is decided by the **flag**, not the error type. The ways out
    // of a stop differ by path (Rhai's ErrorTerminated, core's
    // Error::Interrupted, or a clean finish when `sleep` was the last
    // statement), so the error alone would miss some of them.
    let interrupted = interrupt.is_stopping();

    RunOutcome {
        ok: result.is_ok() && !interrupted,
        output,
        error: result.err().map(|e| e.to_string()),
        interrupted,
        timed_out: false,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

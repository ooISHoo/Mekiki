//! Tauri backend for the Mekiki IDE.
//!
//! Design plan phase 3 links the core engine **directly**, without IPC. Crossing
//! a process boundary would add material overhead to repeated operations such
//! as live match preview.

// Public so examples/worker_probe.rs can isolate the worker without Tauri and
// distinguish engine failures from IPC failures.
pub mod engine;
pub mod run_indicator;

use std::path::PathBuf;

use engine::{CapturedImage, EngineHandle, ImageResource, PreviewResult, RunResult, WindowSummary};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

/// IDE crate version from `Cargo.toml`.
///
/// The product version used by installers and `package_info` is canonical in
/// `tauri.conf.json`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize)]
struct AppVersions {
    engine: String,
    ide: String,
}

/// Image literal returned to the frontend.
#[derive(Serialize)]
pub struct ImageLiteralDto {
    /// Start offset in UTF-16 code units, ready for CodeMirror.
    pub from: usize,
    pub to: usize,
    pub raw: String,
    pub reference: String,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Find image literals in a script.
///
/// Rhai tokenization, rather than a regular expression, excludes image paths
/// in comments and unrelated string assignments.
#[tauri::command]
fn detect_image_literals(source: String) -> Vec<ImageLiteralDto> {
    mekiki_scripting::scan::image_literals(&source)
        .into_iter()
        .map(|l| ImageLiteralDto {
            from: l.from,
            to: l.to,
            raw: l.raw,
            reference: l.reference,
        })
        .collect()
}

/// List visible windows.
///
/// Enumeration is fast, but the first call waits roughly 500 ms for GPU and
/// capture initialization. There is no reason to block the UI thread.
#[tauri::command]
async fn list_windows(app: tauri::AppHandle) -> Result<Vec<WindowSummary>, String> {
    off_ui(move || engine_of(&app).list_windows()).await
}

/// Stop the running script.
///
/// This bypasses the job queue so it remains available while a script occupies
/// the worker.
///
/// These control commands may remain synchronous because they only touch atomic
/// state and do not block the UI thread. Synchronous dispatch is more responsive,
/// provided that expensive commands use [`off_ui`].
#[tauri::command]
fn stop_script(engine: State<'_, EngineHandle>) {
    engine.stop();
}

/// Pause the entire engine until resumed.
#[tauri::command]
fn pause_script(engine: State<'_, EngineHandle>) {
    engine.pause();
}

#[tauri::command]
fn resume_script(engine: State<'_, EngineHandle>) {
    engine.resume();
}

/// Execution status used for UI state and line highlighting.
#[derive(Serialize)]
pub struct RunStatus {
    pub state: String,
    /// Current one-based line number, or zero when unknown.
    pub line: u32,
    pub stepping: bool,
}

/// Return the current execution state and line.
///
/// This bypasses the job queue and remains responsive during script execution.
/// The frontend polls it only while running to update the line highlight.
#[tauri::command]
fn run_status(engine: State<'_, EngineHandle>) -> RunStatus {
    RunStatus {
        state: match engine.run_state() {
            mekiki_core::RunState::Running => "running",
            mekiki_core::RunState::Paused => "paused",
            mekiki_core::RunState::Stopping => "stopping",
        }
        .to_string(),
        line: engine.current_line(),
        stepping: engine.is_stepping(),
    }
}

/// Reflect the execution state on the taskbar button.
///
/// The frontend calls this from `setRunState`, which every transition passes
/// through (start, pause, resume, stop, completion, emergency stop). See
/// [`run_indicator`] for the presentation.
#[tauri::command]
fn set_run_indicator(
    window: tauri::Window,
    state: run_indicator::RunIndicator,
) -> Result<(), String> {
    run_indicator::apply(&window, state).map_err(|e| e.to_string())
}

/// Enable or disable statement-by-statement execution, including while running.
#[tauri::command]
fn set_stepping(engine: State<'_, EngineHandle>, enabled: bool) {
    engine.set_stepping(enabled);
}

/// Advance step execution by one statement.
#[tauri::command]
fn step_next(engine: State<'_, EngineHandle>) {
    engine.step_next();
}

/// Run expensive work outside the UI thread.
///
/// # Why this is required
///
/// **Tauri executes synchronous commands directly inside the IPC handler.**
/// `tauri-macros` calls `$path(...)` from `body_blocking`, which runs on the UI
/// thread. A command that takes seconds or minutes would freeze the window and
/// even prevent the Stop command from being dispatched.
///
/// Make the command async, then route blocking work through this helper. Merely
/// declaring an async command moves it off the UI thread, but running blocking
/// work directly there would occupy an async executor worker.
async fn off_ui<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("Background task failed: {e}"))?
}

/// Retrieve the managed [`EngineHandle`] with a `'static` lifetime.
///
/// Borrowed `State<'_, _>` cannot enter `spawn_blocking`. `AppHandle` is cloned
/// so the closure can retrieve state again.
fn engine_of(app: &tauri::AppHandle) -> tauri::State<'_, EngineHandle> {
    use tauri::Manager as _;
    app.state::<EngineHandle>()
}

/// List images under the working directory for the asset pane.
#[tauri::command]
async fn list_images(
    app: tauri::AppHandle,
    base_dir: String,
) -> Result<Vec<ImageResource>, String> {
    off_ui(move || engine_of(&app).list_images(PathBuf::from(base_dir))).await
}

/// Rename an image without moving it to another directory.
///
/// Export a stored image under the working directory and return its new reference.
#[tauri::command]
fn rename_image(base_dir: String, reference: String, new_name: String) -> Result<String, String> {
    engine::rename_image(&PathBuf::from(base_dir), &reference, &new_name)
}

/// Delete an image inside the working directory or asset store.
#[tauri::command]
fn delete_image(base_dir: String, reference: String) -> Result<(), String> {
    engine::delete_image(&PathBuf::from(base_dir), &reference)
}

#[tauri::command]
async fn capture_rect(
    app: tauri::AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<CapturedImage, String> {
    if width == 0 || height == 0 {
        return Err("Width and height must be greater than zero.".into());
    }
    off_ui(move || engine_of(&app).capture_rect(mekiki_core::Rect::new(x, y, width, height))).await
}

/// Resolve an image reference to a thumbnail data URL.
#[tauri::command]
async fn resolve_image(
    app: tauri::AppHandle,
    base_dir: String,
    reference: String,
) -> Result<CapturedImage, String> {
    off_ui(move || engine_of(&app).resolve_image(PathBuf::from(base_dir), reference)).await
}

/// Import a clipboard image into the asset store and return its reference.
///
/// This is the import path for captures made with Snipping Tool (`Win+Shift+S`).
#[tauri::command]
async fn import_clipboard(app: tauri::AppHandle, base_dir: String) -> Result<String, String> {
    off_ui(move || {
        let png = engine::clipboard_png()?;
        engine_of(&app).import_image(PathBuf::from(base_dir), png)
    })
    .await
}

/// Capture a rectangle and import it into the asset store.
#[tauri::command]
async fn capture_to_asset(
    app: tauri::AppHandle,
    base_dir: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<String, String> {
    off_ui(move || {
        let engine = engine_of(&app);
        let captured = engine.capture_rect(mekiki_core::Rect::new(x, y, width, height))?;
        let png = decode_data_url(&captured.data_url)?;
        engine.import_image(PathBuf::from(base_dir), png)
    })
    .await
}

/// Open the operating system's screen capture UI.
///
/// The result is placed on the clipboard and can then be imported with
/// `import_clipboard`. Reusing the familiar system UI is preferable to a custom
/// capture interface.
#[tauri::command]
fn open_snipping_tool() -> Result<(), String> {
    // `ms-screenclip:` opens the same capture UI as Win+Shift+S.
    std::process::Command::new("cmd")
        .args(["/C", "start", "", "ms-screenclip:"])
        .spawn()
        .map_err(|e| format!("Could not open Snipping Tool: {e}"))?;
    Ok(())
}

/// Return where an image currently matches on screen.
#[tauri::command]
async fn preview_match(
    app: tauri::AppHandle,
    base_dir: String,
    reference: String,
    similarity: f64,
) -> Result<PreviewResult, String> {
    off_ui(move || {
        engine_of(&app).preview(
            PathBuf::from(base_dir),
            reference,
            similarity.clamp(0.0, 1.0) as f32,
        )
    })
    .await
}

/// Run a script.
///
/// This **must remain async**. A synchronous command runs on the UI thread,
/// freezing the window and Stop button until the script exits. See [`off_ui`].
#[tauri::command]
async fn run_script(
    app: tauri::AppHandle,
    base_dir: String,
    source: String,
) -> Result<RunResult, String> {
    off_ui(move || engine_of(&app).run_script(PathBuf::from(base_dir), source)).await
}

#[tauri::command]
fn read_script(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))
}

#[tauri::command]
fn write_script(path: String, source: String) -> Result<(), String> {
    std::fs::write(&path, source).map_err(|e| format!("{path}: {e}"))
}

/// Return the default working directory.
///
/// Use the operating system's Documents directory rather than the executable
/// directory. Tauri resolves this to Documents on Windows, `~/Documents` on
/// macOS, and `XDG_DOCUMENTS_DIR` on Linux. Fall back to the home directory,
/// then the current directory.
#[tauri::command]
fn default_base_dir(app: AppHandle) -> String {
    app.path()
        .document_dir()
        .or_else(|_| app.path().home_dir())
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .to_string_lossy()
        .into_owned()
}

#[derive(Serialize)]
pub struct StartupScript {
    pub path: String,
    pub source: String,
    pub base_dir: String,
}

/// Return version information shown in the output pane at startup.
///
/// Release builds have no console (`windows_subsystem = "windows"`), so
/// `log::info!` alone cannot present this information to users.
#[tauri::command]
fn app_versions(app: AppHandle) -> AppVersions {
    AppVersions {
        engine: mekiki_core::VERSION.to_string(),
        ide: app.package_info().version.to_string(),
    }
}

/// Open the script passed on the command line.
///
/// Supporting `mekiki-ide script.rhai` also enables file associations.
#[tauri::command]
fn startup_script() -> Option<StartupScript> {
    let arg = std::env::args().nth(1)?;
    let path = PathBuf::from(&arg);
    if !path.is_file() {
        return None;
    }
    let source = std::fs::read_to_string(&path).ok()?;
    // Match the CLI rule: resolve relative assets from the script directory.
    let base_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());

    Some(StartupScript {
        path: path.to_string_lossy().into_owned(),
        source,
        base_dir,
    })
}

fn decode_data_url(data_url: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let encoded = data_url.split_once(",").ok_or("Invalid data URL.")?.1;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Emergency stop shortcut
// ---------------------------------------------------------------------------

/// Global shortcut that stops a script.
///
/// The combination follows SikuliX. It deliberately works when the IDE is not
/// focused, allowing runaway automation to be stopped even when it controls the
/// pointer and prevents access to the Stop button.
const STOP_HOTKEY: &str = "Shift+Alt+C";

/// Register the shortcut only while a script is running.
///
/// Do not reserve it permanently. Global shortcuts are shared OS resources and
/// prevent other applications from using the combination while registered.
#[tauri::command]
fn arm_stop_hotkey(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let shortcut = app.global_shortcut();
    // Avoid duplicate registration and recover from a failed prior unregister.
    if shortcut.is_registered(STOP_HOTKEY) {
        return Ok(());
    }
    shortcut.register(STOP_HOTKEY).map_err(|e| e.to_string())
}

#[tauri::command]
fn disarm_stop_hotkey(app: tauri::AppHandle) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    // Unregister failure does not change the execution result.
    let _ = app.global_shortcut().unregister(STOP_HOTKEY);
}

/// Display form of the emergency shortcut for UI messages.
#[tauri::command]
fn stop_hotkey_label() -> String {
    STOP_HOTKEY.to_string()
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    use tauri::Manager as _;
                    use tauri_plugin_global_shortcut::ShortcutState;

                    // Handle only the press event; release produces another
                    // event and would otherwise stop twice.
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    log::info!("Emergency stop shortcut triggered");
                    // This only sets a flag, so it works while the worker is busy.
                    app.state::<EngineHandle>().stop();
                })
                .build(),
        )
        .manage(EngineHandle::spawn())
        .setup(|app| {
            // Engine version comes from the workspace; IDE version comes from
            // tauri.conf.json.
            log::info!("Mekiki engine {}", mekiki_core::VERSION);
            log::info!("Mekiki-IDE {}", app.package_info().version);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            detect_image_literals,
            list_windows,
            list_images,
            rename_image,
            delete_image,
            capture_rect,
            capture_to_asset,
            resolve_image,
            import_clipboard,
            open_snipping_tool,
            preview_match,
            run_script,
            stop_script,
            pause_script,
            resume_script,
            run_status,
            set_run_indicator,
            set_stepping,
            step_next,
            arm_stop_hotkey,
            disarm_stop_hotkey,
            stop_hotkey_label,
            read_script,
            write_script,
            default_base_dir,
            startup_script,
            app_versions,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start the Tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_literals_through_the_command() {
        let found = detect_image_literals(
            r#"
            // "comment.png"
            let p = "assigned.png";
            click("ok.png");
        "#
            .to_string(),
        );
        let refs: Vec<&str> = found.iter().map(|l| l.reference.as_str()).collect();
        assert_eq!(refs, vec!["ok.png"]);
    }

    #[test]
    fn data_url_roundtrip() {
        use base64::Engine as _;
        let payload = b"hello";
        let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
        let url = format!("data:image/png;base64,{encoded}");
        assert_eq!(decode_data_url(&url).unwrap(), payload);
    }

    #[test]
    fn malformed_data_url_is_rejected() {
        assert!(decode_data_url("not-a-data-url").is_err());
    }
}

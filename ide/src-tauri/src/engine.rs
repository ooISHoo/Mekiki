//! Worker thread that owns the core engine.
//!
//! # Why a dedicated thread
//!
//! There are two reasons:
//!
//! 1. **State must persist.** GPU devices, capture sessions, and pattern caches
//!    are expensive to create. Rebuilding them for every command would make
//!    repeated operations such as live preview unusable.
//! 2. **The runtime is not `Send`.** The scripting layer uses
//!    `Rc<RefCell<Runtime>>`, so it must stay on one thread and be accessed over
//!    channels.
//!
//! This also keeps the UI responsive because searches can take tens of
//! milliseconds.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use mekiki_core::{Interrupt, Mekiki, Rect, RunState, Settings};
use mekiki_scripting::assets::AssetStore;
use mekiki_scripting::step::StepControl;
use mekiki_scripting::{ScriptHost, api::Runtime};
use serde::Serialize;

/// Work sent to the engine thread, with a channel for the response.
enum Job {
    ListWindows(Sender<Result<Vec<WindowSummary>, String>>),
    CaptureRect {
        rect: Rect,
        reply: Sender<Result<CapturedImage, String>>,
    },
    /// Resolve an image reference to a PNG data URL for thumbnails.
    ResolveImage {
        base_dir: PathBuf,
        reference: String,
        reply: Sender<Result<CapturedImage, String>>,
    },
    /// List image assets under the working directory.
    ListImages {
        base_dir: PathBuf,
        reply: Sender<Result<Vec<ImageResource>, String>>,
    },
    /// Import an image into the asset store.
    ImportImage {
        base_dir: PathBuf,
        png: Vec<u8>,
        reply: Sender<Result<String, String>>,
    },
    /// Find current on-screen matches.
    Preview {
        base_dir: PathBuf,
        reference: String,
        similarity: f32,
        reply: Sender<Result<PreviewResult, String>>,
    },
    RunScript {
        base_dir: PathBuf,
        source: String,
        interrupt: Interrupt,
        step: StepControl,
        reply: Sender<RunResult>,
    },
}

#[derive(Serialize, Clone)]
pub struct WindowSummary {
    pub title: String,
    /// Executable name, preferred for insertion because it is more stable than
    /// a window title.
    pub exe: String,
    pub class_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub z_order: usize,
    /// Application icon as a PNG data URL, or empty when unavailable.
    pub icon: String,
}

#[derive(Serialize, Clone)]
pub struct CapturedImage {
    /// `data:image/png;base64,...`
    pub data_url: String,
    pub width: u32,
    pub height: u32,
}

/// One image shown in the asset pane.
#[derive(Serialize, Clone)]
pub struct ImageResource {
    /// Reference written to scripts: a path relative to the working directory
    /// for files, or `sha256:...` for stored content.
    pub reference: String,
    /// Short display name.
    pub name: String,
    /// Original dimensions, displayed to help distinguish assets.
    pub width: u32,
    pub height: u32,
    /// Thumbnail as a PNG data URL.
    pub thumbnail: String,
    /// Whether the image belongs to the content-addressed store.
    pub in_store: bool,
}

#[derive(Serialize, Clone)]
pub struct PreviewMatch {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub score: f32,
}

#[derive(Serialize, Clone)]
pub struct PreviewResult {
    pub matches: Vec<PreviewMatch>,
}

#[derive(Serialize, Clone, Default)]
pub struct RunResult {
    pub ok: bool,
    /// Output captured from script `print` calls.
    pub output: Vec<String>,
    pub error: Option<String>,
    /// Whether the user stopped execution; displayed separately from failure.
    pub interrupted: bool,
    pub elapsed_ms: u64,
}

/// Interface to the engine worker.
pub struct EngineHandle {
    tx: Mutex<Sender<Job>>,
    /// Stop and pause control shared outside the job queue.
    ///
    /// This deliberately bypasses the queue. While a script occupies the worker,
    /// queued control commands would not arrive until execution had ended.
    interrupt: Interrupt,
    /// Step control and current line, shared for the same reason as `interrupt`.
    step: StepControl,
}

impl EngineHandle {
    /// Spawn the engine worker thread.
    ///
    /// GPU and capture initialization happens on that thread. The IDE still
    /// starts when initialization fails so it remains usable as an editor and
    /// can present the failure reason.
    pub fn spawn() -> Self {
        let (tx, rx) = channel::<Job>();

        std::thread::Builder::new()
            .name("mekiki-engine".into())
            .spawn(move || {
                let mut engine = match Mekiki::with_settings(ide_settings()) {
                    Ok(m) => {
                        log::info!("Engine initialized: {}", m.capture_diagnostics());
                        Some(m)
                    }
                    Err(e) => {
                        log::error!("Failed to initialize engine: {e}");
                        None
                    }
                };

                while let Ok(job) = rx.recv() {
                    handle(&mut engine, job);
                }
            })
            .expect("failed to spawn the engine worker thread");

        Self {
            tx: Mutex::new(tx),
            interrupt: Interrupt::new(),
            step: StepControl::new(),
        }
    }

    /// Enable or disable statement stepping, including while running.
    pub fn set_stepping(&self, enabled: bool) {
        self.step.set_stepping(enabled);
    }

    pub fn is_stepping(&self) -> bool {
        self.step.is_stepping()
    }

    /// Advance step execution by one statement.
    pub fn step_next(&self) {
        self.step.advance();
    }

    /// Current one-based line number, or zero when unknown.
    pub fn current_line(&self) -> u32 {
        self.step.line()
    }

    /// Stop the running script.
    pub fn stop(&self) {
        self.interrupt.stop();
    }

    /// Pause script execution.
    ///
    /// This pauses the worker itself, so other engine operations such as match
    /// preview and asset listing are unavailable. The IDE disables those controls
    /// while paused.
    pub fn pause(&self) {
        self.interrupt.pause();
    }

    pub fn resume(&self) {
        self.interrupt.resume();
    }

    pub fn run_state(&self) -> RunState {
        self.interrupt.state()
    }

    fn send<T>(&self, make: impl FnOnce(Sender<T>) -> Job) -> Result<T, String> {
        let (reply_tx, reply_rx) = channel::<T>();
        self.tx
            .lock()
            .map_err(|_| "Could not access the engine worker queue.".to_string())?
            .send(make(reply_tx))
            .map_err(|_| "The engine worker has stopped.".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "The engine worker closed without returning a result.".to_string())
    }

    pub fn list_windows(&self) -> Result<Vec<WindowSummary>, String> {
        self.send(Job::ListWindows)?
    }

    pub fn capture_rect(&self, rect: Rect) -> Result<CapturedImage, String> {
        self.send(|reply| Job::CaptureRect { rect, reply })?
    }

    pub fn resolve_image(
        &self,
        base_dir: PathBuf,
        reference: String,
    ) -> Result<CapturedImage, String> {
        self.send(|reply| Job::ResolveImage {
            base_dir,
            reference,
            reply,
        })?
    }

    pub fn list_images(&self, base_dir: PathBuf) -> Result<Vec<ImageResource>, String> {
        self.send(|reply| Job::ListImages { base_dir, reply })?
    }

    pub fn import_image(&self, base_dir: PathBuf, png: Vec<u8>) -> Result<String, String> {
        self.send(|reply| Job::ImportImage {
            base_dir,
            png,
            reply,
        })?
    }

    pub fn preview(
        &self,
        base_dir: PathBuf,
        reference: String,
        similarity: f32,
    ) -> Result<PreviewResult, String> {
        self.send(|reply| Job::Preview {
            base_dir,
            reference,
            similarity,
            reply,
        })?
    }

    pub fn run_script(&self, base_dir: PathBuf, source: String) -> Result<RunResult, String> {
        // Never carry a previous stop request or line number into a new run.
        self.interrupt.reset();
        self.step.reset();
        let interrupt = self.interrupt.clone();
        let step = self.step.clone();
        self.send(|reply| Job::RunScript {
            base_dir,
            source,
            interrupt,
            step,
            reply,
        })
    }
}

/// Default settings for the IDE.
fn ide_settings() -> Settings {
    Settings {
        // Keep diagnostics in temporary storage instead of modifying the
        // directory being edited.
        artifact_dir: Some(std::env::temp_dir().join("mekiki-artifacts")),
        ..Default::default()
    }
}

fn handle(engine: &mut Option<Mekiki>, job: Job) {
    match job {
        Job::ListWindows(reply) => {
            let _ = reply.send(list_windows());
        }
        Job::CaptureRect { rect, reply } => {
            let _ = reply.send(with_engine(engine, |m| capture_rect(m, rect)));
        }
        Job::ResolveImage {
            base_dir,
            reference,
            reply,
        } => {
            let _ = reply.send(resolve_image(&base_dir, &reference));
        }
        Job::ListImages { base_dir, reply } => {
            let _ = reply.send(list_images(&base_dir));
        }
        Job::ImportImage {
            base_dir,
            png,
            reply,
        } => {
            let store = AssetStore::new(&base_dir);
            let _ = reply.send(store.import_bytes(&png).map_err(|e| e.to_string()));
        }
        Job::Preview {
            base_dir,
            reference,
            similarity,
            reply,
        } => {
            let _ = reply.send(with_engine(engine, |m| {
                preview(m, &base_dir, &reference, similarity)
            }));
        }
        Job::RunScript {
            base_dir,
            source,
            interrupt,
            step,
            reply,
        } => {
            // Reuse the worker-owned engine. Creating another one here conflicts
            // with the DXGI duplication held by match preview and produces
            // `0x80070057` (invalid parameter).
            let result = match engine.take() {
                Some(m) => {
                    let (m, result) = run_script(m, &base_dir, &source, interrupt, step);
                    *engine = m.or_else(|| match Mekiki::with_settings(ide_settings()) {
                        Ok(rebuilt) => {
                            log::warn!(
                                "Recreated the engine after script execution failed to return it"
                            );
                            Some(rebuilt)
                        }
                        Err(e) => {
                            log::error!("Failed to recreate engine: {e}");
                            None
                        }
                    });
                    result
                }
                None => RunResult {
                    ok: false,
                    error: Some(
                        "The engine is unavailable. Check GPU and screen capture support.".into(),
                    ),
                    ..Default::default()
                },
            };
            let _ = reply.send(result);
        }
    }
}

fn with_engine<T>(
    engine: &mut Option<Mekiki>,
    f: impl FnOnce(&mut Mekiki) -> Result<T, String>,
) -> Result<T, String> {
    match engine {
        Some(m) => f(m),
        None => Err("The engine is unavailable. Check GPU and screen capture support.".to_string()),
    }
}

fn list_windows() -> Result<Vec<WindowSummary>, String> {
    let list = mekiki_core::window_list().map_err(|e| e.to_string())?;
    Ok(list
        .into_iter()
        .map(|w| WindowSummary {
            title: w.title,
            exe: w.exe,
            class_name: w.class_name,
            x: w.bounds.x,
            y: w.bounds.y,
            width: w.bounds.width,
            height: w.bounds.height,
            z_order: w.z_order,
            icon: window_icon_data_url(w.hwnd),
        })
        .collect())
}

fn window_icon_data_url(hwnd: isize) -> String {
    let Some((width, height, bgra)) = mekiki_core::window_icon_bgra(hwnd) else {
        return String::new();
    };
    let rgba: Vec<u8> = bgra
        .chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], p[3]])
        .collect();
    let Some(img) = image::RgbaImage::from_raw(width, height, rgba) else {
        return String::new();
    };
    to_captured(&img).map(|c| c.data_url).unwrap_or_default()
}

fn capture_rect(mekiki: &mut Mekiki, rect: Rect) -> Result<CapturedImage, String> {
    let region = mekiki.region(rect);
    let frame = mekiki.capture_region(region).map_err(|e| e.to_string())?;
    let rgba = bgra_to_rgba(&frame.bgra);
    let img = image::RgbaImage::from_raw(frame.width, frame.height, rgba)
        .ok_or("Captured image dimensions do not match the buffer length.")?;
    to_captured(&img)
}

fn resolve_image(base_dir: &PathBuf, reference: &str) -> Result<CapturedImage, String> {
    let store = AssetStore::new(base_dir);
    let path = store.resolve(reference).map_err(|e| e.to_string())?;
    let img = image::open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .to_rgba8();
    to_captured(&img)
}

/// Collect images for the asset pane.
///
/// Sources are:
///
/// 1. Image files directly in the working directory, referenced by file name.
/// 2. Content-addressed store entries, referenced as `sha256:...`.
///
/// Pasted captures enter the second source, allowing captured and manually added
/// files to appear in one list.
fn list_images(base_dir: &PathBuf) -> Result<Vec<ImageResource>, String> {
    let store = AssetStore::new(base_dir);
    let store_dir = store.store_dir().to_path_buf();

    let mut out = Vec::new();

    // 1) Files directly beside the script. Child directories are separate asset
    // scopes; descending into them mixes unrelated scripts and projects.
    let mut files = collect_direct_images(base_dir);
    files.sort();

    for path in files {
        let Some(reference) = relative_reference(base_dir, &path) else {
            continue;
        };
        if let Some(res) = load_resource(&path, reference.clone(), reference, false) {
            out.push(res);
        }
    }

    // 2) Store entries whose file names encode their hashes.
    let mut stored = Vec::new();
    collect_store_files(&store_dir, 0, &mut stored);
    stored.sort();

    for path in stored {
        let Some(hex) = hash_from_store_path(&store_dir, &path) else {
            continue;
        };
        let reference = format!("sha256:{hex}");
        // Shorten only the display name; preserve the complete reference.
        let name = format!("{}…", &hex[..8]);
        if let Some(res) = load_resource(&path, reference, name, true) {
            out.push(res);
        }
    }

    Ok(out)
}

/// Listing limit that prevents loading thousands of files from a mistaken folder.
const MAX_IMAGES: usize = 400;

/// Maximum content-addressed store traversal depth.
const MAX_STORE_DEPTH: usize = 3;

fn collect_direct_images(dir: &PathBuf) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_image(path))
        .take(MAX_IMAGES)
        .collect()
}

fn collect_store_files(dir: &PathBuf, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_STORE_DEPTH || out.len() >= MAX_IMAGES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        if out.len() >= MAX_IMAGES {
            return;
        }
        let path = entry.path();

        if path.is_dir() {
            collect_store_files(&path, depth + 1, out);
        } else if is_image(&path) {
            out.push(path);
        }
    }
}

fn is_image(path: &std::path::Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp"
    )
}

/// Return a slash-separated path relative to the working directory for scripts.
fn relative_reference(base_dir: &PathBuf, path: &std::path::Path) -> Option<String> {
    let rel = path.strip_prefix(base_dir).ok()?;
    Some(rel.to_str()?.replace('\\', "/"))
}

/// Reconstruct a hash from a store path: `ab/cdef....png` becomes `abcdef...`.
fn hash_from_store_path(store_dir: &PathBuf, path: &std::path::Path) -> Option<String> {
    let rel = path.strip_prefix(store_dir).ok()?;
    let mut parts = rel.components();
    let head = parts.next()?.as_os_str().to_str()?;
    let tail = path.file_stem()?.to_str()?;
    let hex = format!("{head}{tail}");
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

/// Maximum thumbnail dimension.
///
/// Display size is adjustable, but transferred pixels are capped here. This is
/// sharp enough at the largest tile size while keeping hundreds of base64
/// thumbnails practical.
const THUMBNAIL_MAX: u32 = 256;

fn load_resource(
    path: &std::path::Path,
    reference: String,
    name: String,
    in_store: bool,
) -> Option<ImageResource> {
    // Skip unreadable images. A partial list is more useful than failing the
    // entire pane because one file is invalid.
    let img = image::open(path).ok()?.to_rgba8();
    let (width, height) = (img.width(), img.height());

    let longest = width.max(height);
    let thumb = if longest > THUMBNAIL_MAX {
        let s = THUMBNAIL_MAX as f32 / longest as f32;
        image::imageops::resize(
            &img,
            ((width as f32 * s) as u32).max(1),
            ((height as f32 * s) as u32).max(1),
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };

    Some(ImageResource {
        reference,
        name,
        width,
        height,
        thumbnail: to_captured(&thumb).ok()?.data_url,
        in_store,
    })
}

fn preview(
    mekiki: &mut Mekiki,
    base_dir: &PathBuf,
    reference: &str,
    similarity: f32,
) -> Result<PreviewResult, String> {
    let store = AssetStore::new(base_dir);
    let path = store.resolve(reference).map_err(|e| e.to_string())?;
    let pattern = mekiki
        .pattern_from_file(&path)
        .map_err(|e| e.to_string())?
        .similar(similarity);

    let screen = mekiki.primary_screen().map_err(|e| e.to_string())?;
    let target = mekiki.target(screen, &pattern);

    // Preview reports the current screen state. No match is a valid empty result.
    let matches = match mekiki.on(&target).resolve_all() {
        Ok(v) => v,
        Err(mekiki_core::Error::NotFound(_)) => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };

    // Results go to the output pane; draw match outlines on the desktop here.
    const OVERLAY_MS: u64 = 3000;
    if matches.is_empty() {
        if let Err(e) = mekiki.hide_match_marks() {
            log::warn!("Could not hide desktop match outlines: {e}");
        }
    } else if let Err(e) = mekiki.show_match_marks(
        matches.iter().map(|m| (m.rect, m.score)),
        Duration::from_millis(OVERLAY_MS),
    ) {
        log::warn!("Could not show desktop match outlines: {e}");
    }

    Ok(PreviewResult {
        matches: matches
            .into_iter()
            .map(|m| PreviewMatch {
                x: m.rect.x,
                y: m.rect.y,
                width: m.rect.width,
                height: m.rect.height,
                score: m.score,
            })
            .collect(),
    })
}

fn run_script(
    mekiki: Mekiki,
    base_dir: &PathBuf,
    source: &str,
    interrupt: Interrupt,
    step: StepControl,
) -> (Option<Mekiki>, RunResult) {
    let started = std::time::Instant::now();

    let mut host = ScriptHost::from_runtime(Runtime::new(mekiki, AssetStore::new(base_dir)));
    host.set_interrupt(interrupt.clone());
    // Always register the debugger for line highlighting, even outside step mode.
    // Measured cost is below 100 ns per operation (docs/architecture/ide.md).
    host.set_debugger(step, interrupt.clone());

    // Capture `print` output for the IDE output pane.
    let collected = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    host.capture_output(collected.clone());

    let result = host.run(source);
    let output = collected.borrow().clone();
    let mekiki = host.into_mekiki();

    // A user-requested stop is not a failure and should not appear as an error.
    //
    // Inspect the flag rather than the error type. Interruption can surface as
    // Rhai ErrorTerminated, core Error::Interrupted, or even successful completion
    // when the final statement is `sleep`; the flag is the only consistent fact.
    let interrupted = interrupt.is_stopping();

    (
        mekiki,
        RunResult {
            ok: result.is_ok(),
            output,
            error: if interrupted {
                None
            } else {
                result.err().map(|e| e.to_string())
            },
            interrupted,
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
    )
}

// ---------------------------------------------------------------------------
// Image transfer
// ---------------------------------------------------------------------------

fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], 255])
        .collect()
}

fn to_captured(img: &image::RgbaImage) -> Result<CapturedImage, String> {
    use base64::Engine as _;

    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;

    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    Ok(CapturedImage {
        data_url: format!("data:image/png;base64,{encoded}"),
        width: img.width(),
        height: img.height(),
    })
}

/// Encode the clipboard image as PNG bytes.
///
/// Snipping Tool (`Win+Shift+S`) places captures on the clipboard. Design plan
/// phase 3-4 imports that capture as a pattern through this function.
pub fn clipboard_png() -> Result<Vec<u8>, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let img = clipboard
        .get_image()
        .map_err(|e| format!("No image is available on the clipboard: {e}"))?;

    let buf =
        image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())
            .ok_or("Clipboard image dimensions do not match the buffer length.")?;

    let mut png = std::io::Cursor::new(Vec::new());
    buf.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png.into_inner())
}

/// Give an image a file name.
///
/// - Regular files are renamed inside their current folder.
/// - Store references (`sha256:`) are exported directly under the working
///   directory and given a readable name; the temporary store copy is removed.
///
/// The new name must be a file name, not a path. On success this returns a
/// slash-separated reference suitable for scripts.
pub fn rename_image(base_dir: &PathBuf, reference: &str, new_name: &str) -> Result<String, String> {
    if reference.starts_with(mekiki_scripting::assets::HASH_PREFIX) {
        return export_store_image(base_dir, reference, new_name);
    }

    let store = AssetStore::new(base_dir);
    let src = store.resolve(reference).map_err(|e| e.to_string())?;
    if !src.is_file() {
        return Err(format!("Image not found: '{reference}'."));
    }

    let file_name = sanitize_new_file_name(new_name, &src)?;
    let dest = src
        .parent()
        .ok_or("The image has no parent directory.")?
        .join(&file_name);

    let new_ref = relative_reference(base_dir, &dest)
        .ok_or("Images can only be renamed within the working directory.")?;

    if dest == src {
        return Ok(new_ref);
    }

    if dest.exists() {
        if same_path(&src, &dest) {
            // Windows needs an intermediate name for case-only renames.
            let tmp = dest.with_file_name(format!(".{file_name}.renaming"));
            std::fs::rename(&src, &tmp).map_err(|e| format!("Could not rename image: {e}"))?;
            std::fs::rename(&tmp, &dest).map_err(|e| {
                let _ = std::fs::rename(&tmp, &src);
                format!("Could not rename image: {e}")
            })?;
            return Ok(new_ref);
        }
        return Err(format!("An image named '{new_ref}' already exists."));
    }

    std::fs::rename(&src, &dest).map_err(|e| format!("Could not rename image: {e}"))?;
    Ok(new_ref)
}

/// Delete an image file.
///
/// Only images inside the working directory or store are accepted. Store files
/// cannot be addressed indirectly by relative path. Script contents are not
/// modified.
pub fn delete_image(base_dir: &PathBuf, reference: &str) -> Result<(), String> {
    let store = AssetStore::new(base_dir);
    let path = store.resolve(reference).map_err(|e| e.to_string())?;
    if !path.is_file() {
        return Err(format!("Image not found: '{reference}'."));
    }
    if !is_image(&path) {
        return Err(format!("'{reference}' is not an image file."));
    }

    let in_store = reference.starts_with(mekiki_scripting::assets::HASH_PREFIX);
    let allowed = if in_store {
        is_under(store.store_dir(), &path)
    } else {
        is_under(base_dir, &path) && !is_under(store.store_dir(), &path)
    };
    if !allowed {
        return Err("Images outside the working directory cannot be deleted.".into());
    }

    let parent = path.parent().map(std::path::Path::to_path_buf);
    std::fs::remove_file(&path).map_err(|e| format!("Could not delete image: {e}"))?;

    // Remove an empty hash shard directory after deleting a store entry.
    if in_store {
        if let Some(dir) = parent {
            if dir != store.store_dir() {
                let _ = std::fs::remove_dir(dir);
            }
        }
    }
    Ok(())
}

/// Export a stored image as a named file under the working directory.
fn export_store_image(
    base_dir: &PathBuf,
    reference: &str,
    new_name: &str,
) -> Result<String, String> {
    let store = AssetStore::new(base_dir);
    let src = store.resolve(reference).map_err(|e| e.to_string())?;
    if !src.is_file() {
        return Err(format!("Image not found: '{reference}'."));
    }

    let file_name = sanitize_new_file_name(new_name, &src)?;
    let dest = base_dir.join(&file_name);
    let new_ref = relative_reference(base_dir, &dest)
        .ok_or("Images can only be exported to the working directory.")?;
    if dest.exists() {
        return Err(format!("An image named '{new_ref}' already exists."));
    }

    // Prefer an atomic rename on the same volume, then fall back to copy/delete.
    if let Err(rename_err) = std::fs::rename(&src, &dest) {
        std::fs::copy(&src, &dest).map_err(|e| format!("Could not export image: {e}"))?;
        std::fs::remove_file(&src).map_err(|e| {
            format!(
                "Exported the image, but could not remove the temporary copy: {e} \
                     (rename failed: {rename_err})"
            )
        })?;
    }

    if let Some(dir) = src.parent() {
        if dir != store.store_dir() {
            let _ = std::fs::remove_dir(dir);
        }
    }
    Ok(new_ref)
}

fn sanitize_new_file_name(new_name: &str, old_path: &std::path::Path) -> Result<String, String> {
    let name = new_name.trim();
    if name.is_empty() {
        return Err("File name cannot be empty.".into());
    }
    if name == "." || name == ".." {
        return Err("Choose a different file name.".into());
    }
    if name.chars().any(|c| {
        matches!(
            c,
            '/' | '\\' | '\0' | '<' | '>' | ':' | '"' | '|' | '?' | '*'
        ) || c.is_control()
    }) {
        return Err("File name contains invalid characters.".into());
    }
    if std::path::Path::new(name).components().count() != 1 {
        return Err("The image must remain in its current folder.".into());
    }

    if is_image(std::path::Path::new(name)) {
        return Ok(name.to_string());
    }
    if std::path::Path::new(name).extension().is_some() {
        return Err("Unsupported image file extension.".into());
    }
    let ext = old_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");
    Ok(format!("{name}.{ext}"))
}

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn is_under(parent: &std::path::Path, child: &std::path::Path) -> bool {
    match (parent.canonicalize(), child.canonicalize()) {
        (Ok(p), Ok(c)) => c.starts_with(p),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mekiki-ide-list-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_png(path: &Path, w: u32, h: u32) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 80, 40, 255]));
        img.save(path).unwrap();
    }

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn empty_directory_lists_nothing() {
        let dir = temp_dir("empty");
        assert!(list_images(&dir).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_direct_files_and_store_without_descending() {
        let dir = temp_dir("files-and-store");
        write_png(&dir.join("ok.png"), 4, 3);
        write_png(&dir.join("parts").join("save.png"), 8, 6);

        let store = AssetStore::new(&dir);
        let reference = store.import_bytes(&png_bytes(2, 2)).unwrap();

        write_png(&dir.join(".git").join("hidden.png"), 2, 2);
        std::fs::write(dir.join("broken.png"), b"not a png").unwrap();

        let list = list_images(&dir).unwrap();
        let refs: Vec<&str> = list.iter().map(|r| r.reference.as_str()).collect();

        assert!(refs.contains(&"ok.png"), "{refs:?}");
        assert!(
            !refs.contains(&"parts/save.png"),
            "child-directory assets were included: {refs:?}"
        );
        assert!(refs.contains(&reference.as_str()), "{refs:?}");
        assert!(
            !refs.iter().any(|r| r.contains("hidden")),
            "hidden-directory assets were included: {refs:?}"
        );
        assert!(
            !refs.iter().any(|r| r.contains("broken")),
            "unreadable images were included: {refs:?}"
        );
        assert!(
            !refs.iter().any(|r| r.contains(".mekiki")),
            "store entries were also exposed as relative paths: {refs:?}"
        );

        let ok = list.iter().find(|r| r.reference == "ok.png").unwrap();
        assert_eq!((ok.width, ok.height), (4, 3));
        assert!(!ok.in_store);
        assert!(ok.thumbnail.starts_with("data:image/png;base64,"));

        let stored = list.iter().find(|r| r.reference == reference).unwrap();
        assert!(stored.in_store);
        assert_eq!(stored.name.chars().count(), 9, "{}", stored.name);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_reference_uses_forward_slashes() {
        let base = PathBuf::from("base");
        let path = base.join("parts").join("a.png");
        assert_eq!(
            relative_reference(&base, &path).as_deref(),
            Some("parts/a.png")
        );
    }

    #[test]
    fn store_path_roundtrips_to_hash() {
        let hex = format!("ab{}", "cd".repeat(31));
        assert_eq!(hex.len(), 64);
        let store = PathBuf::from("base").join(".mekiki").join("images");
        let path = store.join(&hex[..2]).join(format!("{}.png", &hex[2..]));
        assert_eq!(
            hash_from_store_path(&store, &path).as_deref(),
            Some(hex.as_str())
        );
        assert!(hash_from_store_path(&store, &store.join("nope.png")).is_none());
    }

    #[test]
    fn rename_stays_in_the_same_folder() {
        let dir = temp_dir("rename");
        write_png(&dir.join("ok.png"), 2, 2);
        write_png(&dir.join("parts").join("save.png"), 2, 2);

        assert_eq!(rename_image(&dir, "ok.png", "yes").unwrap(), "yes.png");
        assert!(dir.join("yes.png").is_file());
        assert!(!dir.join("ok.png").exists());

        assert_eq!(
            rename_image(&dir, "parts/save.png", "go.png").unwrap(),
            "parts/go.png"
        );
        assert!(dir.join("parts").join("go.png").is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_rejects_store_overwrite_and_bad_names() {
        let dir = temp_dir("rename-reject");
        write_png(&dir.join("ok.png"), 2, 2);
        write_png(&dir.join("taken.png"), 2, 2);
        let store = AssetStore::new(&dir);
        let reference = store.import_bytes(&png_bytes(2, 2)).unwrap();

        assert_eq!(
            rename_image(&dir, &reference, "named").unwrap(),
            "named.png"
        );
        assert!(dir.join("named.png").is_file());
        assert!(
            store.resolve(&reference).is_err(),
            "naming a stored image should remove its temporary copy"
        );
        assert!(rename_image(&dir, "ok.png", "taken.png").is_err());
        assert!(rename_image(&dir, "ok.png", "../escape.png").is_err());
        assert!(rename_image(&dir, "ok.png", "a/b.png").is_err());
        assert!(rename_image(&dir, "ok.png", "x.txt").is_err());
        assert!(dir.join("ok.png").is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_is_noop_when_the_name_is_unchanged() {
        let dir = temp_dir("rename-noop");
        write_png(&dir.join("ok.png"), 2, 2);
        assert_eq!(rename_image(&dir, "ok.png", "ok.png").unwrap(), "ok.png");
        assert!(dir.join("ok.png").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_file_and_store() {
        let dir = temp_dir("delete");
        write_png(&dir.join("ok.png"), 2, 2);
        write_png(&dir.join("parts").join("save.png"), 2, 2);
        let store = AssetStore::new(&dir);
        let reference = store.import_bytes(&png_bytes(2, 2)).unwrap();
        let stored = store.resolve(&reference).unwrap();
        let hash_dir = stored.parent().unwrap().to_path_buf();

        delete_image(&dir, "ok.png").unwrap();
        assert!(!dir.join("ok.png").exists());

        delete_image(&dir, "parts/save.png").unwrap();
        assert!(!dir.join("parts").join("save.png").exists());
        assert!(dir.join("parts").is_dir());

        delete_image(&dir, &reference).unwrap();
        assert!(store.resolve(&reference).is_err());
        assert!(!hash_dir.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_rejects_escape_and_missing() {
        let dir = temp_dir("delete-reject");
        write_png(&dir.join("ok.png"), 2, 2);
        write_png(
            &dir.join(".mekiki")
                .join("images")
                .join("ab")
                .join("nope.png"),
            2,
            2,
        );

        assert!(delete_image(&dir, "missing.png").is_err());
        assert!(delete_image(&dir, "../escape.png").is_err());
        assert!(delete_image(&dir, ".mekiki/images/ab/nope.png").is_err());
        assert!(dir.join("ok.png").is_file());
        assert!(
            dir.join(".mekiki")
                .join("images")
                .join("ab")
                .join("nope.png")
                .is_file()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

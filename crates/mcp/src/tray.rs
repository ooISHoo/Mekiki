//! The tray icon that says "mekiki-mcp is running".
//!
//! # Why
//!
//! The server is a stdio child of the MCP host. It has no window and no
//! console of its own, so the only sign that it exists is what it does to the
//! desktop. The tray icon makes the process visible, and its menu gives a
//! human a way to end it that does not go through the agent — the same
//! reasoning as the emergency stop in [`crate::hotkey`].
//!
//! # Boundary
//!
//! `main.rs` sees only [`Tray::show`], [`Tray::close`] and the [`TrayInfo`] it
//! fills in. Everything the OS dictates lives in the private `platform`
//! module, one body per target:
//!
//! - **Windows** (implemented): the `tray-icon` crate creates a hidden window
//!   on the calling thread and needs that thread to pump Win32 messages. The
//!   icon therefore lives on its own thread, like the hotkey. Closing posts
//!   `WM_QUIT` to that thread and joins it, which removes the icon; a killed
//!   process leaves a stale icon until the mouse passes over it.
//! - **macOS** (stub): the same crate can draw an `NSStatusItem`, but AppKit
//!   requires the *main* thread to run its event loop. Adding it means moving
//!   the tokio runtime off the main thread in `main.rs`; nothing else here
//!   needs to change.
//! - **Linux** (stub): the same crate uses GTK and `libappindicator` on a
//!   thread that runs the GTK loop. GNOME shows it only with the AppIndicator
//!   extension installed, so the icon must remain optional there.
//!
//! The tray never touches the engine directly. It is handed an `on_quit`
//! callback, which keeps this module free of tokio and testable on its own.
//!
//! # Icon
//!
//! `assets/tray-icon.png` is a copy of the IDE application icon, kept as a
//! separate file so a designer can replace it without touching the IDE. It is
//! embedded at build time and decoded at startup with the `image` crate.

use std::path::PathBuf;

/// What the tray shows about this server.
#[derive(Debug, Clone)]
pub struct TrayInfo {
    /// The `--base` directory.
    pub base: PathBuf,
    /// Whether the server refuses desktop-changing tools.
    pub observe_only: bool,
    /// Whether the emergency stop hotkey was registered.
    pub emergency_stop_armed: bool,
}

/// The menu item that ends the server.
pub const QUIT_LABEL: &str = "Quit Mekiki MCP";

/// Windows shows at most 127 characters of a tray tooltip, and silently drops
/// the rest. Truncate deliberately so the useful tail of the path survives.
const TOOLTIP_MAX_CHARS: usize = 127;

/// A visible tray icon. Dropping it without [`Tray::close`] leaves the icon
/// on screen until the process exits.
pub struct Tray {
    inner: platform::Handle,
}

impl Tray {
    /// Show the icon. `on_quit` runs on the tray's own thread when the user
    /// picks [`QUIT_LABEL`]; it may be called more than once and must not
    /// block.
    ///
    /// Returns `None` when the platform has no implementation or the icon
    /// could not be created. Neither is fatal: the server runs without it.
    pub fn show(info: &TrayInfo, on_quit: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        platform::show(info, Box::new(on_quit)).map(|inner| Tray { inner })
    }

    /// Remove the icon and stop its thread.
    pub fn close(self) {
        platform::close(self.inner);
    }
}

/// The first line of the tooltip.
pub fn mode_line(info: &TrayInfo) -> String {
    let mode = if info.observe_only {
        "observation only"
    } else {
        "running"
    };
    let stop = if info.observe_only {
        "emergency stop off".to_string()
    } else if info.emergency_stop_armed {
        format!("{} armed", crate::hotkey::DESCRIPTION)
    } else {
        format!("{} NOT armed", crate::hotkey::DESCRIPTION)
    };
    format!("Mekiki MCP — {mode}, {stop}")
}

/// The base directory as a person would type it.
///
/// `--base` is canonicalized, which on Windows yields the verbatim `\\?\`
/// form. That is right for the file system and wrong for a menu.
pub fn base_display(info: &TrayInfo) -> String {
    let shown = info.base.display().to_string();
    match shown.strip_prefix("\\\\?\\UNC\\") {
        Some(unc) => format!("\\\\{unc}"),
        None => shown
            .strip_prefix("\\\\?\\")
            .map(str::to_string)
            .unwrap_or(shown),
    }
}

/// The hover text. Fits the Windows limit by shortening the base path from
/// the front, because the last path segments are the ones that tell servers
/// apart.
pub fn tooltip(info: &TrayInfo) -> String {
    let head = format!("{}\nbase: ", mode_line(info));
    let base = base_display(info);
    let room = TOOLTIP_MAX_CHARS.saturating_sub(head.chars().count());
    let base_chars = base.chars().count();
    if base_chars <= room {
        return format!("{head}{base}");
    }
    let keep = room.saturating_sub(1);
    let tail: String = base.chars().skip(base_chars - keep).collect();
    format!("{head}…{tail}")
}

#[cfg(windows)]
mod platform {
    use std::sync::mpsc::channel;
    use std::thread::JoinHandle;

    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{Icon, TrayIconBuilder};
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, TranslateMessage, WM_QUIT,
    };

    use super::{QUIT_LABEL, TrayInfo};

    /// The tray thread. `thread_id` is where `WM_QUIT` must be posted.
    pub struct Handle {
        thread_id: u32,
        join: JoinHandle<()>,
    }

    /// Embedded at build time; see the module docs for where it comes from.
    const ICON_PNG: &[u8] = include_bytes!("../assets/tray-icon.png");

    fn load_icon() -> Result<Icon, String> {
        let decoded = image::load_from_memory(ICON_PNG)
            .map_err(|e| format!("cannot decode assets/tray-icon.png: {e}"))?
            .into_rgba8();
        let (width, height) = decoded.dimensions();
        Icon::from_rgba(decoded.into_raw(), width, height)
            .map_err(|e| format!("cannot build the tray icon: {e}"))
    }

    pub fn show(info: &TrayInfo, on_quit: Box<dyn Fn() + Send + Sync>) -> Option<Handle> {
        let tooltip = super::tooltip(info);
        let (ready_tx, ready_rx) = channel::<Option<u32>>();

        let join = std::thread::Builder::new()
            .name("mekiki-mcp-tray".into())
            .spawn(move || {
                // Everything that can fail happens before `ready` is sent, so
                // the caller learns about it instead of assuming an icon exists.
                let built = (|| -> Result<_, String> {
                    let icon = load_icon()?;
                    // The menu is an action list, nothing else. The mode and
                    // the base directory are in the tooltip.
                    let menu = Menu::new();
                    let quit = MenuItem::new(QUIT_LABEL, true, None);
                    menu.append(&quit)
                        .map_err(|e| format!("cannot build the tray menu: {e}"))?;

                    // The handler runs inside the message loop below, on this
                    // thread. `on_quit` only flips flags, so that is fine.
                    let quit_id = quit.id().clone();
                    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                        if *event.id() == quit_id {
                            log::warn!("quit requested from the tray icon");
                            on_quit();
                        }
                    }));

                    TrayIconBuilder::new()
                        .with_tooltip(&tooltip)
                        .with_icon(icon)
                        .with_menu(Box::new(menu))
                        .build()
                        .map_err(|e| format!("cannot create the tray icon: {e}"))
                })();

                let tray = match built {
                    Ok(tray) => tray,
                    Err(e) => {
                        log::error!("{e}");
                        let _ = ready_tx.send(None);
                        return;
                    }
                };
                let _ = ready_tx.send(Some(unsafe { GetCurrentThreadId() }));

                // The hidden window behind the icon needs its messages
                // dispatched, unlike the hotkey thread which only reads them.
                // GetMessageW blocks, so this costs nothing while idle, and
                // returns 0 on WM_QUIT (or -1 on error, which also ends the
                // loop rather than spinning).
                let mut msg = MSG::default();
                while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                // Dropping on this thread removes the icon and destroys the
                // hidden window, which must happen on the thread that owns it.
                drop(tray);
            })
            .ok()?;

        match ready_rx.recv() {
            Ok(Some(thread_id)) => Some(Handle { thread_id, join }),
            _ => None,
        }
    }

    pub fn close(handle: Handle) {
        // Failure means the thread is already gone; joining then returns at once.
        let _ = unsafe { PostThreadMessageW(handle.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        let _ = handle.join.join();
    }
}

#[cfg(not(windows))]
mod platform {
    use super::TrayInfo;

    pub struct Handle;

    pub fn show(_info: &TrayInfo, _on_quit: Box<dyn Fn() + Send + Sync>) -> Option<Handle> {
        // See the module docs for what each platform needs before this can
        // be filled in.
        log::info!("tray icon is not implemented on this platform");
        None
    }

    pub fn close(_handle: Handle) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(base: &str) -> TrayInfo {
        TrayInfo {
            base: PathBuf::from(base),
            observe_only: false,
            emergency_stop_armed: true,
        }
    }

    #[test]
    fn mode_line_reports_the_emergency_stop_state() {
        assert!(mode_line(&info("C:\\w")).contains("Shift+Alt+C armed"));

        let mut unarmed = info("C:\\w");
        unarmed.emergency_stop_armed = false;
        assert!(mode_line(&unarmed).contains("NOT armed"));

        let mut observe = info("C:\\w");
        observe.observe_only = true;
        let line = mode_line(&observe);
        assert!(line.contains("observation only"), "{line}");
        assert!(line.contains("emergency stop off"), "{line}");
    }

    #[test]
    fn verbatim_prefixes_are_hidden_from_people() {
        assert_eq!(base_display(&info("\\\\?\\D:\\w")), "D:\\w");
        assert_eq!(
            base_display(&info("\\\\?\\UNC\\server\\share\\w")),
            "\\\\server\\share\\w"
        );
        assert_eq!(base_display(&info("D:\\w")), "D:\\w");
    }

    #[test]
    fn short_tooltip_keeps_the_whole_path() {
        let text = tooltip(&info("D:\\project\\MekikiWork"));
        assert!(text.ends_with("base: D:\\project\\MekikiWork"), "{text}");
        assert!(text.chars().count() <= TOOLTIP_MAX_CHARS);
    }

    #[test]
    fn long_tooltip_is_cut_from_the_front_of_the_path() {
        let long = format!("D:\\{}\\MekikiWork", "very-long-segment\\".repeat(10));
        let text = tooltip(&info(&long));
        assert_eq!(text.chars().count(), TOOLTIP_MAX_CHARS, "{text}");
        assert!(text.contains("base: …"), "{text}");
        assert!(text.ends_with("\\MekikiWork"), "{text}");
    }
}

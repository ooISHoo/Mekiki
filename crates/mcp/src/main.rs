//! The `mekiki-mcp` binary.
//!
//! ```text
//! mekiki-mcp --base <dir> [--scope "<window spec>"] [--observe] [--max-timeout <ms>] [--no-tray]
//!
//! `--scope` is a default search area for the perception and locator tools.
//! `type_text` / `press` / `scroll` require their own `scope` on every call.
//! ```
//!
//! # stdout belongs to the protocol
//!
//! JSON-RPC travels on stdout. **A single stray byte there breaks the session**,
//! so nothing in this process may print to it: logs go to stderr, script `print`
//! output is collected and returned inside tool results, and `println!` is
//! banned throughout the crate.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use mekiki_mcp::engine::EngineHandle;
use mekiki_mcp::server::{Limits, MekikiServer};
use mekiki_mcp::tray::{Tray, TrayInfo};
use rmcp::ServiceExt;

fn main() -> ExitCode {
    // stderr by default, which is exactly where it has to go.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();

    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(Some(o)) => o,
        Ok(None) => return ExitCode::SUCCESS, // --help
        Err(e) => {
            eprintln!("error: {e}\n");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            log::error!("cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let outcome = runtime.block_on(serve(options));

    // Do not let the runtime's Drop run. tokio's stdin is a blocking read on a
    // helper thread that cannot be cancelled, and a plain drop waits for it.
    // When the host closes stdin that read returns and nothing is lost; when
    // the tray's Quit ends the session, stdin is still open and the process
    // would hang here forever. Detaching the runtime lets `main` return, and
    // returning from `main` ends the process regardless of that thread.
    runtime.shutdown_background();

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(options: Options) -> Result<(), String> {
    let lease = mekiki_mcp::desktop::acquire_desktop_lease(options.takeover)?;
    let lease_mode = lease.mode();
    let mut limits = options.limits;
    if !lease_mode.can_act() {
        limits.observe_only = true;
        log::warn!("desktop action lease is owned by another server; starting observation-only");
    } else if lease_mode == mekiki_mcp::desktop::DesktopLeaseMode::Takeover {
        log::warn!("--takeover bypassed an existing desktop action lease");
    }

    let engine = Arc::new(EngineHandle::spawn(options.base.clone()));

    // Arm the emergency stop before any tool can run. If the combination is
    // already taken this is the only warning anyone gets, so say it loudly.
    let emergency_stop_armed = if limits.observe_only {
        false
    } else {
        mekiki_mcp::hotkey::arm(engine.interrupt())
    };
    if emergency_stop_armed {
        log::info!(
            "emergency stop armed: {} interrupts whatever is running",
            mekiki_mcp::hotkey::DESCRIPTION
        );
    } else if !limits.observe_only {
        log::warn!(
            "COULD NOT ARM THE EMERGENCY STOP ({}). Another application may already own it. \
             The stop tool still works, but only through the agent.",
            mekiki_mcp::hotkey::DESCRIPTION
        );
    } else {
        log::info!("emergency stop not armed in observation-only mode");
    }

    log::info!("base directory: {}", options.base.display());
    if let Some(scope) = &limits.scope {
        log::info!("default scope: {scope}");
    }
    if limits.observe_only {
        log::info!("read-only: perception tools only");
    }
    log::info!("desktop lease: {}", lease_mode.as_str());
    log::info!("run_script ceiling: {:?}", limits.max_timeout);

    // The tray is the one place a human can see this process and end it
    // without the agent's cooperation. Its "quit" only raises flags: the
    // running script is interrupted, and the session below is cancelled.
    let quit = Arc::new(tokio::sync::Notify::new());
    let tray = if options.tray {
        let engine = engine.clone();
        let quit = quit.clone();
        Tray::show(
            &TrayInfo {
                base: options.base.clone(),
                observe_only: limits.observe_only,
                emergency_stop_armed,
            },
            move || {
                engine.stop();
                quit.notify_one();
            },
        )
    } else {
        None
    };
    if tray.is_some() {
        log::info!("tray icon shown; its menu can end this server");
    }

    let server = MekikiServer::new(engine.clone(), limits, emergency_stop_armed, lease_mode);
    let outcome = run_session(server, engine, quit).await;

    // On every exit path, or a stale icon stays in the tray until the mouse
    // passes over it.
    if let Some(tray) = tray {
        tray.close();
    }
    outcome
}

/// Serve until the host closes the session or the tray asks to quit.
async fn run_session(
    server: MekikiServer,
    engine: Arc<EngineHandle>,
    quit: Arc<tokio::sync::Notify>,
) -> Result<(), String> {
    // `serve` does not return until the host completes the initialize
    // handshake, and a host may take its time or never send it. Quit has to
    // work in that window too, so it is raced against the handshake here and
    // only afterwards handed to the session's cancellation token.
    let service = tokio::select! {
        biased;
        _ = quit.notified() => {
            log::warn!("quit requested before the host opened a session");
            return Ok(());
        }
        started = server.serve(rmcp::transport::stdio()) => {
            started.map_err(|e| format!("cannot start the MCP server: {e}"))?
        }
    };

    let cancel = service.cancellation_token();
    let quit_watch = tokio::spawn(async move {
        quit.notified().await;
        log::warn!("closing the MCP session at the tray's request");
        cancel.cancel();
    });

    let outcome = service.waiting().await;
    quit_watch.abort();

    // A quit during a script has only raised the stop flag. Give the worker a
    // moment to unwind so its cleanup (released keys and buttons) runs before
    // the process ends. Bounded, because a script stuck in native code cannot
    // be waited out.
    let grace = Duration::from_secs(3);
    let deadline = std::time::Instant::now() + grace;
    while engine.is_running() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if engine.is_running() {
        log::warn!("a script was still running after {grace:?}; exiting anyway");
    }

    outcome.map_err(|e| format!("the MCP session ended abnormally: {e}"))?;
    Ok(())
}

struct Options {
    base: PathBuf,
    limits: Limits,
    takeover: bool,
    /// Show the tray icon. Off for the protocol tests, which start many
    /// servers and kill some of them.
    tray: bool,
}

const USAGE: &str = "\
mekiki-mcp — drive the Windows desktop from an AI agent over MCP

Usage:
  mekiki-mcp --base <dir> [options]

Options:
  --base <dir>          Where scripts, assets and the handover log live.
                        Required. Created if missing.
  --scope <spec>        Default search area for the perception and locator
                        tools, e.g. --scope \"window:exe=notepad.exe\". Off by
                        default. Raw input (type_text, press, scroll) always
                        names its own target and ignores this.
  --observe             Register perception tools only; nothing can move the
                        desktop. Off by default.
  --takeover            Explicitly allow actions even when another Mekiki MCP
                        process owns the desktop lease. Diagnostic use only.
  --capture <backend>   'auto' (default) or 'gdi'. 'gdi' skips Desktop
                        Duplication entirely — for machines where it is
                        unsupported (some hybrid-GPU laptops, RDP). Slower,
                        and the hardware cursor is not captured.
  --max-timeout <ms>    Ceiling on run_script. Default 120000.
  --allow-launch <path> Allow launch of this exact absolute executable path.
                        Repeat for additional programs. Empty by default.
  --no-tray             Do not show the tray icon. By default the server sits
                        in the notification area while it runs, and its menu
                        can end it.
  -h, --help            This text.

Logs go to stderr; stdout carries the protocol and nothing else.";

impl Options {
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut base: Option<PathBuf> = None;
        let mut limits = Limits::default();
        let mut takeover = false;
        let mut tray = true;
        let mut args = args.peekable();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => {
                    eprintln!("{USAGE}");
                    return Ok(None);
                }
                "--base" => {
                    base = Some(PathBuf::from(
                        args.next().ok_or("--base needs a directory")?,
                    ));
                }
                "--scope" => {
                    limits.scope = Some(args.next().ok_or("--scope needs a window spec")?);
                }
                "--observe" => limits.observe_only = true,
                "--takeover" => takeover = true,
                "--no-tray" => tray = false,
                "--capture" => {
                    let backend = args.next().ok_or("--capture needs 'auto' or 'gdi'")?;
                    match backend.as_str() {
                        "auto" => {}
                        // The engine reads the environment variable when it
                        // opens the capture backend; the flag is just the
                        // discoverable spelling of it.
                        "gdi" => unsafe { std::env::set_var("MEKIKI_CAPTURE", "gdi") },
                        other => {
                            return Err(format!(
                                "--capture must be 'auto' or 'gdi', not '{other}'"
                            ));
                        }
                    }
                }
                "--max-timeout" => {
                    let ms: u64 = args
                        .next()
                        .ok_or("--max-timeout needs a number of milliseconds")?
                        .parse()
                        .map_err(|_| "--max-timeout must be a number of milliseconds")?;
                    limits.max_timeout = Duration::from_millis(ms.max(1));
                }
                "--allow-launch" => {
                    let path = PathBuf::from(
                        args.next()
                            .ok_or("--allow-launch needs an executable path")?,
                    );
                    if !path.is_absolute() {
                        return Err("--allow-launch needs an absolute path".into());
                    }
                    let path = path.canonicalize().map_err(|e| {
                        format!("cannot resolve --allow-launch {}: {e}", path.display())
                    })?;
                    limits.allowed_launch.push(path);
                }
                other => return Err(format!("unknown option: '{other}'")),
            }
        }

        let base = base.ok_or("--base is required")?;
        std::fs::create_dir_all(&base)
            .map_err(|e| format!("cannot create {}: {e}", base.display()))?;
        let base = base
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {e}", base.display()))?;

        Ok(Some(Options {
            base,
            limits,
            takeover,
            tray,
        }))
    }
}

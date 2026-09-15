//! The emergency stop.
//!
//! # Why a hotkey and not a tool
//!
//! `stop` exists as a tool, but it is only reachable through the agent. If the
//! agent has stopped listening, or is confidently doing the wrong thing, or the
//! script it started is dragging the mouse across the desktop, the human in the
//! room needs a way out that does not go through the thing that is misbehaving.
//!
//! **Shift+Alt+C is that way out**, the same combination the IDE uses. It is
//! deliberately the one piece of safety machinery that stays armed no matter
//! how permissive everything else is.
//!
//! # Why a thread with a message loop
//!
//! `RegisterHotKey` delivers `WM_HOTKEY` to **the thread that registered it**,
//! and only while that thread pumps messages. So the registration and the loop
//! have to live together on a thread that does nothing else. The handler only
//! raises the interrupt flag — no work happens on this thread, because a slow
//! handler would stall the message queue it lives on.
//!
//! Registration can fail when another application already owns the combination.
//! That is not fatal: the server still runs, having logged loudly that the
//! emergency stop is not armed.

use mekiki_core::Interrupt;

/// The hotkey as text, for logs and documentation.
pub const DESCRIPTION: &str = "Shift+Alt+C";

/// Arm the emergency stop.
///
/// Returns whether it was armed. The thread runs for the life of the process.
#[cfg(windows)]
pub fn arm(interrupt: Interrupt) -> bool {
    use std::sync::mpsc::channel;

    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

    /// Any value in 0x0000..=0xBFFF works; it only has to be unique within this
    /// thread.
    const HOTKEY_ID: i32 = 0x4D45;

    let (ready_tx, ready_rx) = channel::<bool>();

    std::thread::Builder::new()
        .name("mekiki-mcp-hotkey".into())
        .spawn(move || {
            // MOD_NOREPEAT: holding the keys down must raise one stop, not a
            // stream of them.
            let registered = unsafe {
                RegisterHotKey(
                    None,
                    HOTKEY_ID,
                    MOD_SHIFT | MOD_ALT | MOD_NOREPEAT,
                    'C' as u32,
                )
            }
            .is_ok();

            let _ = ready_tx.send(registered);
            if !registered {
                return;
            }

            let mut msg = MSG::default();
            // `None` for the window handle means "messages posted to this
            // thread", which is where WM_HOTKEY lands when RegisterHotKey was
            // called with no window. GetMessageW blocks, so this thread costs
            // nothing while idle.
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID {
                    log::warn!("emergency stop ({DESCRIPTION}) pressed");
                    interrupt.stop();
                }
            }
        })
        .expect("cannot start the hotkey thread");

    ready_rx.recv().unwrap_or(false)
}

#[cfg(not(windows))]
pub fn arm(_interrupt: Interrupt) -> bool {
    false
}

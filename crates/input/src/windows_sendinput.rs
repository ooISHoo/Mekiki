//! Windows: input injection via `SendInput`.
//!
//! # Things worth knowing
//!
//! - **Absolute coordinates are normalised to 0..65535.** Rather than passing
//!   pixel values, pass the whole virtual desktop mapped onto 0..65535. Without
//!   `MOUSEEVENTF_VIRTUALDESK` the origin is the primary monitor, which shifts
//!   coordinates on a multi-monitor setup.
//! - **Text entry uses `KEYEVENTF_UNICODE`.** Going through virtual key codes
//!   moves symbols around depending on the keyboard layout.
//! - **UIPI.** Input cannot be sent to a window at a higher integrity level (an
//!   app running as administrator). It fails in the shape of `SendInput`
//!   reporting success while nothing happens.
//! - **Do not add `MOUSEEVENTF_MOVE` to button events.** With the right button
//!   and a move set together, some games consume it as a camera drag and it
//!   never becomes a click.
//! - **Bring the window under the pointer to the front.** A right click while
//!   unfocused is sometimes discarded.
//! - **Honour `SM_SWAPBUTTON`.** Same as Java Robot / SikuliX. A left-handed
//!   configuration swaps left and right.
//! - **Move smoothly (caller's job).** SikuliX takes about half a second.
//!   Games sometimes do not apply a single absolute jump to their internal
//!   cursor.

use std::collections::HashSet;
use std::mem::size_of;

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_BACK,
    VK_CAPITAL, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_INSERT,
    VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SNAPSHOT,
    VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetCursorPos, GetDesktopWindow, GetForegroundWindow, GetSystemMetrics,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_SWAPBUTTON, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SetForegroundWindow, WindowFromPoint,
};

use crate::{InputError, InputInjector, Key, Modifiers, MouseButton, Result};

/// The value of one wheel notch. `WHEEL_DELTA`.
const WHEEL_DELTA: i32 = 120;

pub struct SendInputInjector {
    held_buttons: HashSet<MouseButton>,
    held_keys: HashSet<Key>,
}

impl SendInputInjector {
    pub fn new() -> Result<Self> {
        // Without the same DPI setting as the capture side, coordinates would
        // mean different things. This fails if it is already set, which is fine.
        unsafe {
            if let Err(e) =
                SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            {
                log::debug!("DPI awareness is already set or cannot be set: {e}");
            }
        }
        Ok(Self {
            held_buttons: HashSet::new(),
            held_keys: HashSet::new(),
        })
    }
}

fn button_flag(
    button: MouseButton,
    down: bool,
) -> windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS {
    // Same as Java Robot / SikuliX. A left-handed configuration swaps the two.
    let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
    let button = match (button, swapped) {
        (MouseButton::Left, true) => MouseButton::Right,
        (MouseButton::Right, true) => MouseButton::Left,
        (other, _) => other,
    };
    match (button, down) {
        (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
        (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
        (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
        (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
        (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
        (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
    }
}

/// Bring the top-level window under the pointer to the front. The click is sent
/// even if this fails.
fn activate_window_at(x: i32, y: i32) {
    unsafe {
        let hwnd = WindowFromPoint(POINT { x, y });
        if hwnd.is_invalid() || hwnd == GetDesktopWindow() {
            return;
        }
        let root = GetAncestor(hwnd, GA_ROOT);
        if root.is_invalid() || root == GetForegroundWindow() {
            return;
        }
        if !SetForegroundWindow(root).as_bool() {
            log::debug!("cannot bring the target window to the front ({x},{y})");
        }
    }
}

/// The virtual desktop rectangle (physical pixels).
fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// Map a pixel coordinate onto the 0..65535 that `MOUSEEVENTF_ABSOLUTE`
/// expects.
///
/// The denominator is `width - 1` because `SendInput`'s absolute coordinates
/// are defined so that 0 is the centre of the leftmost pixel and 65535 the
/// centre of the rightmost. Dividing by the width is off by one pixel.
pub(crate) fn normalize_absolute(x: i32, y: i32, screen: (i32, i32, i32, i32)) -> (i32, i32) {
    let (vx, vy, vw, vh) = screen;
    let denom_x = (vw - 1).max(1);
    let denom_y = (vh - 1).max(1);
    let nx = ((x - vx) as i64 * 65535 + denom_x as i64 / 2) / denom_x as i64;
    let ny = ((y - vy) as i64 * 65535 + denom_y as i64 / 2) / denom_y as i64;
    (nx as i32, ny as i32)
}

fn send(inputs: &[INPUT]) -> Result<()> {
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(InputError::Rejected(format!(
            "SendInput accepted only {} of {} events \
             (possibly blocked by UIPI, or the secure desktop is showing)",
            sent,
            inputs.len()
        )));
    }
    Ok(())
}

fn mouse_input(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> INPUT {
    mouse_input_full(0, 0, 0, flags)
}

fn mouse_input_full(
    dx: i32,
    dy: i32,
    mouse_data: i32,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub(crate) fn virtual_key(key: Key) -> u16 {
    match key {
        Key::Enter => VK_RETURN.0,
        Key::Tab => VK_TAB.0,
        Key::Escape => VK_ESCAPE.0,
        Key::Backspace => VK_BACK.0,
        Key::Delete => VK_DELETE.0,
        Key::Insert => VK_INSERT.0,
        Key::Home => VK_HOME.0,
        Key::End => VK_END.0,
        Key::PageUp => VK_PRIOR.0,
        Key::PageDown => VK_NEXT.0,
        Key::Up => VK_UP.0,
        Key::Down => VK_DOWN.0,
        Key::Left => VK_LEFT.0,
        Key::Right => VK_RIGHT.0,
        Key::Space => VK_SPACE.0,
        Key::Shift => VK_SHIFT.0,
        Key::Ctrl => VK_CONTROL.0,
        Key::Alt => VK_MENU.0,
        Key::Meta => VK_LWIN.0,
        Key::CapsLock => VK_CAPITAL.0,
        Key::PrintScreen => VK_SNAPSHOT.0,
        // VK_F1..VK_F24 are contiguous. Clamp n into 1..=24.
        Key::F(n) => VK_F1.0 + u16::from(n.clamp(1, 24) - 1),
        // The same VK as the main Enter. The EXTENDED flag makes it the keypad one.
        Key::NumEnter => VK_RETURN.0,
        Key::Raw(v) => v,
    }
}

fn key_event_flags(key: Key, up: bool) -> KEYBD_EVENT_FLAGS {
    let mut flags = if up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    if matches!(key, Key::NumEnter) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    flags
}

fn chord_events(key: Key, modifiers: Modifiers) -> Vec<(Key, bool)> {
    let mods = modifiers.keys();
    let mut events = Vec::with_capacity(mods.len() * 2 + 2);
    events.extend(mods.iter().copied().map(|modifier| (modifier, false)));
    events.push((key, false));
    events.push((key, true));
    events.extend(mods.iter().rev().copied().map(|modifier| (modifier, true)));
    events
}

fn key_inputs(events: &[(Key, bool)]) -> Vec<INPUT> {
    events
        .iter()
        .map(|(key, up)| key_input(virtual_key(*key), 0, key_event_flags(*key, *up)))
        .collect()
}

impl InputInjector for SendInputInjector {
    fn mouse_move(&mut self, x: i32, y: i32) -> Result<()> {
        let (nx, ny) = normalize_absolute(x, y, virtual_screen());
        send(&[mouse_input_full(
            nx,
            ny,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )])
    }

    fn mouse_down(&mut self, button: MouseButton) -> Result<()> {
        send(&[mouse_input(button_flag(button, true))])?;
        self.held_buttons.insert(button);
        Ok(())
    }

    fn mouse_up(&mut self, button: MouseButton) -> Result<()> {
        let result = send(&[mouse_input(button_flag(button, false))]);
        self.held_buttons.remove(&button);
        result
    }

    fn activate_at(&mut self, x: i32, y: i32) -> Result<()> {
        activate_window_at(x, y);
        Ok(())
    }

    fn scroll(&mut self, horizontal: i32, vertical: i32) -> Result<()> {
        let mut inputs = Vec::new();
        if vertical != 0 {
            inputs.push(mouse_input_full(
                0,
                0,
                vertical * WHEEL_DELTA,
                MOUSEEVENTF_WHEEL,
            ));
        }
        if horizontal != 0 {
            inputs.push(mouse_input_full(
                0,
                0,
                horizontal * WHEEL_DELTA,
                MOUSEEVENTF_HWHEEL,
            ));
        }
        if inputs.is_empty() {
            return Ok(());
        }
        send(&inputs)
    }

    fn key_down(&mut self, key: Key) -> Result<()> {
        send(&[key_input(virtual_key(key), 0, key_event_flags(key, false))])?;
        self.held_keys.insert(key);
        Ok(())
    }

    fn key_up(&mut self, key: Key) -> Result<()> {
        let result = send(&[key_input(virtual_key(key), 0, key_event_flags(key, true))]);
        self.held_keys.remove(&key);
        result
    }

    fn key_press(&mut self, key: Key, modifiers: Modifiers) -> Result<()> {
        let events = chord_events(key, modifiers);
        if let Err(error) = send(&key_inputs(&events)) {
            // A partial SendInput can leave the target or a modifier down.
            // Release all members best-effort before returning the original error.
            let mods = modifiers.keys();
            let mut releases = Vec::with_capacity(mods.len() + 1);
            releases.push((key, true));
            releases.extend(mods.iter().rev().copied().map(|modifier| (modifier, true)));
            let _ = send(&key_inputs(&releases));
            return Err(error);
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<()> {
        // Sending an "up" for a button that was never pressed is treated by
        // some games as a click at the current position. Release only what was
        // actually pressed.
        let buttons: Vec<_> = self.held_buttons.iter().copied().collect();
        for button in buttons {
            let _ = self.mouse_up(button);
        }
        let keys: Vec<_> = self.held_keys.iter().copied().collect();
        for key in keys {
            let _ = self.key_up(key);
        }
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> Result<()> {
        let mut inputs = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            // With KEYEVENTF_UNICODE, wVk is 0 and wScan carries the UTF-16
            // code unit. Surrogate pairs can be sent as two units in order and
            // the OS reassembles them.
            inputs.push(key_input(0, unit, KEYEVENTF_UNICODE));
            inputs.push(key_input(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
        if inputs.is_empty() {
            return Ok(());
        }
        // SendInput guarantees that the events within one call are not
        // interleaved with input from other processes. Sending even a long
        // string in a single call keeps anything else from cutting in.
        send(&inputs)
    }

    fn cursor_position(&self) -> Result<(i32, i32)> {
        let mut p = windows::Win32::Foundation::POINT::default();
        unsafe { GetCursorPos(&mut p) }.map_err(|e| InputError::Os(format!("{e}")))?;
        Ok((p.x, p.y))
    }

    fn backend_name(&self) -> &'static str {
        "windows/sendinput"
    }
}

// KEYEVENTF_SCANCODE is unused for now, but the import is kept for when
// layout-dependent key events become necessary.
const _: KEYBD_EVENT_FLAGS = KEYEVENTF_SCANCODE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_normalization_maps_corners() {
        // A single 1920x1080 monitor.
        let screen = (0, 0, 1920, 1080);
        assert_eq!(normalize_absolute(0, 0, screen), (0, 0));
        assert_eq!(normalize_absolute(1919, 1079, screen), (65535, 65535));
    }

    #[test]
    fn absolute_normalization_handles_negative_origin() {
        // A second 1920x1080 added to the left. The virtual desktop is
        // 3840x1080 starting at (-1920, 0).
        let screen = (-1920, 0, 3840, 1080);
        assert_eq!(normalize_absolute(-1920, 0, screen), (0, 0));
        assert_eq!(
            normalize_absolute(3839 - 1920, 1079, screen),
            (65535, 65535)
        );
        // The primary monitor's origin lands near the middle.
        let (nx, _) = normalize_absolute(0, 0, screen);
        assert!((32000..33600).contains(&nx), "nx = {nx}");
    }

    #[test]
    fn function_keys_are_contiguous() {
        assert_eq!(virtual_key(Key::F(1)), VK_F1.0);
        assert_eq!(virtual_key(Key::F(12)), VK_F1.0 + 11);
        // Out-of-range values are clamped rather than panicking.
        assert_eq!(virtual_key(Key::F(0)), VK_F1.0);
        assert_eq!(virtual_key(Key::F(99)), VK_F1.0 + 23);
    }

    #[test]
    fn raw_key_passes_through() {
        assert_eq!(virtual_key(Key::Raw(0x41)), 0x41);
    }

    #[test]
    fn numpad_enter_uses_return_vk() {
        assert_eq!(virtual_key(Key::NumEnter), VK_RETURN.0);
        assert_eq!(virtual_key(Key::Enter), VK_RETURN.0);
        assert_ne!(
            key_event_flags(Key::NumEnter, false).0,
            key_event_flags(Key::Enter, false).0
        );
    }

    #[test]
    fn modifier_chord_is_one_ordered_event_batch() {
        let modifiers = Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            chord_events(Key::Raw(b'S' as u16), modifiers),
            vec![
                (Key::Ctrl, false),
                (Key::Shift, false),
                (Key::Raw(b'S' as u16), false),
                (Key::Raw(b'S' as u16), true),
                (Key::Shift, true),
                (Key::Ctrl, true),
            ]
        );
    }
}

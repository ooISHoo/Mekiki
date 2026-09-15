//! Mouse and keyboard input injection.
//!
//! Phase 1-2 of the development plan. **Windows is the only target platform**,
//! and the implementation is `SendInput`.
//!
//! The [`InputInjector`] trait is the boundary with the OS-dependent part.
//! Supporting other operating systems is out of scope, but the boundary is kept
//! deliberately (see the `mekiki-capture` crate docs for why).
//!
//! This crate holds **primitives only**. Timing policy — the interval within a
//! click, the intermediate steps of a drag — belongs to the semantics of an
//! action, so it is assembled in `mekiki-core`.
//!
//! Coordinates are **physical pixels in virtual desktop space**, the same as
//! `mekiki-capture`.

use std::fmt;

#[cfg(windows)]
mod windows_clipboard;
#[cfg(windows)]
mod windows_sendinput;

#[cfg(windows)]
pub use windows_clipboard::{clipboard_text, set_clipboard_text};

/// Read the clipboard text. Unsupported off Windows.
#[cfg(not(windows))]
pub fn clipboard_text() -> Result<String> {
    Err(InputError::Unsupported("this platform".into()))
}

/// Replace the clipboard contents with text. Unsupported off Windows.
#[cfg(not(windows))]
pub fn set_clipboard_text(_text: &str) -> Result<()> {
    Err(InputError::Unsupported("this platform".into()))
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Modifier keys, combined as bit flags.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// The Windows key / Command key.
    pub meta: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
    };

    pub fn ctrl() -> Self {
        Self {
            ctrl: true,
            ..Self::NONE
        }
    }

    pub fn shift() -> Self {
        Self {
            shift: true,
            ..Self::NONE
        }
    }

    pub fn alt() -> Self {
        Self {
            alt: true,
            ..Self::NONE
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.shift && !self.ctrl && !self.alt && !self.meta
    }

    /// The order to press in. Modifiers go down before the target key.
    pub fn keys(&self) -> Vec<Key> {
        let mut v = Vec::new();
        if self.ctrl {
            v.push(Key::Ctrl);
        }
        if self.alt {
            v.push(Key::Alt);
        }
        if self.shift {
            v.push(Key::Shift);
        }
        if self.meta {
            v.push(Key::Meta);
        }
        v
    }
}

/// A key. Covers everything SikuliX exposes as `Key.*`.
///
/// For text entry use [`InputInjector::type_text`] instead; that path is
/// Unicode based and does not depend on the keyboard layout.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    Space,
    Shift,
    Ctrl,
    Alt,
    Meta,
    CapsLock,
    PrintScreen,
    F(u8),
    /// The numeric keypad's Enter. A different key from the main Enter (it
    /// needs the extended flag).
    NumEnter,
    /// A raw virtual-key code. The escape hatch for keys not in the list above.
    Raw(u16),
}

#[derive(Debug)]
pub enum InputError {
    Unsupported(&'static str),
    /// The OS refused the input.
    ///
    /// On Windows this happens with UIPI (input cannot be sent to a window at a
    /// higher integrity level) or while the secure desktop is displayed.
    Rejected(String),
    Os(String),
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(p) => write!(f, "input injection is not implemented for {p}"),
            Self::Rejected(e) => write!(f, "the input was refused: {e}"),
            Self::Os(e) => write!(f, "an OS API call failed: {e}"),
        }
    }
}

impl std::error::Error for InputError {}

pub type Result<T> = std::result::Result<T, InputError>;

/// Input injection primitives.
pub trait InputInjector: Send {
    /// Move the cursor to a virtual-desktop coordinate.
    fn mouse_move(&mut self, x: i32, y: i32) -> Result<()>;

    fn mouse_down(&mut self, button: MouseButton) -> Result<()>;
    fn mouse_up(&mut self, button: MouseButton) -> Result<()>;

    /// Press and immediately release. May be sent as a single injection.
    fn mouse_click(&mut self, button: MouseButton) -> Result<()> {
        self.mouse_down(button)?;
        self.mouse_up(button)
    }

    /// Bring the window under the pointer to the front. Games discard right
    /// clicks when they are not focused.
    fn activate_at(&mut self, _x: i32, _y: i32) -> Result<()> {
        Ok(())
    }

    /// Wheel. The unit is notches (positive is up / right).
    fn scroll(&mut self, horizontal: i32, vertical: i32) -> Result<()>;

    fn key_down(&mut self, key: Key) -> Result<()>;
    fn key_up(&mut self, key: Key) -> Result<()>;

    /// Type a string.
    ///
    /// This uses layout-independent Unicode input, so symbols do not shift on a
    /// Japanese keyboard layout.
    fn type_text(&mut self, text: &str) -> Result<()>;

    fn cursor_position(&self) -> Result<(i32, i32)>;

    fn backend_name(&self) -> &'static str;

    /// Release every input that is being held down.
    ///
    /// The cleanup for a script stopped part way through. `drag_to` can be
    /// interrupted between press, move and release, and modifiers can be left
    /// held the same way. **Leaving something pressed on the user's desktop is
    /// the worst outcome**, so this runs on every interruption.
    ///
    /// An implementation must release only what it pressed. Sending an "up" for
    /// a left button that was never down is treated by some games as a left
    /// click at the current position.
    fn release_all(&mut self) -> Result<()> {
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            let _ = self.mouse_up(button);
        }
        for key in [Key::Shift, Key::Ctrl, Key::Alt, Key::Meta] {
            let _ = self.key_up(key);
        }
        Ok(())
    }

    /// Tap a key once while holding the given modifiers.
    fn key_press(&mut self, key: Key, modifiers: Modifiers) -> Result<()> {
        let mods = modifiers.keys();
        for m in &mods {
            self.key_down(*m)?;
        }
        let result = (|| {
            self.key_down(key)?;
            self.key_up(key)
        })();
        // Release the modifiers even if the target key failed. Leaving them
        // held would drag every subsequent user action down with them.
        for m in mods.iter().rev() {
            let _ = self.key_up(*m);
        }
        result
    }
}

/// Open the default backend for this platform.
///
/// Returns [`InputError::Unsupported`] on anything but Windows. To add support,
/// implement [`InputInjector`] and swap the branch here — nothing else changes.
pub fn open() -> Result<Box<dyn InputInjector>> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows_sendinput::SendInputInjector::new()?))
    }
    #[cfg(target_os = "macos")]
    {
        // This would be CGEvent, plus a path for granting accessibility
        // permission. Out of scope for now.
        Err(InputError::Unsupported("macOS"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // This would be uinput. Out of scope for now.
        Err(InputError::Unsupported("Linux"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_order_puts_ctrl_first() {
        let m = Modifiers {
            shift: true,
            ctrl: true,
            alt: false,
            meta: false,
        };
        assert_eq!(m.keys(), vec![Key::Ctrl, Key::Shift]);
    }

    #[test]
    fn empty_modifiers_produce_no_keys() {
        assert!(Modifiers::NONE.keys().is_empty());
        assert!(Modifiers::NONE.is_empty());
        assert!(!Modifiers::ctrl().is_empty());
    }
}

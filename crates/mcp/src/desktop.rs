//! Interactive-desktop diagnostics and the cross-process action lease.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopLeaseMode {
    Owner,
    Observer,
    Takeover,
}

impl DesktopLeaseMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Observer => "observer",
            Self::Takeover => "takeover",
        }
    }

    pub fn can_act(self) -> bool {
        !matches!(self, Self::Observer)
    }
}

#[cfg(windows)]
pub struct DesktopLease {
    mode: DesktopLeaseMode,
    handle: Option<windows::Win32::Foundation::HANDLE>,
}

#[cfg(not(windows))]
pub struct DesktopLease {
    mode: DesktopLeaseMode,
}

impl DesktopLease {
    pub fn mode(&self) -> DesktopLeaseMode {
        self.mode
    }
}

#[cfg(windows)]
impl Drop for DesktopLease {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            unsafe {
                let _ = windows::Win32::System::Threading::ReleaseMutex(handle);
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
        }
    }
}

/// Acquire the action lease for this Windows session.
///
/// `takeover` is an explicit diagnostic escape hatch: it permits actions even
/// when another process owns the mutex, and is therefore never implicit.
#[cfg(windows)]
pub fn acquire_desktop_lease(takeover: bool) -> Result<DesktopLease, String> {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::PCWSTR;

    let mut session_id = 0u32;
    unsafe { ProcessIdToSessionId(std::process::id(), &mut session_id) }
        .map_err(|e| format!("cannot identify the Windows session: {e}"))?;
    let name = std::env::var("MEKIKI_DESKTOP_LEASE_NAME")
        .unwrap_or_else(|_| format!("Local\\mekiki-mcp-desktop-{session_id}"));
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) }
        .map_err(|e| format!("cannot create desktop lease '{name}': {e}"))?;
    let already_owned = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

    if already_owned {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Ok(DesktopLease {
            mode: if takeover {
                DesktopLeaseMode::Takeover
            } else {
                DesktopLeaseMode::Observer
            },
            handle: None,
        });
    }

    Ok(DesktopLease {
        mode: DesktopLeaseMode::Owner,
        handle: Some(handle),
    })
}

#[cfg(not(windows))]
pub fn acquire_desktop_lease(_takeover: bool) -> Result<DesktopLease, String> {
    Ok(DesktopLease {
        mode: DesktopLeaseMode::Owner,
    })
}

/// Whether this process can inspect the interactive input desktop.
#[cfg(windows)]
pub fn input_desktop_accessible() -> (bool, Option<String>) {
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, OpenInputDesktop,
    };

    match unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) } {
        Ok(desktop) => {
            unsafe {
                let _ = CloseDesktop(desktop);
            }
            (true, None)
        }
        Err(error) => (false, Some(format!("desktop_access_denied: {error}"))),
    }
}

#[cfg(not(windows))]
pub fn input_desktop_accessible() -> (bool, Option<String>) {
    (
        false,
        Some("desktop_access_denied: unsupported platform".into()),
    )
}

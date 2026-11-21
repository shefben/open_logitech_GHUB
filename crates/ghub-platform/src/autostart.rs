//! Starting with Windows. Stands in for the Avalonia build's autostart entry.
//!
//! One registry value under the current user's `Run` key. Three properties, each of which is the
//! reason not to do it another way:
//!
//! * **`HKEY_CURRENT_USER`, never `HKEY_LOCAL_MACHINE`.** The machine-wide key needs
//!   administrator rights and starts the application for every account on the machine. This
//!   application manages one person's peripherals.
//! * **No scheduled task.** A task can be created with "run with highest privileges", which is
//!   how applications quietly end up elevated at logon. Nothing here elevates, and the `Run` key
//!   cannot.
//! * **The path is read from the running image, not stored from a build.** A user who moves the
//!   executable and re-ticks the box gets the new path; a stale path would simply fail at logon
//!   with no visible cause.

/// The value name under `Run`. Also what the user sees in Task Manager's startup list.
pub const VALUE_NAME: &str = "ghub-rs";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Whether this application is registered to start with Windows.
#[cfg(windows)]
pub fn is_enabled() -> bool {
    windows::read().is_some()
}

/// The command line that would be run at logon, if any. Shown in Options so the setting is
/// inspectable rather than just a tick.
#[cfg(windows)]
pub fn registered_command() -> Option<String> {
    windows::read()
}

/// Adds or removes the value. `Ok` means the registry now says what was asked for.
#[cfg(windows)]
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        // The *agent*, not whichever binary is asking. Two reasons, and both are things a user
        // would otherwise report as a bug: registering the window would put a window on screen at
        // every logon, and it would cost its ~390 MB from logon onwards to do a job the 1.6 MB
        // agent does. The window is started by the user, or by the agent's own menu.
        //
        // Falling back to the running image when the agent is not beside it keeps the tick
        // honest on a half-installed copy: something starts, and the window says in its
        // diagnostics that no agent was found.
        let path = match crate::handoff::sibling_executable(crate::handoff::AGENT_EXECUTABLE) {
            Some(agent) => agent.display().to_string(),
            None => windows::executable_path()?,
        };
        // Quoted, because a path with a space in it is otherwise read as a command plus
        // arguments - and `C:\Program Files\...` is the normal case.
        windows::write(&format!("\"{path}\""))
    } else {
        windows::remove()
    }
}

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn registered_command() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn set_enabled(_enabled: bool) -> Result<(), String> {
    Err("starting with the system is a Windows feature here".to_owned())
}

#[cfg(windows)]
mod windows {
    use super::{RUN_KEY, VALUE_NAME};
    use crate::sys;

    /// Long paths are allowed, so the buffer is the documented maximum rather than `MAX_PATH`.
    const PATH_BUFFER: usize = 32 * 1024;

    pub fn executable_path() -> Result<String, String> {
        let mut buffer = vec![0u16; PATH_BUFFER];
        // SAFETY: `buffer` is a live allocation of `nSize` UTF-16 units.
        let written = unsafe {
            sys::GetModuleFileNameW(
                std::ptr::null_mut(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        };
        if written == 0 || written as usize >= buffer.len() {
            return Err("the path of this program could not be read".to_owned());
        }
        Ok(sys::from_wide(&buffer[..written as usize]))
    }

    pub fn read() -> Option<String> {
        let key = open(sys::KEY_READ).ok()?;
        let name = sys::to_wide(VALUE_NAME);
        let mut kind = 0u32;
        let mut bytes = 0u32;

        // SAFETY: a size query - a null data pointer with a live length is how the API is asked
        // how much space the value needs.
        let status = unsafe {
            sys::RegQueryValueExW(
                key.0,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                std::ptr::null_mut(),
                &mut bytes,
            )
        };
        if status != 0 || kind != sys::REG_SZ || bytes == 0 {
            return None;
        }

        let mut buffer = vec![0u16; (bytes as usize / 2) + 1];
        let mut length = (buffer.len() * 2) as u32;
        // SAFETY: `buffer` holds at least `length` bytes.
        let status = unsafe {
            sys::RegQueryValueExW(
                key.0,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                buffer.as_mut_ptr().cast::<u8>(),
                &mut length,
            )
        };
        if status != 0 {
            return None;
        }
        let value = sys::from_wide(&buffer);
        (!value.is_empty()).then_some(value)
    }

    pub fn write(command: &str) -> Result<(), String> {
        let key = open(sys::KEY_WRITE)?;
        let name = sys::to_wide(VALUE_NAME);
        let value = sys::to_wide(command);
        // Bytes, including the terminator: a `REG_SZ` written without it is read as running past
        // its own end by some consumers.
        let bytes = (value.len() * 2) as u32;
        // SAFETY: `value` outlives the call and `bytes` is its real length in bytes.
        let status = unsafe {
            sys::RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                sys::REG_SZ,
                value.as_ptr().cast::<u8>(),
                bytes,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(format!("the startup entry could not be written (error {status})"))
        }
    }

    pub fn remove() -> Result<(), String> {
        let key = open(sys::KEY_WRITE)?;
        let name = sys::to_wide(VALUE_NAME);
        // SAFETY: `name` outlives the call.
        let status = unsafe { sys::RegDeleteValueW(key.0, name.as_ptr()) };
        // Already absent is success: the caller asked for it to be off and it is off.
        if status == 0 || status as u32 == sys::ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!("the startup entry could not be removed (error {status})"))
        }
    }

    /// An open registry key that closes itself.
    struct Key(sys::HANDLE);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a key this type opened and has not closed.
            unsafe {
                sys::RegCloseKey(self.0);
            }
        }
    }

    fn open(access: u32) -> Result<Key, String> {
        let path = sys::to_wide(RUN_KEY);
        let mut handle: sys::HANDLE = std::ptr::null_mut();
        // SAFETY: `path` outlives the call and `handle` is a live local the API writes once.
        let status = unsafe {
            sys::RegOpenKeyExW(
                sys::HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                access,
                &mut handle,
            )
        };
        if status == 0 {
            Ok(Key(handle))
        } else {
            Err(format!("the Run key could not be opened (error {status})"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name is what the user sees in Task Manager's startup list, so it is not an
    /// implementation detail that can be renamed freely.
    #[test]
    fn the_value_name_is_the_one_the_user_sees() {
        assert_eq!(VALUE_NAME, "ghub-rs");
        assert!(RUN_KEY.ends_with(r"CurrentVersion\Run"));
        assert!(
            !RUN_KEY.starts_with('\\') && !RUN_KEY.to_lowercase().contains("machine"),
            "the machine-wide key needs administrator rights and is deliberately not used"
        );
    }

    /// A path with a space must be quoted, or Windows runs `C:\Program` with `Files\...` as an
    /// argument. This is the one formatting rule the value has.
    #[test]
    fn the_command_is_quoted() {
        let command = format!("\"{}\"", r"C:\Program Files\ghub-rs\ghub.exe");
        assert!(command.starts_with('"') && command.ends_with('"'));
        assert_eq!(command.matches('"').count(), 2);
    }

    /// Reading the current state must never fail or panic, because Options reads it on every
    /// load; a machine with the key absent simply reports "off".
    #[test]
    fn reading_the_state_is_always_answerable() {
        let enabled = is_enabled();
        assert_eq!(enabled, registered_command().is_some());
    }
}

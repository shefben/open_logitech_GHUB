//! One running copy at a time.
//!
//! Two programs driving the same device's lighting fight each other, and two copies of *this*
//! program would also both install low-level input hooks and both answer on the automation port.
//! So a second launch does not start: it tells the first one to show itself and exits.
//!
//! The mechanism is a named mutex plus a broadcast message, and both halves are deliberate:
//!
//! * **A named mutex, not a lock file.** A lock file left behind by a crash looks exactly like a
//!   running instance, and the user is then told to close a program that is not running. A mutex
//!   is released by the kernel when the process ends, however it ends.
//! * **`Local\`, not `Global\`.** The name is per-session, so two users logged into the same
//!   machine each get their own instance - which is right, because each has their own profile
//!   store and their own devices.

/// The mutex name. `Local\` is the session's own namespace.
pub const MUTEX_NAME: &str = r"Local\ghub-rs.single-instance";

/// What a launch found.
#[derive(Debug, PartialEq, Eq)]
pub enum Instance {
    /// Nothing else is running. The handle must be kept for the lifetime of the process.
    First(Guard),
    /// Another copy is already running and has been asked to show its window.
    AlreadyRunning,
}

/// Holds the mutex. Dropping it releases the name, so it is kept alive for the whole process.
#[derive(Debug, PartialEq, Eq)]
pub struct Guard {
    #[cfg(windows)]
    handle: usize,
    #[cfg(not(windows))]
    _private: (),
}

// SAFETY: the value is a kernel handle, which is process-wide and not bound to a thread. It is
// only ever closed, in `Drop`.
#[cfg(windows)]
unsafe impl Send for Guard {}
#[cfg(windows)]
unsafe impl Sync for Guard {}

#[cfg(windows)]
impl Drop for Guard {
    fn drop(&mut self) {
        if self.handle != 0 {
            // SAFETY: a handle this type created with `CreateMutexW` and has not closed.
            unsafe {
                crate::sys::CloseHandle(self.handle as *mut std::ffi::c_void);
            }
        }
    }
}

/// Claims the single-instance name, asking an existing instance to show itself if there is one.
#[cfg(windows)]
pub fn claim() -> Instance {
    let claimed = claim_named(MUTEX_NAME);
    if claimed == Instance::AlreadyRunning {
        show_existing();
    }
    claimed
}

/// The claim itself, without the broadcast.
///
/// Named rather than fixed so the tests can claim a name of their own: a test that used the real
/// name would fail on a machine where the application is running, and two tests using it would
/// race each other inside one test process.
#[cfg(windows)]
pub fn claim_named(name: &str) -> Instance {
    use crate::sys;

    let wide = sys::to_wide(name);
    // SAFETY: `wide` outlives the call. A null attributes pointer means the default descriptor.
    let handle = unsafe { sys::CreateMutexW(std::ptr::null(), 1, wide.as_ptr()) };
    // SAFETY: reads this thread's last error, which the call above has just set.
    let already = unsafe { sys::GetLastError() } == sys::ERROR_ALREADY_EXISTS;

    if handle.is_null() {
        // The name could not be created at all - an unusual failure, and not a reason to refuse
        // to start: a single instance is a courtesy, and a program that will not run because it
        // cannot create a mutex is worse than two copies of it.
        return Instance::First(Guard { handle: 0 });
    }

    if already {
        // The handle is still ours and has to be closed; what we do not have is the *claim*.
        // SAFETY: a handle just returned by `CreateMutexW`.
        unsafe {
            sys::CloseHandle(handle);
        }
        return Instance::AlreadyRunning;
    }

    Instance::First(Guard { handle: handle as usize })
}

/// Asks a running instance to bring its window forward.
///
/// A broadcast rather than a search for the window: finding it would mean enumerating every
/// top-level window and matching a class name, which is both slower and wrong the moment the
/// window does not exist yet (the first instance may still be starting). A registered message id
/// is unique system-wide, so no other application can mistake it for one of its own.
#[cfg(windows)]
pub fn show_existing() {
    use crate::sys;

    let name = sys::to_wide(crate::tray::SHOW_MESSAGE_NAME);
    // SAFETY: `name` outlives the call.
    let message = unsafe { sys::RegisterWindowMessageW(name.as_ptr()) };
    if message == 0 {
        return;
    }
    // SAFETY: a broadcast of a registered private message. Windows that do not know it ignore it.
    unsafe {
        sys::PostMessageW(sys::HWND_BROADCAST, message, 0, 0);
    }
}

#[cfg(not(windows))]
pub fn claim() -> Instance {
    Instance::First(Guard { _private: () })
}

#[cfg(not(windows))]
pub fn claim_named(_name: &str) -> Instance {
    claim()
}

#[cfg(not(windows))]
pub fn show_existing() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The namespace is the difference between "one copy per logged-in user" and "one copy per
    /// machine". Two users sharing a machine each manage their own devices.
    #[test]
    fn the_name_is_per_session_not_machine_wide() {
        assert!(MUTEX_NAME.starts_with(r"Local\"));
        assert!(!MUTEX_NAME.starts_with(r"Global\"));
    }

    /// A second claim of the same name has to be refused, including from inside one process:
    /// that is the case a lock file gets wrong after a crash.
    #[test]
    #[cfg(windows)]
    fn a_second_claim_of_the_same_name_is_refused() {
        let name = unique("second-claim");
        let first = claim_named(&name);
        assert!(matches!(first, Instance::First(_)));
        assert_eq!(claim_named(&name), Instance::AlreadyRunning);
    }

    /// Dropping the guard has to release the name, or a restart after a clean exit would be
    /// refused.
    #[test]
    #[cfg(windows)]
    fn the_name_is_free_again_after_the_guard_is_dropped() {
        let name = unique("release");
        let first = claim_named(&name);
        assert!(matches!(first, Instance::First(_)));
        drop(first);
        assert!(
            matches!(claim_named(&name), Instance::First(_)),
            "the name was not released"
        );
    }

    /// A name of this test run's own, so the suite never collides with a real instance of the
    /// application on the same machine or with another test.
    #[cfg(windows)]
    fn unique(what: &str) -> String {
        format!(
            r"Local\ghub-rs.test.{what}.{}.{:?}",
            std::process::id(),
            std::thread::current().id()
        )
    }
}

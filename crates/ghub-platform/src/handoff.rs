//! Passing the hardware between the resident agent and the window.
//!
//! The application ships as two executables. `ghub-agent.exe` sits resident: it owns the devices,
//! the low-level input hooks, the automation port and the notification-area icon, and it links no
//! GUI toolkit at all. `ghub.exe` is the window, and it is started and exited rather than hidden,
//! so that while nobody is looking at it the machine is carrying 1.6 MB of agent rather than a GL
//! context, a swapchain and two 20 MB device renders.
//!
//! Exactly one of them may hold the hardware at a time. A HID device opened twice, two sets of
//! low-level hooks or two listeners on the automation port are each their own kind of wrong, so
//! the two processes hand ownership over explicitly:
//!
//! 1. The window asks, by sending [`MESSAGES`]`[`[`TAKEOVER`]`]` to the agent's hidden window with
//!    its own process id in `wParam`.
//! 2. The agent closes every device session, uninstalls the hooks, stops the listener, and then
//!    sets the named event [`RELEASED_EVENT_NAME`].
//! 3. The window waits on that event and only then builds the half of its stack that touches
//!    hardware.
//! 4. On exit the window sends [`RESUME`], and the agent takes everything back. If the window
//!    dies without sending it, the agent is also waiting on the window's process handle, so the
//!    hardware comes back either way.
//!
//! **Why an event and a window message rather than one call.** A message is synchronous for the
//! *sender*, but the work the agent has to do belongs to the thread that owns its stack, not to
//! the thread pumping its window. Doing a full device release inside a window procedure would
//! block the agent's own message pump - including the tray - for as long as the release took.
//! Splitting it means the request is cheap to receive and the answer is a thing the window can
//! wait on with a timeout.
//!
//! The one state this encodes is worth saying plainly: **the event is set exactly when the agent
//! is not holding the hardware.** The agent clears it before it takes anything and sets it after
//! it has let go, so a window that starts while the agent is already released does not wait at
//! all.

/// The registered window messages the agent listens for, in the order [`TAKEOVER`], [`RESUME`] and
/// [`QUIT`] index into.
///
/// Registered rather than invented numbers: `RegisterWindowMessageW` maps a string to the same id
/// in every process on the machine, and to an id no other application can collide with. These are
/// passed to `Tray::start`, which registers them and forwards arrivals as
/// `TrayEvent::Message { index, .. }`.
pub const MESSAGES: [&str; 3] =
    ["ghub-rs.takeover", "ghub-rs.resume", "ghub-rs.quit"];

/// "Give me the hardware." `wParam` is the sender's process id, so the agent can wait on the
/// sender's process handle and recover if it dies.
pub const TAKEOVER: usize = 0;
/// "I am done with the hardware." Sent by the window as it exits.
pub const RESUME: usize = 1;
/// "Exit." Chosen from the window's own menu; the agent is the process that has to act on it,
/// because it is the one that outlives the window.
pub const QUIT: usize = 2;

/// The event that says the agent is not holding the hardware.
///
/// `Local\` rather than `Global\`: both processes run as the same user in the same session, and a
/// global name would be visible to - and claimable by - another session on a shared machine.
pub const RELEASED_EVENT_NAME: &str = r"Local\ghub-rs.hardware-released";

/// The file name of the resident agent, next to whichever executable is asking.
pub const AGENT_EXECUTABLE: &str = "ghub-agent.exe";
/// The file name of the window.
pub const WINDOW_EXECUTABLE: &str = "ghub.exe";

/// The flag that starts the agent without letting it take the hardware.
///
/// Used when the *window* starts the agent, which is the one case where the agent must not scan:
/// the window is about to own the devices itself, and an agent that took them first would perform
/// a full enumeration - six ping timeouts on a machine with a Unifying receiver - purely to give
/// it all back a moment later.
pub const START_RELEASED: &str = "--released";

/// How long the window waits for the agent to let go.
///
/// Generous because a release is real work: every open device gets its spy mapping, its G-keys and
/// its lighting handed back to firmware before the handle is closed, and a sleeping wireless mouse
/// answers that slowly. A timeout here is reported and then overridden - see
/// [`Takeover::TimedOut`] - rather than being fatal, because a window that refuses to open is
/// worse than one that opens with a warning.
pub const TAKEOVER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// What came of asking for the hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takeover {
    /// The agent let go, or there was an agent that had already let go.
    Granted,
    /// No agent is running, and one could not be started. The caller owns the hardware by
    /// default - there is nothing to contend with.
    NoAgent,
    /// An agent answered but did not release inside [`TAKEOVER_TIMEOUT`].
    ///
    /// The caller carries on and takes the hardware anyway. That is the lesser of the two
    /// failures: the devices may briefly be open in both processes, which produces failed
    /// transactions and a diagnostics entry, whereas refusing to continue produces an application
    /// that cannot be opened at all and gives the user no way to find out why.
    TimedOut,
}

#[cfg(windows)]
mod platform {
    use super::*;
    use crate::sys;
    use crate::tray::{WINDOW_CLASS, WINDOW_TITLE};

    /// The agent's hidden window, if an agent is running.
    ///
    /// Found by class *and* title: a class registration belongs to the process that made it and
    /// goes away with it, but matching both is what rules out a half-created window from a process
    /// that is on its way down.
    pub fn agent_window() -> Option<sys::HANDLE> {
        let class = sys::to_wide(WINDOW_CLASS);
        let title = sys::to_wide(WINDOW_TITLE);
        // SAFETY: two NUL-terminated buffers that outlive the call.
        let hwnd = unsafe { sys::FindWindowW(class.as_ptr(), title.as_ptr()) };
        (!hwnd.is_null()).then_some(hwnd)
    }

    /// Sends one of [`MESSAGES`] to the agent, with `wparam`, and reports whether an agent took
    /// it.
    ///
    /// `SendMessageTimeoutW` rather than `PostMessageW` for the ones whose answer matters: a post
    /// to a window that has just gone succeeds and tells the caller nothing, whereas this returns
    /// zero when the target stopped pumping messages.
    pub fn send(message: usize, wparam: usize) -> bool {
        let Some(hwnd) = agent_window() else { return false };
        let Some(name) = MESSAGES.get(message) else { return false };
        let wide = sys::to_wide(name);
        // SAFETY: a registered message id and a window found a moment ago. A window that closes
        // in between makes the send fail, which is the answer this returns.
        unsafe {
            let id = sys::RegisterWindowMessageW(wide.as_ptr());
            if id == 0 {
                return false;
            }
            let mut answer: usize = 0;
            // Three seconds is for the agent to *receive* this, not to act on it. The acting is
            // waited for separately, on the event.
            let sent = sys::SendMessageTimeoutW(
                hwnd,
                id,
                wparam,
                0,
                sys::SMTO_ABORTIFHUNG,
                3_000,
                &mut answer,
            );
            sent != 0 && answer != 0
        }
    }

    /// A handle to the released event, closed on drop.
    pub struct ReleasedEvent(sys::HANDLE);

    impl ReleasedEvent {
        /// Opens the hand-off event, creating it if it does not exist yet.
        ///
        /// Either process may be the one that creates it: the window can start before any agent
        /// does. A named event is created once and opened thereafter, so both doing this is
        /// correct.
        pub fn open() -> Option<Self> {
            Self::open_named(RELEASED_EVENT_NAME)
        }

        /// The same, under a given name.
        ///
        /// Exists for the tests, for the reason `instance::claim_named` exists: a test that
        /// clears or sets the real event would be telling a running agent's next window that the
        /// hardware was free. Its own name makes the test independent of what is running on the
        /// machine.
        pub fn open_named(name: &str) -> Option<Self> {
            let name = sys::to_wide(name);
            // SAFETY: a named manual-reset event, initially unset. The buffer outlives the call.
            let handle =
                unsafe { sys::CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) };
            (!handle.is_null()).then_some(Self(handle))
        }

        /// Says the hardware is free. Called by the agent once its release has finished.
        pub fn signal(&self) {
            // SAFETY: an event handle this type owns.
            unsafe { sys::SetEvent(self.0) };
        }

        /// Says the hardware is taken. Called by the agent before it starts taking it back, so a
        /// window starting at that moment waits rather than racing.
        pub fn clear(&self) {
            // SAFETY: as `signal`.
            unsafe { sys::ResetEvent(self.0) };
        }

        /// Waits for the hardware to be free. True if it is, false on timeout.
        pub fn wait(&self, timeout: std::time::Duration) -> bool {
            let millis = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
            // SAFETY: as `signal`.
            unsafe { sys::WaitForSingleObject(self.0, millis) == sys::WAIT_OBJECT_0 }
        }
    }

    impl Drop for ReleasedEvent {
        fn drop(&mut self) {
            // SAFETY: a handle this type owns and has not closed.
            unsafe { sys::CloseHandle(self.0) };
        }
    }

    /// An executable beside the one that is running.
    ///
    /// Beside, rather than looked up on `PATH` or in the registry: the two binaries are installed
    /// together and are versioned together, and finding a *different* copy of the agent than the
    /// window was installed with is a way to end up running two incompatible halves.
    pub fn sibling_executable(name: &str) -> Option<std::path::PathBuf> {
        let mut buffer = [0u16; 32_768];
        // SAFETY: a buffer of the length passed, filled by the OS.
        let written = unsafe {
            sys::GetModuleFileNameW(
                std::ptr::null_mut(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        };
        if written == 0 {
            return None;
        }
        let own = std::path::PathBuf::from(sys::from_wide(&buffer[..written as usize]));
        let directory = own.parent()?;
        let candidate = directory.join(name);
        candidate.is_file().then_some(candidate)
    }

    /// Starts a sibling executable, detached. True if the shell accepted it.
    pub fn spawn(name: &str, arguments: &str) -> bool {
        let Some(path) = sibling_executable(name) else { return false };
        let file = sys::to_wide(&path.display().to_string());
        let parameters = sys::to_wide(arguments);
        let directory = path
            .parent()
            .map(|p| sys::to_wide(&p.display().to_string()))
            .unwrap_or_else(|| sys::to_wide(""));
        // SAFETY: four NUL-terminated buffers that outlive the call. `ShellExecuteW` returns a
        // value above 32 on success; below that it is an error code, not a handle.
        let result = unsafe {
            sys::ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                file.as_ptr(),
                if arguments.is_empty() { std::ptr::null() } else { parameters.as_ptr() },
                directory.as_ptr(),
                sys::SW_SHOWNORMAL,
            )
        };
        result as isize > 32
    }

    /// Waits for a process to exit. `None` waits for as long as it takes.
    ///
    /// True means it has exited. False means either that the wait timed out or that the process
    /// could not be opened at all - and the caller must treat those the same, because an
    /// already-exited pid is indistinguishable from a bad one from here. Both mean "stop waiting
    /// and check": the agent's answer to either is to take the hardware back, which is idempotent.
    ///
    /// Only `SYNCHRONIZE` is asked for. The agent has no business reading the window's memory or
    /// terminating it; it only needs to know when it is gone.
    pub fn wait_for_process(pid: u32, timeout: Option<std::time::Duration>) -> bool {
        // `INFINITE`. Used by the watcher thread, which has nothing else to do and no deadline:
        // polling instead would be a timer running for the life of every window session.
        const INFINITE: u32 = 0xFFFF_FFFF;
        let millis = match timeout {
            // `u32::MAX` is `INFINITE`, so a finite wait is clamped below it rather than onto it.
            Some(timeout) => timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32,
            None => INFINITE,
        };
        // SAFETY: a pid from the message the window sent. An invalid or exited pid fails to open,
        // which is reported rather than waited on.
        unsafe {
            let handle = sys::OpenProcess(sys::SYNCHRONIZE, 0, pid);
            if handle.is_null() {
                return false;
            }
            let result = sys::WaitForSingleObject(handle, millis);
            sys::CloseHandle(handle);
            result == sys::WAIT_OBJECT_0
        }
    }

    /// What to do with a window belonging to another process.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Act {
        /// Restore and bring to the front.
        Raise,
        /// Ask it to close, the same way its own close button does.
        Close,
    }

    struct Search {
        pid: u32,
        act: Act,
        /// Whether a window was found at all. The caller reports this rather than assuming: a
        /// window that has not finished creating its first frame has no visible top-level window
        /// yet, and saying "done" then would be a lie.
        found: bool,
    }

    /// SAFETY: called by `EnumWindows` with `lparam` the pointer passed in `for_each_window`.
    unsafe extern "system" fn visit(hwnd: sys::HANDLE, lparam: sys::LPARAM) -> sys::BOOL {
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut owner = 0u32;
        unsafe {
            sys::GetWindowThreadProcessId(hwnd, &mut owner);
            if owner != search.pid || sys::IsWindowVisible(hwnd) == 0 {
                return 1;
            }
            match search.act {
                Act::Raise => {
                    // Restore first: a minimised window that is only brought forward stays
                    // minimised, and the click appears to have done nothing.
                    sys::ShowWindow(hwnd, sys::SW_RESTORE);
                    sys::SetForegroundWindow(hwnd);
                }
                // `WM_CLOSE`, not a terminate: this is the message the window's own close button
                // sends, so the window runs its exit path - handing the hardware back and telling
                // this process to take it - instead of being killed with devices open.
                Act::Close => {
                    sys::PostMessageW(hwnd, sys::WM_CLOSE, 0, 0);
                }
            }
            search.found = true;
        }
        // Keep going for `Close`, because a toolkit may own more than one visible top-level
        // window and leaving one behind would leave the process running.
        sys::BOOL::from(search.act == Act::Close)
    }

    fn for_each_window(pid: u32, act: Act) -> bool {
        let mut search = Search { pid, act, found: false };
        // SAFETY: a function pointer valid for the call and a pointer to a live local.
        unsafe {
            if act == Act::Raise {
                // The agent is not the foreground process, so without this the foreground change
                // is refused and the window flashes in the taskbar instead of coming forward.
                sys::AllowSetForegroundWindow(sys::ASFW_ANY);
            }
            sys::EnumWindows(visit, &mut search as *mut Search as sys::LPARAM);
        }
        search.found
    }

    /// Brings a process's visible top-level windows to the front.
    ///
    /// How the agent answers "show the window" when the window is already running: it has the pid
    /// from the take-over message, and the window is drawn by a toolkit whose window class is not
    /// ours to rely on, so the lookup is by owning process rather than by class.
    pub fn raise_windows_of(pid: u32) -> bool {
        for_each_window(pid, Act::Raise)
    }

    /// Asks a process's windows to close, as their own close buttons would.
    ///
    /// Used for Quit: the agent outlives the window, so quitting from the tray has to end both,
    /// and ending the window politely is what lets it give the hardware back on the way out.
    pub fn close_windows_of(pid: u32) -> bool {
        for_each_window(pid, Act::Close)
    }

    /// Asks the agent for the hardware and waits until it has let go.
    ///
    /// `start_agent` decides what happens when no agent is running: the window passes true, so
    /// that the agent - which owns the tray icon and is what keeps profile rules running after
    /// the window closes - exists even when the user started the window directly. It is started
    /// with [`START_RELEASED`], so it comes up already released and no hardware changes hands
    /// twice.
    pub fn request_takeover(pid: u32, start_agent: bool) -> Takeover {
        let event = ReleasedEvent::open();

        if agent_window().is_none() {
            if !start_agent || !spawn(AGENT_EXECUTABLE, START_RELEASED) {
                return Takeover::NoAgent;
            }
            // Wait for its window rather than for a fixed delay: the agent opens a profile store
            // and registers an icon first, and how long that takes is not ours to guess.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while agent_window().is_none() {
                if std::time::Instant::now() > deadline {
                    return Takeover::NoAgent;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }

        if !send(TAKEOVER, pid as usize) {
            return Takeover::NoAgent;
        }
        match event {
            Some(event) if event.wait(TAKEOVER_TIMEOUT) => Takeover::Granted,
            Some(_) => Takeover::TimedOut,
            // The event could not even be opened, which means no agent ever created it and this
            // process is alone with the hardware.
            None => Takeover::NoAgent,
        }
    }

    /// Tells the agent the hardware is free again. Best effort by design: this runs while the
    /// window is exiting, and an agent that has already gone is not a failure.
    pub fn notify_resume(pid: u32) {
        send(RESUME, pid as usize);
    }

    /// Tells the agent to exit, for Quit chosen in the window rather than in the tray.
    pub fn request_quit() -> bool {
        send(QUIT, 0)
    }

    /// Whether an agent is running right now.
    ///
    /// Read live rather than remembered from the hand-over: the agent can be quit from its own
    /// menu, or started, while a window is open, and the window's diagnostics says which is the
    /// case. The window owns the hardware either way.
    pub fn agent_running() -> bool {
        agent_window().is_some()
    }
}

#[cfg(windows)]
pub use platform::{
    ReleasedEvent, agent_running, agent_window, close_windows_of, notify_resume, raise_windows_of, request_quit,
    request_takeover, send, sibling_executable, spawn, wait_for_process,
};

/// Off Windows there is no second binary to hand anything to: the agent is a Windows service in
/// all but name, and the window is built on Linux only to work on the layout. Every call reports
/// "there is no agent", which leaves the window owning its own hardware - the behaviour the
/// Avalonia build had.
#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn request_takeover(_pid: u32, _start_agent: bool) -> Takeover {
        Takeover::NoAgent
    }
    pub fn notify_resume(_pid: u32) {}
    pub fn request_quit() -> bool {
        false
    }
    pub fn agent_running() -> bool {
        false
    }
}

#[cfg(not(windows))]
pub use platform::{agent_running, notify_resume, request_quit, request_takeover};

#[cfg(test)]
mod tests {
    use super::*;

    /// The indices are an API: the agent matches on them and the window sends them. A reordered
    /// list would silently turn a resume into a quit.
    #[test]
    fn the_message_indices_name_the_right_messages() {
        assert_eq!(MESSAGES[TAKEOVER], "ghub-rs.takeover");
        assert_eq!(MESSAGES[RESUME], "ghub-rs.resume");
        assert_eq!(MESSAGES[QUIT], "ghub-rs.quit");
        assert_eq!(MESSAGES.len(), 3);
    }

    /// `Local\`, not `Global\`: a global name is reachable from every session on the machine, so
    /// another logged-in user could set it and tell this user's window the hardware was free.
    #[test]
    fn the_event_is_scoped_to_the_session() {
        assert!(RELEASED_EVENT_NAME.starts_with(r"Local\"));
    }

    /// The window starting the agent must not make the agent scan: the whole point of that path
    /// is that the window is about to own the devices itself.
    #[test]
    fn the_window_starts_the_agent_already_released() {
        assert_eq!(START_RELEASED, "--released");
    }

    #[cfg(windows)]
    #[test]
    fn asking_with_no_agent_running_reports_no_agent_rather_than_waiting() {
        // `false`: this must not start a real agent from the test suite. With no agent window and
        // no permission to make one, the answer is immediate.
        assert_eq!(request_takeover(std::process::id(), false), Takeover::NoAgent);
    }

    /// The event's two states are the whole protocol, so they are pinned rather than assumed.
    /// The event's two states are the whole protocol, so they are pinned rather than assumed.
    ///
    /// Under a name of this test's own: the real one is shared with whatever is running on the
    /// machine, and setting it here would tell a live agent's next window that the hardware was
    /// free.
    #[cfg(windows)]
    #[test]
    fn the_released_event_waits_while_clear_and_passes_once_set() {
        let name = format!(
            r"Local\ghub-rs.test.handoff.{}.{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let event = ReleasedEvent::open_named(&name).expect("the named event opens");
        assert!(!event.wait(std::time::Duration::from_millis(50)), "a fresh event must wait");
        event.signal();
        assert!(event.wait(std::time::Duration::from_millis(50)), "a set event must pass");
        event.clear();
        assert!(!event.wait(std::time::Duration::from_millis(50)), "a cleared event must wait");
    }

    #[cfg(windows)]
    #[test]
    fn the_sibling_lookup_finds_the_test_binary_itself() {
        // The test executable's own file name, which is by definition beside itself. Proves the
        // directory arithmetic without depending on either shipped binary being built.
        let own = std::env::current_exe().expect("the test binary has a path");
        let name = own.file_name().expect("it has a file name").to_string_lossy().into_owned();
        assert_eq!(sibling_executable(&name).as_deref(), Some(own.as_path()));
        assert_eq!(sibling_executable("ghub-rs.no-such-file.exe"), None);
    }
}

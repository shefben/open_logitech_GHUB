//! The resident half of the application: devices, lighting, rules, macros and the tray icon, with
//! no GUI toolkit linked into it at all.
//!
//! This is what G HUB itself does - `lghub_agent.exe` runs all day and `lghub.exe` is the window -
//! and for the same reason. Measured on this machine, the window costs about 390 MB of working set
//! and 467 MB of private bytes once a GL context, a swapchain and the two device renders (3600 x
//! 1374 and 1440 x 2858 pixels) are resident. Almost none of that is needed to keep a keyboard
//! lit, and the application used to pay all of it for as long as the user left it running, because
//! closing the window only hid it.
//!
//! So the window closes for real now, and this binary is what stays:
//!
//! * It owns the devices, the low-level input hooks, the profile rules, the macro runtime, the
//!   automation API and the OpenRGB integration - all of it through [`ghub_host::Stack`], the same
//!   type the window builds, so neither half can drift from the other.
//! * It owns the notification-area icon, because that icon has to work when no window exists.
//! * It hands the hardware to the window when the window starts and takes it back when the window
//!   exits, including when the window exits by crashing. See [`ghub_platform::handoff`] for the
//!   protocol and why it is a message plus an event rather than one call.
//!
//! It is not a service and does not elevate: it runs as the user, from their own `Run` key, and
//! can be quit from its own menu.

// A tray application, not a console one. Debug builds keep the console so `env_logger` output is
// visible while working on it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod icon;
mod tray_menu;

use ghub_host::Stack;
use ghub_platform::handoff;
use ghub_platform::tray::{Tray, TrayEvent};

/// The agent's own single-instance name.
///
/// Its own, not the window's: both may run at once, and that is the normal case. Two *agents* must
/// not, because each would install hooks and drive the same devices.
const MUTEX_NAME: &str = r"Local\ghub-rs.agent";

/// How long the loop sleeps when nothing woke it.
///
/// It is woken by the tray thread whenever an event arrives, so this is only the ceiling on
/// noticing something nothing signals: a profile that the rules switched on a foreground change,
/// which moves the radio dot in a menu the user is not currently holding open. A second is well
/// under the time it takes to right-click.
const TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// How long Quit waits for the window to close before exiting anyway.
///
/// The window is asked with `WM_CLOSE`, which runs its exit path - handing devices back to
/// firmware. Waiting is what lets that finish; exiting anyway after the wait is what stops a wedged
/// window from making Quit do nothing.
const QUIT_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

fn main() {
    env_logger::init();

    if ghub_platform::instance::claim_named(MUTEX_NAME)
        == ghub_platform::instance::Instance::AlreadyRunning
    {
        // Silent: this is the normal outcome of the window starting an agent while one is already
        // there, and of a `Run` key entry firing after a manual start.
        return;
    }

    // `--released`: started by the window, which is about to own the devices itself. Taking them
    // first would mean a full enumeration - six ping timeouts on a machine with a Unifying
    // receiver - performed only to give it all back a moment later.
    let start_released = std::env::args().any(|argument| argument == handoff::START_RELEASED);

    let mut stack = Stack::new(!start_released);
    // The agent has no window to show its diagnostics in, so they go to the `log` crate: visible
    // with `RUST_LOG=info` and stderr redirected, which is how a misbehaving agent is debugged.
    stack.log.subscribe(|entry| {
        use ghub_core::diagnostics::DiagnosticLevel;
        let level = match entry.level {
            DiagnosticLevel::Trace => log::Level::Trace,
            DiagnosticLevel::Info => log::Level::Info,
            DiagnosticLevel::Warning => log::Level::Warn,
            DiagnosticLevel::Error => log::Level::Error,
        };
        match &entry.detail {
            Some(detail) => log::log!(level, "[{}] {} ({detail})", entry.source, entry.message),
            None => log::log!(level, "[{}] {}", entry.source, entry.message),
        }
    });
    // Opened before anything is handed over, and *always* set to match reality: the whole protocol
    // is "this event is set exactly when the agent is not holding the hardware".
    let released = handoff::ReleasedEvent::open();
    sync_event(&released, &stack);

    let icon = match icon::tray_image() {
        Ok(icon) => Some(icon),
        Err(e) => {
            stack.log.error("Tray", format!("no notification-area icon: {e}"));
            None
        }
    };

    // The tray runs its own thread with its own window and message pump, and wakes this one. The
    // handle it is given is this thread's, so an event parks for at most as long as it takes to
    // unpark.
    let main_thread = std::thread::current();
    let tray = icon.and_then(|icon| {
        match Tray::start(icon, tray_menu::TOOLTIP, &handoff::MESSAGES, move || {
            main_thread.unpark()
        }) {
            Ok(tray) => Some(tray),
            Err(e) => {
                stack.log.error("Tray", format!("no notification-area icon: {e}"));
                None
            }
        }
    });

    if tray.is_none() {
        // Without the tray there is no window to receive a take-over request and no menu to quit
        // from, so this process would sit holding the hardware with no way to reach it. Giving up
        // leaves the window able to run on its own, which is what it did before the split.
        stack.log.error(
            "Agent",
            "the agent has no notification-area icon and no way to be reached, so it is exiting; \
             the window will run on its own"
                .to_owned(),
        );
        stack.shutdown();
        return;
    }
    let tray = tray.expect("checked just above");

    stack.log.info(
        "Agent",
        if start_released {
            "Agent started by the window; the window owns the devices".to_owned()
        } else {
            "Agent started and holding the devices".to_owned()
        },
    );

    let mut agent = Agent { stack, released, window: None, quitting: false };
    let mut menu_state = tray_menu::TrayMenu::new(std::sync::Arc::clone(&agent.stack.service));

    while !agent.quitting {
        for event in tray.drain() {
            match event {
                TrayEvent::Message { index, wparam } => agent.on_message(index, wparam as u32),
                other => {
                    let outcome = menu_state.handle(other, !agent.stack.holding());
                    if outcome.show_window {
                        agent.show_window();
                    }
                    if outcome.quit {
                        agent.begin_quit();
                    }
                }
            }
        }

        agent.pump_service_events();

        // Keyed on holding the hardware rather than on a window existing, because what
        // the collapsed menu protects is the *document*, and this process stops owning that
        // the moment it releases - which is a little before the window's pid is known when the
        // window is the one that started this process.
        if let Some(items) = menu_state.refresh(!agent.stack.holding()) {
            tray.set_menu(items);
        }

        if !agent.quitting {
            std::thread::park_timeout(TICK);
        }
    }

    let mut tray = tray;
    // The icon first: an unregistered tray icon stays in the notification area as a ghost until
    // the user happens to hover over it.
    tray.stop();
    agent.stack.shutdown();
}

/// Everything the loop acts on.
struct Agent {
    stack: Stack,
    released: Option<handoff::ReleasedEvent>,
    /// The window's process id while a window is running.
    window: Option<u32>,
    quitting: bool,
}

impl Agent {
    /// Makes the released event say what is actually true.
    ///
    /// Called after every ownership change rather than at each call site, because the one way this
    /// protocol can fail quietly is for the event and the hardware to disagree: a window starting
    /// then either waits for a release that already happened, or proceeds while the devices are
    /// still open here.
    fn sync(&self) {
        sync_event(&self.released, &self.stack);
    }

    /// One of [`handoff::MESSAGES`], forwarded from the tray's window procedure.
    fn on_message(&mut self, index: usize, pid: u32) {
        match index {
            handoff::TAKEOVER => self.release_to(pid),
            handoff::RESUME => self.take_back(pid),
            handoff::QUIT => self.begin_quit(),
            _ => {}
        }
    }

    /// Hands the hardware to the window and starts watching its process.
    fn release_to(&mut self, pid: u32) {
        if pid == 0 {
            return;
        }
        self.window = Some(pid);
        if self.stack.holding() {
            self.stack.log.info("Agent", format!("Releasing the devices to the window (pid {pid})"));
            self.stack.release();
        }
        self.sync();
        self.watch(pid);
    }

    /// Takes the hardware back, if `pid` is the window this process let go for.
    ///
    /// The pid check is what makes this idempotent, and it has to be: the window sends a resume as
    /// it exits *and* the watcher below sends one when the process goes, so the common case is two
    /// resumes for one window. Without the check, the second would be treated as a fresh one and
    /// would re-scan every device for nothing.
    fn take_back(&mut self, pid: u32) {
        if self.window != Some(pid) {
            return;
        }
        self.window = None;
        if self.quitting {
            // Quit asked the window to close and is waiting for exactly this. Taking the devices
            // back here only to hand them to firmware a moment later would add a full enumeration
            // to the time Quit takes.
            return;
        }
        // Cleared *before* the devices are taken, so a window starting at this instant waits for
        // the scan rather than racing it.
        if let Some(released) = &self.released {
            released.clear();
        }
        self.stack.log.info("Agent", format!("The window (pid {pid}) exited; taking the devices"));
        self.stack.resume();
        self.sync();
    }

    /// Watches the window's process so a window that crashes still gives the hardware back.
    ///
    /// A thread per window, which is at most one at a time: the window holds its own
    /// single-instance claim. It reports through the same message the window would have sent, so
    /// there is one path into [`Agent::take_back`] rather than two.
    fn watch(&self, pid: u32) {
        let _ = std::thread::Builder::new().name("ghub window watch".to_owned()).spawn(move || {
            // No timeout: the thread has nothing else to do, and a window may be left open for
            // days. False here means the process could not be opened at all, which is what an
            // already-exited pid looks like - the same answer either way.
            handoff::wait_for_process(pid, None);
            handoff::send(handoff::RESUME, pid as usize);
        });
    }

    /// Raises the window if it is running, starts it if it is not.
    fn show_window(&mut self) {
        if let Some(pid) = self.window {
            if handoff::raise_windows_of(pid) {
                return;
            }
            // A window whose process is known but has no visible top-level window yet: it is
            // still starting. Starting a second one would be refused by its own single-instance
            // claim anyway, so this waits for the one that is coming.
            self.stack.log.info("Agent", "The window is still starting".to_owned());
            return;
        }
        if !handoff::spawn(handoff::WINDOW_EXECUTABLE, "") {
            self.stack.log.error(
                "Agent",
                format!(
                    "{} is not beside the agent, so the window could not be started",
                    handoff::WINDOW_EXECUTABLE
                ),
            );
        }
        // No hardware changes hands here. The window asks for it itself once it is up, which is
        // the same path it takes when the user starts it from a shortcut.
    }

    /// Quit: end the window too, politely, then exit.
    fn begin_quit(&mut self) {
        self.quitting = true;
        let Some(pid) = self.window else { return };

        // `WM_CLOSE`, not a terminate: the window runs its exit path, which hands the devices back
        // to firmware. Killing it would leave a keyboard dark and its G-keys inert.
        if !handoff::close_windows_of(pid) {
            self.stack.log.warning(
                "Agent",
                format!("The window (pid {pid}) could not be asked to close"),
            );
            return;
        }
        if !handoff::wait_for_process(pid, Some(QUIT_GRACE)) {
            self.stack.log.warning(
                "Agent",
                format!(
                    "The window (pid {pid}) did not close within {}s; quitting anyway",
                    QUIT_GRACE.as_secs()
                ),
            );
        }
        self.window = None;
    }

    /// Drains the service's own event queue and reconciles what changed.
    ///
    /// The queue has to be drained by somebody or it grows for the life of the process - in the
    /// window that is the frame loop's job. The only event this half acts on is a document change,
    /// for the same reason the window acts on it: the settings in `app` can be changed by the
    /// automation API or by a restored backup, and neither of those is a page that could apply
    /// them itself.
    fn pump_service_events(&mut self) {
        let mut document_changed = false;
        for event in self.stack.drain_events() {
            if matches!(event, ghub_core::service::ServiceEvent::DocumentChanged) {
                document_changed = true;
            }
        }
        // Read to clear it. Nothing here redraws, so a device arriving needs no further action -
        // the manager has already applied the active profile to it.
        let _ = self.stack.take_device_change();
        if document_changed {
            self.stack.apply_app_settings();
        }
    }
}

/// Sets or clears the released event to match whether the stack is holding the hardware.
fn sync_event(released: &Option<handoff::ReleasedEvent>, stack: &Stack) {
    let Some(released) = released else { return };
    if stack.holding() {
        released.clear();
    } else {
        released.signal();
    }
}

#[cfg(test)]
mod tests {
    /// The agent's single-instance name must be its own: the window and the agent run together in
    /// the normal case, and sharing a name would make each refuse to start while the other was up.
    #[test]
    fn the_agent_claims_a_different_name_from_the_window() {
        assert_ne!(super::MUTEX_NAME, ghub_platform::instance::MUTEX_NAME);
        // Session-scoped, like the window's: a global name would be claimable from another
        // logged-in user's session on a shared machine.
        assert!(super::MUTEX_NAME.starts_with(r"Local\"));
    }
}

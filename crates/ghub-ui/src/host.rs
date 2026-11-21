//! The window's half of the host: a worker thread, and a wrapper round the shared stack.
//!
//! This is the port target of two things the C# got from its host rather than from a file of its
//! own: Artemis's dependency-injection container, which built the store, the provider, the device
//! manager and the service and handed them to every tab as `IPeripheralsService`, and
//! `PeripheralTabBase.Run`, which pushed device work onto the thread pool with
//! `await Task.Run(...)` so a slow HID transaction could not freeze the window.
//!
//! The container half now lives in [`ghub_host::Stack`], because the resident agent builds the
//! same stack without ever linking egui. What is left here is what only a window needs:
//!
//! * [`Worker`] is a single thread with a job queue. One thread, not a pool, because the jobs are
//!   HID transactions against devices that serialise on their own channel anyway - running two at
//!   once would queue them one layer lower, and a queue the UI can see the length of is easier to
//!   reason about than one inside the driver.
//! * Results come back through a mutex-guarded queue that the UI drains once per frame, which is
//!   the same shape as `Dispatcher.UIThread.Post` without needing a dispatcher.
//! * [`Host`] is the stack plus that worker. It dereferences to the stack, so a page still writes
//!   `shared.host.service` and does not need to know the split happened.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

use ghub_host::Stack;

/// Jobs queued ahead of the worker before the UI starts dropping them.
///
/// A bounded queue rather than an unbounded one: an unbounded queue turns a user holding down a
/// slider into thousands of pending device writes, which arrive long after the control stopped
/// moving. 32 is deep enough that no realistic burst of clicks is refused, and shallow enough
/// that the oldest job in it is still relevant when it runs.
const JOB_QUEUE_DEPTH: usize = 32;

/// Why a job failed, which decides how the banner is worded.
///
/// Two variants rather than one string because `PeripheralTabBase.Run` had two catch blocks with
/// different wording, and the difference is meaningful: a validation failure is the user's input
/// being rejected (`"Keyboard lighting: a profile must have a name"`), while anything else is the
/// operation itself going wrong (`"Keyboard lighting failed: the device is asleep"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    /// `catch (ProfileValidationException e)` - shown as `"{what}: {errors}"`.
    Validation(String),
    /// `catch (Exception e)` - shown as `"{what} failed: {message}"`.
    Failed(String),
}

impl JobError {
    /// Formats the banner exactly as the C# did.
    fn describe(&self, what: &str) -> String {
        match self {
            JobError::Validation(errors) => format!("{what}: {errors}"),
            JobError::Failed(message) => format!("{what} failed: {message}"),
        }
    }
}

/// What a job returns: an optional status message, or a failure.
pub type JobResult = Result<Option<String>, JobError>;

/// One unit of work for the [`Worker`], and what to do with its outcome.
struct Job {
    /// The caption the status line is prefixed with, e.g. `"Read lighting power timers"`.
    what: String,
    /// Runs off the UI thread. `Ok` becomes a status message, `Err` becomes the error banner.
    run: Box<dyn FnOnce() -> JobResult + Send + 'static>,
}

/// What a finished job left for the UI to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Status(String),
    Error(String),
}

/// The background worker: one thread, a bounded job queue, and a drain-once-per-frame result queue.
pub struct Worker {
    jobs: SyncSender<Job>,
    outcomes: Arc<Mutex<Vec<Outcome>>>,
    /// Jobs queued or running. The pages disable their buttons while this is non-zero, which is
    /// what `PeripheralTabBase.Busy` did.
    pending: Arc<AtomicUsize>,
}

impl Worker {
    /// `context` is the egui context to wake when a job finishes.
    ///
    /// This is the piece that makes background work visible at all. egui repaints on input or on
    /// an explicit request, so without it a result that arrives while the window is idle sits
    /// unshown until the user happens to move the pointer - the status line would lag a device
    /// write by however long the user took to touch the mouse. There is no polling equivalent: a
    /// frame that is never drawn cannot notice anything.
    fn new(context: egui::Context) -> Self {
        let (jobs, receiver) = sync_channel::<Job>(JOB_QUEUE_DEPTH);
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let pending = Arc::new(AtomicUsize::new(0));

        let sink = Arc::clone(&outcomes);
        let counter = Arc::clone(&pending);
        // Detached on purpose. The thread ends when the sender drops, which happens when the
        // window closes; joining it would make closing the window wait out whatever HID
        // transaction is in flight, and the service's own shutdown already hands the hardware
        // back.
        let _ = std::thread::Builder::new()
            .name("ghub ui worker".to_owned())
            .spawn(move || {
                for job in receiver {
                    let Job { what, run } = job;
                    // A panicking job must not take the worker with it: the window would stay up
                    // with every button permanently disabled.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                        .unwrap_or_else(|_| {
                            Err(JobError::Failed("the operation panicked".to_owned()))
                        });
                    let outcome = match result {
                        Ok(Some(message)) => Outcome::Status(format!("{what}: {message}")),
                        // `Status = result ?? $"{what}: done"` - the C#'s own fallback wording.
                        Ok(None) => Outcome::Status(format!("{what}: done")),
                        Err(e) => Outcome::Error(e.describe(&what)),
                    };
                    if let Ok(mut sink) = sink.lock() {
                        sink.push(outcome);
                    }
                    // The count drops only after the outcome is in the queue, so `busy` can
                    // never read clear while a result is still unpublished.
                    counter.fetch_sub(1, Ordering::SeqCst);
                    context.request_repaint();
                }
            });

        Self { jobs, outcomes, pending }
    }

    /// Queues work. Returns false when the queue is full, which the caller reports rather than
    /// silently dropping - a click that did nothing with no explanation is worse than a refusal.
    pub fn spawn<F>(&self, what: impl Into<String>, run: F) -> bool
    where
        F: FnOnce() -> JobResult + Send + 'static,
    {
        let what = what.into();
        self.pending.fetch_add(1, Ordering::SeqCst);
        match self.jobs.try_send(Job { what, run: Box::new(run) }) {
            Ok(()) => true,
            Err(_) => {
                self.pending.fetch_sub(1, Ordering::SeqCst);
                if let Ok(mut sink) = self.outcomes.lock() {
                    sink.push(Outcome::Error(
                        "The device is still working on the last request. Try again in a moment."
                            .to_owned(),
                    ));
                }
                false
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0
    }

    /// Takes everything finished since the last frame.
    pub fn drain(&self) -> Vec<Outcome> {
        match self.outcomes.lock() {
            Ok(mut outcomes) => std::mem::take(&mut *outcomes),
            Err(e) => std::mem::take(&mut *e.into_inner()),
        }
    }

}

/// Everything the pages need from below the UI: the shared stack, plus the worker they run device
/// work on.
///
/// `Deref` rather than re-exported fields. Every page reaches the service, the device manager, the
/// log, the provider and the OpenRGB integration through this type, and twenty forwarding methods
/// would be twenty places for the window and the agent to drift apart.
pub struct Host {
    stack: Stack,
    pub worker: Worker,
}

impl Host {
    /// Builds the stack and the worker. `hold_hardware` is false when the agent still owns the
    /// devices and has not finished handing them over yet.
    pub fn new(context: egui::Context, hold_hardware: bool) -> Self {
        let stack = Stack::new(hold_hardware);
        Self { stack, worker: Worker::new(context) }
    }
}

impl Deref for Host {
    type Target = Stack;

    fn deref(&self) -> &Stack {
        &self.stack
    }
}

impl DerefMut for Host {
    fn deref_mut(&mut self) -> &mut Stack {
        &mut self.stack
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_reports_its_caption_with_the_outcome() {
        let worker = Worker::new(egui::Context::default());
        assert!(worker.spawn("Read battery", || Ok(Some("72%".to_owned()))));
        let outcomes = wait_for(&worker);
        assert_eq!(outcomes, vec![Outcome::Status("Read battery: 72%".to_owned())]);
    }

    #[test]
    fn a_job_with_no_message_still_says_it_finished() {
        let worker = Worker::new(egui::Context::default());
        worker.spawn("Save", || Ok(None));
        assert_eq!(wait_for(&worker), vec![Outcome::Status("Save: done".to_owned())]);
    }

    /// The two failure wordings are the C#'s two catch blocks, and they are not interchangeable:
    /// one says the input was rejected, the other says the operation broke.
    #[test]
    fn a_failure_keeps_the_reason_the_backend_gave() {
        let worker = Worker::new(egui::Context::default());
        worker.spawn("Set DPI", || Err(JobError::Failed("the device is asleep".to_owned())));
        assert_eq!(
            wait_for(&worker),
            vec![Outcome::Error("Set DPI failed: the device is asleep".to_owned())]
        );
    }

    #[test]
    fn a_rejected_edit_reads_as_the_rule_it_broke() {
        let worker = Worker::new(egui::Context::default());
        worker.spawn("Profile", || Err(JobError::Validation("a profile needs a name".to_owned())));
        assert_eq!(
            wait_for(&worker),
            vec![Outcome::Error("Profile: a profile needs a name".to_owned())]
        );
    }

    /// A panicking job is the one case where the worker could strand the UI with every control
    /// disabled, so the recovery is pinned.
    #[test]
    fn a_panicking_job_does_not_take_the_worker_with_it() {
        let worker = Worker::new(egui::Context::default());
        worker.spawn("Bad", || panic!("boom"));
        assert_eq!(
            wait_for(&worker),
            vec![Outcome::Error("Bad failed: the operation panicked".to_owned())]
        );

        worker.spawn("Good", || Ok(None));
        assert_eq!(wait_for(&worker), vec![Outcome::Status("Good: done".to_owned())]);
        assert!(!worker.busy());
    }

    /// Every queued job eventually reports, and `busy` goes clear afterwards.
    ///
    /// `busy` is polled rather than asserted the instant the last outcome arrives: the worker
    /// pushes an outcome *before* decrementing the pending count, so there is a window where the
    /// result is visible and the counter has not dropped yet. That ordering is deliberate - a
    /// result must never be announced as finished while it is still only queued - so the test
    /// waits for the counter instead of racing it.
    #[test]
    fn busy_is_clear_once_the_queue_drains() {
        let worker = Worker::new(egui::Context::default());
        for i in 0..5 {
            worker.spawn(format!("Job {i}"), || Ok(None));
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut seen = Vec::new();
        while seen.len() < 5 {
            seen.extend(worker.drain());
            assert!(std::time::Instant::now() < deadline, "only {} of 5 jobs reported", seen.len());
            std::thread::yield_now();
        }
        assert_eq!(seen.len(), 5);

        while worker.busy() {
            assert!(std::time::Instant::now() < deadline, "busy never cleared");
            std::thread::yield_now();
        }
    }

    /// The bounded queue is the whole reason a held-down slider cannot bury the device, so the
    /// refusal has to be visible rather than silent.
    #[test]
    fn an_overfull_queue_says_so_instead_of_dropping_the_click() {
        let worker = Worker::new(egui::Context::default());
        let gate = Arc::new(std::sync::Barrier::new(2));
        let held = Arc::clone(&gate);
        worker.spawn("Blocker", move || {
            held.wait();
            Ok(None)
        });

        // Fill the queue behind the blocked job, then overflow it. The first job may already have
        // been taken off the channel, so the overflow needs one more than the queue is deep.
        let mut refusals = 0;
        for i in 0..JOB_QUEUE_DEPTH + 2 {
            if !worker.spawn(format!("Job {i}"), || Ok(None)) {
                refusals += 1;
            }
        }
        assert!(refusals > 0, "the queue should have refused at least one job");

        let waiting = worker.drain();
        assert!(
            waiting.iter().any(|o| matches!(o, Outcome::Error(m) if m.contains("still working"))),
            "a refusal must leave a message, got {waiting:?}"
        );
        gate.wait();
    }


    /// Blocks until the worker produces something. The worker is a real thread, so a test either
    /// polls or sleeps a fixed time; polling keeps the suite fast and cannot be flaky.
    fn wait_for(worker: &Worker) -> Vec<Outcome> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let outcomes = worker.drain();
            if !outcomes.is_empty() {
                return outcomes;
            }
            assert!(std::time::Instant::now() < deadline, "the worker produced nothing");
            std::thread::yield_now();
        }
    }
}

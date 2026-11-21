//! Executes actions and software macros. Port of
//! `Artemis.Peripherals/Actions/MacroRuntime.cs`.
//!
//! The guarantees are the point of this module, and each one exists because of a way a macro
//! engine ruins someone's day:
//!
//! - **Bounded loops.** A `WhileHeld` macro whose release event is lost stops at
//!   [`MacroDefinition::max_loop_iterations`] rather than typing until the cable comes out.
//! - **Everything gets released.** Each run has its own [`TrackingInjector`], so stopping a run
//!   releases what it pressed and nothing else.
//! - **No recursion.** A macro cannot call itself, directly or through another macro, and nesting
//!   stops at [`MAX_NESTED_MACRO_DEPTH`].
//! - **Releases use the action from press time.** The binding may have changed in between - bank
//!   switch, G-Shift, profile change - and releasing the *new* action would leave the old one's
//!   key down forever.
//!
//! The C# was `async` and queued every binding onto a per-trigger task chain so the keyboard hook
//! could not block. Here a binding that cannot sleep (key, mouse button, DPI, G-Shift) runs
//! inline - it is one `SendInput` call, which is what every macro tool does from a hook - and
//! only the things that *do* sleep (macros, chords, launching a program) get a thread. That keeps
//! press/release ordering exact without a scheduler, and makes the tests deterministic.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::action::{
    ActionDefinition, DpiCommand, KeyModifiers, KeyStroke, LightingCommand, MacroCondition,
    MacroDefinition, MacroRepeatMode, MacroStep, MediaCommand, ProfileCommand,
};
use crate::id::Id;
use crate::injection::{InputInjector, TrackingInjector};

/// How deep one macro may call another.
pub const MAX_NESTED_MACRO_DEPTH: usize = 4;

/// What the runtime needs from the rest of the application.
pub trait ActionHost: Send + Sync {
    fn find_macro(&self, id: Id) -> Option<MacroDefinition>;
    /// DPI stage change. `pressed` is false for the release of a held DPI-shift.
    fn dpi(&self, command: DpiCommand, stage: usize, pressed: bool);
    fn set_gshift(&self, held: bool);
    fn profile(&self, command: ProfileCommand, profile_id: Option<Id>);
    fn bank(&self, bank: usize);
    fn lighting(&self, command: LightingCommand);
    /// Whether the *user* is holding a key, for a macro's `KeyHeld` condition. Must not count
    /// keys this runtime synthesised, or a macro that presses Shift would satisfy its own test.
    fn is_user_key_held(&self, virtual_key: u16) -> bool;
    fn foreground_executable_name(&self) -> Option<String>;
    /// Starts a program, file or URL with the user's own token. Never elevates.
    fn launch(&self, target: &str, arguments: Option<&str>);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroRunState {
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroRunStatus {
    pub run_id: Id,
    pub macro_id: Id,
    pub name: String,
    pub state: MacroRunState,
    pub iteration: u32,
    pub detail: Option<String>,
}

/// Why a run stopped, for the status line.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum RunError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Refused(String),
}

type RunResult = std::result::Result<(), RunError>;

struct Run {
    run_id: Id,
    macro_id: Id,
    repeat_mode: MacroRepeatMode,
    cancel: Arc<AtomicBool>,
    released: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Run {
    fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
    }
}

pub struct MacroRuntime {
    injector: Arc<TrackingInjector>,
    host: Arc<dyn ActionHost>,
    /// One run per physical control, so repeat modes and the recursion guard are per control.
    runs: Arc<Mutex<HashMap<String, Run>>>,
    /// The action each trigger was pressed with, kept so the release matches the press.
    held_bindings: Mutex<HashMap<String, ActionDefinition>>,
    /// Threads spawned for sleeping actions, joined on shutdown.
    detached: Mutex<Vec<JoinHandle<()>>>,
    status: Arc<Mutex<Vec<MacroRunStatus>>>,
    /// Bumped by `stop_all`, which makes every run in flight see a cancel.
    generation: AtomicU64,
}

impl MacroRuntime {
    pub fn new(injector: Arc<dyn InputInjector>, host: Arc<dyn ActionHost>) -> Self {
        Self {
            injector: Arc::new(TrackingInjector::new(injector)),
            host,
            runs: Arc::new(Mutex::new(HashMap::new())),
            held_bindings: Mutex::new(HashMap::new()),
            detached: Mutex::new(Vec::new()),
            status: Arc::new(Mutex::new(Vec::new())),
            generation: AtomicU64::new(0),
        }
    }

    pub fn injector(&self) -> &TrackingInjector {
        &self.injector
    }

    /// Triggers with a macro running on them.
    pub fn active_triggers(&self) -> Vec<String> {
        let mut all: Vec<String> = self.runs.lock().unwrap().keys().cloned().collect();
        all.sort();
        all
    }

    /// Macros running right now, as (run id, macro id), for the UI's run list and for refusing
    /// to delete a macro that is mid-playback.
    pub fn running(&self) -> Vec<(Id, Id)> {
        let mut all: Vec<(Id, Id)> =
            self.runs.lock().unwrap().values().map(|r| (r.run_id, r.macro_id)).collect();
        all.sort();
        all
    }

    /// Status updates since the last call, for the UI's run list.
    pub fn take_status(&self) -> Vec<MacroRunStatus> {
        std::mem::take(&mut self.status.lock().unwrap())
    }

    /// Called on a trigger press or release. `trigger` identifies the physical control, such as
    /// `gkey:G3:M1` or `button:XButton1`.
    pub fn on_trigger(&self, trigger: &str, action: Option<&ActionDefinition>, pressed: bool) {
        if !pressed {
            if let Some(run) = self.runs.lock().unwrap().get(trigger) {
                if run.repeat_mode == MacroRepeatMode::WhileHeld {
                    run.release();
                }
            }
            // The action recorded at press time, not whatever is bound now.
            if let Some(action) = self.held_bindings.lock().unwrap().remove(trigger) {
                self.execute_binding(&action, false);
            }
            return;
        }

        let Some(action) = action else { return };
        if let ActionDefinition::MacroRef { macro_id } = action {
            match self.host.find_macro(*macro_id) {
                Some(definition) => self.on_macro_trigger(trigger, definition),
                None => log::warn!("trigger {trigger} references a macro that is not in the profile"),
            }
            return;
        }

        self.held_bindings.lock().unwrap().insert(trigger.to_owned(), action.clone());
        self.execute_binding(action, true);
    }

    /// Releases every binding held under triggers starting with `prefix`, and stops their macros.
    ///
    /// Called on a bank change and on a device disconnect: the bindings the old bank held must
    /// come up before the new bank's take over, or a key stays down under a binding that no
    /// longer exists.
    pub fn release_held(&self, prefix: &str, reason: &str) {
        self.stop_for_trigger_prefix(prefix, reason);
        let held: Vec<(String, ActionDefinition)> = {
            let mut bindings = self.held_bindings.lock().unwrap();
            let keys: Vec<String> =
                bindings.keys().filter(|k| k.starts_with(prefix)).cloned().collect();
            keys.into_iter().filter_map(|k| bindings.remove(&k).map(|a| (k, a))).collect()
        };
        for (_, action) in held {
            self.execute_binding(&action, false);
        }
    }

    fn execute_binding(&self, action: &ActionDefinition, pressed: bool) {
        match action {
            ActionDefinition::Key(stroke) => {
                if pressed {
                    press_modifiers(self.injector.as_ref(), stroke.modifiers);
                    self.injector.key_down(stroke.virtual_key);
                } else {
                    self.injector.key_up(stroke.virtual_key);
                    release_modifiers(self.injector.as_ref(), stroke.modifiers);
                }
            }
            ActionDefinition::MouseButton { button } => {
                if pressed {
                    self.injector.mouse_down(*button);
                } else {
                    self.injector.mouse_up(*button);
                }
            }
            ActionDefinition::Dpi { command, stage } => {
                self.host.dpi(*command, *stage, pressed)
            }
            ActionDefinition::GShift => self.host.set_gshift(pressed),
            // Both are "do nothing", and the difference is upstream: Disabled is still diverted
            // so the key's default behaviour is swallowed, PassThrough is never diverted at all.
            ActionDefinition::PassThrough | ActionDefinition::Disabled => {}
            // Everything else is a tap: it fires on press and the release is nothing.
            other if pressed => {
                let other = other.clone();
                let runtime_injector = self.injector.child();
                let host = Arc::clone(&self.host);
                let cancel = Arc::new(AtomicBool::new(false));
                if sleeps(&other) {
                    self.spawn(move || {
                        let mut context =
                            Context { injector: &runtime_injector, host: host.as_ref(), cancel: &cancel };
                        let _ = execute_once(&mut context, &other, 0, &HashSet::new());
                    });
                } else {
                    let mut context =
                        Context { injector: &runtime_injector, host: host.as_ref(), cancel: &cancel };
                    let _ = execute_once(&mut context, &other, 0, &HashSet::new());
                }
            }
            _ => {}
        }
    }

    fn on_macro_trigger(&self, trigger: &str, definition: MacroDefinition) {
        {
            let runs = self.runs.lock().unwrap();
            if let Some(run) = runs.get(trigger) {
                // Toggle: a second press stops it. Every other mode ignores a press while
                // playing, which is also what stops a macro re-triggering itself.
                if definition.repeat_mode == MacroRepeatMode::Toggle {
                    run.cancel();
                }
                return;
            }
        }
        self.start(trigger, definition);
    }

    /// Runs a macro and waits for it, for the editor's play button.
    pub fn play(&self, definition: MacroDefinition) -> MacroRunState {
        let trigger = format!("editor:{}", definition.id);
        let state = Arc::new(Mutex::new(MacroRunState::Running));
        self.start_with_sink(&trigger, definition, Some(Arc::clone(&state)));

        // Wait for the run to leave the map, which happens as the thread finishes.
        loop {
            let handle = self.runs.lock().unwrap().remove(&trigger).and_then(|mut r| r.handle.take());
            match handle {
                Some(handle) => {
                    let _ = handle.join();
                    break;
                }
                None => {
                    if *state.lock().unwrap() != MacroRunState::Running {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        let final_state = *state.lock().unwrap();
        final_state
    }

    fn start(&self, trigger: &str, definition: MacroDefinition) {
        self.start_with_sink(trigger, definition, None)
    }

    fn start_with_sink(
        &self,
        trigger: &str,
        definition: MacroDefinition,
        final_state: Option<Arc<Mutex<MacroRunState>>>,
    ) {
        let run_id = Id::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));

        self.report(MacroRunStatus {
            run_id,
            macro_id: definition.id,
            name: definition.name.clone(),
            state: MacroRunState::Running,
            iteration: 0,
            detail: None,
        });

        // Its own tracker, so stopping this run releases what it pressed and leaves a key another
        // binding is holding alone.
        let injector = self.injector.child();
        let host = Arc::clone(&self.host);
        let shared_status = Arc::clone(&self.status);

        let handle = {
            let cancel = Arc::clone(&cancel);
            let released = Arc::clone(&released);
            let definition = definition.clone();
            // The run takes itself out of the map as it finishes. Without this, a trigger whose
            // macro has already ended still looks busy, so it could never start a second run.
            let runs = Arc::clone(&self.runs);
            let trigger = trigger.to_owned();
            std::thread::Builder::new()
                .name(format!("ghub-macro-{}", definition.name))
                .spawn(move || {
                    let mut iteration = 0u32;
                    let max = match definition.repeat_mode {
                        MacroRepeatMode::RepeatN => definition.repeat_count.clamp(1, MacroDefinition::MAX_REPEAT_COUNT),
                        MacroRepeatMode::WhileHeld | MacroRepeatMode::Toggle => {
                            definition.max_loop_iterations.clamp(1, 100_000)
                        }
                        _ => 1,
                    };

                    let mut outcome: RunResult = Ok(());
                    let mut context =
                        Context { injector: &injector, host: host.as_ref(), cancel: &cancel };
                    let mut call_stack = HashSet::new();
                    call_stack.insert(definition.id);

                    while iteration < max {
                        iteration += 1;
                        if context.cancelled() {
                            outcome = Err(RunError::Cancelled);
                            break;
                        }
                        outcome = run_steps(&mut context, &definition, &definition.steps, 0, &call_stack);
                        if outcome.is_err() {
                            break;
                        }
                        if definition.repeat_mode == MacroRepeatMode::WhileHeld
                            && released.load(Ordering::SeqCst)
                        {
                            break;
                        }
                        if iteration < max {
                            if !context.sleep(definition.repeat_delay_milliseconds.max(1)) {
                                outcome = Err(RunError::Cancelled);
                                break;
                            }
                            if definition.repeat_mode == MacroRepeatMode::WhileHeld
                                && released.load(Ordering::SeqCst)
                            {
                                break;
                            }
                        }
                    }

                    let mut detail = None;
                    let state = match &outcome {
                        Ok(()) => {
                            if iteration >= max
                                && matches!(
                                    definition.repeat_mode,
                                    MacroRepeatMode::WhileHeld | MacroRepeatMode::Toggle
                                )
                            {
                                detail = Some(format!("Stopped at loop bound ({max} iterations)"));
                            }
                            MacroRunState::Completed
                        }
                        Err(RunError::Cancelled) => MacroRunState::Cancelled,
                        Err(RunError::Refused(why)) => {
                            detail = Some(why.clone());
                            MacroRunState::Failed
                        }
                    };

                    // Never leave anything this run pressed. `release_all` does not fail; a
                    // release the OS refused stays tracked for the next emergency stop.
                    injector.release_all();

                    let finished = MacroRunStatus {
                        run_id,
                        macro_id: definition.id,
                        name: definition.name.clone(),
                        state,
                        iteration,
                        detail,
                    };
                    // Taken out before the status is posted, so a caller that sees the finished
                    // status and then asks for the active triggers gets a consistent answer.
                    // Dropping our own `JoinHandle` here detaches this thread, which is fine: it
                    // is about to return. `stop_all` drains the map before it joins anything, so
                    // it can never be holding this lock while waiting for us.
                    {
                        let mut runs = runs.lock().unwrap();
                        if runs.get(&trigger).is_some_and(|r| r.run_id == run_id) {
                            runs.remove(&trigger);
                        }
                    }
                    shared_status.lock().unwrap().push(finished);
                    if let Some(sink) = final_state {
                        *sink.lock().unwrap() = state;
                    }
                })
                .expect("spawn macro thread")
        };

        self.runs.lock().unwrap().insert(
            trigger.to_owned(),
            Run {
                run_id,
                macro_id: definition.id,
                repeat_mode: definition.repeat_mode,
                cancel,
                released,
                handle: Some(handle),
            },
        );
    }

    /// Emergency stop: cancels every macro and releases everything held.
    pub fn stop_all(&self, reason: &str) {
        log::info!("stopping all macros: {reason}");
        self.generation.fetch_add(1, Ordering::SeqCst);
        let mut runs: Vec<Run> = self.runs.lock().unwrap().drain().map(|(_, r)| r).collect();
        for run in &runs {
            run.cancel();
        }
        for run in &mut runs {
            if let Some(handle) = run.handle.take() {
                let _ = handle.join();
            }
        }
        // Held bindings come up through their own actions first, then anything left over.
        self.release_held("", reason);
        self.join_detached();
        self.injector.release_all();
    }

    /// Cancels macros bound to one device's triggers, for a device that went away.
    pub fn stop_for_trigger_prefix(&self, prefix: &str, reason: &str) {
        let mut stopped: Vec<Run> = {
            let mut runs = self.runs.lock().unwrap();
            let keys: Vec<String> =
                runs.keys().filter(|k| k.starts_with(prefix)).cloned().collect();
            keys.into_iter().filter_map(|k| runs.remove(&k)).collect()
        };
        for run in &stopped {
            run.cancel();
        }
        for run in &mut stopped {
            if let Some(handle) = run.handle.take() {
                let _ = handle.join();
            }
        }
        if !stopped.is_empty() {
            log::info!("stopped {} macro(s) for {prefix}: {reason}", stopped.len());
        }
    }

    fn spawn(&self, work: impl FnOnce() + Send + 'static) {
        let handle = std::thread::Builder::new()
            .name("ghub-action".to_owned())
            .spawn(work)
            .expect("spawn action thread");
        let mut detached = self.detached.lock().unwrap();
        detached.retain(|h| !h.is_finished());
        detached.push(handle);
    }

    fn join_detached(&self) {
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut self.detached.lock().unwrap());
        for handle in handles {
            let _ = handle.join();
        }
    }

    fn report(&self, status: MacroRunStatus) {
        self.status.lock().unwrap().push(status);
    }
}

impl Drop for MacroRuntime {
    fn drop(&mut self) {
        self.stop_all("shutdown");
    }
}

/// Whether an action can block for a noticeable time, and so needs its own thread.
fn sleeps(action: &ActionDefinition) -> bool {
    matches!(
        action,
        ActionDefinition::Chord { .. }
            | ActionDefinition::MacroRef { .. }
            | ActionDefinition::Launch { .. }
    )
}

struct Context<'a> {
    injector: &'a TrackingInjector,
    host: &'a dyn ActionHost,
    cancel: &'a AtomicBool,
}

impl Context<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Sleeps in short slices so a cancel is noticed within a few milliseconds rather than at the
    /// end of a ten-minute delay.
    fn sleep(&self, milliseconds: u32) -> bool {
        let total = Duration::from_millis(
            milliseconds.min(MacroDefinition::MAX_DELAY_MILLISECONDS) as u64,
        );
        let slice = Duration::from_millis(5);
        let mut slept = Duration::ZERO;
        while slept < total {
            if self.cancelled() {
                return false;
            }
            let step = slice.min(total - slept);
            std::thread::sleep(step);
            slept += step;
        }
        !self.cancelled()
    }
}

fn run_steps(
    context: &mut Context<'_>,
    definition: &MacroDefinition,
    steps: &[MacroStep],
    depth: usize,
    call_stack: &HashSet<Id>,
) -> RunResult {
    for step in steps {
        if context.cancelled() {
            return Err(RunError::Cancelled);
        }
        match step {
            MacroStep::KeyDown { virtual_key } => context.injector.key_down(*virtual_key),
            MacroStep::KeyUp { virtual_key } => context.injector.key_up(*virtual_key),
            MacroStep::MouseDown { button } => context.injector.mouse_down(*button),
            MacroStep::MouseUp { button } => context.injector.mouse_up(*button),
            MacroStep::MouseMove { dx, dy } => context.injector.mouse_move(*dx, *dy),
            MacroStep::Wheel { delta, horizontal } => context.injector.wheel(*delta, *horizontal),
            MacroStep::Text { text } => context.injector.type_text(text),
            MacroStep::Delay { milliseconds } => {
                let delay = if definition.use_recorded_delays {
                    *milliseconds
                } else {
                    definition.fixed_delay_milliseconds
                };
                if delay > 0 && !context.sleep(delay) {
                    return Err(RunError::Cancelled);
                }
            }
            MacroStep::Action { action } => {
                execute_nested(context, action, depth, call_stack)?;
            }
            MacroStep::Condition { condition, value, then, otherwise } => {
                let holds = match condition {
                    MacroCondition::KeyHeld => value
                        .parse::<u16>()
                        .is_ok_and(|vk| context.host.is_user_key_held(vk)),
                    MacroCondition::ForegroundApp => context
                        .host
                        .foreground_executable_name()
                        .is_some_and(|name| name.eq_ignore_ascii_case(value)),
                };
                let branch: &[MacroStep] = if holds {
                    then
                } else {
                    otherwise.as_deref().unwrap_or(&[])
                };
                run_steps(context, definition, branch, depth, call_stack)?;
            }
        }
    }
    Ok(())
}

fn execute_nested(
    context: &mut Context<'_>,
    action: &ActionDefinition,
    depth: usize,
    call_stack: &HashSet<Id>,
) -> RunResult {
    if let ActionDefinition::MacroRef { macro_id } = action {
        if call_stack.contains(macro_id) {
            return Err(RunError::Refused(
                "macro recursion blocked: this macro calls itself".into(),
            ));
        }
        if depth >= MAX_NESTED_MACRO_DEPTH {
            return Err(RunError::Refused(format!(
                "macro nesting stopped at {MAX_NESTED_MACRO_DEPTH} levels"
            )));
        }
        let Some(nested) = context.host.find_macro(*macro_id) else {
            return Err(RunError::Refused("nested macro is not in the profile".into()));
        };
        let mut stack = call_stack.clone();
        stack.insert(*macro_id);
        return run_steps(context, &nested, &nested.steps, depth + 1, &stack);
    }
    execute_once(context, action, depth, call_stack)
}

/// Tap semantics for any library action: press and release in one go.
fn execute_once(
    context: &mut Context<'_>,
    action: &ActionDefinition,
    depth: usize,
    call_stack: &HashSet<Id>,
) -> RunResult {
    match action {
        ActionDefinition::Key(stroke) => tap_key(context, stroke),
        ActionDefinition::Chord { steps } => {
            for stroke in steps {
                if context.cancelled() {
                    return Err(RunError::Cancelled);
                }
                tap_key(context, stroke);
                // A chord is a sequence of combinations; applications need a gap between them to
                // see two keystrokes rather than one.
                if !context.sleep(10) {
                    return Err(RunError::Cancelled);
                }
            }
        }
        ActionDefinition::MouseButton { button } => {
            context.injector.mouse_down(*button);
            context.injector.mouse_up(*button);
        }
        ActionDefinition::MouseWheel { delta, horizontal } => {
            context.injector.wheel(*delta, *horizontal)
        }
        ActionDefinition::Text { text } => context.injector.type_text(text),
        ActionDefinition::Media { command } => {
            let vk = media_virtual_key(*command);
            context.injector.key_down(vk);
            context.injector.key_up(vk);
        }
        ActionDefinition::Launch { target, arguments } => {
            context.host.launch(target, arguments.as_deref())
        }
        ActionDefinition::Dpi { command, stage } => {
            context.host.dpi(*command, *stage, true);
            // A shift is a hold; tapping it has to let go again or the sniper DPI sticks.
            if *command == DpiCommand::Shift {
                context.host.dpi(*command, *stage, false);
            }
        }
        ActionDefinition::Profile { command, profile_id } => {
            context.host.profile(*command, *profile_id)
        }
        ActionDefinition::Bank { bank } => context.host.bank(*bank),
        ActionDefinition::Lighting { command } => context.host.lighting(*command),
        ActionDefinition::MacroRef { .. } => {
            return execute_nested(context, action, depth, call_stack);
        }
        ActionDefinition::GShift | ActionDefinition::PassThrough | ActionDefinition::Disabled => {}
    }
    Ok(())
}

fn tap_key(context: &mut Context<'_>, stroke: &KeyStroke) {
    press_modifiers(context.injector, stroke.modifiers);
    context.injector.key_down(stroke.virtual_key);
    context.injector.key_up(stroke.virtual_key);
    release_modifiers(context.injector, stroke.modifiers);
}

/// Left-hand modifiers specifically: a generic VK_SHIFT does reach most applications, but games
/// reading raw input see the left-hand key, which is what a user pressing the binding by hand
/// would have produced.
fn press_modifiers(injector: &TrackingInjector, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) {
        injector.key_down(0xA2);
    }
    if modifiers.contains(KeyModifiers::SHIFT) {
        injector.key_down(0xA0);
    }
    if modifiers.contains(KeyModifiers::ALT) {
        injector.key_down(0xA4);
    }
    if modifiers.contains(KeyModifiers::WINDOWS) {
        injector.key_down(0x5B);
    }
}

/// Released in the reverse order they were pressed, which is what applications watching modifier
/// state expect.
fn release_modifiers(injector: &TrackingInjector, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::WINDOWS) {
        injector.key_up(0x5B);
    }
    if modifiers.contains(KeyModifiers::ALT) {
        injector.key_up(0xA4);
    }
    if modifiers.contains(KeyModifiers::SHIFT) {
        injector.key_up(0xA0);
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        injector.key_up(0xA2);
    }
}

fn media_virtual_key(command: MediaCommand) -> u16 {
    match command {
        MediaCommand::PlayPause => 0xB3,
        MediaCommand::Stop => 0xB2,
        MediaCommand::Next => 0xB0,
        MediaCommand::Previous => 0xB1,
        MediaCommand::VolumeUp => 0xAF,
        MediaCommand::VolumeDown => 0xAE,
        MediaCommand::Mute => 0xAD,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::MouseButtonKind;
    use crate::injection::RecordingInjector;

    /// A host that records what it was asked to do and answers questions from fixed state.
    #[derive(Default)]
    struct TestHost {
        macros: Mutex<Vec<MacroDefinition>>,
        calls: Mutex<Vec<String>>,
        held_keys: Mutex<HashSet<u16>>,
        foreground: Mutex<Option<String>>,
    }

    impl TestHost {
        fn add(&self, definition: MacroDefinition) -> Id {
            let id = definition.id;
            self.macros.lock().unwrap().push(definition);
            id
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn note(&self, what: String) {
            self.calls.lock().unwrap().push(what);
        }
    }

    impl ActionHost for TestHost {
        fn find_macro(&self, id: Id) -> Option<MacroDefinition> {
            self.macros.lock().unwrap().iter().find(|m| m.id == id).cloned()
        }
        fn dpi(&self, command: DpiCommand, stage: usize, pressed: bool) {
            self.note(format!("dpi:{command:?}:{stage}:{pressed}"));
        }
        fn set_gshift(&self, held: bool) {
            self.note(format!("gshift:{held}"));
        }
        fn profile(&self, command: ProfileCommand, _: Option<Id>) {
            self.note(format!("profile:{command:?}"));
        }
        fn bank(&self, bank: usize) {
            self.note(format!("bank:{bank}"));
        }
        fn lighting(&self, command: LightingCommand) {
            self.note(format!("lighting:{command:?}"));
        }
        fn is_user_key_held(&self, virtual_key: u16) -> bool {
            self.held_keys.lock().unwrap().contains(&virtual_key)
        }
        fn foreground_executable_name(&self) -> Option<String> {
            self.foreground.lock().unwrap().clone()
        }
        fn launch(&self, target: &str, arguments: Option<&str>) {
            self.note(format!("launch:{target}:{}", arguments.unwrap_or("")));
        }
    }

    fn runtime() -> (Arc<RecordingInjector>, Arc<TestHost>, MacroRuntime) {
        let injector = Arc::new(RecordingInjector::new());
        let host = Arc::new(TestHost::default());
        let runtime = MacroRuntime::new(
            Arc::clone(&injector) as Arc<dyn InputInjector>,
            Arc::clone(&host) as Arc<dyn ActionHost>,
        );
        (injector, host, runtime)
    }

    fn key(vk: u16) -> ActionDefinition {
        ActionDefinition::Key(KeyStroke::new(vk))
    }

    /// Waits for a condition rather than sleeping a fixed time, so a loaded machine does not make
    /// the test flaky and a broken runtime fails fast.
    fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn a_key_binding_goes_down_on_press_and_up_on_release() {
        let (injector, _, runtime) = runtime();
        runtime.on_trigger("gkey:G1:M1", Some(&key(0x41)), true);
        assert_eq!(injector.events(), vec!["down:41"]);
        assert_eq!(runtime.injector().held_keys(), vec![0x41]);

        runtime.on_trigger("gkey:G1:M1", None, false);
        assert_eq!(injector.events(), vec!["down:41", "up:41"]);
        assert!(!runtime.injector().is_holding_anything());
    }

    #[test]
    fn a_modified_key_presses_and_releases_its_modifiers_in_opposite_order() {
        let (injector, _, runtime) = runtime();
        let ctrl_shift_a = ActionDefinition::Key(KeyStroke::with(
            0x41,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        runtime.on_trigger("t", Some(&ctrl_shift_a), true);
        assert_eq!(injector.events(), vec!["down:A2", "down:A0", "down:41"]);
        runtime.on_trigger("t", None, false);
        assert_eq!(
            injector.events()[3..],
            ["up:41", "up:A0", "up:A2"],
            "the letter first, then shift, then ctrl"
        );
    }

    #[test]
    fn a_release_uses_the_action_recorded_at_press_time() {
        // The bank could have switched between press and release. Releasing the new binding would
        // leave the old binding's key held down forever.
        let (injector, _, runtime) = runtime();
        runtime.on_trigger("gkey:G1:M1", Some(&key(0x41)), true);
        runtime.on_trigger("gkey:G1:M1", Some(&key(0x42)), false);
        assert_eq!(injector.events(), vec!["down:41", "up:41"]);
        assert!(!runtime.injector().is_holding_anything());
    }

    #[test]
    fn a_release_with_no_recorded_press_does_nothing() {
        // A release can arrive after a profile change released everything already.
        let (injector, _, runtime) = runtime();
        runtime.on_trigger("gkey:G1:M1", Some(&key(0x41)), false);
        assert!(injector.events().is_empty());
    }

    #[test]
    fn a_tap_action_fires_on_press_only() {
        let (injector, host, runtime) = runtime();
        let mute = ActionDefinition::Media { command: MediaCommand::Mute };
        runtime.on_trigger("t", Some(&mute), true);
        assert_eq!(injector.events(), vec!["down:AD", "up:AD"]);
        runtime.on_trigger("t", None, false);
        assert_eq!(injector.events().len(), 2, "the release adds nothing");
        assert!(host.calls().is_empty());
    }

    #[test]
    fn pass_through_and_disabled_emit_nothing() {
        let (injector, host, runtime) = runtime();
        for action in [ActionDefinition::PassThrough, ActionDefinition::Disabled] {
            runtime.on_trigger("t", Some(&action), true);
            runtime.on_trigger("t", None, false);
        }
        assert!(injector.events().is_empty());
        assert!(host.calls().is_empty());
    }

    #[test]
    fn gshift_and_dpi_shift_are_held_and_released() {
        let (_, host, runtime) = runtime();
        runtime.on_trigger("shift", Some(&ActionDefinition::GShift), true);
        runtime.on_trigger("shift", None, false);
        let sniper = ActionDefinition::Dpi { command: DpiCommand::Shift, stage: 0 };
        runtime.on_trigger("sniper", Some(&sniper), true);
        runtime.on_trigger("sniper", None, false);
        assert_eq!(
            host.calls(),
            vec!["gshift:true", "gshift:false", "dpi:Shift:0:true", "dpi:Shift:0:false"]
        );
    }

    #[test]
    fn host_actions_are_dispatched_once_each() {
        let (_, host, runtime) = runtime();
        for action in [
            ActionDefinition::Profile { command: ProfileCommand::Next, profile_id: None },
            ActionDefinition::Bank { bank: 2 },
            ActionDefinition::Lighting { command: LightingCommand::BrightnessUp },
        ] {
            runtime.on_trigger("t", Some(&action), true);
            runtime.on_trigger("t", None, false);
        }
        assert_eq!(host.calls(), vec!["profile:Next", "bank:2", "lighting:BrightnessUp"]);
    }

    #[test]
    fn a_macro_plays_its_steps_in_order() {
        let (injector, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("hello");
        definition.steps = vec![
            MacroStep::KeyDown { virtual_key: 0x41 },
            MacroStep::KeyUp { virtual_key: 0x41 },
            MacroStep::Text { text: "hi".into() },
            MacroStep::MouseDown { button: MouseButtonKind::Left },
            MacroStep::MouseUp { button: MouseButtonKind::Left },
            MacroStep::Wheel { delta: 120, horizontal: false },
            MacroStep::MouseMove { dx: 3, dy: -4 },
        ];
        let id = host.add(definition.clone());
        let state = runtime.play(definition);

        assert_eq!(state, MacroRunState::Completed);
        assert_eq!(
            injector.events(),
            vec!["down:41", "up:41", "text:hi", "mdown:Left", "mup:Left", "wheel:120", "move:3,-4"]
        );
        assert!(runtime.active_triggers().is_empty());
        let status = runtime.take_status();
        assert_eq!(status.first().map(|s| s.state), Some(MacroRunState::Running));
        let last = status.last().unwrap();
        assert_eq!(last.state, MacroRunState::Completed);
        assert_eq!(last.macro_id, id);
        assert_eq!(last.iteration, 1);
    }

    #[test]
    fn a_macro_that_holds_a_key_has_it_released_when_the_run_ends() {
        // This is the whole reason the tracking injector exists: a macro that leaves Alt down
        // otherwise strands the user's desktop in Alt-Tab.
        let (injector, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("sloppy");
        definition.steps = vec![
            MacroStep::KeyDown { virtual_key: 0xA4 },
            MacroStep::KeyDown { virtual_key: 0x09 },
        ];
        host.add(definition.clone());
        runtime.play(definition);

        let events = injector.events();
        assert!(events.contains(&"up:09".to_owned()), "{events:?}");
        assert!(events.contains(&"up:A4".to_owned()), "{events:?}");
        assert!(
            events.iter().position(|e| e == "up:09") < events.iter().position(|e| e == "up:A4"),
            "non-modifiers come up first: {events:?}"
        );
    }

    #[test]
    fn repeat_n_plays_the_requested_number_of_times_and_no_more() {
        let (injector, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("thrice");
        definition.repeat_mode = MacroRepeatMode::RepeatN;
        definition.repeat_count = 3;
        definition.repeat_delay_milliseconds = 1;
        definition.steps = vec![MacroStep::Text { text: "x".into() }];
        host.add(definition.clone());
        assert_eq!(runtime.play(definition), MacroRunState::Completed);
        assert_eq!(injector.events().len(), 3);
    }

    #[test]
    fn an_absurd_repeat_count_is_clamped_rather_than_honoured() {
        // The store rejects this, but a profile that reached memory another way must not be able
        // to type a million times.
        let (_, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("hostile");
        definition.repeat_mode = MacroRepeatMode::RepeatN;
        definition.repeat_count = 0;
        definition.steps = vec![MacroStep::Text { text: "x".into() }];
        host.add(definition.clone());
        runtime.play(definition);
        let status = runtime.take_status();
        assert_eq!(status.last().unwrap().iteration, 1, "zero is clamped up to one");
    }

    #[test]
    fn a_while_held_macro_stops_at_its_loop_bound_if_the_release_never_arrives() {
        // A dropped release event must not mean a macro that types forever.
        let (injector, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("runaway");
        definition.repeat_mode = MacroRepeatMode::WhileHeld;
        definition.max_loop_iterations = 4;
        definition.repeat_delay_milliseconds = 1;
        definition.steps = vec![MacroStep::Text { text: "x".into() }];
        host.add(definition.clone());

        assert_eq!(runtime.play(definition), MacroRunState::Completed);
        assert_eq!(injector.events().len(), 4);
        let detail = runtime.take_status().last().unwrap().detail.clone();
        assert_eq!(detail.as_deref(), Some("Stopped at loop bound (4 iterations)"));
    }

    #[test]
    fn a_while_held_macro_stops_soon_after_the_release() {
        let (_, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("held");
        definition.repeat_mode = MacroRepeatMode::WhileHeld;
        definition.max_loop_iterations = 100_000;
        definition.repeat_delay_milliseconds = 1;
        definition.steps = vec![MacroStep::Delay { milliseconds: 5 }];
        host.add(definition.clone());
        let action = ActionDefinition::MacroRef { macro_id: definition.id };

        runtime.on_trigger("gkey:G1:M1", Some(&action), true);
        assert_eq!(runtime.active_triggers(), vec!["gkey:G1:M1".to_owned()]);
        std::thread::sleep(Duration::from_millis(30));
        runtime.on_trigger("gkey:G1:M1", None, false);
        wait_until("the run to finish", || {
            runtime.take_status().iter().any(|s| s.state == MacroRunState::Completed)
        });
        runtime.stop_all("end of test");
    }

    #[test]
    fn a_second_press_of_a_toggle_macro_stops_it() {
        let (_, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("toggle");
        definition.repeat_mode = MacroRepeatMode::Toggle;
        definition.max_loop_iterations = 100_000;
        definition.repeat_delay_milliseconds = 1;
        definition.steps = vec![MacroStep::Delay { milliseconds: 5 }];
        host.add(definition.clone());
        let action = ActionDefinition::MacroRef { macro_id: definition.id };

        runtime.on_trigger("t", Some(&action), true);
        assert_eq!(runtime.active_triggers(), vec!["t".to_owned()]);
        runtime.on_trigger("t", None, false);
        assert_eq!(runtime.active_triggers(), vec!["t".to_owned()], "a release does not stop it");

        runtime.on_trigger("t", Some(&action), true);
        wait_until("the toggle to stop", || runtime.active_triggers().is_empty());
        assert!(runtime.take_status().iter().any(|s| s.state == MacroRunState::Cancelled));
    }

    #[test]
    fn a_press_while_a_non_toggle_macro_is_playing_is_ignored() {
        // This is also what stops a macro re-triggering itself through its own output.
        let (_, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("slow");
        definition.steps = vec![MacroStep::Delay { milliseconds: 200 }];
        host.add(definition.clone());
        let action = ActionDefinition::MacroRef { macro_id: definition.id };

        runtime.on_trigger("t", Some(&action), true);
        let first = runtime.running();
        assert_eq!(first.len(), 1);
        runtime.on_trigger("t", Some(&action), true);
        assert_eq!(runtime.running(), first, "no second run was started");
        runtime.stop_all("end of test");
    }

    #[test]
    fn a_missing_macro_reference_is_refused_without_starting_anything() {
        let (injector, _, runtime) = runtime();
        let action = ActionDefinition::MacroRef { macro_id: Id::new() };
        runtime.on_trigger("t", Some(&action), true);
        assert!(runtime.active_triggers().is_empty());
        assert!(injector.events().is_empty());
    }

    #[test]
    fn a_macro_cannot_call_itself() {
        let (injector, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("recursive");
        let id = definition.id;
        definition.steps = vec![
            MacroStep::Text { text: "before".into() },
            MacroStep::Action { action: ActionDefinition::MacroRef { macro_id: id } },
            MacroStep::Text { text: "after".into() },
        ];
        host.add(definition.clone());

        assert_eq!(runtime.play(definition), MacroRunState::Failed);
        assert_eq!(injector.events(), vec!["text:before"], "it stopped at the recursion");
        let detail = runtime.take_status().last().unwrap().detail.clone().unwrap_or_default();
        assert!(detail.contains("recursion"), "{detail}");
    }

    #[test]
    fn mutual_recursion_between_two_macros_is_refused_too() {
        let (_, host, runtime) = runtime();
        let mut a = MacroDefinition::new("a");
        let mut b = MacroDefinition::new("b");
        a.steps = vec![MacroStep::Action { action: ActionDefinition::MacroRef { macro_id: b.id } }];
        b.steps = vec![MacroStep::Action { action: ActionDefinition::MacroRef { macro_id: a.id } }];
        host.add(a.clone());
        host.add(b.clone());
        assert_eq!(runtime.play(a), MacroRunState::Failed);
    }

    #[test]
    fn nesting_stops_at_the_documented_depth() {
        let (_, host, runtime) = runtime();
        // A chain of distinct macros, each calling the next: no cycle, just depth.
        let mut chain: Vec<MacroDefinition> = (0..MAX_NESTED_MACRO_DEPTH + 2)
            .map(|i| MacroDefinition::new(format!("m{i}")))
            .collect();
        for i in 0..chain.len() - 1 {
            let next = chain[i + 1].id;
            chain[i].steps =
                vec![MacroStep::Action { action: ActionDefinition::MacroRef { macro_id: next } }];
        }
        for definition in &chain {
            host.add(definition.clone());
        }
        assert_eq!(runtime.play(chain[0].clone()), MacroRunState::Failed);
        let detail = runtime.take_status().last().unwrap().detail.clone().unwrap_or_default();
        assert!(detail.contains("nesting"), "{detail}");
    }

    #[test]
    fn a_nested_macro_within_the_depth_limit_runs() {
        let (injector, host, runtime) = runtime();
        let mut inner = MacroDefinition::new("inner");
        inner.steps = vec![MacroStep::Text { text: "inner".into() }];
        let mut outer = MacroDefinition::new("outer");
        outer.steps = vec![
            MacroStep::Text { text: "outer".into() },
            MacroStep::Action { action: ActionDefinition::MacroRef { macro_id: inner.id } },
        ];
        host.add(inner.clone());
        host.add(outer.clone());
        assert_eq!(runtime.play(outer), MacroRunState::Completed);
        assert_eq!(injector.events(), vec!["text:outer", "text:inner"]);
    }

    #[test]
    fn a_key_held_condition_asks_the_host_about_the_user_not_about_our_own_output() {
        let (injector, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("conditional");
        definition.steps = vec![MacroStep::Condition {
            condition: MacroCondition::KeyHeld,
            value: "16".into(),
            then: vec![MacroStep::Text { text: "shifted".into() }],
            otherwise: Some(vec![MacroStep::Text { text: "plain".into() }]),
        }];
        host.add(definition.clone());

        runtime.play(definition.clone());
        assert_eq!(injector.take(), vec!["text:plain"]);

        host.held_keys.lock().unwrap().insert(16);
        runtime.play(definition);
        assert_eq!(injector.take(), vec!["text:shifted"]);
    }

    #[test]
    fn a_foreground_app_condition_matches_case_insensitively() {
        let (injector, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("in game");
        definition.steps = vec![MacroStep::Condition {
            condition: MacroCondition::ForegroundApp,
            value: "game.exe".into(),
            then: vec![MacroStep::Text { text: "in".into() }],
            otherwise: None,
        }];
        host.add(definition.clone());

        runtime.play(definition.clone());
        assert!(injector.take().is_empty(), "no foreground app means the branch is skipped");

        *host.foreground.lock().unwrap() = Some("GAME.EXE".into());
        runtime.play(definition);
        assert_eq!(injector.take(), vec!["text:in"]);
    }

    #[test]
    fn an_unparseable_key_held_value_takes_the_else_branch_rather_than_failing() {
        let (injector, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("broken condition");
        definition.steps = vec![MacroStep::Condition {
            condition: MacroCondition::KeyHeld,
            value: "not a key".into(),
            then: vec![MacroStep::Text { text: "then".into() }],
            otherwise: Some(vec![MacroStep::Text { text: "else".into() }]),
        }];
        host.add(definition.clone());
        assert_eq!(runtime.play(definition), MacroRunState::Completed);
        assert_eq!(injector.events(), vec!["text:else"]);
    }

    #[test]
    fn fixed_delays_replace_recorded_ones_when_the_macro_says_so() {
        let (_, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("fast");
        definition.use_recorded_delays = false;
        definition.fixed_delay_milliseconds = 0;
        definition.steps = vec![MacroStep::Delay { milliseconds: 60_000 }];
        host.add(definition.clone());

        let started = std::time::Instant::now();
        assert_eq!(runtime.play(definition), MacroRunState::Completed);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the recorded minute should have been replaced"
        );
    }

    #[test]
    fn stop_all_cancels_a_long_delay_within_a_few_milliseconds() {
        // A ten-minute delay must not make the emergency stop take ten minutes.
        let (_, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("patient");
        definition.steps = vec![MacroStep::Delay { milliseconds: 600_000 }];
        host.add(definition.clone());
        let action = ActionDefinition::MacroRef { macro_id: definition.id };
        runtime.on_trigger("t", Some(&action), true);
        std::thread::sleep(Duration::from_millis(20));

        let started = std::time::Instant::now();
        runtime.stop_all("emergency stop");
        assert!(started.elapsed() < Duration::from_secs(1), "took {:?}", started.elapsed());
        assert!(runtime.active_triggers().is_empty());
        assert!(runtime.take_status().iter().any(|s| s.state == MacroRunState::Cancelled));
    }

    #[test]
    fn stop_all_releases_held_bindings_and_held_keys() {
        let (injector, _, runtime) = runtime();
        runtime.on_trigger("gkey:G1:M1", Some(&key(0x41)), true);
        runtime.on_trigger("gkey:G2:M1", Some(&ActionDefinition::GShift), true);
        injector.clear();

        runtime.stop_all("emergency stop");
        assert!(injector.events().contains(&"up:41".to_owned()), "{:?}", injector.events());
        assert!(!runtime.injector().is_holding_anything());
    }

    #[test]
    fn releasing_one_banks_bindings_leaves_another_banks_alone() {
        let (injector, _, runtime) = runtime();
        runtime.on_trigger("gkey:G1:M1", Some(&key(0x41)), true);
        runtime.on_trigger("gkey:G1:M2", Some(&key(0x42)), true);
        injector.clear();

        runtime.release_held("gkey:G1:M1", "bank change");
        assert_eq!(injector.events(), vec!["up:41"]);
        assert_eq!(runtime.injector().held_keys(), vec![0x42]);
        runtime.stop_all("end of test");
    }

    #[test]
    fn stopping_one_devices_triggers_leaves_another_devices_running() {
        let (_, host, runtime) = runtime();
        let mut definition = MacroDefinition::new("long");
        definition.steps = vec![MacroStep::Delay { milliseconds: 5_000 }];
        host.add(definition.clone());
        let action = ActionDefinition::MacroRef { macro_id: definition.id };

        runtime.on_trigger("mouse:1", Some(&action), true);
        runtime.on_trigger("keyboard:1", Some(&action), true);
        assert_eq!(runtime.active_triggers().len(), 2);

        runtime.stop_for_trigger_prefix("mouse:", "device gone");
        assert_eq!(runtime.active_triggers(), vec!["keyboard:1".to_owned()]);
        runtime.stop_all("end of test");
    }

    #[test]
    fn a_chord_taps_each_combination_with_a_gap() {
        let (injector, _, runtime) = runtime();
        let chord = ActionDefinition::Chord {
            steps: vec![
                KeyStroke::with(0x4B, KeyModifiers::CONTROL),
                KeyStroke::with(0x43, KeyModifiers::CONTROL),
            ],
        };
        runtime.on_trigger("t", Some(&chord), true);
        // A chord sleeps, so it runs on its own thread.
        wait_until("the chord to finish", || injector.events().len() >= 8);
        assert_eq!(
            injector.events(),
            vec!["down:A2", "down:4B", "up:4B", "up:A2", "down:A2", "down:43", "up:43", "up:A2"]
        );
    }

    #[test]
    fn launching_a_program_goes_through_the_host() {
        let (_, host, runtime) = runtime();
        let launch = ActionDefinition::Launch {
            target: "C:/Games/game.exe".into(),
            arguments: Some("-windowed".into()),
        };
        runtime.on_trigger("t", Some(&launch), true);
        wait_until("the launch to reach the host", || !host.calls().is_empty());
        assert_eq!(host.calls(), vec!["launch:C:/Games/game.exe:-windowed"]);
    }

    #[test]
    fn every_media_command_maps_to_a_distinct_virtual_key() {
        let keys: HashSet<u16> = [
            MediaCommand::PlayPause,
            MediaCommand::Stop,
            MediaCommand::Next,
            MediaCommand::Previous,
            MediaCommand::VolumeUp,
            MediaCommand::VolumeDown,
            MediaCommand::Mute,
        ]
        .into_iter()
        .map(media_virtual_key)
        .collect();
        assert_eq!(keys.len(), 7, "a duplicate would make two commands do the same thing");
        assert_eq!(media_virtual_key(MediaCommand::PlayPause), 0xB3);
        assert_eq!(media_virtual_key(MediaCommand::Mute), 0xAD);
    }

    #[test]
    fn only_the_sleeping_actions_get_their_own_thread() {
        assert!(sleeps(&ActionDefinition::Chord { steps: vec![] }));
        assert!(sleeps(&ActionDefinition::MacroRef { macro_id: Id::NIL }));
        assert!(sleeps(&ActionDefinition::Launch { target: "x".into(), arguments: None }));
        assert!(!sleeps(&key(0x41)));
        assert!(!sleeps(&ActionDefinition::Text { text: "x".into() }));
        assert!(!sleeps(&ActionDefinition::Media { command: MediaCommand::Mute }));
    }
}

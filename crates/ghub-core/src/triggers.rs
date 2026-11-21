//! Trigger evaluation: the engine half of `Automation/Triggers.cs`.
//!
//! [`crate::profile::TriggerRule`] and [`crate::profile::TriggerEvent`] live with the rest of the
//! document model, because they are persisted. This module is the part that decides *whether* a
//! rule fires and runs its action.
//!
//! Guarantees, all of them tested below:
//!
//! * per-rule cooldown, so a trigger whose action causes its own event cannot spin,
//! * no re-entrancy: events raised while a trigger action runs do not fire further triggers,
//! * `BatteryBelow` is edge-triggered - it fires once on crossing below the threshold and re-arms
//!   only after the level goes back above it, so a mouse sitting at 9% does not fire every poll,
//! * a failing action is logged and the remaining rules still run.
//!
//! A trigger may never *launch* a program. That rule is enforced here rather than in the UI, in
//! [`TriggerEngine::is_permitted`]: a rule arrives from a profile document, which can come from an
//! import or from a hand-edited file, so "the editor does not offer it" is not a control. The
//! reason is that triggers fire from device and foreground events without the user present -
//! a `ForegroundApp` rule that launched something would be an auto-run hook on every app switch.
//!
//! ## Divergence from the C#
//!
//! The C# re-entrancy guard was an `AsyncLocal<bool>`, which does not follow an action onto a
//! thread pool thread. A trigger action that hopped threads - which the macro runtime does for any
//! sleeping action - could therefore cascade. Here the guard is a single process-wide flag, so
//! while *any* trigger action is in flight no trigger fires. Triggers are rare, user-visible
//! events, so serialising them costs nothing and closes the hole.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::action::ActionDefinition;
use crate::diagnostics::DiagnosticsLog;
use crate::id::Id;
use crate::profile::{TriggerEvent, TriggerRule};
use crate::time::now_unix_millis;

/// One occurrence the engine can react to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerOccurrence {
    pub event: TriggerEvent,
    /// Device name, profile name or executable name, depending on the event.
    pub subject: String,
    /// Battery percent, DPI value or bank number. Zero when the event carries no number.
    pub value: i32,
}

impl TriggerOccurrence {
    pub fn new(event: TriggerEvent, subject: impl Into<String>) -> Self {
        Self { event, subject: subject.into(), value: 0 }
    }

    pub fn with_value(event: TriggerEvent, subject: impl Into<String>, value: i32) -> Self {
        Self { event, subject: subject.into(), value }
    }
}

/// Whether the rule's filter and numeric condition accept this occurrence.
///
/// Free function, as `TriggerEngine.Matches` was static in the C#: the profile editor calls it to
/// show "this rule would fire now".
pub fn matches(rule: &TriggerRule, occurrence: &TriggerOccurrence) -> bool {
    if !rule.enabled || rule.event != occurrence.event {
        return false;
    }
    let filter = rule.r#match.trim();
    let subject_ok = filter.is_empty()
        || match occurrence.event {
            // An executable or profile name is matched whole: "game.exe" must not also match
            // "notagame.exe".
            TriggerEvent::ForegroundApp | TriggerEvent::ProfileActivated => {
                filter.eq_ignore_ascii_case(&occurrence.subject)
            }
            // Device names are matched as a substring, because the user types "G903" and the
            // device reports "G903 LIGHTSPEED Wireless Gaming Mouse".
            _ => contains_ignore_case(&occurrence.subject, filter),
        };
    if !subject_ok {
        return false;
    }
    match occurrence.event {
        TriggerEvent::BatteryBelow => occurrence.value <= rule.threshold,
        TriggerEvent::DpiChanged | TriggerEvent::BankChanged => {
            rule.threshold == 0 || occurrence.value == rule.threshold
        }
        _ => true,
    }
}

/// `haystack.Contains(needle, OrdinalIgnoreCase)`.
///
/// ASCII-only folding, which is what `OrdinalIgnoreCase` does for the characters that appear in a
/// device or window name. A full Unicode fold would need a case table in the binary for no gain.
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack = haystack.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    haystack.contains(&needle)
}

/// What a trigger action is allowed to do.
pub type Executor = Box<dyn Fn(&ActionDefinition) -> Result<(), String> + Send + Sync + 'static>;
type RulesSource = Box<dyn Fn() -> Vec<TriggerRule> + Send + Sync + 'static>;
type Clock = Box<dyn Fn() -> i64 + Send + Sync + 'static>;
type FiredListener = Box<dyn Fn(&TriggerRule, &TriggerOccurrence) + Send + Sync + 'static>;

pub struct TriggerEngine {
    rules: RulesSource,
    execute: Executor,
    log: Arc<DiagnosticsLog>,
    clock: Mutex<Clock>,
    /// Unix milliseconds of the last firing, per rule.
    last_fired: Mutex<HashMap<Id, i64>>,
    /// Edge state for `BatteryBelow`. Absent means armed.
    armed: Mutex<HashMap<Id, bool>>,
    listeners: Mutex<Vec<FiredListener>>,
    in_trigger: AtomicBool,
}

impl std::fmt::Debug for TriggerEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriggerEngine")
            .field("in_trigger", &self.in_trigger.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl TriggerEngine {
    pub fn new(
        rules: impl Fn() -> Vec<TriggerRule> + Send + Sync + 'static,
        execute: impl Fn(&ActionDefinition) -> Result<(), String> + Send + Sync + 'static,
        log: Arc<DiagnosticsLog>,
    ) -> Self {
        Self {
            rules: Box::new(rules),
            execute: Box::new(execute),
            log,
            clock: Mutex::new(Box::new(now_unix_millis)),
            last_fired: Mutex::new(HashMap::new()),
            armed: Mutex::new(HashMap::new()),
            listeners: Mutex::new(Vec::new()),
            in_trigger: AtomicBool::new(false),
        }
    }

    /// Replaces the clock. For tests: cooldowns are otherwise only observable by sleeping.
    pub fn set_clock(&self, clock: impl Fn() -> i64 + Send + Sync + 'static) {
        if let Ok(mut slot) = self.clock.lock() {
            *slot = Box::new(clock);
        }
    }

    pub fn on_fired(
        &self,
        listener: impl Fn(&TriggerRule, &TriggerOccurrence) + Send + Sync + 'static,
    ) {
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.push(Box::new(listener));
        }
    }

    /// Whether an action is allowed as a trigger action at all. See the module note.
    pub fn is_permitted(action: &ActionDefinition) -> bool {
        !matches!(action, ActionDefinition::Launch { .. })
    }

    fn now(&self) -> i64 {
        self.clock.lock().map(|clock| clock()).unwrap_or_else(|_| now_unix_millis())
    }

    /// Evaluates every rule against one occurrence and returns how many fired.
    pub fn raise(&self, occurrence: &TriggerOccurrence) -> usize {
        // Events caused by a trigger's own action never cascade.
        if self
            .in_trigger
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return 0;
        }
        let fired = self.evaluate(occurrence);
        self.in_trigger.store(false, Ordering::SeqCst);
        fired
    }

    fn evaluate(&self, occurrence: &TriggerOccurrence) -> usize {
        let mut fired = 0;
        for rule in (self.rules)() {
            if !rule.enabled || rule.event != occurrence.event {
                continue;
            }
            let matched = matches(&rule, occurrence);
            if rule.event == TriggerEvent::BatteryBelow {
                if !self.update_battery_edge(&rule, matched) {
                    continue;
                }
            } else if !matched {
                continue;
            }

            let now = self.now();
            let cooldown_ms = i64::from(rule.cooldown_seconds.max(0)) * 1000;
            if let Ok(mut last) = self.last_fired.lock() {
                if let Some(previous) = last.get(&rule.id) {
                    if now.saturating_sub(*previous) < cooldown_ms {
                        continue;
                    }
                }
                last.insert(rule.id, now);
            }

            if !Self::is_permitted(&rule.action) {
                self.log.warning_with(
                    "Triggers",
                    format!("Trigger '{}' was skipped", rule.name),
                    "launching a program from a trigger is not allowed",
                );
                continue;
            }

            fired += 1;
            if let Ok(listeners) = self.listeners.lock() {
                for listener in listeners.iter() {
                    listener(&rule, occurrence);
                }
            }
            if let Err(e) = (self.execute)(&rule.action) {
                self.log.warning_with(
                    "Triggers",
                    format!("Trigger '{}' action failed", rule.name),
                    e,
                );
            }
        }
        fired
    }

    /// Edge detection for `BatteryBelow`. Returns whether the rule should fire now.
    fn update_battery_edge(&self, rule: &TriggerRule, matched: bool) -> bool {
        let Ok(mut armed) = self.armed.lock() else {
            return false;
        };
        let was_armed = *armed.entry(rule.id).or_insert(true);
        if !matched {
            // Back above the threshold: re-arm for the next crossing.
            armed.insert(rule.id, true);
            return false;
        }
        if !was_armed {
            return false;
        }
        armed.insert(rule.id, false);
        true
    }

    /// Clears cooldown and edge state. Called when the document changes, so editing a rule does
    /// not leave it muted by a cooldown recorded against its previous shape.
    pub fn reset(&self) {
        if let Ok(mut last) = self.last_fired.lock() {
            last.clear();
        }
        if let Ok(mut armed) = self.armed.lock() {
            armed.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI64;

    struct Harness {
        engine: TriggerEngine,
        ran: Arc<Mutex<Vec<ActionDefinition>>>,
        clock: Arc<AtomicI64>,
        log: Arc<DiagnosticsLog>,
    }

    fn harness(rules: Vec<TriggerRule>) -> Harness {
        harness_with(rules, |_| Ok(()))
    }

    fn harness_with(
        rules: Vec<TriggerRule>,
        outcome: impl Fn(&ActionDefinition) -> Result<(), String> + Send + Sync + 'static,
    ) -> Harness {
        let ran = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&ran);
        let log = Arc::new(DiagnosticsLog::default());
        let engine = TriggerEngine::new(
            move || rules.clone(),
            move |action| {
                recorded.lock().unwrap().push(action.clone());
                outcome(action)
            },
            Arc::clone(&log),
        );
        let clock = Arc::new(AtomicI64::new(1_000_000));
        let ticking = Arc::clone(&clock);
        engine.set_clock(move || ticking.load(Ordering::Relaxed));
        Harness { engine, ran, clock, log }
    }

    fn rule(event: TriggerEvent, filter: &str, threshold: i32) -> TriggerRule {
        let mut rule = TriggerRule::new("test", event);
        rule.r#match = filter.to_owned();
        rule.threshold = threshold;
        rule.action = ActionDefinition::Dpi { command: crate::action::DpiCommand::Next, stage: 0 };
        rule
    }

    #[test]
    fn a_matching_rule_fires_once_and_runs_its_action() {
        let h = harness(vec![rule(TriggerEvent::DeviceConnected, "", 0)]);
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::new(TriggerEvent::DeviceConnected, "G903")),
            1
        );
        assert_eq!(h.ran.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_disabled_rule_never_fires() {
        let mut r = rule(TriggerEvent::DeviceConnected, "", 0);
        r.enabled = false;
        let h = harness(vec![r]);
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::new(TriggerEvent::DeviceConnected, "G903")),
            0
        );
    }

    #[test]
    fn device_filters_match_as_a_substring_ignoring_case() {
        let h = harness(vec![rule(TriggerEvent::DeviceConnected, "g903", 0)]);
        let occurrence = TriggerOccurrence::new(
            TriggerEvent::DeviceConnected,
            "G903 LIGHTSPEED Wireless Gaming Mouse",
        );
        assert_eq!(h.engine.raise(&occurrence), 1);
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::new(TriggerEvent::DeviceConnected, "G815")),
            0
        );
    }

    #[test]
    fn foreground_and_profile_filters_match_the_whole_name() {
        let h = harness(vec![rule(TriggerEvent::ForegroundApp, "game.exe", 0)]);
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::new(TriggerEvent::ForegroundApp, "GAME.EXE")),
            1
        );
        h.engine.reset();
        // A substring match here would be a real bug: a different program must not borrow a
        // rule written for this one.
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::new(TriggerEvent::ForegroundApp, "notagame.exe")),
            0
        );
    }

    #[test]
    fn an_empty_filter_matches_anything() {
        let h = harness(vec![rule(TriggerEvent::ProfileActivated, "   ", 0)]);
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::new(TriggerEvent::ProfileActivated, "Default")),
            1
        );
    }

    #[test]
    fn dpi_and_bank_thresholds_of_zero_mean_any_value() {
        let h = harness(vec![
            rule(TriggerEvent::DpiChanged, "", 0),
            rule(TriggerEvent::BankChanged, "", 3),
        ]);
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::with_value(TriggerEvent::DpiChanged, "G903", 1600)),
            1
        );
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::with_value(TriggerEvent::BankChanged, "G903", 2)),
            0
        );
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::with_value(TriggerEvent::BankChanged, "G903", 3)),
            1
        );
    }

    #[test]
    fn the_cooldown_blocks_a_second_firing_until_it_elapses() {
        let mut r = rule(TriggerEvent::DeviceConnected, "", 0);
        r.cooldown_seconds = 5;
        let h = harness(vec![r]);
        let occurrence = TriggerOccurrence::new(TriggerEvent::DeviceConnected, "G903");
        assert_eq!(h.engine.raise(&occurrence), 1);
        h.clock.fetch_add(4_999, Ordering::Relaxed);
        assert_eq!(h.engine.raise(&occurrence), 0, "still inside the cooldown");
        h.clock.fetch_add(2, Ordering::Relaxed);
        assert_eq!(h.engine.raise(&occurrence), 1);
        assert_eq!(h.ran.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_zero_cooldown_fires_every_time() {
        let mut r = rule(TriggerEvent::DeviceConnected, "", 0);
        r.cooldown_seconds = 0;
        let h = harness(vec![r]);
        let occurrence = TriggerOccurrence::new(TriggerEvent::DeviceConnected, "G903");
        for _ in 0..3 {
            assert_eq!(h.engine.raise(&occurrence), 1);
        }
    }

    #[test]
    fn a_negative_cooldown_is_treated_as_zero_not_as_the_distant_past() {
        let mut r = rule(TriggerEvent::DeviceConnected, "", 0);
        r.cooldown_seconds = -60;
        let h = harness(vec![r]);
        let occurrence = TriggerOccurrence::new(TriggerEvent::DeviceConnected, "G903");
        assert_eq!(h.engine.raise(&occurrence), 1);
        assert_eq!(h.engine.raise(&occurrence), 1);
    }

    #[test]
    fn battery_below_is_edge_triggered() {
        let mut r = rule(TriggerEvent::BatteryBelow, "", 10);
        r.cooldown_seconds = 0;
        let h = harness(vec![r]);
        let low = TriggerOccurrence::with_value(TriggerEvent::BatteryBelow, "G903", 9);
        let lower = TriggerOccurrence::with_value(TriggerEvent::BatteryBelow, "G903", 4);
        let charged = TriggerOccurrence::with_value(TriggerEvent::BatteryBelow, "G903", 80);

        assert_eq!(h.engine.raise(&low), 1, "the crossing fires");
        assert_eq!(h.engine.raise(&lower), 0, "already below: no second alert");
        assert_eq!(h.engine.raise(&low), 0);
        assert_eq!(h.engine.raise(&charged), 0, "going above only re-arms");
        assert_eq!(h.engine.raise(&low), 1, "and the next crossing fires again");
        assert_eq!(h.ran.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_trigger_action_cannot_cascade_into_more_triggers() {
        // The action raises the very event it is reacting to.
        let rules = vec![{
            let mut r = rule(TriggerEvent::DeviceConnected, "", 0);
            r.cooldown_seconds = 0;
            r
        }];
        let depth = Arc::new(AtomicI64::new(0));
        let peak = Arc::new(AtomicI64::new(0));
        let log = Arc::new(DiagnosticsLog::default());
        let engine: Arc<Mutex<Option<Arc<TriggerEngine>>>> = Arc::new(Mutex::new(None));

        let inner = Arc::clone(&engine);
        let counting = Arc::clone(&depth);
        let highest = Arc::clone(&peak);
        let built = Arc::new(TriggerEngine::new(
            move || rules.clone(),
            move |_| {
                let now = counting.fetch_add(1, Ordering::SeqCst) + 1;
                highest.fetch_max(now, Ordering::SeqCst);
                if let Some(engine) = inner.lock().unwrap().clone() {
                    engine.raise(&TriggerOccurrence::new(TriggerEvent::DeviceConnected, "G903"));
                }
                counting.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            },
            log,
        ));
        *engine.lock().unwrap() = Some(Arc::clone(&built));

        built.raise(&TriggerOccurrence::new(TriggerEvent::DeviceConnected, "G903"));
        assert_eq!(peak.load(Ordering::SeqCst), 1, "the action re-entered the engine");
        assert_eq!(depth.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_failing_action_is_logged_and_the_next_rule_still_runs() {
        let rules = vec![
            rule(TriggerEvent::DeviceConnected, "", 0),
            rule(TriggerEvent::DeviceConnected, "", 0),
        ];
        let h = harness_with(rules, |_| Err("device went away".to_owned()));
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::new(TriggerEvent::DeviceConnected, "G903")),
            2
        );
        assert_eq!(h.ran.lock().unwrap().len(), 2);
        let text = h.log.to_text();
        assert!(text.contains("action failed"), "{text}");
        assert!(text.contains("device went away"), "{text}");
    }

    #[test]
    fn a_launch_action_is_refused_even_if_a_document_asks_for_it() {
        let mut r = rule(TriggerEvent::ForegroundApp, "", 0);
        r.action = ActionDefinition::Launch {
            target: "cmd.exe".to_owned(),
            arguments: None,
        };
        let h = harness(vec![r]);
        assert_eq!(
            h.engine.raise(&TriggerOccurrence::new(TriggerEvent::ForegroundApp, "game.exe")),
            0
        );
        assert!(h.ran.lock().unwrap().is_empty(), "nothing may be launched");
        assert!(h.log.to_text().contains("not allowed"));
        assert!(!TriggerEngine::is_permitted(&ActionDefinition::Launch {
            target: "x".to_owned(),
            arguments: None
        }));
    }

    #[test]
    fn listeners_see_the_rule_and_the_occurrence() {
        let h = harness(vec![rule(TriggerEvent::DeviceConnected, "", 0)]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        h.engine.on_fired(move |rule, occurrence| {
            recorder.lock().unwrap().push((rule.name.clone(), occurrence.subject.clone()));
        });
        h.engine.raise(&TriggerOccurrence::new(TriggerEvent::DeviceConnected, "G903"));
        assert_eq!(seen.lock().unwrap().as_slice(), [("test".to_owned(), "G903".to_owned())]);
    }

    #[test]
    fn reset_clears_cooldowns_and_the_battery_edge() {
        let mut r = rule(TriggerEvent::BatteryBelow, "", 10);
        r.cooldown_seconds = 3600;
        let h = harness(vec![r]);
        let low = TriggerOccurrence::with_value(TriggerEvent::BatteryBelow, "G903", 5);
        assert_eq!(h.engine.raise(&low), 1);
        assert_eq!(h.engine.raise(&low), 0);
        h.engine.reset();
        assert_eq!(h.engine.raise(&low), 1, "an edited rule starts from a clean slate");
    }

    #[test]
    fn matches_is_usable_from_the_editor_without_firing_anything() {
        let r = rule(TriggerEvent::BatteryBelow, "G903", 15);
        assert!(matches(
            &r,
            &TriggerOccurrence::with_value(TriggerEvent::BatteryBelow, "G903 HERO", 10)
        ));
        assert!(!matches(
            &r,
            &TriggerOccurrence::with_value(TriggerEvent::BatteryBelow, "G903 HERO", 20)
        ));
        assert!(!matches(
            &r,
            &TriggerOccurrence::with_value(TriggerEvent::DpiChanged, "G903 HERO", 10)
        ));
    }
}

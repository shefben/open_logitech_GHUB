//! Deterministic profile selection. Port of
//! `Artemis.Peripherals/Profiles/ProfileResolver.cs`.
//!
//! Order: manual lock, then exact executable path, then executable name plus title, then
//! executable name, then title alone, then the last manual selection, then the default profile.
//! Ties break on rule priority, then profile order, then profile id - so the same foreground
//! window always picks the same profile, run after run.
//!
//! Every resolution carries the reason it happened, because "why did my profile change?" is the
//! question this feature generates and a log line saying only the new name does not answer it.

use regex_lite::RegexBuilder;

use crate::id::Id;
use crate::profile::{AppMatchKind, AppRule, PeripheralProfile, ProfileDocument};

/// Longest title pattern accepted. The store rejects longer ones; this is the second line of
/// defence, for a document that reached memory another way.
pub const MAX_PATTERN_LENGTH: usize = 500;

/// The foreground window, as the platform layer reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForegroundApp {
    pub executable_path: Option<String>,
    pub window_title: Option<String>,
}

impl ForegroundApp {
    pub fn new(path: impl Into<String>, title: impl Into<String>) -> Self {
        Self { executable_path: Some(path.into()), window_title: Some(title.into()) }
    }

    pub fn executable_name(&self) -> Option<&str> {
        let path = self.executable_path.as_deref()?;
        path.rsplit(['/', '\\']).next().filter(|n| !n.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionReason {
    ManualLock,
    ExactPathRule,
    ExecutableRule,
    ExecutableAndTitleRule,
    TitleRule,
    PersistentSelection,
    Default,
}

impl ResolutionReason {
    fn rule_name(&self) -> &'static str {
        match self {
            Self::ExactPathRule => "ExactPathRule",
            Self::ExecutableRule => "ExecutableRule",
            Self::ExecutableAndTitleRule => "ExecutableAndTitleRule",
            Self::TitleRule => "TitleRule",
            Self::ManualLock => "ManualLock",
            Self::PersistentSelection => "PersistentSelection",
            Self::Default => "Default",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileResolution {
    pub profile_id: Id,
    pub profile_name: String,
    pub reason: ResolutionReason,
    /// The rule that matched, when a rule did.
    pub rule_id: Option<Id>,
    rule_subject: Option<String>,
}

impl ProfileResolution {
    /// One sentence for the diagnostics page and the status line.
    pub fn explain(&self) -> String {
        match self.reason {
            ResolutionReason::ManualLock => {
                format!("'{}' is manually locked", self.profile_name)
            }
            ResolutionReason::PersistentSelection => {
                format!("'{}' was selected manually", self.profile_name)
            }
            ResolutionReason::Default => {
                format!("'{}' is the default profile", self.profile_name)
            }
            other => format!(
                "'{}' matched {} rule ({})",
                self.profile_name,
                other.rule_name(),
                self.rule_subject.as_deref().unwrap_or("")
            ),
        }
    }
}

/// Picks the profile for a foreground window.
///
/// `None` only when the document has no profiles at all, which the store refuses to save.
pub fn resolve(doc: &ProfileDocument, app: Option<&ForegroundApp>) -> Option<ProfileResolution> {
    let fallback = doc.default_profile()?;

    // A manual lock is the user saying "stop switching". It wins over every rule, including a
    // more specific one, or the lock would not mean anything.
    if let Some(locked) = doc.locked_profile_id.and_then(|id| doc.profile(id)) {
        return Some(resolution(locked, ResolutionReason::ManualLock, None));
    }

    if doc.auto_switch_enabled {
        if let Some(app) = app {
            let mut candidates: Vec<(u8, i32, i32, Id, &PeripheralProfile, &AppRule)> = doc
                .profiles
                .iter()
                .flat_map(|profile| {
                    profile
                        .rules
                        .iter()
                        .filter(|rule| rule.enabled)
                        .map(move |rule| (profile, rule))
                })
                .filter_map(|(profile, rule)| {
                    let rank = rank(rule, app);
                    (rank > 0).then_some((
                        rank,
                        rule.priority,
                        profile.order,
                        profile.id,
                        profile,
                        rule,
                    ))
                })
                .collect();

            // Rank and priority descending, then order and id ascending: a total order, so the
            // answer does not depend on hash iteration or on the order rules were added.
            candidates.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then(b.1.cmp(&a.1))
                    .then(a.2.cmp(&b.2))
                    .then(a.3.cmp(&b.3))
            });

            if let Some((_, _, _, _, profile, rule)) = candidates.first() {
                let reason = match rule.kind {
                    AppMatchKind::ExactPath => ResolutionReason::ExactPathRule,
                    AppMatchKind::ExecutableName => ResolutionReason::ExecutableRule,
                    AppMatchKind::ExecutableAndTitle => ResolutionReason::ExecutableAndTitleRule,
                    AppMatchKind::TitleOnly => ResolutionReason::TitleRule,
                };
                return Some(resolution(profile, reason, Some(rule)));
            }
        }
    }

    if let Some(persistent) = doc.persistent_profile_id.and_then(|id| doc.profile(id)) {
        return Some(resolution(persistent, ResolutionReason::PersistentSelection, None));
    }
    Some(resolution(fallback, ResolutionReason::Default, None))
}

fn resolution(
    profile: &PeripheralProfile,
    reason: ResolutionReason,
    rule: Option<&AppRule>,
) -> ProfileResolution {
    ProfileResolution {
        profile_id: profile.id,
        profile_name: profile.name.clone(),
        reason,
        rule_id: rule.map(|r| r.id),
        rule_subject: rule.and_then(|r| {
            r.executable_path
                .clone()
                .or_else(|| r.executable_name.clone())
                .or_else(|| r.title_pattern.clone())
        }),
    }
}

/// 0 means no match; higher is more specific. The gaps leave room for a future rule kind without
/// renumbering a persisted file.
pub fn rank(rule: &AppRule, app: &ForegroundApp) -> u8 {
    match rule.kind {
        AppMatchKind::ExactPath => {
            let (Some(want), Some(got)) = (&rule.executable_path, &app.executable_path) else {
                return 0;
            };
            if normalize_path(want).eq_ignore_ascii_case(&normalize_path(got)) {
                40
            } else {
                0
            }
        }
        AppMatchKind::ExecutableName => {
            if name_matches(rule, app) {
                20
            } else {
                0
            }
        }
        AppMatchKind::ExecutableAndTitle => {
            if name_matches(rule, app) && title_matches(rule, app) {
                30
            } else {
                0
            }
        }
        AppMatchKind::TitleOnly => {
            if title_matches(rule, app) {
                10
            } else {
                0
            }
        }
    }
}

fn name_matches(rule: &AppRule, app: &ForegroundApp) -> bool {
    match (&rule.executable_name, app.executable_name()) {
        (Some(want), Some(got)) => want.trim().eq_ignore_ascii_case(got),
        _ => false,
    }
}

fn title_matches(rule: &AppRule, app: &ForegroundApp) -> bool {
    let (Some(pattern), Some(title)) = (&rule.title_pattern, &app.window_title) else {
        return false;
    };
    if pattern.is_empty() || pattern.len() > MAX_PATTERN_LENGTH {
        return false;
    }
    // A pattern that does not compile matches nothing. The C# had to add a 100 ms match timeout
    // on top of this because .NET's engine backtracks without bound; `regex-lite` memoises its
    // backtracker, so a pathological pattern costs time proportional to the title length rather
    // than exponential in it, and no timeout is needed.
    match RegexBuilder::new(pattern).case_insensitive(true).build() {
        Ok(re) => re.is_match(title),
        Err(_) => false,
    }
}

/// Trims quotes and trailing separators, and normalises separators to backslashes.
///
/// Deliberately *not* a filesystem canonicalisation: the C# used `Path.GetFullPath`, which
/// resolves against the current directory and touches the disk. Doing that for every rule on
/// every foreground change would mean filesystem access on the UI's critical path, and a rule
/// for a program on a drive that is currently unplugged would stop matching.
pub fn normalize_path(path: &str) -> String {
    path.trim()
        .trim_matches('"')
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::AppRule;

    fn doc_with_rules(rules: Vec<(&str, Vec<AppRule>)>) -> ProfileDocument {
        let mut doc = ProfileDocument::default();
        for (index, (name, rules)) in rules.into_iter().enumerate() {
            let mut profile = PeripheralProfile::new(name);
            profile.order = index as i32;
            profile.rules = rules;
            profile.is_default = index == 0;
            doc.profiles.push(profile);
        }
        doc
    }

    fn exact(path: &str) -> AppRule {
        AppRule {
            kind: AppMatchKind::ExactPath,
            executable_path: Some(path.to_owned()),
            ..AppRule::by_executable("")
        }
    }

    fn title(pattern: &str) -> AppRule {
        AppRule {
            kind: AppMatchKind::TitleOnly,
            executable_name: None,
            title_pattern: Some(pattern.to_owned()),
            ..AppRule::by_executable("")
        }
    }

    #[test]
    fn the_executable_name_comes_off_either_separator() {
        assert_eq!(
            ForegroundApp::new(r"C:\Games\game.exe", "").executable_name(),
            Some("game.exe")
        );
        assert_eq!(ForegroundApp::new("/usr/bin/game", "").executable_name(), Some("game"));
        assert_eq!(ForegroundApp::new("game.exe", "").executable_name(), Some("game.exe"));
        // A path ending in a separator has no executable name, and must not answer "".
        assert_eq!(ForegroundApp::new(r"C:\Games\", "").executable_name(), None);
        assert_eq!(ForegroundApp::default().executable_name(), None);
    }

    #[test]
    fn a_more_specific_rule_wins_over_a_broader_one() {
        let doc = doc_with_rules(vec![
            ("broad", vec![title(".*")]),
            ("by name", vec![AppRule::by_executable("game.exe")]),
            ("by path", vec![exact(r"C:\Games\game.exe")]),
        ]);
        let app = ForegroundApp::new(r"C:\Games\game.exe", "Game Window");
        let picked = resolve(&doc, Some(&app)).unwrap();
        assert_eq!(picked.profile_name, "by path");
        assert_eq!(picked.reason, ResolutionReason::ExactPathRule);
        assert!(picked.explain().contains(r"C:\Games\game.exe"), "{}", picked.explain());
    }

    #[test]
    fn a_manual_lock_beats_even_an_exact_path_match() {
        let mut doc = doc_with_rules(vec![
            ("locked", vec![]),
            ("by path", vec![exact(r"C:\Games\game.exe")]),
        ]);
        doc.locked_profile_id = Some(doc.profiles[0].id);
        let app = ForegroundApp::new(r"C:\Games\game.exe", "Game");
        let picked = resolve(&doc, Some(&app)).unwrap();
        assert_eq!(picked.profile_name, "locked");
        assert_eq!(picked.reason, ResolutionReason::ManualLock);
        assert_eq!(picked.explain(), "'locked' is manually locked");

        // A lock naming a profile that has since been deleted must not strand the user on
        // nothing; resolution falls through to the rules.
        doc.locked_profile_id = Some(Id::new());
        assert_eq!(resolve(&doc, Some(&app)).unwrap().profile_name, "by path");
    }

    #[test]
    fn auto_switch_off_means_rules_are_not_consulted_at_all() {
        let mut doc = doc_with_rules(vec![
            ("default", vec![]),
            ("by path", vec![exact(r"C:\Games\game.exe")]),
        ]);
        doc.auto_switch_enabled = false;
        let app = ForegroundApp::new(r"C:\Games\game.exe", "Game");
        let picked = resolve(&doc, Some(&app)).unwrap();
        assert_eq!(picked.reason, ResolutionReason::Default);
        assert_eq!(picked.profile_name, "default");

        // The persistent selection still applies: turning auto-switch off should keep whatever
        // the user last chose, not snap back to the default.
        doc.persistent_profile_id = Some(doc.profiles[1].id);
        let picked = resolve(&doc, Some(&app)).unwrap();
        assert_eq!(picked.reason, ResolutionReason::PersistentSelection);
        assert_eq!(picked.profile_name, "by path");
    }

    #[test]
    fn a_disabled_rule_is_ignored() {
        let mut doc = doc_with_rules(vec![
            ("default", vec![]),
            ("by path", vec![exact(r"C:\Games\game.exe")]),
        ]);
        doc.profiles[1].rules[0].enabled = false;
        let app = ForegroundApp::new(r"C:\Games\game.exe", "Game");
        assert_eq!(resolve(&doc, Some(&app)).unwrap().reason, ResolutionReason::Default);
    }

    #[test]
    fn ties_break_on_priority_then_order_then_id_so_the_answer_is_stable() {
        let mut doc = doc_with_rules(vec![
            ("first", vec![AppRule::by_executable("game.exe")]),
            ("second", vec![AppRule::by_executable("game.exe")]),
        ]);
        let app = ForegroundApp::new(r"C:\game.exe", "");
        assert_eq!(resolve(&doc, Some(&app)).unwrap().profile_name, "first", "lower order wins");

        doc.profiles[1].rules[0].priority = 5;
        assert_eq!(
            resolve(&doc, Some(&app)).unwrap().profile_name,
            "second",
            "higher rule priority beats profile order"
        );

        // Same priority and same order: the id decides, which is at least deterministic. Running
        // the resolution twice must give the same answer.
        doc.profiles[1].rules[0].priority = 0;
        doc.profiles[1].order = 0;
        let a = resolve(&doc, Some(&app)).unwrap();
        let b = resolve(&doc, Some(&app)).unwrap();
        assert_eq!(a.profile_id, b.profile_id);
    }

    #[test]
    fn path_matching_ignores_case_quotes_and_separator_style() {
        // Quoted, forward slashes, surrounding whitespace: all three are what a user pasting a
        // path out of a shortcut's properties dialog actually produces.
        let rule = exact("  \"C:/Games/game.exe\"  ");
        let app = ForegroundApp::new(r"c:\games\GAME.EXE", "");
        assert_eq!(rank(&rule, &app), 40);

        // A different program in the same directory must not match.
        let other = ForegroundApp::new(r"C:\Games\other.exe", "");
        assert_eq!(rank(&rule, &other), 0);
    }

    #[test]
    fn a_title_rule_uses_a_case_insensitive_regular_expression() {
        let rule = title("^Counter-Strike");
        assert_eq!(rank(&rule, &ForegroundApp::new("x.exe", "counter-strike 2")), 10);
        assert_eq!(rank(&rule, &ForegroundApp::new("x.exe", "Play Counter-Strike")), 0, "anchored");
        // No title at all cannot match a title rule.
        assert_eq!(
            rank(&rule, &ForegroundApp { executable_path: Some("x".into()), window_title: None }),
            0
        );
    }

    #[test]
    fn an_invalid_or_oversized_pattern_matches_nothing_instead_of_panicking() {
        // A hand-edited profile can contain anything. An unparseable pattern must behave as "no
        // match", not take the foreground watcher down.
        let broken = title("(unclosed");
        assert_eq!(rank(&broken, &ForegroundApp::new("x.exe", "unclosed")), 0);

        let huge = title(&"a".repeat(MAX_PATTERN_LENGTH + 1));
        assert_eq!(rank(&huge, &ForegroundApp::new("x.exe", "a".repeat(600))), 0);

        // A pattern that would make a backtracking engine explode. The point is that it returns.
        let nested = title("(a+)+$");
        let title_text = "a".repeat(40) + "b";
        let started = std::time::Instant::now();
        let _ = rank(&nested, &ForegroundApp::new("x.exe", title_text));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "a pathological pattern must not hang the resolver"
        );
    }

    #[test]
    fn an_executable_and_title_rule_needs_both() {
        let rule = AppRule {
            kind: AppMatchKind::ExecutableAndTitle,
            executable_name: Some("game.exe".into()),
            title_pattern: Some("Level 2".into()),
            ..AppRule::by_executable("game.exe")
        };
        assert_eq!(rank(&rule, &ForegroundApp::new(r"C:\game.exe", "Level 2")), 30);
        assert_eq!(rank(&rule, &ForegroundApp::new(r"C:\game.exe", "Level 1")), 0);
        assert_eq!(rank(&rule, &ForegroundApp::new(r"C:\other.exe", "Level 2")), 0);
    }

    #[test]
    fn no_foreground_window_falls_back_rather_than_failing() {
        let doc = doc_with_rules(vec![("default", vec![AppRule::by_executable("game.exe")])]);
        let picked = resolve(&doc, None).unwrap();
        assert_eq!(picked.reason, ResolutionReason::Default);
        // And a document with no profiles has no answer at all.
        assert!(resolve(&ProfileDocument::default(), None).is_none());
    }
}

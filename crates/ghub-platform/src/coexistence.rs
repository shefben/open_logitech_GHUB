//! Which other Logitech-controlling programs are running.
//!
//! Port of `PeripheralsService.ConflictingProcesses` in `Artemis.Core`. The C# had
//! `Process.GetProcesses()`; there is no equivalent in `std`, so this walks a toolhelp snapshot.
//!
//! The list exists because two programs writing the same device's lighting fight each other, and
//! the symptom - settings that revert a second after being applied - looks exactly like a bug in
//! this program. Naming the other program turns an unexplainable fault into an explainable one.
//!
//! Nothing here closes, suspends or otherwise interferes with another process. The C# said so in
//! the warning text it showed the user ("Artemis never closes them"), and that remains true: this
//! function only reads the process list.

/// Process names that control Logitech peripherals, without the `.exe`.
///
/// The same five the C# listed: G HUB's three processes, Logitech Gaming Software, and OpenRGB.
const KNOWN_CONFLICTS: &[&str] =
    &["lghub", "lghub_agent", "lghub_system_tray", "LCore", "OpenRGB"];

/// The conflicting programs currently running, de-duplicated, in the order [`KNOWN_CONFLICTS`]
/// lists them.
///
/// Returns an empty list rather than an error if the snapshot cannot be taken: the C# wrapped the
/// whole thing in `catch { return []; }`, and the reasoning holds - failing to enumerate processes
/// is not a reason to refuse to show a settings page.
pub fn conflicting_processes() -> Vec<String> {
    let running = running_process_names();
    KNOWN_CONFLICTS
        .iter()
        .filter(|name| running.iter().any(|r| r.eq_ignore_ascii_case(name)))
        .map(|name| (*name).to_owned())
        .collect()
}

/// The warning the tabs show, or an empty string. Kept here next to the list so the wording and
/// the promise not to close anything cannot drift apart.
pub fn conflict_warning() -> String {
    let conflicts = conflicting_processes();
    if conflicts.is_empty() {
        return String::new();
    }
    format!(
        "Also running: {}. They may overwrite settings and lighting. Close them for exclusive \
         control (this program never closes them).",
        conflicts.join(", ")
    )
}

#[cfg(windows)]
fn running_process_names() -> Vec<String> {
    use crate::sys;

    let mut names = Vec::new();
    // SAFETY: a toolhelp snapshot of the process list. The handle is checked against both null and
    // INVALID_HANDLE_VALUE before use and closed on every exit path. `entry` is a local the API
    // only writes within its own size, which `dwSize` tells it.
    unsafe {
        let snapshot = sys::CreateToolhelp32Snapshot(sys::TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot == sys::INVALID_HANDLE_VALUE {
            return names;
        }

        let mut entry = sys::PROCESSENTRY32W::default();
        if sys::Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                // `szExeFile` is the image name with its extension; the C# compared against
                // `Process.ProcessName`, which has the extension stripped.
                let file = sys::from_wide(&entry.szExeFile);
                let stem = file.strip_suffix(".exe").unwrap_or(&file);
                names.push(stem.to_owned());
                entry = sys::PROCESSENTRY32W::default();
                if sys::Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        sys::CloseHandle(snapshot);
    }
    names
}

/// Off Windows there is no G HUB to conflict with, and the pages simply show no warning.
#[cfg(not(windows))]
fn running_process_names() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The filter is case-insensitive because the image name's casing is whatever the installer
    /// wrote; "LGHUB.EXE" and "lghub.exe" are the same program.
    #[test]
    fn matching_ignores_case_and_keeps_the_canonical_spelling() {
        let running = ["LGHUB".to_owned(), "notepad".to_owned(), "openrgb".to_owned()];
        let matched: Vec<String> = KNOWN_CONFLICTS
            .iter()
            .filter(|name| running.iter().any(|r| r.eq_ignore_ascii_case(name)))
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(matched, vec!["lghub".to_owned(), "OpenRGB".to_owned()]);
    }

    #[test]
    fn no_conflicts_means_no_warning() {
        // On a machine with G HUB installed and running this is non-empty, so the assertion is on
        // the shape rather than the content: a warning exists exactly when the list does.
        assert_eq!(conflict_warning().is_empty(), conflicting_processes().is_empty());
    }

    /// Reading the process list must not panic or hang even on a machine with thousands of
    /// processes, and must never report a process this program cannot see.
    #[test]
    fn enumeration_completes_and_only_reports_known_names() {
        for name in conflicting_processes() {
            assert!(
                KNOWN_CONFLICTS.contains(&name.as_str()),
                "{name} is not in the known-conflict list"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_snapshot_finds_this_test_process() {
        // Proves the walk actually reads entries rather than silently returning nothing, which is
        // what every other assertion here would accept.
        let names = running_process_names();
        assert!(!names.is_empty(), "the process list came back empty");
        assert!(
            names.iter().any(|n| n.contains("ghub") || n.contains("test") || n.contains("cargo")),
            "the snapshot did not include this test runner: {names:?}"
        );
    }
}

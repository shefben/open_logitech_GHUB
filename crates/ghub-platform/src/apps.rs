//! The list of programs the user could write a rule for. Port of the `LoadApps()` method that the
//! Profiles and App-lighting tabs shared.
//!
//! The C# walked `Process.GetProcesses()` and kept the ones with a non-zero `MainWindowHandle`.
//! There is no `std` equivalent, and the window handle is the important half of that test: a rule
//! matches on the foreground executable, so the only useful candidates are processes that can
//! *be* in the foreground. So this goes the other way round - enumerate top-level visible windows
//! and resolve each one's process - which is both cheaper and closer to what the list means.
//!
//! Protected and higher-integrity processes cannot be opened for their image path. Those are
//! skipped rather than listed by window title, because a rule needs the executable name and a
//! title is not one.

/// One program the user can point a rule at.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RunningApp {
    /// The file name, which is what a rule matches on.
    pub name: String,
    /// The full image path, shown so two programs with the same file name can be told apart.
    pub path: String,
}

#[cfg(windows)]
pub fn running_apps() -> Vec<RunningApp> {
    windows::running_apps()
}

/// Nothing to enumerate off Windows; the pages that call this already say the manager is
/// Windows-only.
#[cfg(not(windows))]
pub fn running_apps() -> Vec<RunningApp> {
    Vec::new()
}

#[cfg(windows)]
mod windows {
    use std::collections::HashSet;

    use super::RunningApp;
    use crate::sys;

    /// Collected by the callback. A raw pointer to this is the only thing `EnumWindows` can carry.
    struct Collector {
        pids: Vec<u32>,
    }

    pub fn running_apps() -> Vec<RunningApp> {
        let mut collector = Collector { pids: Vec::new() };

        // SAFETY: `lParam` is a pointer to `collector`, which outlives the call because
        // `EnumWindows` is synchronous. The callback does nothing but push to it.
        unsafe {
            sys::EnumWindows(collect, &mut collector as *mut Collector as sys::LPARAM);
        }

        let mut seen_pid = HashSet::new();
        let mut seen_path = HashSet::new();
        let mut apps: Vec<RunningApp> = Vec::new();
        for pid in collector.pids {
            // One process can own many windows, so the pid is de-duplicated before the syscall
            // that resolves it rather than after.
            if !seen_pid.insert(pid) {
                continue;
            }
            let Some(path) = process_path(pid) else { continue };
            // And one program can be running twice, which is one entry in the list.
            if !seen_path.insert(path.to_lowercase()) {
                continue;
            }
            let name = file_name_of(&path);
            if name.is_empty() {
                continue;
            }
            apps.push(RunningApp { name, path });
        }

        apps.sort_by_key(|app| app.name.to_lowercase());
        apps
    }

    /// `EnumWindows` callback: keeps visible top-level windows that have a title.
    ///
    /// The title test is what excludes the invisible message-only windows that most processes own;
    /// without it the list is every service on the machine.
    unsafe extern "system" fn collect(window: sys::HANDLE, param: sys::LPARAM) -> sys::BOOL {
        // SAFETY: `param` is the pointer passed to `EnumWindows` just above, and the collector is
        // alive for the whole enumeration.
        let collector = unsafe { &mut *(param as *mut Collector) };

        // SAFETY: `window` is a handle the OS just handed us.
        unsafe {
            if sys::IsWindowVisible(window) == 0 || sys::GetWindowTextLengthW(window) == 0 {
                return 1;
            }
            let mut pid = 0u32;
            sys::GetWindowThreadProcessId(window, &mut pid);
            if pid != 0 && pid != sys::GetCurrentProcessId() {
                collector.pids.push(pid);
            }
        }
        1
    }

    /// Full image path of a process id, or `None` when it cannot be opened.
    fn process_path(pid: u32) -> Option<String> {
        // SAFETY: the handle is checked for null, closed on every path, and the buffer length is
        // passed by pointer as the API requires.
        unsafe {
            let process = sys::OpenProcess(sys::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut buffer = vec![0u16; 1024];
            let mut size = buffer.len() as u32;
            let ok =
                sys::QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size) != 0;
            sys::CloseHandle(process);
            if !ok || size == 0 {
                return None;
            }
            buffer.truncate(size as usize);
            Some(sys::from_wide(&buffer))
        }
    }

    fn file_name_of(path: &str) -> String {
        path.rsplit(['\\', '/']).next().unwrap_or_default().to_owned()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The enumeration has to actually find something, or a silently empty walk would look
        /// like "no programs are running" and the rule editor would offer an empty list forever.
        /// The test runner owns no window, so this asserts on the machine's own: a Windows session
        /// running a test always has at least one visible titled window (the console, the IDE, the
        /// shell).
        #[test]
        fn the_enumeration_finds_the_programs_on_this_machine() {
            let apps = running_apps();
            assert!(!apps.is_empty(), "no windowed process was found at all");
            for app in &apps {
                assert!(!app.name.is_empty());
                assert!(app.path.ends_with(&app.name), "{:?} is not its own path's tail", app);
            }
        }

        #[test]
        fn each_program_is_listed_once() {
            let apps = running_apps();
            let mut paths: Vec<String> = apps.iter().map(|a| a.path.to_lowercase()).collect();
            paths.sort();
            let before = paths.len();
            paths.dedup();
            assert_eq!(paths.len(), before, "a program was listed twice");
        }

        #[test]
        fn the_list_is_sorted_by_name_so_it_does_not_reshuffle_between_refreshes() {
            let apps = running_apps();
            let names: Vec<String> = apps.iter().map(|a| a.name.to_lowercase()).collect();
            let mut sorted = names.clone();
            sorted.sort();
            assert_eq!(names, sorted);
        }

        #[test]
        fn a_path_with_either_separator_yields_its_file_name() {
            assert_eq!(file_name_of(r"C:\Program Files\Game\game.exe"), "game.exe");
            assert_eq!(file_name_of("/usr/bin/game"), "game");
            assert_eq!(file_name_of("game.exe"), "game.exe");
            assert_eq!(file_name_of(""), "");
        }
    }
}

//! The two file dialogs. Stands in for `IWindowService.CreateOpenFileDialog` /
//! `CreateSaveFileDialog`.
//!
//! Export, import and backup all name a file, and a text box for a path is not a file picker -
//! the user would have to know where their profile store lives. So this is the real OS dialog,
//! declared directly in [`crate::sys`] like the rest of the platform layer rather than pulling in
//! a dependency for it.
//!
//! Both calls are **blocking and modal**, which is why the pages treat them as part of the click
//! rather than as background work: the dialog has to be on the UI thread, and a file chooser
//! running off it would be behind the window it belongs to.

/// One entry in a dialog's type dropdown.
pub struct FileFilter {
    /// What the dropdown shows, such as "JSON".
    pub label: &'static str,
    /// The extension without its dot, such as `json`.
    pub extension: &'static str,
}

impl FileFilter {
    pub const JSON: Self = Self { label: "JSON", extension: "json" };
}

#[cfg(windows)]
pub fn open_file(title: &str, filter: &FileFilter) -> Option<String> {
    windows::show(title, filter, None, false)
}

/// `suggested_name` is the file name the dialog opens with, which is what gives an export a
/// sensible default instead of an empty box.
#[cfg(windows)]
pub fn save_file(title: &str, filter: &FileFilter, suggested_name: &str) -> Option<String> {
    windows::show(title, filter, Some(suggested_name), true)
}

#[cfg(not(windows))]
pub fn open_file(_title: &str, _filter: &FileFilter) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn save_file(_title: &str, _filter: &FileFilter, _suggested_name: &str) -> Option<String> {
    None
}

#[cfg(windows)]
mod windows {
    use super::FileFilter;
    use crate::sys;

    /// The path buffer. `MAX_PATH` is not the limit on a long path, and a truncated path would be
    /// a silently wrong file, so this is the documented maximum the dialog will write.
    const BUFFER: usize = 32 * 1024;

    pub fn show(
        title: &str,
        filter: &FileFilter,
        suggested_name: Option<&str>,
        saving: bool,
    ) -> Option<String> {
        // The filter is a double-NUL-terminated list of NUL-separated pairs, so it cannot be
        // built with the usual wide-string helper.
        let mut pattern: Vec<u16> = Vec::new();
        for part in [
            format!("{} (*.{})", filter.label, filter.extension),
            format!("*.{}", filter.extension),
            "All files (*.*)".to_owned(),
            "*.*".to_owned(),
        ] {
            pattern.extend(part.encode_utf16());
            pattern.push(0);
        }
        pattern.push(0);

        let title = sys::to_wide(title);
        let default_extension = sys::to_wide(filter.extension);

        let mut file = vec![0u16; BUFFER];
        if let Some(name) = suggested_name {
            let encoded: Vec<u16> = name.encode_utf16().collect();
            // Only when it fits with room for the terminator; a name longer than the buffer is a
            // caller bug, and truncating it would suggest a file the user did not mean.
            if encoded.len() < file.len() {
                file[..encoded.len()].copy_from_slice(&encoded);
            }
        }

        let mut dialog = sys::OPENFILENAMEW {
            lpstrFilter: pattern.as_ptr(),
            nFilterIndex: 1,
            lpstrFile: file.as_mut_ptr(),
            nMaxFile: file.len() as u32,
            lpstrTitle: title.as_ptr(),
            lpstrDefExt: default_extension.as_ptr(),
            Flags: sys::OFN_EXPLORER
                | sys::OFN_PATHMUSTEXIST
                | sys::OFN_NOCHANGEDIR
                | if saving { sys::OFN_OVERWRITEPROMPT } else { sys::OFN_FILEMUSTEXIST },
            ..sys::OPENFILENAMEW::default()
        };

        // SAFETY: every pointer in `dialog` points at a live local that outlives the call, and
        // `nMaxFile` is the real length of the buffer behind `lpstrFile`.
        let chosen = unsafe {
            if saving {
                sys::GetSaveFileNameW(&mut dialog)
            } else {
                sys::GetOpenFileNameW(&mut dialog)
            }
        };
        // Zero is also what cancelling returns, so there is nothing to report either way: a
        // cancelled dialog is not an error.
        if chosen == 0 {
            return None;
        }
        let path = sys::from_wide(&file);
        if path.is_empty() { None } else { Some(path) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dialogs themselves cannot be opened in a test - they are modal and wait for a person -
    /// so what is checked is the one thing that is easy to get wrong and invisible when wrong.
    #[test]
    fn the_json_filter_names_the_extension_without_its_dot() {
        assert_eq!(FileFilter::JSON.extension, "json");
        assert!(!FileFilter::JSON.extension.starts_with('.'));
        assert_eq!(FileFilter::JSON.label, "JSON");
    }
}

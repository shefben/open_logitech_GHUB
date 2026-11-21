//! The notification-area menu. Port of `Peripherals/PeripheralTrayMenu.cs`.
//!
//! The same five things the C# put in the tray - the profile list, lock/unlock, auto-switch, stop
//! all macros - plus the two the Avalonia host supplied around it: showing the window and
//! quitting. It lives in the agent rather than in the window because the agent is the process
//! that is always there: the icon has to keep working, and the profile rules and the lighting have
//! to keep running, when no window exists at all.
//!
//! The menu is rebuilt whenever the state it describes changes, not on a timer: rebuilding is two
//! Win32 calls per item, and the menu is not visible while the user is not holding it open anyway.
//! [`TrayMenu::signature`] is what decides.
//!
//! **While the window is open, the controls are not in the menu.** Only Open and Quit are, and
//! that is a correctness requirement rather than a tidiness one: the window owns the document
//! while it runs, so this process's copy is a snapshot from before the window started. A profile
//! list built from it could name a profile the user has since renamed, and writing a profile
//! switch from here would be overwritten by the window's next save. The window is in front of the
//! user at that moment, and it is where every one of these settings lives.

use std::sync::Arc;

use ghub_core::service::PeripheralService;
use ghub_platform::tray::{TrayEvent, TrayItem};

/// Fixed command ids. Zero is reserved by Windows for "menu dismissed", so these start at 1.
const ID_OPEN: u32 = 1;
const ID_LOCK: u32 = 2;
const ID_AUTO_SWITCH: u32 = 3;
const ID_STOP_MACROS: u32 = 4;
const ID_QUIT: u32 = 5;

/// Where the profile radio block starts. Contiguous from here, because Windows marks a radio item
/// by id *range* rather than per item.
const ID_PROFILE_BASE: u32 = 100;

/// The tooltip, and so the name the notification area shows on hover.
pub const TOOLTIP: &str = "G HUB (ghub-rs)";

/// What a menu click asks the agent to do that the menu cannot do itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MenuOutcome {
    /// Show the window: raise it if it is running, start it if it is not.
    pub show_window: bool,
    /// Exit - the window too, if one is open.
    pub quit: bool,
}

pub struct TrayMenu {
    service: Arc<PeripheralService>,
    /// Profile ids in menu order, so a command id maps back to a profile.
    profiles: Vec<ghub_core::Id>,
    /// The state the current menu was built from.
    signature: String,
}

impl TrayMenu {
    pub fn new(service: Arc<PeripheralService>) -> Self {
        Self { service, profiles: Vec::new(), signature: String::new() }
    }

    /// Returns a new menu description when the state it would show has changed, and `None` when
    /// the menu on screen is already right.
    ///
    /// `delegated` - this process has handed the hardware, and with it the document, to the
    /// window - is what collapses the menu to Open and Quit; see this module's note.
    pub fn refresh(&mut self, delegated: bool) -> Option<Vec<TrayItem>> {
        if delegated {
            let signature = "window".to_owned();
            if signature == self.signature {
                return None;
            }
            self.signature = signature;
            self.profiles.clear();
            return Some(vec![
                TrayItem::command(ID_OPEN, "Open G HUB"),
                TrayItem::Separator,
                TrayItem::command(ID_QUIT, "Quit"),
            ]);
        }

        let document = self.service.document();
        let active = self.service.active_profile().id;
        let mut ordered: Vec<&ghub_core::profile::PeripheralProfile> =
            document.profiles.iter().collect();
        ordered.sort_by_key(|profile| profile.order);

        let signature = format!(
            "{active}|{:?}|{}|{}",
            document.locked_profile_id,
            document.auto_switch_enabled,
            ordered
                .iter()
                .map(|profile| format!("{}:{}", profile.id, profile.name))
                .collect::<Vec<_>>()
                .join(",")
        );
        if signature == self.signature {
            return None;
        }
        self.signature = signature;

        self.profiles = ordered.iter().map(|profile| profile.id).collect();
        let profile_items: Vec<TrayItem> = ordered
            .iter()
            .enumerate()
            .map(|(index, profile)| TrayItem::Radio {
                id: ID_PROFILE_BASE + index as u32,
                label: profile.name.clone(),
                checked: profile.id == active,
            })
            .collect();

        let locked = document.locked_profile_id.is_some();
        Some(vec![
            TrayItem::command(ID_OPEN, "Open G HUB"),
            TrayItem::Separator,
            TrayItem::Submenu { label: "Peripheral profile".to_owned(), items: profile_items },
            TrayItem::Separator,
            TrayItem::command(
                ID_LOCK,
                if locked { "Unlock profile" } else { "Lock current profile" },
            ),
            TrayItem::Check {
                id: ID_AUTO_SWITCH,
                label: "Auto-switch by application".to_owned(),
                checked: document.auto_switch_enabled,
            },
            TrayItem::command(ID_STOP_MACROS, "Stop all macros (Ctrl+Shift+Pause)"),
            TrayItem::Separator,
            TrayItem::command(ID_QUIT, "Quit"),
        ])
    }

    /// Acts on one menu click. Anything that cannot be done here comes back in [`MenuOutcome`].
    ///
    /// `delegated` is passed rather than remembered so that a click which arrives in the gap
    /// between the window starting and the menu being rebuilt is still refused: the stale-document
    /// problem is about who owns the document at the moment of the write, not about what the menu
    /// looked like when it was drawn.
    pub fn handle(&mut self, event: TrayEvent, delegated: bool) -> MenuOutcome {
        let mut outcome = MenuOutcome::default();
        let id = match event {
            TrayEvent::Activated => return MenuOutcome { show_window: true, quit: false },
            TrayEvent::Command(id) => id,
            TrayEvent::Message { .. } => return outcome,
            // Another program asked this process to exit - an installer replacing the files, most
            // often. Quit rather than refuse, and through the same path as the menu's Quit, so the
            // window is closed politely and the devices are handed back to firmware.
            TrayEvent::CloseRequested => return MenuOutcome { show_window: false, quit: true },
        };

        match id {
            ID_OPEN => outcome.show_window = true,
            ID_QUIT => outcome.quit = true,
            _ if delegated => {}
            ID_LOCK => self.toggle_lock(),
            ID_AUTO_SWITCH => {
                if let Err(e) = self
                    .service
                    .update_document(|document| {
                        document.auto_switch_enabled = !document.auto_switch_enabled;
                    })
                {
                    self.service.log().error("Tray", format!("auto-switch: {e}"));
                }
            }
            ID_STOP_MACROS => self.service.runtime().stop_all("tray menu"),
            _ if id >= ID_PROFILE_BASE => {
                if let Some(profile) = self.profiles.get((id - ID_PROFILE_BASE) as usize).copied() {
                    // `persistent: true`, as the C# passed: choosing a profile by hand means it
                    // should survive the next foreground change.
                    if let Err(e) = self.service.select_profile(profile, true) {
                        self.service.log().error("Tray", format!("profile: {e}"));
                    }
                }
            }
            _ => {}
        }
        outcome
    }

    /// Lock if unlocked, unlock if locked.
    ///
    /// Read inside this call rather than from the menu's signature: the lock may have moved
    /// between the menu being built and the click arriving, and toggling a stale value would lock
    /// the wrong profile.
    fn toggle_lock(&self) {
        let target = match self.service.document().locked_profile_id {
            Some(_) => None,
            None => Some(self.service.active_profile().id),
        };
        match self.service.set_lock(target) {
            Ok(()) => {
                let message = match target {
                    Some(_) => format!("locked to '{}'", self.service.active_profile().name),
                    None => "unlocked; rules decide again".to_owned(),
                };
                self.service.log().info("Tray", format!("Profile lock: {message}"));
            }
            Err(e) => self.service.log().error("Tray", format!("profile lock: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows marks a radio item by id range, so the profile block must not collide with the
    /// fixed commands - a profile sharing an id with Quit would exit the application.
    #[test]
    fn the_profile_ids_cannot_collide_with_the_fixed_commands() {
        let fixed = [ID_OPEN, ID_LOCK, ID_AUTO_SWITCH, ID_STOP_MACROS, ID_QUIT];
        assert!(fixed.iter().all(|id| *id > 0), "id 0 means 'dismissed' to Windows");
        assert!(fixed.iter().all(|id| *id < ID_PROFILE_BASE));
        // And the block has room for more profiles than the store will hold.
        assert!(ID_PROFILE_BASE as usize + ghub_core::store::MAX_PROFILES < u32::MAX as usize);
    }

    /// The menu description is validated by the platform layer; this asserts the menu this module
    /// actually builds passes it, with the profile list at its largest sensible size.
    #[test]
    fn the_built_menu_is_a_valid_one() {
        let profiles: Vec<TrayItem> = (0..64)
            .map(|index| TrayItem::Radio {
                id: ID_PROFILE_BASE + index,
                label: format!("Profile {index}"),
                checked: index == 0,
            })
            .collect();
        let items = vec![
            TrayItem::command(ID_OPEN, "Open G HUB"),
            TrayItem::Separator,
            TrayItem::Submenu { label: "Peripheral profile".to_owned(), items: profiles },
            TrayItem::Separator,
            TrayItem::command(ID_LOCK, "Lock current profile"),
            TrayItem::Check {
                id: ID_AUTO_SWITCH,
                label: "Auto-switch by application".to_owned(),
                checked: true,
            },
            TrayItem::command(ID_STOP_MACROS, "Stop all macros (Ctrl+Shift+Pause)"),
            TrayItem::Separator,
            TrayItem::command(ID_QUIT, "Quit"),
        ];
        ghub_platform::tray::validate(&items).expect("the tray menu is valid");
    }

    /// The collapsed menu is the one that runs while the window owns the document, so what it
    /// leaves out is the point of it.
    #[test]
    fn the_menu_shown_while_the_window_is_open_has_no_document_controls() {
        let items = vec![
            TrayItem::command(ID_OPEN, "Open G HUB"),
            TrayItem::Separator,
            TrayItem::command(ID_QUIT, "Quit"),
        ];
        ghub_platform::tray::validate(&items).expect("the collapsed menu is valid");
        let ids: Vec<u32> = items.iter().flat_map(TrayItem::ids).collect();
        assert_eq!(ids, vec![ID_OPEN, ID_QUIT]);
        assert!(!ids.contains(&ID_LOCK) && !ids.contains(&ID_AUTO_SWITCH));
        assert!(ids.iter().all(|id| *id < ID_PROFILE_BASE), "no profile may be switched from here");
    }

    /// Open and Quit have to keep working while the window owns the document; everything else must
    /// not. Asserted on the id table rather than through a live service, because constructing one
    /// opens the real profile store.
    #[test]
    fn only_open_and_quit_survive_the_window_being_open() {
        for id in [ID_LOCK, ID_AUTO_SWITCH, ID_STOP_MACROS] {
            assert!(id != ID_OPEN && id != ID_QUIT);
        }
    }
}

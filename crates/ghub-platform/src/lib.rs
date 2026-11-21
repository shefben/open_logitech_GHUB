//! Host integration: input injection, low-level input hooks, foreground detection and launching.
//!
//! This is the only crate that talks to the operating system for anything other than HID. It is
//! the port of `Artemis.UI.Windows/Peripherals/WindowsPeripheralPlatform.cs` and
//! `Artemis.Peripherals/Input/InputRules.cs`, and it is the only piece of the rewrite that is new
//! Windows code rather than a translation, because the C# leaned on `System.Windows.Forms` and
//! Avalonia for the tray and on `Process.Start` for launching.
//!
//! The split is deliberate:
//!
//! * [`rules`] is pure arithmetic over virtual-key codes and hook message numbers. It compiles
//!   and is tested on any host, which is what lets the suppression invariant - *an up is
//!   swallowed exactly when its down was* - be tested at all.
//! * [`sys`] is the raw Win32 surface, in one file, with the struct layouts asserted.
//! * [`windows`] is the live implementation, behind `cfg(windows)`.
//! * [`stub`] is a recording implementation used by the service tests, so the orchestrator can be
//!   exercised end to end without installing a hook or moving the real cursor.
//!
//! Nothing here elevates, installs a driver or filters input at kernel level. The visible
//! consequence is that input to an elevated window is neither seen nor injectable (UIPI); the
//! diagnostics page says so rather than letting the user guess why a macro does nothing in one
//! particular game.

pub mod apps;
pub mod audio_capture;
pub mod autostart;
pub mod coexistence;
pub mod dialogs;
pub mod handoff;
pub mod instance;
pub mod rules;
pub mod screen_capture;
pub mod stub;
pub mod sys;
pub mod tray;

#[cfg(windows)]
pub mod windows;

#[cfg(windows)]
pub use windows::{HookHost, WindowsInjector, WindowsPlatform, INJECTION_MARKER, system_random};

/// There is no OS CSPRNG to reach for off Windows, and inventing one here would be worse than
/// having none: the automation token is the only caller, and it refuses to be created without
/// real entropy.
#[cfg(not(windows))]
pub fn system_random(_length: usize) -> Result<Vec<u8>, String> {
    Err("no system random source on this platform".to_owned())
}

// The host contract itself lives in `ghub-core`, because the orchestrator there implements
// `HookSink` and calls `PlatformServices`; see that module for why. Re-exported so callers of
// this crate get everything from one place.
pub use ghub_core::platform::{HookSink, HookStatus, PlatformServices};

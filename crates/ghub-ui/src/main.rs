//! The window of the G HUB-parity peripheral manager for the G903 LIGHTSPEED and G815.
//!
//! **This process is only the window.** `ghub-agent.exe` is what stays resident, owns the
//! notification-area icon, and keeps the hooks, the profile rules, the macros and the lighting
//! running; this one starts, borrows the hardware from it, and exits completely when closed. The
//! reason is measurable: a window costs ~390 MB of working set once a GL context, a swapchain and
//! two device renders of 3600x1374 and 1440x2858 pixels are resident, against ~7 MB for the
//! agent, and before the split the application paid the window's price for as long as the user
//! left it running - closing it only hid it. See `ghub_platform::handoff` for the hand-over
//! protocol.
//!
//! The shell mirrors `Artemis.UI/Screens/Peripherals/PeripheralsView.axaml`: a top navigation
//! strip over a content frame, with the same ten pages in the same order as
//! `PeripheralsViewModel.Tabs`.

// G HUB is a desktop app, not a console one.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]


mod artwork;
mod color_picker;
mod controls;
mod host;
mod icons;
mod shared;
mod theme;
mod widgets;

mod pages;

use ghub_platform::handoff::{self, Takeover};

use shared::{Changes, Shared};

/// The pages from `PeripheralsViewModel.Tabs`, in order. The route names are kept so deep links
/// and the tray menu can address a page the way the Artemis build did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Devices,
    Mouse,
    Keyboard,
    AppLighting,
    Macros,
    Profiles,
    Automation,
    Import,
    Diagnostics,
    Options,
}

impl Page {
    pub const ALL: [Page; 10] = [
        Page::Devices,
        Page::Mouse,
        Page::Keyboard,
        Page::AppLighting,
        Page::Macros,
        Page::Profiles,
        Page::Automation,
        Page::Import,
        Page::Diagnostics,
        Page::Options,
    ];

    /// The caption shown in the nav strip - the exact strings from `PeripheralsViewModel`.
    pub fn title(self) -> &'static str {
        match self {
            Page::Devices => "Devices",
            Page::Mouse => "Mouse",
            Page::Keyboard => "Keyboard",
            Page::AppLighting => "App lighting",
            Page::Macros => "Macros",
            Page::Profiles => "Profiles & rules",
            Page::Automation => "Automation",
            Page::Import => "G HUB import",
            Page::Diagnostics => "Diagnostics",
            Page::Options => "Options",
        }
    }

    /// The route it was registered under in `Routes.cs`.
    pub fn route(self) -> &'static str {
        match self {
            Page::Devices => "peripherals/devices",
            Page::Mouse => "peripherals/mouse",
            Page::Keyboard => "peripherals/keyboard",
            Page::AppLighting => "peripherals/app-lighting",
            Page::Macros => "peripherals/macros",
            Page::Profiles => "peripherals/profiles",
            Page::Automation => "peripherals/automation",
            Page::Import => "peripherals/import",
            Page::Diagnostics => "peripherals/diagnostics",
            Page::Options => "peripherals/options",
        }
    }
}

pub struct App {
    page: usize,
    shared: Shared,
    icons: icons::IconCache,
    pages: pages::Pages,
    /// Set when a page has not yet seen its first load. `RoutableScreen` called `Refresh()` from
    /// `WhenActivated`, so a tab loaded its state the first time it was shown rather than at
    /// start-up; the same here, which keeps the first frame cheap.
    pending: Changes,
}

impl App {
    /// `takeover` is what the agent said when asked for the hardware. It is reported through the
    /// diagnostics log rather than acted on: whatever the answer, this process ends up owning the
    /// devices, because a window that refused to open would leave the user no way to find out why.
    fn new(ctx: &egui::Context, takeover: Takeover) -> Self {
        theme::install(ctx);
        let shared = Shared::new(host::Host::new(ctx.clone(), true));
        match takeover {
            Takeover::Granted => {}
            Takeover::NoAgent => shared.host.log.warning(
                "Agent",
                "No background agent is running, so closing this window will stop remapped keys, \
                 macros, profile rules and lighting. Reinstall to restore ghub-agent.exe beside \
                 this one."
                    .to_owned(),
            ),
            Takeover::TimedOut => shared.host.log.error(
                "Agent",
                "The background agent did not release the devices in time. Both processes may \
                 have them open, which shows up as failed transactions until the agent finishes."
                    .to_owned(),
            ),
        }
        Self {
            page: 0,
            shared,
            icons: icons::IconCache::new(),
            pages: pages::Pages::default(),
            pending: Changes::all(),
        }
    }
}

impl eframe::App for App {
    /// `Border.gh-root` fills the window with `GHubBg`, so the frame behind it must match or the
    /// rounded corners show the default panel grey.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // One drain per frame, before anything draws, so every page this frame sees the same
        // state. This is `Dispatcher.UIThread.Post` collapsed into the frame loop.
        let changes = self.shared.pump();
        self.pending.document |= changes.document;
        self.pending.devices |= changes.devices;
        self.pending.recording |= changes.recording;

        // A finished job wakes the context from the worker thread, so there is nothing to poll
        // for here. The one case that still needs a standing request is work *in flight*: the
        // busy state disables controls, and that has to un-grey itself without user input.
        if self.shared.busy() {
            ui.ctx().request_repaint();
        }

        // The document's application-wide settings can move without this page being open - an
        // import, a restored backup, the automation API - so they are reconciled from the event
        // rather than from the page that usually changes them.
        if changes.document {
            self.shared.host.apply_app_settings();
        }

        // Nothing cancels the close any more. The window used to hide itself so that the hooks,
        // the rules and the lighting kept running; they now run in the agent, which is the whole
        // point of it, so closing can mean closing and the memory this process costs goes back to
        // the machine. Quit for the whole application lives in the agent's tray menu, and on the
        // Options page.
        let titles: Vec<&str> = Page::ALL.iter().map(|p| p.title()).collect();

        egui::Panel::top("nav")
            .frame(egui::Frame::new().fill(theme::BG))
            .show_separator_line(false)
            .resizable(false)
            .show(ui, |ui| {
                widgets::top_nav(ui, &titles, &mut self.page);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(theme::CONTENT_PAD))
            .show(ui, |ui| {
                let page = Page::ALL[self.page];
                let pending = std::mem::take(&mut self.pending);
                self.pages.show(
                    &mut self.shared,
                    &mut self.icons,
                    ui,
                    page,
                    pending,
                );
            });

    }

    /// Hands the hardware back: spy mapping to identity, G-keys to firmware control, per-key
    /// lighting to an onboard effect. Doing this on close is what keeps a closed app from leaving
    /// a keyboard dark and its G-keys inert.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shared.host.shutdown();
        // Only once every device is closed, because the agent starts opening them the moment it
        // reads this. The agent is also waiting on this process's handle, so a crash that skips
        // this still gives the hardware back - just a moment later.
        handoff::notify_resume(std::process::id());
    }
}

fn main() -> eframe::Result {
    env_logger::init();

    // One window at a time. Two would both ask the agent for the hardware and both get it. A
    // second launch broadcasts "show the window" and exits; the agent receives that and answers it
    // by raising the window that is already up, which is what double-clicking a desktop shortcut
    // while the application is running should do.
    let instance = ghub_platform::instance::claim();
    if instance == ghub_platform::instance::Instance::AlreadyRunning {
        return Ok(());
    }

    // Before the window exists, because the stack it builds opens the devices. The agent is
    // started if it is not already running: it owns the tray icon and is what keeps profile rules
    // alive after this window closes, so it has to exist even when the user starts the window
    // directly.
    let takeover = handoff::request_takeover(std::process::id(), true);

    let mut viewport = egui::ViewportBuilder::default()
        // G HUB's device screen at its default size, measured from the 1920x1080 captures.
        .with_inner_size([1280.0, 860.0])
        .with_min_inner_size([1024.0, 720.0])
        .with_title("G HUB");
    // The window and taskbar icon is G HUB's own G mark, white, from the extracted asset set.
    if let Some((width, height, rgba)) = icons::rgba(icons::Icon::GLogo, [255, 255, 255]) {
        viewport = viewport.with_icon(std::sync::Arc::new(egui::IconData {
            rgba,
            width,
            height,
        }));
    }

    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let result = eframe::run_native(
        "ghub",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(&cc.egui_ctx, takeover)))),
    );
    // Held until here, so the name is claimed for exactly as long as the process is running.
    drop(instance);
    result
}

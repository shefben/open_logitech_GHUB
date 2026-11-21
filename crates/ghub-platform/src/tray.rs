//! The notification-area icon and its menu. Stands in for Avalonia's `TrayIcon` plus
//! `Peripherals/PeripheralTrayMenu.cs`.
//!
//! Avalonia supplied the icon and the native menu; egui has neither, so this is the Win32 version:
//! a hidden window on a thread of its own, a `Shell_NotifyIconW` registration against it, and a
//! popup menu built from a description the caller hands over.
//!
//! Three decisions worth stating:
//!
//! * **The window is a real top-level window, not `HWND_MESSAGE`.** A message-only window is
//!   cheaper and would be the obvious choice for something that never draws, but it does not
//!   receive broadcast messages - and the broadcast is how a second instance of the application
//!   tells the first one to show itself. See [`crate::instance`].
//! * **The menu is rebuilt from a description, not mutated.** The C# found its existing
//!   `NativeMenuItem` and replaced its submenu; here the caller passes the whole menu every time
//!   something changes. A menu built fresh cannot disagree with the document it was built from,
//!   which is the entire class of bug the C#'s in-place update was exposed to.
//! * **Clicks come back through a queue the caller drains, with a wake-up.** egui repaints on
//!   input or on request, and a tray click is neither: without waking the context, choosing a
//!   profile from the tray would do nothing visible until the user touched the window.
//!
//! Nothing here runs a second message loop on the UI thread. The tray's window has its own
//! thread, which is also why [`Tray::stop`] posts rather than calls.

/// What the caller wants in the menu. Ids are the caller's own; the tray only hands them back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayItem {
    Command { id: u32, label: String, enabled: bool },
    Check { id: u32, label: String, checked: bool },
    /// A radio item. Every radio id inside one menu must be a contiguous block, because Windows
    /// marks the dot by id range (`CheckMenuRadioItem`) rather than per item.
    Radio { id: u32, label: String, checked: bool },
    Separator,
    Submenu { label: String, items: Vec<TrayItem> },
}

impl TrayItem {
    pub fn command(id: u32, label: impl Into<String>) -> Self {
        Self::Command { id, label: label.into(), enabled: true }
    }

    /// The ids this item and everything under it can produce.
    pub fn ids(&self) -> Vec<u32> {
        match self {
            Self::Command { id, .. } | Self::Check { id, .. } | Self::Radio { id, .. } => vec![*id],
            Self::Separator => Vec::new(),
            Self::Submenu { items, .. } => items.iter().flat_map(Self::ids).collect(),
        }
    }
}

/// A 32-bit RGBA image for the icon. Supplied by the caller because the icon is one of the
/// extracted G HUB assets and decoding PNGs is not this crate's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, RGBA, top row first.
    pub rgba: Vec<u8>,
}

impl TrayImage {
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.rgba.len() == (self.width as usize) * (self.height as usize) * 4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// A menu item was chosen.
    Command(u32),
    /// The icon was clicked, or another instance asked this one to show itself.
    Activated,
    /// One of the registered messages named in [`Tray::start`] arrived.
    ///
    /// `index` is that name's position in the list, so a caller matches on its own constant
    /// rather than on a registered id whose value is assigned by the OS at run time. Forwarded
    /// rather than handled in the window procedure, which runs on the tray thread: the things
    /// these messages ask for - releasing every device, taking them back - belong to whichever
    /// thread owns the stack, and doing them inside the procedure would mean doing them while the
    /// sender is blocked.
    Message { index: usize, wparam: usize },
    /// The hidden window was sent `WM_CLOSE`.
    ///
    /// That is how other programs ask a windowed process to exit: installers closing an
    /// application before replacing its files, `taskkill` without `/F`, the Restart Manager. Left
    /// to `DefWindowProcW` it would destroy only this window - the tray thread would end and the
    /// icon vanish while the process kept its hooks and its devices with no way left to reach it.
    /// So it is forwarded instead, and the owner treats it as Quit.
    CloseRequested,
}

/// The name of the registered window message a second instance broadcasts to ask the running one
/// to show itself.
///
/// Registered rather than invented: `RegisterWindowMessageW` returns an id that is unique
/// system-wide, so no other application can mistake the broadcast for one of its own private
/// messages. Lives here because the tray's window is what receives it - see [`crate::instance`].
pub const SHOW_MESSAGE_NAME: &str = "ghub-rs.show-window";

/// The window class the tray's hidden window is registered under.
///
/// Public because the other binary finds that window by class to send it a message: the agent is
/// the process with the tray icon, so its tray window is also its address.
pub const WINDOW_CLASS: &str = "ghub-rs.tray";
/// The hidden window's title. Paired with the class in `FindWindowW` so a stale class registration
/// from a crashed process cannot match.
pub const WINDOW_TITLE: &str = "ghub-rs tray";

/// The ids a menu description must not reuse.
///
/// Windows treats `0` from `TrackPopupMenu` as "nothing was chosen", so an item with id 0 would be
/// indistinguishable from a dismissed menu.
pub const RESERVED_ID: u32 = 0;

/// Whether a menu description can be used as it stands.
///
/// Checked before the menu is built rather than after a click produces the wrong action: a
/// duplicate id is a menu where two items do the same thing, which is not visible by looking at
/// it.
pub fn validate(items: &[TrayItem]) -> Result<(), String> {
    let mut ids: Vec<u32> = items.iter().flat_map(TrayItem::ids).collect();
    if ids.contains(&RESERVED_ID) {
        return Err("a tray item cannot use id 0: it is how Windows reports a dismissed menu"
            .to_owned());
    }
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != total {
        return Err("two tray items share an id".to_owned());
    }
    Ok(())
}

#[cfg(windows)]
pub use windows::Tray;

#[cfg(not(windows))]
pub use stub::Tray;

/// Off Windows there is no notification area, so the tray accepts everything and reports nothing.
/// The window still runs; it simply has no tray icon.
#[cfg(not(windows))]
mod stub {
    use super::{TrayEvent, TrayImage, TrayItem};

    pub struct Tray;

    impl Tray {
        pub fn start(
            _icon: TrayImage,
            _tooltip: &str,
            _wake: impl Fn() + Send + Sync + 'static,
        ) -> Result<Self, String> {
            Err("the notification area is a Windows feature".to_owned())
        }

        pub fn set_menu(&self, _items: Vec<TrayItem>) {}

        pub fn drain(&self) -> Vec<TrayEvent> {
            Vec::new()
        }

        pub fn stop(&mut self) {}
    }
}

#[cfg(windows)]
mod windows {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{SHOW_MESSAGE_NAME, TrayEvent, TrayImage, TrayItem, WINDOW_CLASS, WINDOW_TITLE, validate};
    use crate::sys;

    /// The tray's own callback message. `uCallbackMessage` must be in the `WM_APP` range.
    const WM_TRAY: u32 = sys::WM_APP + 1;
    /// Posted to ask the tray thread to rebuild its menu from [`Shared::pending_menu`].
    const WM_REBUILD: u32 = sys::WM_APP + 2;
    /// Posted to ask the tray thread to shut down.
    const WM_SHUTDOWN: u32 = sys::WM_APP + 3;

    struct Shared {
        events: Mutex<Vec<TrayEvent>>,
        pending_menu: Mutex<Option<Vec<TrayItem>>>,
        wake: Box<dyn Fn() + Send + Sync>,
        /// Set once the window exists, so `start` can report a failure rather than returning a
        /// handle to nothing.
        ready: AtomicBool,
        failed: Mutex<Option<String>>,
    }

    // Per-thread state for the window procedure. A thread-local rather than
    // `SetWindowLongPtrW` user data: the window is created on this thread and its procedure only
    // ever runs on this thread, so there is nothing to look up.
    thread_local! {
        static STATE: RefCell<Option<ThreadState>> = const { RefCell::new(None) };
    }

    struct ThreadState {
        shared: Arc<Shared>,
        menu: Option<sys::HANDLE>,
        show_message: u32,
        /// Registered ids for the names passed to `start`, in the order they were given.
        messages: Vec<u32>,
    }

    pub struct Tray {
        shared: Arc<Shared>,
        thread_id: u32,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Tray {
        /// Registers the icon. `wake` is called whenever an event is queued.
        pub fn start(
            icon: TrayImage,
            tooltip: &str,
            messages: &[&str],
            wake: impl Fn() + Send + Sync + 'static,
        ) -> Result<Self, String> {
            if !icon.is_valid() {
                return Err("the tray icon image is not width * height * 4 bytes".to_owned());
            }

            let shared = Arc::new(Shared {
                events: Mutex::new(Vec::new()),
                pending_menu: Mutex::new(None),
                wake: Box::new(wake),
                ready: AtomicBool::new(false),
                failed: Mutex::new(None),
            });

            let (sender, receiver) = std::sync::mpsc::channel::<u32>();
            let thread_shared = Arc::clone(&shared);
            let tooltip = tooltip.to_owned();
            let names: Vec<String> = messages.iter().map(|name| (*name).to_owned()).collect();
            let worker = std::thread::Builder::new()
                .name("ghub tray".to_owned())
                .spawn(move || run(thread_shared, icon, &tooltip, &names, &sender))
                .map_err(|e| format!("the tray thread could not be started: {e}"))?;

            // The thread id comes back from the thread itself, because posting to it is the only
            // way to ask it to do anything and a wrong id would silently do nothing.
            let thread_id = receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| "the tray thread did not start".to_owned())?;

            if !shared.ready.load(Ordering::SeqCst) {
                let reason = shared
                    .failed
                    .lock()
                    .ok()
                    .and_then(|slot| slot.clone())
                    .unwrap_or_else(|| "the tray icon could not be registered".to_owned());
                let _ = worker.join();
                return Err(reason);
            }

            Ok(Self { shared, thread_id, worker: Some(worker) })
        }

        /// Replaces the menu. Rejected descriptions are reported rather than shown wrongly.
        pub fn set_menu(&self, items: Vec<TrayItem>) {
            if let Err(reason) = validate(&items) {
                debug_assert!(false, "{reason}");
                return;
            }
            if let Ok(mut slot) = self.shared.pending_menu.lock() {
                *slot = Some(items);
            }
            // SAFETY: a thread message to a thread this handle owns. A posted message to a thread
            // that has exited is simply refused.
            unsafe {
                sys::PostThreadMessageW(self.thread_id, WM_REBUILD, 0, 0);
            }
        }

        pub fn drain(&self) -> Vec<TrayEvent> {
            match self.shared.events.lock() {
                Ok(mut events) => std::mem::take(&mut *events),
                Err(e) => std::mem::take(&mut *e.into_inner()),
            }
        }

        /// Removes the icon and ends the thread. The icon must be removed explicitly: an
        /// unregistered icon stays in the notification area as a ghost until the user hovers it.
        pub fn stop(&mut self) {
            // SAFETY: as `set_menu`.
            unsafe {
                sys::PostThreadMessageW(self.thread_id, WM_SHUTDOWN, 0, 0);
            }
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn run(
        shared: Arc<Shared>,
        icon: TrayImage,
        tooltip: &str,
        names: &[String],
        sender: &std::sync::mpsc::Sender<u32>,
    ) {
        // SAFETY: the whole function is one thread's window lifetime. Every handle created is
        // destroyed on the way out, and every pointer handed to Win32 points at a live local.
        unsafe {
            let thread_id = sys::GetCurrentThreadId();
            let instance = sys::GetModuleHandleW(std::ptr::null());
            let class = sys::to_wide(WINDOW_CLASS);

            let descriptor = sys::WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
            };
            // A second registration of the same class fails harmlessly; the window creation below
            // is what actually has to succeed.
            sys::RegisterClassW(&descriptor);

            let title = sys::to_wide(WINDOW_TITLE);
            let hwnd = sys::CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                fail(&shared, format!("the tray window could not be created (error {})", sys::GetLastError()));
                let _ = sender.send(thread_id);
                return;
            }

            let hicon = create_icon(&icon);
            let show_message = sys::RegisterWindowMessageW(sys::to_wide(SHOW_MESSAGE_NAME).as_ptr());
            // The same name registers to the same id in every process, which is what makes this
            // an address the other binary can write to without being told a number.
            let messages: Vec<u32> = names
                .iter()
                .map(|name| sys::RegisterWindowMessageW(sys::to_wide(name).as_ptr()))
                .collect();

            STATE.with(|state| {
                *state.borrow_mut() = Some(ThreadState {
                    shared: Arc::clone(&shared),
                    menu: None,
                    show_message,
                    messages,
                });
            });

            let mut data = sys::NOTIFYICONDATAW {
                hWnd: hwnd,
                uID: 1,
                uFlags: sys::NIF_MESSAGE | sys::NIF_ICON | sys::NIF_TIP,
                uCallbackMessage: WM_TRAY,
                hIcon: hicon,
                ..Default::default()
            };
            let tip = sys::to_wide(tooltip);
            let copied = tip.len().min(data.szTip.len() - 1);
            data.szTip[..copied].copy_from_slice(&tip[..copied]);

            if sys::Shell_NotifyIconW(sys::NIM_ADD, &mut data) == 0 {
                fail(&shared, "the notification area refused the icon".to_owned());
                sys::DestroyWindow(hwnd);
                if !hicon.is_null() {
                    sys::DestroyIcon(hicon);
                }
                let _ = sender.send(thread_id);
                return;
            }

            shared.ready.store(true, Ordering::SeqCst);
            let _ = sender.send(thread_id);

            let mut message: sys::MSG = std::mem::zeroed();
            loop {
                let result = sys::GetMessageW(&mut message, std::ptr::null_mut(), 0, 0);
                if result <= 0 {
                    break;
                }
                if message.hwnd.is_null() && message.message == WM_SHUTDOWN {
                    break;
                }
                // A thread message (`hwnd` null) has no window to dispatch to, so the rebuild
                // request is handled here rather than in the procedure.
                if message.hwnd.is_null() && message.message == WM_REBUILD {
                    rebuild_menu();
                    continue;
                }
                sys::TranslateMessage(&message);
                sys::DispatchMessageW(&message);
            }

            sys::Shell_NotifyIconW(sys::NIM_DELETE, &mut data);
            if !hicon.is_null() {
                sys::DestroyIcon(hicon);
            }
            STATE.with(|state| {
                if let Some(state) = state.borrow_mut().take() {
                    if let Some(menu) = state.menu {
                        sys::DestroyMenu(menu);
                    }
                }
            });
            sys::DestroyWindow(hwnd);
        }
    }

    fn fail(shared: &Arc<Shared>, reason: String) {
        if let Ok(mut slot) = shared.failed.lock() {
            *slot = Some(reason);
        }
    }

    fn push(event: TrayEvent) {
        STATE.with(|state| {
            if let Some(state) = state.borrow().as_ref() {
                if let Ok(mut events) = state.shared.events.lock() {
                    events.push(event);
                }
                // Outside the lock on the events list would be tidier, but the waker only
                // requests a repaint and the UI thread's drain takes the same lock, so there is
                // nothing it can deadlock against.
                (state.shared.wake)();
            }
        });
    }

    /// SAFETY: called by Windows on the tray thread with a window this module created.
    unsafe extern "system" fn window_proc(
        hwnd: sys::HANDLE,
        message: u32,
        wparam: sys::WPARAM,
        lparam: sys::LPARAM,
    ) -> sys::LRESULT {
        let (show_message, forwarded) = STATE.with(|state| {
            let state = state.borrow();
            match state.as_ref() {
                Some(state) => (
                    Some(state.show_message),
                    state.messages.iter().position(|id| *id == message),
                ),
                None => (None, None),
            }
        });

        if Some(message) == show_message {
            push(TrayEvent::Activated);
            return 0;
        }
        if let Some(index) = forwarded {
            push(TrayEvent::Message { index, wparam });
            // 1 rather than 0: a sender using `SendMessageTimeoutW` reads this as "the agent is
            // there and took the message", which is how the window tells a live agent from a
            // window handle that belonged to one.
            return 1;
        }

        match message {
            WM_TRAY => {
                // The low word only. With the record's `uVersion` left at 0 - which is what this
                // registers - `lParam` is the bare mouse message, but under version 4 the icon id
                // is packed into the high word, and comparing the whole word would then match
                // nothing. Masking is correct in both cases.
                match (lparam as u32) & 0xFFFF {
                    sys::WM_LBUTTONUP | sys::WM_LBUTTONDBLCLK => push(TrayEvent::Activated),
                    sys::WM_RBUTTONUP => show_menu(hwnd),
                    _ => {}
                }
                0
            }
            sys::WM_CLOSE => {
                // Not destroyed here: the owner quits through its normal path, which ends with
                // `Tray::stop` taking the icon down and destroying this window itself.
                push(TrayEvent::CloseRequested);
                0
            }
            sys::WM_DESTROY => {
                // SAFETY: ends this thread's own message loop.
                unsafe { sys::PostQuitMessage(0) };
                0
            }
            // SAFETY: the default handler for everything this module does not claim.
            _ => unsafe { sys::DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    fn show_menu(hwnd: sys::HANDLE) {
        let menu = STATE.with(|state| state.borrow().as_ref().and_then(|state| state.menu));
        let Some(menu) = menu else { return };

        // SAFETY: `menu` is a popup menu this module created and has not destroyed, and `point`
        // is a live local.
        unsafe {
            let mut point = sys::POINT { x: 0, y: 0 };
            sys::GetCursorPos(&mut point);
            // Required before `TrackPopupMenu` on a window that is not foreground, or the menu
            // appears and then closes on the next click anywhere.
            sys::SetForegroundWindow(hwnd);
            let chosen = sys::TrackPopupMenu(
                menu,
                sys::TPM_RIGHTBUTTON | sys::TPM_RETURNCMD | sys::TPM_NONOTIFY,
                point.x,
                point.y,
                0,
                hwnd,
                std::ptr::null(),
            );
            // Zero means dismissed, which is why no item may use id 0.
            if chosen > 0 {
                push(TrayEvent::Command(chosen as u32));
            }
        }
    }

    fn rebuild_menu() {
        let items = STATE.with(|state| {
            state
                .borrow()
                .as_ref()
                .and_then(|state| state.shared.pending_menu.lock().ok().and_then(|mut s| s.take()))
        });
        let Some(items) = items else { return };

        // SAFETY: builds a new popup menu and destroys the previous one only after the new one is
        // in place, so a click that arrives in between finds a valid menu.
        unsafe {
            let built = build_menu(&items);
            STATE.with(|state| {
                if let Some(state) = state.borrow_mut().as_mut() {
                    let previous = state.menu.replace(built);
                    if let Some(previous) = previous {
                        sys::DestroyMenu(previous);
                    }
                }
            });
        }
    }

    /// SAFETY: creates a popup menu. The returned handle is owned by the caller.
    unsafe fn build_menu(items: &[TrayItem]) -> sys::HANDLE {
        unsafe {
            let menu = sys::CreatePopupMenu();
            let mut radio_low = u32::MAX;
            let mut radio_high = 0u32;
            let mut radio_checked = None;

            for item in items {
                match item {
                    TrayItem::Separator => {
                        sys::AppendMenuW(menu, sys::MF_SEPARATOR, 0, std::ptr::null());
                    }
                    TrayItem::Command { id, label, enabled } => {
                        let text = sys::to_wide(label);
                        let flags =
                            sys::MF_STRING | if *enabled { 0 } else { sys::MF_GRAYED };
                        sys::AppendMenuW(menu, flags, *id as usize, text.as_ptr());
                    }
                    TrayItem::Check { id, label, checked } => {
                        let text = sys::to_wide(label);
                        let flags =
                            sys::MF_STRING | if *checked { sys::MF_CHECKED } else { 0 };
                        sys::AppendMenuW(menu, flags, *id as usize, text.as_ptr());
                    }
                    TrayItem::Radio { id, label, checked } => {
                        let text = sys::to_wide(label);
                        sys::AppendMenuW(menu, sys::MF_STRING, *id as usize, text.as_ptr());
                        radio_low = radio_low.min(*id);
                        radio_high = radio_high.max(*id);
                        if *checked {
                            radio_checked = Some(*id);
                        }
                    }
                    TrayItem::Submenu { label, items } => {
                        let text = sys::to_wide(label);
                        let child = build_menu(items);
                        sys::AppendMenuW(
                            menu,
                            sys::MF_STRING | sys::MF_POPUP,
                            child as usize,
                            text.as_ptr(),
                        );
                    }
                }
            }

            // The dot, rather than a tick, for the profile list. Windows marks it by id range,
            // which is why a menu's radio ids have to be contiguous.
            if let Some(checked) = radio_checked {
                sys::CheckMenuRadioItem(menu, radio_low, radio_high, checked, sys::MF_BYCOMMAND);
            }
            menu
        }
    }

    /// Builds an `HICON` from RGBA pixels.
    ///
    /// A 32-bit DIB with its own alpha plus an all-zero mask, which is the combination that gives
    /// a smoothly anti-aliased tray icon. `CreateIcon` with a monochrome mask - the shorter route
    /// - cannot express partial transparency, and the glyph would have hard edges.
    ///
    /// SAFETY: every handle created here is either consumed by `CreateIconIndirect` or deleted
    /// before returning.
    unsafe fn create_icon(icon: &TrayImage) -> sys::HANDLE {
        unsafe {
            let header = sys::BITMAPINFOHEADER {
                biSize: std::mem::size_of::<sys::BITMAPINFOHEADER>() as u32,
                biWidth: icon.width as i32,
                // Negative: top-down, so the rows go in the order the caller has them.
                biHeight: -(icon.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: sys::BI_RGB,
                ..Default::default()
            };

            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let colour = sys::CreateDIBSection(
                std::ptr::null_mut(),
                &header,
                sys::DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if colour.is_null() || bits.is_null() {
                return std::ptr::null_mut();
            }

            // RGBA in, BGRA out: GDI's 32-bit DIB is little-endian `0xAARRGGBB`.
            let pixels = (icon.width as usize) * (icon.height as usize);
            let destination = bits.cast::<u8>();
            for index in 0..pixels {
                let source = &icon.rgba[index * 4..index * 4 + 4];
                destination.add(index * 4).write(source[2]);
                destination.add(index * 4 + 1).write(source[1]);
                destination.add(index * 4 + 2).write(source[0]);
                destination.add(index * 4 + 3).write(source[3]);
            }

            // All bits zero: show the colour bitmap everywhere and let its alpha do the masking.
            let mask = sys::CreateBitmap(
                icon.width as i32,
                icon.height as i32,
                1,
                1,
                std::ptr::null(),
            );
            let info = sys::ICONINFO {
                fIcon: 1,
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: colour,
            };
            let handle = sys::CreateIconIndirect(&info);
            // `CreateIconIndirect` copies both bitmaps, so these are ours to delete.
            sys::DeleteObject(colour);
            sys::DeleteObject(mask);
            handle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Vec<TrayItem> {
        vec![
            TrayItem::command(1, "Open"),
            TrayItem::Separator,
            TrayItem::Submenu {
                label: "Peripheral profile".to_owned(),
                items: vec![
                    TrayItem::Radio { id: 100, label: "Default".to_owned(), checked: true },
                    TrayItem::Radio { id: 101, label: "Gaming".to_owned(), checked: false },
                ],
            },
            TrayItem::Check { id: 2, label: "Auto-switch".to_owned(), checked: true },
            TrayItem::command(3, "Quit"),
        ]
    }

    #[test]
    fn a_menu_reports_every_id_it_can_produce() {
        let mut ids = menu().iter().flat_map(TrayItem::ids).collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2, 3, 100, 101]);
    }

    /// Two items with one id is a menu where choosing either does the same thing, and it is
    /// invisible by looking at the menu.
    #[test]
    fn a_duplicate_id_is_refused() {
        let items = vec![TrayItem::command(7, "A"), TrayItem::command(7, "B")];
        assert!(validate(&items).unwrap_err().contains("share an id"));
    }

    /// `TrackPopupMenu` returns 0 for a dismissed menu, so an item with id 0 would fire whenever
    /// the user pressed Escape.
    #[test]
    fn id_zero_is_refused_because_windows_uses_it_for_dismissed() {
        let items = vec![TrayItem::command(0, "Nothing")];
        assert!(validate(&items).unwrap_err().contains("id 0"));
    }

    #[test]
    fn a_valid_menu_passes() {
        validate(&menu()).expect("the real menu is valid");
    }

    /// The icon is handed over as raw pixels, and a wrong length would be read past the end of the
    /// buffer by the DIB copy.
    #[test]
    fn an_icon_must_be_exactly_four_bytes_a_pixel() {
        assert!(TrayImage { width: 2, height: 2, rgba: vec![0; 16] }.is_valid());
        assert!(!TrayImage { width: 2, height: 2, rgba: vec![0; 15] }.is_valid());
        assert!(!TrayImage { width: 0, height: 2, rgba: Vec::new() }.is_valid());
    }
}

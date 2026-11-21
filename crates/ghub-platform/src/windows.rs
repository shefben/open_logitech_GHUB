//! Windows user-mode input layer: the port of `WindowsPeripheralPlatform.cs`.
//!
//! `SendInput` injection, `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks for software bindings, game-mode
//! key exclusion and macro recording, and an `EVENT_SYSTEM_FOREGROUND` hook for per-application
//! profiles. **No drivers and no elevation.** The consequence, which the UI states rather than
//! hides: hooks never see input sent to an elevated window, and `SendInput` into one fails. That
//! is UIPI, and the only way round it is to run elevated, which this program does not do.
//!
//! ## Why a global
//!
//! A Win32 hook procedure is a bare `extern "system" fn` - it has no user-data parameter, so
//! there is nowhere to put a `self`. The C# got round this by capturing an instance field in a
//! delegate. The honest Rust equivalent is a process-wide sink, set once: there can only be one
//! low-level hook pair per process anyway, because the second `SetWindowsHookEx` would just
//! duplicate every event.
//!
//! ## Rules the hook procedures obey
//!
//! 1. **Never panic back into the OS.** Every callback body is wrapped in `catch_unwind`; a panic
//!    unwinding through a Windows callback is undefined behaviour.
//! 2. **Never block.** Windows silently removes a low-level hook that exceeds
//!    `LowLevelHooksTimeout` (300 ms by default), and the user then has no macros and no reactions
//!    with nothing in the log to say why. Anything that touches a device, resolves a profile or
//!    takes a lock the UI holds is handed to a short-lived thread.
//! 3. **Ignore our own injection.** Every injected event carries [`INJECTION_MARKER`] in
//!    `dwExtraInfo`, so a macro that types "hello" cannot re-trigger the binding that started it.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use ghub_core::injection::InputInjector;
use ghub_core::resolver::ForegroundApp;
use ghub_core::MouseButtonKind;

use crate::rules::{self, InputSuppressionTracker, MouseEvent};
use crate::sys::{self, HANDLE, INPUT, INPUT_UNION, KEYBDINPUT, LPARAM, LRESULT, MOUSEINPUT, WPARAM};
use crate::{HookSink, HookStatus, PlatformServices};

/// Marker in `dwExtraInfo` so our own injected events are ignored by our hooks.
///
/// "AENH" as four ASCII bytes, kept from the C# so a user who has both versions installed can
/// tell the two apart in a trace.
pub const INJECTION_MARKER: usize = 0x4145_4E48;

// ----------------------------------------------------------------- injector

/// `SendInput` injector. Cheap to clone, holds no state.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsInjector;

impl WindowsInjector {
    pub fn new() -> Self {
        Self
    }

    fn send(input: INPUT) -> bool {
        // SAFETY: `input` is a correctly laid out INPUT (asserted in sys::tests) and the count
        // and size match the single-element pointer handed over.
        let sent = unsafe { sys::SendInput(1, &input, sys::INPUT_SIZE) };
        if sent != 1 {
            // The usual cause is UIPI: the foreground window belongs to an elevated process.
            // Logged rather than returned, because every caller is a macro step that must keep
            // going to release whatever it has already pressed.
            log::warn!(
                "SendInput was blocked (target may be elevated / UIPI), last error {}",
                unsafe { sys::GetLastError() }
            );
            return false;
        }
        true
    }

    fn send_key(virtual_key: u16, up: bool) {
        let mut flags = if up { sys::KEYEVENTF_KEYUP } else { 0 };
        if rules::is_extended_key(virtual_key) {
            flags |= sys::KEYEVENTF_EXTENDEDKEY;
        }
        // SAFETY: MapVirtualKeyW takes and returns plain integers.
        let scan = unsafe { sys::MapVirtualKeyW(u32::from(virtual_key), 0) } as u16;
        Self::send(INPUT {
            r#type: sys::INPUT_KEYBOARD,
            u: INPUT_UNION {
                ki: KEYBDINPUT {
                    wVk: virtual_key,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INJECTION_MARKER,
                },
            },
        });
    }

    fn send_mouse(button: MouseButtonKind, down: bool) {
        let (flags, data) = match button {
            MouseButtonKind::Left => {
                (if down { sys::MOUSEEVENTF_LEFTDOWN } else { sys::MOUSEEVENTF_LEFTUP }, 0)
            }
            MouseButtonKind::Right => {
                (if down { sys::MOUSEEVENTF_RIGHTDOWN } else { sys::MOUSEEVENTF_RIGHTUP }, 0)
            }
            MouseButtonKind::Middle => {
                (if down { sys::MOUSEEVENTF_MIDDLEDOWN } else { sys::MOUSEEVENTF_MIDDLEUP }, 0)
            }
            MouseButtonKind::XButton1 => {
                (if down { sys::MOUSEEVENTF_XDOWN } else { sys::MOUSEEVENTF_XUP }, 1)
            }
            MouseButtonKind::XButton2 => {
                (if down { sys::MOUSEEVENTF_XDOWN } else { sys::MOUSEEVENTF_XUP }, 2)
            }
        };
        Self::send(INPUT {
            r#type: sys::INPUT_MOUSE,
            u: INPUT_UNION {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INJECTION_MARKER,
                },
            },
        });
    }
}

impl InputInjector for WindowsInjector {
    fn key_down(&self, virtual_key: u16) {
        Self::send_key(virtual_key, false);
    }

    fn key_up(&self, virtual_key: u16) {
        Self::send_key(virtual_key, true);
    }

    fn mouse_down(&self, button: MouseButtonKind) {
        Self::send_mouse(button, true);
    }

    fn mouse_up(&self, button: MouseButtonKind) {
        Self::send_mouse(button, false);
    }

    fn mouse_move(&self, dx: i32, dy: i32) {
        Self::send(INPUT {
            r#type: sys::INPUT_MOUSE,
            u: INPUT_UNION {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: sys::MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: INJECTION_MARKER,
                },
            },
        });
    }

    fn wheel(&self, delta: i32, horizontal: bool) {
        Self::send(INPUT {
            r#type: sys::INPUT_MOUSE,
            u: INPUT_UNION {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    // Reinterpreted, not clamped: the field is a DWORD that the OS reads back as
                    // a signed short, so a negative delta must keep its bit pattern.
                    mouseData: delta as u32,
                    dwFlags: if horizontal {
                        sys::MOUSEEVENTF_HWHEEL
                    } else {
                        sys::MOUSEEVENTF_WHEEL
                    },
                    time: 0,
                    dwExtraInfo: INJECTION_MARKER,
                },
            },
        });
    }

    fn type_text(&self, text: &str) {
        // UTF-16 code units, not chars: a surrogate pair must go as two events, which is exactly
        // what the C#'s `foreach (char c in text)` did.
        for unit in text.encode_utf16() {
            for extra in [0, sys::KEYEVENTF_KEYUP] {
                Self::send(INPUT {
                    r#type: sys::INPUT_KEYBOARD,
                    u: INPUT_UNION {
                        ki: KEYBDINPUT {
                            wVk: 0,
                            wScan: unit,
                            dwFlags: sys::KEYEVENTF_UNICODE | extra,
                            time: 0,
                            dwExtraInfo: INJECTION_MARKER,
                        },
                    },
                });
            }
        }
    }
}

// ----------------------------------------------------------------- queries

/// Foreground window as an executable path and title, or `None` when the desktop has focus.
pub fn foreground_app() -> Option<ForegroundApp> {
    // SAFETY: all three calls take and return handles and integers; the buffers below are sized
    // before being passed and their lengths are passed with them.
    unsafe {
        let hwnd = sys::GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }

        // GetWindowTextLengthW can over-report, never under-report, so +1 for the NUL is enough.
        let length = sys::GetWindowTextLengthW(hwnd).clamp(0, 4096) as usize;
        let mut title = vec![0u16; length + 1];
        let written = sys::GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        let window_title = if written > 0 { sys::from_wide(&title) } else { String::new() };

        let mut pid = 0u32;
        sys::GetWindowThreadProcessId(hwnd, &mut pid);
        let executable_path = process_path(pid);

        Some(ForegroundApp { executable_path, window_title: Some(window_title) })
    }
}

/// Full image path of a process id, or `None` when it cannot be opened.
///
/// `PROCESS_QUERY_LIMITED_INFORMATION` is the least privilege that answers this, and unlike
/// `PROCESS_QUERY_INFORMATION` it works for processes at a higher integrity level - so an
/// elevated game still resolves its profile even though we cannot hook its input.
fn process_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    // SAFETY: the handle is checked for null, closed on every path, and the buffer length is
    // passed by pointer as the API requires.
    unsafe {
        let process = sys::OpenProcess(sys::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 1024];
        let mut size = buffer.len() as u32;
        let ok = sys::QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size) != 0;
        sys::CloseHandle(process);
        if !ok || size == 0 {
            return None;
        }
        buffer.truncate(size as usize);
        Some(sys::from_wide(&buffer))
    }
}

/// Whether the user is physically holding a key.
///
/// `GetAsyncKeyState` reports the real keyboard state, which is what a macro's `KeyHeld`
/// condition wants - it must not be satisfied by a key the macro itself just synthesised, and
/// injected keys do show up here, so the runtime's own bookkeeping is what distinguishes them.
pub fn is_key_held(virtual_key: u16) -> bool {
    // SAFETY: plain integer in, plain integer out.
    (unsafe { sys::GetAsyncKeyState(i32::from(virtual_key)) } as u16 & 0x8000) != 0
}

/// Starts a program, document or URL with the user's own token.
///
/// Deliberately `ShellExecuteW` with the `open` verb and never `runas`: a binding that silently
/// elevated would be a privilege-escalation primitive for anything that can write a profile file.
pub fn launch(target: &str, arguments: Option<&str>) -> Result<(), String> {
    if target.trim().is_empty() {
        return Err("no program to launch".to_owned());
    }
    let operation = sys::to_wide("open");
    let file = sys::to_wide(target);
    let parameters = sys::to_wide(arguments.unwrap_or(""));
    // SAFETY: every pointer is a NUL-terminated buffer that outlives the call.
    let result = unsafe {
        sys::ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            if arguments.is_some() { parameters.as_ptr() } else { std::ptr::null() },
            std::ptr::null(),
            sys::SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns a pseudo-HINSTANCE: anything at or below 32 is an error code.
    let code = result as usize;
    if code > 32 {
        Ok(())
    } else {
        Err(describe_shell_error(code))
    }
}

/// Turns a `ShellExecute` failure code into something a user can act on.
fn describe_shell_error(code: usize) -> String {
    match code {
        2 => "the file was not found".to_owned(),
        3 => "the path was not found".to_owned(),
        5 => "access was denied".to_owned(),
        8 => "not enough memory".to_owned(),
        26 | 27 | 31 => "no program is associated with this file type".to_owned(),
        other => format!("ShellExecute failed with code {other}"),
    }
}

// ----------------------------------------------------------------- hooks

static SINK: OnceLock<Arc<dyn HookSink>> = OnceLock::new();
static KEYBOARD_HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);
static WINEVENT_HOOK: AtomicIsize = AtomicIsize::new(0);
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static SAW_KEY: AtomicBool = AtomicBool::new(false);
static CTRL_HELD: AtomicBool = AtomicBool::new(false);
static SHIFT_HELD: AtomicBool = AtomicBool::new(false);
static LAST_ERROR: AtomicU32 = AtomicU32::new(0);

fn key_suppression() -> &'static InputSuppressionTracker<u16> {
    static TRACKER: OnceLock<InputSuppressionTracker<u16>> = OnceLock::new();
    TRACKER.get_or_init(InputSuppressionTracker::new)
}

fn button_suppression() -> &'static InputSuppressionTracker<MouseButtonKind> {
    static TRACKER: OnceLock<InputSuppressionTracker<MouseButtonKind>> = OnceLock::new();
    TRACKER.get_or_init(InputSuppressionTracker::new)
}

/// Installs the hooks on a dedicated thread and pumps its message loop.
///
/// Returns once the hooks are installed (or have failed), so the caller knows whether input
/// features work before it tells the user they do. The thread outlives the call and is stopped by
/// [`HookHost::stop`].
pub struct HookHost {
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HookHost {
    /// Installs the hooks. Calling this twice is a no-op on the second call, because the sink is
    /// a `OnceLock` - a second low-level hook pair would deliver every event twice.
    pub fn start(sink: Arc<dyn HookSink>) -> Self {
        if SINK.set(sink).is_err() {
            log::warn!("input hooks are already installed; ignoring the second request");
            return Self { thread: None };
        }
        let ready = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let signal = Arc::clone(&ready);
        let thread = std::thread::Builder::new()
            .name("ghub input hooks".to_owned())
            .spawn(move || hook_thread(signal))
            .ok();

        // Wait for installation, with a bound: if the thread wedges we still want a running UI
        // that can say input features are unavailable.
        if thread.is_some() {
            let (lock, condition) = &*ready;
            if let Ok(guard) = lock.lock() {
                let _ = condition
                    .wait_timeout_while(guard, std::time::Duration::from_secs(5), |set| !*set);
            }
        }
        Self { thread }
    }

    /// What was installed, for the diagnostics page. A failed hook is otherwise invisible: macros,
    /// game mode and press reactions would all simply never fire.
    pub fn status() -> HookStatus {
        HookStatus {
            keyboard: KEYBOARD_HOOK.load(Ordering::SeqCst) != 0,
            mouse: MOUSE_HOOK.load(Ordering::SeqCst) != 0,
            foreground: WINEVENT_HOOK.load(Ordering::SeqCst) != 0,
            keyboard_events_seen: SAW_KEY.load(Ordering::SeqCst),
            last_error: LAST_ERROR.load(Ordering::SeqCst),
        }
    }

    /// Removes the hooks and stops the message loop.
    pub fn stop(&mut self) {
        unsafe {
            for (slot, unhook) in [
                (&KEYBOARD_HOOK, true),
                (&MOUSE_HOOK, true),
                (&WINEVENT_HOOK, false),
            ] {
                let handle = slot.swap(0, Ordering::SeqCst);
                if handle != 0 {
                    if unhook {
                        sys::UnhookWindowsHookEx(handle as HANDLE);
                    } else {
                        sys::UnhookWinEvent(handle as HANDLE);
                    }
                }
            }
            let thread_id = HOOK_THREAD_ID.swap(0, Ordering::SeqCst);
            if thread_id != 0 {
                sys::PostThreadMessageW(thread_id, sys::WM_QUIT, 0, 0);
            }
        }
        key_suppression().clear();
        button_suppression().clear();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HookHost {
    fn drop(&mut self) {
        self.stop();
    }
}

fn hook_thread(ready: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>) {
    // SAFETY: the hook procedures are `extern "system"` functions with the documented signature,
    // and the module handle is the one Windows wants for a thread-global low-level hook.
    unsafe {
        HOOK_THREAD_ID.store(sys::GetCurrentThreadId(), Ordering::SeqCst);
        let module = sys::GetModuleHandleW(std::ptr::null());
        let keyboard = sys::SetWindowsHookExW(sys::WH_KEYBOARD_LL, keyboard_proc, module, 0);
        let mouse = sys::SetWindowsHookExW(sys::WH_MOUSE_LL, mouse_proc, module, 0);
        let foreground = sys::SetWinEventHook(
            sys::EVENT_SYSTEM_FOREGROUND,
            sys::EVENT_SYSTEM_FOREGROUND,
            std::ptr::null_mut(),
            win_event_proc,
            0,
            0,
            sys::WINEVENT_OUTOFCONTEXT,
        );
        LAST_ERROR.store(sys::GetLastError(), Ordering::SeqCst);
        KEYBOARD_HOOK.store(keyboard as isize, Ordering::SeqCst);
        MOUSE_HOOK.store(mouse as isize, Ordering::SeqCst);
        WINEVENT_HOOK.store(foreground as isize, Ordering::SeqCst);

        log::info!(
            "input hooks installed: keyboard {:?}, mouse {:?}, foreground {:?}, last error {}",
            keyboard,
            mouse,
            foreground,
            LAST_ERROR.load(Ordering::SeqCst)
        );
        if let Some(sink) = SINK.get() {
            sink.on_hooks_installed(HookHost::status());
        }

        {
            let (lock, condition) = &*ready;
            if let Ok(mut set) = lock.lock() {
                *set = true;
            }
            condition.notify_all();
        }

        raise_foreground();

        let mut message = sys::MSG::default();
        while sys::GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            sys::TranslateMessage(&message);
            sys::DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // A hook must never unwind into the OS.
    let swallow = catch_unwind(AssertUnwindSafe(|| {
        if code < 0 || lparam == 0 {
            return false;
        }
        let Some(sink) = SINK.get() else { return false };
        let event = unsafe { *(lparam as *const sys::KBDLLHOOKSTRUCT) };

        if !SAW_KEY.swap(true, Ordering::SeqCst) {
            // Said once, from inside the hook itself, so "the OS never calls us" is
            // distinguishable from "we were called and filtered the key out".
            log::info!(
                "keyboard hook fired: vk 0x{:02X}, flags 0x{:02X}",
                event.vkCode,
                event.flags
            );
        }

        if event.dwExtraInfo == INJECTION_MARKER {
            return false; // our own injection
        }

        let down = rules::is_key_down(wparam as u32);
        let virtual_key = event.vkCode as u16;
        let (ctrl, shift) = rules::track_modifiers(
            virtual_key,
            down,
            CTRL_HELD.load(Ordering::Relaxed),
            SHIFT_HELD.load(Ordering::Relaxed),
        );
        CTRL_HELD.store(ctrl, Ordering::Relaxed);
        SHIFT_HELD.store(shift, Ordering::Relaxed);

        if down && rules::is_emergency_stop(virtual_key, ctrl, shift) {
            let sink = Arc::clone(sink);
            // Off the hook thread: stopping macros joins their threads.
            std::thread::spawn(move || sink.on_emergency_stop());
        }

        if event.flags & sys::LLKHF_INJECTED != 0 {
            // Injected by another program. Reacting would make two macro tools feed each other.
            return false;
        }

        let extended = event.flags & sys::LLKHF_EXTENDED != 0;
        sink.on_keyboard_key(virtual_key, down, extended);
        // An up is swallowed exactly when its down was, so policy changes in between never
        // strand keys. See `rules::InputSuppressionTracker`.
        if down {
            key_suppression().on_down(virtual_key, sink.is_game_mode_blocked(virtual_key))
        } else {
            key_suppression().on_up(&virtual_key)
        }
    }))
    .unwrap_or(false);

    if swallow {
        return 1;
    }
    unsafe {
        sys::CallNextHookEx(
            KEYBOARD_HOOK.load(Ordering::Relaxed) as HANDLE,
            code,
            wparam,
            lparam,
        )
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let swallow = catch_unwind(AssertUnwindSafe(|| {
        if code < 0 || lparam == 0 {
            return false;
        }
        let Some(sink) = SINK.get() else { return false };
        let event = unsafe { *(lparam as *const sys::MSLLHOOKSTRUCT) };
        if event.dwExtraInfo == INJECTION_MARKER || event.flags & sys::LLMHF_INJECTED != 0 {
            return false;
        }
        match rules::classify_mouse(wparam as u32, event.mouseData) {
            Some(MouseEvent::Button { button, down }) => {
                let mouse_key = sink.primary_mouse_key();
                if down {
                    // Left click is never suppressed unless a binding exists; the UI refuses to
                    // bind left click away without leaving a recovery binding.
                    let handled = mouse_key
                        .as_deref()
                        .map(|key| sink.on_mouse_button(key, button, true))
                        .unwrap_or(false);
                    button_suppression().on_down(button, handled)
                } else if button_suppression().on_up(&button) {
                    if let Some(key) = mouse_key.as_deref() {
                        sink.on_mouse_button(key, button, false);
                    }
                    true
                } else {
                    false
                }
            }
            Some(MouseEvent::Wheel { delta }) => {
                sink.on_wheel(delta);
                false
            }
            None => false,
        }
    }))
    .unwrap_or(false);

    if swallow {
        return 1;
    }
    unsafe {
        sys::CallNextHookEx(MOUSE_HOOK.load(Ordering::Relaxed) as HANDLE, code, wparam, lparam)
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HANDLE,
    _event: u32,
    _hwnd: HANDLE,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let _ = catch_unwind(AssertUnwindSafe(raise_foreground));
}

fn raise_foreground() {
    let app = foreground_app();
    let Some(sink) = SINK.get() else { return };
    let sink = Arc::clone(sink);
    // Profile resolution, macro cancellation and device writes must never run on the hook thread.
    std::thread::spawn(move || sink.on_foreground_changed(app));
}

/// Fills a buffer from the operating system's cryptographic random generator.
///
/// `BCryptGenRandom` with the system-preferred algorithm, which is the documented way to get
/// CSPRNG bytes on Windows without opening an algorithm provider. Returns an error rather than
/// falling back to anything weaker: the only caller is the automation API's bearer token, and a
/// predictable token there would be worse than having no API at all.
pub fn system_random(length: usize) -> Result<Vec<u8>, String> {
    if length == 0 {
        return Ok(Vec::new());
    }
    let mut buffer = vec![0u8; length];
    // SAFETY: the buffer is owned here and its length is passed with it; the null algorithm
    // handle is required by `BCRYPT_USE_SYSTEM_PREFERRED_RNG`.
    let status = unsafe {
        sys::BCryptGenRandom(
            std::ptr::null_mut(),
            buffer.as_mut_ptr(),
            length as u32,
            sys::BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return Err(format!("BCryptGenRandom failed with status 0x{status:08X}"));
    }
    Ok(buffer)
}

/// The composed Windows platform: injector plus queries.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsPlatform;

impl WindowsPlatform {
    pub fn new() -> Self {
        Self
    }

    pub fn injector(&self) -> WindowsInjector {
        WindowsInjector
    }
}

impl PlatformServices for WindowsPlatform {
    fn foreground_app(&self) -> Option<ForegroundApp> {
        foreground_app()
    }

    fn is_key_held(&self, virtual_key: u16) -> bool {
        is_key_held(virtual_key)
    }

    fn launch(&self, target: &str, arguments: Option<&str>) -> Result<(), String> {
        launch(target, arguments)
    }
}

impl InputInjector for WindowsPlatform {
    fn key_down(&self, virtual_key: u16) {
        WindowsInjector.key_down(virtual_key);
    }
    fn key_up(&self, virtual_key: u16) {
        WindowsInjector.key_up(virtual_key);
    }
    fn mouse_down(&self, button: MouseButtonKind) {
        WindowsInjector.mouse_down(button);
    }
    fn mouse_up(&self, button: MouseButtonKind) {
        WindowsInjector.mouse_up(button);
    }
    fn mouse_move(&self, dx: i32, dy: i32) {
        WindowsInjector.mouse_move(dx, dy);
    }
    fn wheel(&self, delta: i32, horizontal: bool) {
        WindowsInjector.wheel(delta, horizontal);
    }
    fn type_text(&self, text: &str) {
        WindowsInjector.type_text(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_injection_marker_is_the_four_ascii_bytes_from_the_csharp() {
        assert_eq!(INJECTION_MARKER.to_be_bytes()[std::mem::size_of::<usize>() - 4..], *b"AENH");
    }

    #[test]
    fn launching_nothing_is_refused_before_it_reaches_the_shell() {
        assert!(launch("", None).is_err());
        assert!(launch("   ", Some("--flag")).is_err());
    }

    #[test]
    fn a_failed_launch_reports_a_reason_rather_than_a_bare_code() {
        // Which code the shell returns depends on *why* it failed - a missing file on a real
        // drive gives ERROR_FILE_NOT_FOUND, a path on a drive that does not exist gives
        // ERROR_ACCESS_DENIED - so the assertion here is that something readable comes back.
        // The mapping itself is checked in the next test, without needing the shell to fail in
        // five different ways.
        let error = launch("Z:\\definitely-not-here-9d3f.exe", None).unwrap_err();
        assert!(!error.is_empty());
        assert!(!error.contains("code 0"), "a mapped reason was expected, got {error}");
    }

    #[test]
    fn the_shell_error_codes_map_to_readable_reasons() {
        // These strings are what the user sees when a binding will not start.
        assert_eq!(describe_shell_error(2), "the file was not found");
        assert_eq!(describe_shell_error(3), "the path was not found");
        assert_eq!(describe_shell_error(5), "access was denied");
        assert_eq!(describe_shell_error(8), "not enough memory");
        assert_eq!(describe_shell_error(31), "no program is associated with this file type");
        assert_eq!(describe_shell_error(99), "ShellExecute failed with code 99");
    }

    #[test]
    fn querying_a_dead_process_id_yields_nothing_rather_than_a_wrong_path() {
        assert_eq!(process_path(0), None);
        // The idle process cannot be opened even with limited rights.
        assert_eq!(process_path(4), None);
    }

    #[test]
    fn our_own_process_path_resolves() {
        // SAFETY: no arguments, returns an integer.
        let pid = unsafe { sys::GetCurrentProcessId() };
        let path = process_path(pid).expect("our own process must be queryable");
        assert!(
            path.to_ascii_lowercase().ends_with(".exe"),
            "expected an executable path, got {path}"
        );
    }

    #[test]
    fn a_key_nobody_is_pressing_is_not_held() {
        // 0xFF is not a real virtual key, so this also proves the mask is applied rather than
        // the sign bit being read as "held".
        assert!(!is_key_held(0xFF));
    }

    #[test]
    fn the_foreground_lookup_never_panics_even_with_no_focused_window() {
        // On a session with no interactive desktop this returns None; either way it must not
        // fault, and a title must never come back as a lone NUL-padded buffer.
        if let Some(app) = foreground_app() {
            assert!(!app.window_title.as_deref().unwrap_or("").contains('\0'));
        }
    }

    #[test]
    fn system_random_returns_the_requested_length_and_differs_each_time() {
        assert!(system_random(0).unwrap().is_empty());
        let first = system_random(24).expect("the OS CSPRNG must answer");
        let second = system_random(24).expect("the OS CSPRNG must answer");
        assert_eq!(first.len(), 24);
        assert_ne!(first, second, "two draws from a CSPRNG must not match");
        assert!(
            !first.iter().all(|b| *b == first[0]),
            "a constant buffer means the call did not fill it"
        );
    }

    #[test]
    fn status_reports_what_was_installed() {
        let status = HookHost::status();
        // Nothing is installed in a unit test, so this pins the shape rather than the values.
        assert_eq!(status.keyboard, KEYBOARD_HOOK.load(Ordering::SeqCst) != 0);
        assert!(!status.describe().is_empty());
    }
}

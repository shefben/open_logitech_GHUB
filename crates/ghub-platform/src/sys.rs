//! Raw Win32 bindings, declared here rather than taken from the `windows` crate.
//!
//! The C# used `DllImport` for exactly these twenty-odd entry points. Declaring them directly is
//! the literal translation, costs nothing in the binary, and keeps the whole Windows surface of
//! this program in one readable file - which matters more than usual, because a wrong struct
//! layout here does not fail to compile, it corrupts input events at runtime.
//!
//! Layout notes that are load-bearing on x86-64:
//!
//! * `dwExtraInfo` is `ULONG_PTR`, so `usize` - not `u32`. Getting it wrong shifts every field
//!   after it and silently breaks the injection marker.
//! * `INPUT` is 40 bytes: a 4-byte tag, 4 bytes of padding, then a 32-byte union. `SendInput`
//!   validates the size it is given and fails the whole call if it disagrees, which is why
//!   [`INPUT_SIZE`] is asserted against `size_of` in the tests.
//! * `MSLLHOOKSTRUCT.mouseData` carries the wheel delta and the X-button index in its *high*
//!   word, and the wheel delta is signed. See `rules::classify_mouse`.

#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::c_void;

pub type HANDLE = *mut c_void;
pub type WPARAM = usize;
pub type LPARAM = isize;
pub type LRESULT = isize;
pub type BOOL = i32;

pub const WH_KEYBOARD_LL: i32 = 13;
pub const WH_MOUSE_LL: i32 = 14;

pub const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
pub const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;

pub const WM_QUIT: u32 = 0x0012;

/// `LLKHF_EXTENDED`: the key is one of the extended-key set (see `rules::is_extended_key`).
pub const LLKHF_EXTENDED: u32 = 0x01;
/// `LLKHF_INJECTED`: the event came from `SendInput`, ours or another program's.
pub const LLKHF_INJECTED: u32 = 0x10;
/// `LLMHF_INJECTED`, the mouse equivalent. Note the different bit.
pub const LLMHF_INJECTED: u32 = 0x01;

pub const INPUT_MOUSE: u32 = 0;
pub const INPUT_KEYBOARD: u32 = 1;

pub const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
pub const KEYEVENTF_KEYUP: u32 = 0x0002;
pub const KEYEVENTF_UNICODE: u32 = 0x0004;

pub const MOUSEEVENTF_MOVE: u32 = 0x0001;
pub const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
pub const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
pub const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
pub const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
pub const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x0020;
pub const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
pub const MOUSEEVENTF_XDOWN: u32 = 0x0080;
pub const MOUSEEVENTF_XUP: u32 = 0x0100;
pub const MOUSEEVENTF_WHEEL: u32 = 0x0800;
pub const MOUSEEVENTF_HWHEEL: u32 = 0x1000;

pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

pub const SW_SHOWNORMAL: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct KBDLLHOOKSTRUCT {
    pub vkCode: u32,
    pub scanCode: u32,
    pub flags: u32,
    pub time: u32,
    pub dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MSLLHOOKSTRUCT {
    pub x: i32,
    pub y: i32,
    pub mouseData: u32,
    pub flags: u32,
    pub time: u32,
    pub dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MSG {
    pub hwnd: HANDLE,
    pub message: u32,
    pub wParam: WPARAM,
    pub lParam: LPARAM,
    pub time: u32,
    pub pt: POINT,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MOUSEINPUT {
    pub dx: i32,
    pub dy: i32,
    pub mouseData: u32,
    pub dwFlags: u32,
    pub time: u32,
    pub dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct KEYBDINPUT {
    pub wVk: u16,
    pub wScan: u16,
    pub dwFlags: u32,
    pub time: u32,
    pub dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union INPUT_UNION {
    pub mi: MOUSEINPUT,
    pub ki: KEYBDINPUT,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct INPUT {
    pub r#type: u32,
    pub u: INPUT_UNION,
}

/// What `SendInput` must be told. Asserted against `size_of::<INPUT>()` in the tests.
pub const INPUT_SIZE: i32 = 40;

pub type HOOKPROC = unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT;
pub type WINEVENTPROC =
    unsafe extern "system" fn(HANDLE, u32, HANDLE, i32, i32, u32, u32);

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    pub fn SetWindowsHookExW(
        idHook: i32,
        lpfn: HOOKPROC,
        hmod: HANDLE,
        dwThreadId: u32,
    ) -> HANDLE;
    pub fn UnhookWindowsHookEx(hhk: HANDLE) -> BOOL;
    pub fn CallNextHookEx(hhk: HANDLE, nCode: i32, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    pub fn SetWinEventHook(
        eventMin: u32,
        eventMax: u32,
        hmodWinEventProc: HANDLE,
        pfnWinEventProc: WINEVENTPROC,
        idProcess: u32,
        idThread: u32,
        dwFlags: u32,
    ) -> HANDLE;
    pub fn UnhookWinEvent(hWinEventHook: HANDLE) -> BOOL;
    pub fn GetMessageW(lpMsg: *mut MSG, hWnd: HANDLE, wMsgFilterMin: u32, wMsgFilterMax: u32) -> i32;
    pub fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
    pub fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
    pub fn PostThreadMessageW(idThread: u32, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> BOOL;
    pub fn GetForegroundWindow() -> HANDLE;
    pub fn GetWindowThreadProcessId(hWnd: HANDLE, lpdwProcessId: *mut u32) -> u32;
    pub fn GetWindowTextW(hWnd: HANDLE, lpString: *mut u16, nMaxCount: i32) -> i32;
    pub fn GetWindowTextLengthW(hWnd: HANDLE) -> i32;
    pub fn IsWindowVisible(hWnd: HANDLE) -> BOOL;
    pub fn EnumWindows(lpEnumFunc: EnumWindowsProc, lParam: LPARAM) -> BOOL;
    pub fn GetAsyncKeyState(vKey: i32) -> i16;
    pub fn SendInput(cInputs: u32, pInputs: *const INPUT, cbSize: i32) -> u32;
    pub fn MapVirtualKeyW(uCode: u32, uMapType: u32) -> u32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    pub fn GetCurrentThreadId() -> u32;
    pub fn GetCurrentProcessId() -> u32;
    pub fn GetModuleHandleW(lpModuleName: *const u16) -> HANDLE;
    pub fn GetLastError() -> u32;
    pub fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: BOOL, dwProcessId: u32) -> HANDLE;
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;
    pub fn QueryFullProcessImageNameW(
        hProcess: HANDLE,
        dwFlags: u32,
        lpExeName: *mut u16,
        lpdwSize: *mut u32,
    ) -> BOOL;
    pub fn CreateToolhelp32Snapshot(dwFlags: u32, th32ProcessID: u32) -> HANDLE;
    pub fn Process32FirstW(hSnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> BOOL;
    pub fn Process32NextW(hSnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> BOOL;
}

/// `EnumWindows`' callback. Returning 0 stops the enumeration.
pub type EnumWindowsProc = unsafe extern "system" fn(hWnd: HANDLE, lParam: LPARAM) -> BOOL;

pub const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

/// `INVALID_HANDLE_VALUE`. `CreateToolhelp32Snapshot` reports failure with this rather than null,
/// so a null check alone would walk a bogus snapshot.
pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

/// `PROCESSENTRY32W`. `dwSize` must be set before the first call or `Process32FirstW` fails.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PROCESSENTRY32W {
    pub dwSize: u32,
    pub cntUsage: u32,
    pub th32ProcessID: u32,
    pub th32DefaultHeapID: usize,
    pub th32ModuleID: u32,
    pub cntThreads: u32,
    pub th32ParentProcessID: u32,
    pub pcPriClassBase: i32,
    pub dwFlags: u32,
    pub szExeFile: [u16; 260],
}

impl Default for PROCESSENTRY32W {
    fn default() -> Self {
        // Zeroed apart from `dwSize`, which the API uses as a version tag.
        Self {
            dwSize: std::mem::size_of::<Self>() as u32,
            cntUsage: 0,
            th32ProcessID: 0,
            th32DefaultHeapID: 0,
            th32ModuleID: 0,
            cntThreads: 0,
            th32ParentProcessID: 0,
            pcPriClassBase: 0,
            dwFlags: 0,
            szExeFile: [0; 260],
        }
    }
}

#[cfg(windows)]
#[link(name = "bcrypt")]
extern "system" {
    /// `BCryptGenRandom` with `BCRYPT_USE_SYSTEM_PREFERRED_RNG`. Returns an `NTSTATUS`, so zero
    /// is success. This is the OS CSPRNG; nothing else is acceptable for a bearer token.
    pub fn BCryptGenRandom(
        hAlgorithm: HANDLE,
        pbBuffer: *mut u8,
        cbBuffer: u32,
        dwFlags: u32,
    ) -> i32;
}

pub const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

// ----------------------------------------------------------------- comdlg32: the file dialogs
//
// `GetOpenFileNameW` rather than `IFileDialog`: the COM dialog needs an apartment initialised on
// the calling thread and a vtable call per property, which is a lot of surface for "pick a .json".
// This one is a struct and a call.

/// `OPENFILENAMEW`. Laid out by hand, and its size is asserted below, because the API validates
/// `lStructSize` against its own idea of the layout and rejects the call if they disagree.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OPENFILENAMEW {
    pub lStructSize: u32,
    pub hwndOwner: HANDLE,
    pub hInstance: HANDLE,
    pub lpstrFilter: *const u16,
    pub lpstrCustomFilter: *mut u16,
    pub nMaxCustFilter: u32,
    pub nFilterIndex: u32,
    pub lpstrFile: *mut u16,
    pub nMaxFile: u32,
    pub lpstrFileTitle: *mut u16,
    pub nMaxFileTitle: u32,
    pub lpstrInitialDir: *const u16,
    pub lpstrTitle: *const u16,
    pub Flags: u32,
    pub nFileOffset: u16,
    pub nFileExtension: u16,
    pub lpstrDefExt: *const u16,
    pub lCustData: LPARAM,
    pub lpfnHook: *const c_void,
    pub lpTemplateName: *const u16,
    pub pvReserved: *mut c_void,
    pub dwReserved: u32,
    pub FlagsEx: u32,
}

impl Default for OPENFILENAMEW {
    fn default() -> Self {
        // SAFETY: every field is a pointer, an integer or a handle, for all of which an all-zero
        // bit pattern is the documented "not supplied" value.
        let mut zeroed: Self = unsafe { core::mem::zeroed() };
        zeroed.lStructSize = core::mem::size_of::<Self>() as u32;
        zeroed
    }
}

pub const OFN_OVERWRITEPROMPT: u32 = 0x0000_0002;
/// Keeps the dialog from changing the process's working directory out from under us.
pub const OFN_NOCHANGEDIR: u32 = 0x0000_0008;
pub const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
pub const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
pub const OFN_EXPLORER: u32 = 0x0008_0000;

#[link(name = "comdlg32")]
unsafe extern "system" {
    pub fn GetOpenFileNameW(lpofn: *mut OPENFILENAMEW) -> BOOL;
    pub fn GetSaveFileNameW(lpofn: *mut OPENFILENAMEW) -> BOOL;
}

#[cfg(windows)]
#[link(name = "shell32")]
extern "system" {
    pub fn ShellExecuteW(
        hwnd: HANDLE,
        lpOperation: *const u16,
        lpFile: *const u16,
        lpParameters: *const u16,
        lpDirectory: *const u16,
        nShowCmd: i32,
    ) -> HANDLE;
}

// ------------------------------------------------------------------ tray icon and menus

pub const WM_DESTROY: u32 = 0x0002;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_COMMAND: u32 = 0x0111;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_RBUTTONUP: u32 = 0x0205;
/// The first message id reserved for applications. The tray callback and the menu-rebuild wake-up
/// use two of them; nothing in the OS sends anything in this range unasked.
pub const WM_APP: u32 = 0x8000;

pub const NIM_ADD: u32 = 0x0000;
pub const NIM_MODIFY: u32 = 0x0001;
pub const NIM_DELETE: u32 = 0x0002;
pub const NIF_MESSAGE: u32 = 0x0001;
pub const NIF_ICON: u32 = 0x0002;
pub const NIF_TIP: u32 = 0x0004;

pub const MF_STRING: u32 = 0x0000;
pub const MF_POPUP: u32 = 0x0010;
pub const MF_SEPARATOR: u32 = 0x0800;
pub const MF_CHECKED: u32 = 0x0008;
pub const MF_GRAYED: u32 = 0x0001;
/// A radio item's dot rather than a tick, which is how the tray's profile list is marked.
pub const MFT_RADIOCHECK: u32 = 0x0200;
pub const MF_BYCOMMAND: u32 = 0x0000;

pub const TPM_RIGHTBUTTON: u32 = 0x0002;
/// Returns the chosen id instead of posting `WM_COMMAND`, which keeps the menu's result in the
/// one place that asked for it.
pub const TPM_RETURNCMD: u32 = 0x0100;
pub const TPM_NONOTIFY: u32 = 0x0080;

pub const BI_RGB: u32 = 0;
pub const DIB_RGB_COLORS: u32 = 0;

/// `HWND_BROADCAST`: every top-level window. Used once, to tell an already-running instance to
/// show itself.
pub const HWND_BROADCAST: HANDLE = 0xFFFF_usize as HANDLE;

pub const ERROR_ALREADY_EXISTS: u32 = 183;
pub const ERROR_FILE_NOT_FOUND: u32 = 2;

pub type WNDPROC =
    unsafe extern "system" fn(hWnd: HANDLE, msg: u32, wParam: WPARAM, lParam: LPARAM) -> LRESULT;

#[repr(C)]
pub struct WNDCLASSW {
    pub style: u32,
    pub lpfnWndProc: Option<WNDPROC>,
    pub cbClsExtra: i32,
    pub cbWndExtra: i32,
    pub hInstance: HANDLE,
    pub hIcon: HANDLE,
    pub hCursor: HANDLE,
    pub hbrBackground: HANDLE,
    pub lpszMenuName: *const u16,
    pub lpszClassName: *const u16,
}

/// The shell's tray-icon record, in its current (Vista and later) form.
///
/// `cbSize` is how the shell decides which version it was handed, so the layout has to be exactly
/// right - a mismatch is not an error, it is a shell that reads the wrong field as the icon
/// handle. The size is asserted in a test.
#[repr(C)]
pub struct NOTIFYICONDATAW {
    pub cbSize: u32,
    pub hWnd: HANDLE,
    pub uID: u32,
    pub uFlags: u32,
    pub uCallbackMessage: u32,
    pub hIcon: HANDLE,
    pub szTip: [u16; 128],
    pub dwState: u32,
    pub dwStateMask: u32,
    pub szInfo: [u16; 256],
    pub uVersion: u32,
    pub szInfoTitle: [u16; 64],
    pub dwInfoFlags: u32,
    pub guidItem: [u8; 16],
    pub hBalloonIcon: HANDLE,
}

impl Default for NOTIFYICONDATAW {
    fn default() -> Self {
        // SAFETY: every field is a plain integer, a handle or an array of them, and zero is the
        // documented "nothing supplied" value for all of them.
        let mut zeroed: Self = unsafe { core::mem::zeroed() };
        zeroed.cbSize = core::mem::size_of::<Self>() as u32;
        zeroed
    }
}

#[repr(C)]
pub struct ICONINFO {
    pub fIcon: BOOL,
    pub xHotspot: u32,
    pub yHotspot: u32,
    pub hbmMask: HANDLE,
    pub hbmColor: HANDLE,
}

#[repr(C)]
#[derive(Default)]
pub struct BITMAPINFOHEADER {
    pub biSize: u32,
    pub biWidth: i32,
    pub biHeight: i32,
    pub biPlanes: u16,
    pub biBitCount: u16,
    pub biCompression: u32,
    pub biSizeImage: u32,
    pub biXPelsPerMeter: i32,
    pub biYPelsPerMeter: i32,
    pub biClrUsed: u32,
    pub biClrImportant: u32,
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    pub fn RegisterClassW(lpWndClass: *const WNDCLASSW) -> u16;
    pub fn CreateWindowExW(
        dwExStyle: u32,
        lpClassName: *const u16,
        lpWindowName: *const u16,
        dwStyle: u32,
        x: i32,
        y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HANDLE,
        hMenu: HANDLE,
        hInstance: HANDLE,
        lpParam: *mut c_void,
    ) -> HANDLE;
    pub fn DestroyWindow(hWnd: HANDLE) -> BOOL;
    pub fn DefWindowProcW(hWnd: HANDLE, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    pub fn PostMessageW(hWnd: HANDLE, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> BOOL;
    pub fn PostQuitMessage(nExitCode: i32);
    pub fn RegisterWindowMessageW(lpString: *const u16) -> u32;
    pub fn CreatePopupMenu() -> HANDLE;
    pub fn AppendMenuW(hMenu: HANDLE, uFlags: u32, uIDNewItem: usize, lpNewItem: *const u16)
    -> BOOL;
    pub fn DestroyMenu(hMenu: HANDLE) -> BOOL;
    pub fn TrackPopupMenu(
        hMenu: HANDLE,
        uFlags: u32,
        x: i32,
        y: i32,
        nReserved: i32,
        hWnd: HANDLE,
        prcRect: *const c_void,
    ) -> BOOL;
    pub fn SetForegroundWindow(hWnd: HANDLE) -> BOOL;
    pub fn GetCursorPos(lpPoint: *mut POINT) -> BOOL;
    pub fn CheckMenuRadioItem(
        hMenu: HANDLE,
        first: u32,
        last: u32,
        check: u32,
        flags: u32,
    ) -> BOOL;
    pub fn CreateIconIndirect(piconinfo: *const ICONINFO) -> HANDLE;
    pub fn DestroyIcon(hIcon: HANDLE) -> BOOL;
}

#[cfg(windows)]
#[link(name = "gdi32")]
unsafe extern "system" {
    pub fn CreateDIBSection(
        hdc: HANDLE,
        pbmi: *const BITMAPINFOHEADER,
        usage: u32,
        ppvBits: *mut *mut c_void,
        hSection: HANDLE,
        offset: u32,
    ) -> HANDLE;
    pub fn CreateBitmap(
        nWidth: i32,
        nHeight: i32,
        nPlanes: u32,
        nBitCount: u32,
        lpBits: *const c_void,
    ) -> HANDLE;
    pub fn DeleteObject(ho: HANDLE) -> BOOL;
}

#[cfg(windows)]
#[link(name = "shell32")]
unsafe extern "system" {
    pub fn Shell_NotifyIconW(dwMessage: u32, lpData: *mut NOTIFYICONDATAW) -> BOOL;
}

// ------------------------------------------------------------- hand-off between the two binaries

/// Rights needed to wait on another process's handle, and nothing more. The agent waits on the
/// window's process so a window that crashes still gives the hardware back.
pub const SYNCHRONIZE: u32 = 0x0010_0000;
/// Rights needed to set or reset a named event the other process created.
pub const EVENT_MODIFY_STATE: u32 = 0x0002;

pub const WAIT_OBJECT_0: u32 = 0;
pub const WAIT_TIMEOUT: u32 = 258;
pub const WAIT_FAILED: u32 = 0xFFFF_FFFF;

pub const SW_RESTORE: i32 = 9;
/// `AllowSetForegroundWindow(ASFW_ANY)` - any process may take the foreground next. Used by the
/// agent before it raises a window belonging to the other binary, because only the process that
/// currently owns the foreground may hand that right on.
pub const ASFW_ANY: u32 = 0xFFFF_FFFF;

/// `SendMessageTimeoutW` flags: give up rather than hang if the target stops pumping messages,
/// and do not let the call be satisfied by an abandoned window.
pub const SMTO_ABORTIFHUNG: u32 = 0x0002;

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    pub fn FindWindowW(lpClassName: *const u16, lpWindowName: *const u16) -> HANDLE;
    pub fn SendMessageTimeoutW(
        hWnd: HANDLE,
        Msg: u32,
        wParam: WPARAM,
        lParam: LPARAM,
        fuFlags: u32,
        uTimeout: u32,
        lpdwResult: *mut usize,
    ) -> isize;
    pub fn ShowWindow(hWnd: HANDLE, nCmdShow: i32) -> BOOL;
    pub fn AllowSetForegroundWindow(dwProcessId: u32) -> BOOL;
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn CreateEventW(
        lpEventAttributes: *const c_void,
        bManualReset: BOOL,
        bInitialState: BOOL,
        lpName: *const u16,
    ) -> HANDLE;
    pub fn OpenEventW(dwDesiredAccess: u32, bInheritHandle: BOOL, lpName: *const u16) -> HANDLE;
    pub fn SetEvent(hEvent: HANDLE) -> BOOL;
    pub fn ResetEvent(hEvent: HANDLE) -> BOOL;
    pub fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: u32) -> u32;
}

// ------------------------------------------------------------------ single instance and autostart

pub const HKEY_CURRENT_USER: HANDLE = 0x8000_0001_u32 as usize as HANDLE;
pub const KEY_READ: u32 = 0x2_0019;
pub const KEY_WRITE: u32 = 0x2_0006;
pub const REG_SZ: u32 = 1;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn CreateMutexW(
        lpMutexAttributes: *const c_void,
        bInitialOwner: BOOL,
        lpName: *const u16,
    ) -> HANDLE;
    pub fn GetModuleFileNameW(hModule: HANDLE, lpFilename: *mut u16, nSize: u32) -> u32;
}

#[cfg(windows)]
#[link(name = "advapi32")]
unsafe extern "system" {
    pub fn RegOpenKeyExW(
        hKey: HANDLE,
        lpSubKey: *const u16,
        ulOptions: u32,
        samDesired: u32,
        phkResult: *mut HANDLE,
    ) -> i32;
    pub fn RegQueryValueExW(
        hKey: HANDLE,
        lpValueName: *const u16,
        lpReserved: *mut u32,
        lpType: *mut u32,
        lpData: *mut u8,
        lpcbData: *mut u32,
    ) -> i32;
    pub fn RegSetValueExW(
        hKey: HANDLE,
        lpValueName: *const u16,
        Reserved: u32,
        dwType: u32,
        lpData: *const u8,
        cbData: u32,
    ) -> i32;
    pub fn RegDeleteValueW(hKey: HANDLE, lpValueName: *const u16) -> i32;
    pub fn RegCloseKey(hKey: HANDLE) -> i32;
}

// ------------------------------------------------------------------ screen sampler (GDI)

pub const SRCCOPY: u32 = 0x00CC_0020;
/// `StretchBlt` point sampling: one source pixel per destination pixel.
pub const COLORONCOLOR: i32 = 3;
pub const SM_CXSCREEN: i32 = 0;
pub const SM_CYSCREEN: i32 = 1;

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    pub fn GetDC(hWnd: HANDLE) -> HANDLE;
    pub fn ReleaseDC(hWnd: HANDLE, hDC: HANDLE) -> i32;
    pub fn GetSystemMetrics(nIndex: i32) -> i32;
}

#[cfg(windows)]
#[link(name = "gdi32")]
unsafe extern "system" {
    pub fn CreateCompatibleDC(hdc: HANDLE) -> HANDLE;
    pub fn DeleteDC(hdc: HANDLE) -> BOOL;
    pub fn SelectObject(hdc: HANDLE, h: HANDLE) -> HANDLE;
    pub fn SetStretchBltMode(hdc: HANDLE, mode: i32) -> i32;
    pub fn StretchBlt(
        hdcDest: HANDLE,
        xDest: i32,
        yDest: i32,
        wDest: i32,
        hDest: i32,
        hdcSrc: HANDLE,
        xSrc: i32,
        ySrc: i32,
        wSrc: i32,
        hSrc: i32,
        rop: u32,
    ) -> BOOL;
    pub fn GdiFlush() -> BOOL;
}

// ------------------------------------------------------------------ audio loopback (COM / WASAPI)

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl GUID {
    pub const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self { data1, data2, data3, data4 }
    }
}

/// `WAVEFORMATEX`. Packed in the SDK headers, so 18 bytes.
#[repr(C, packed(1))]
#[derive(Debug, Clone, Copy)]
pub struct WAVEFORMATEX {
    pub wFormatTag: u16,
    pub nChannels: u16,
    pub nSamplesPerSec: u32,
    pub nAvgBytesPerSec: u32,
    pub nBlockAlign: u16,
    pub wBitsPerSample: u16,
    pub cbSize: u16,
}

/// `WAVEFORMATEXTENSIBLE`, also packed: 18 + 22 = 40 bytes.
#[repr(C, packed(1))]
#[derive(Debug, Clone, Copy)]
pub struct WAVEFORMATEXTENSIBLE {
    pub Format: WAVEFORMATEX,
    pub wValidBitsPerSample: u16,
    pub dwChannelMask: u32,
    pub SubFormat: GUID,
}

pub const WAVE_FORMAT_PCM: u16 = 1;
pub const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
pub const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

pub const KSDATAFORMAT_SUBTYPE_PCM: GUID =
    GUID::new(0x0000_0001, 0x0000, 0x0010, [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]);
pub const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID =
    GUID::new(0x0000_0003, 0x0000, 0x0010, [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]);

pub const CLSID_MMDEVICEENUMERATOR: GUID =
    GUID::new(0xBCDE_0395, 0xE52F, 0x467C, [0x8E, 0x3D, 0xC4, 0x57, 0x92, 0x91, 0x69, 0x2E]);
pub const IID_IMMDEVICEENUMERATOR: GUID =
    GUID::new(0xA956_64D2, 0x9614, 0x4F35, [0xA7, 0x46, 0xDE, 0x8D, 0xB6, 0x36, 0x17, 0xE6]);
pub const IID_IAUDIOCLIENT: GUID =
    GUID::new(0x1CB9_AD4C, 0xDBFA, 0x4C32, [0xB1, 0x78, 0xC2, 0xF5, 0x68, 0xA7, 0x03, 0xB2]);
pub const IID_IAUDIOCAPTURECLIENT: GUID =
    GUID::new(0xC8AD_BD64, 0xE71E, 0x48A0, [0xA4, 0xDE, 0x18, 0x5C, 0x39, 0x5C, 0xD3, 0x17]);

pub const COINIT_MULTITHREADED: u32 = 0x0;
pub const CLSCTX_ALL: u32 = 0x17;
/// `EDataFlow::eRender` and `ERole::eConsole`.
pub const E_RENDER: u32 = 0;
pub const E_CONSOLE: u32 = 0;
pub const AUDCLNT_SHAREMODE_SHARED: u32 = 0;
pub const AUDCLNT_STREAMFLAGS_LOOPBACK: u32 = 0x0002_0000;
pub const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;

pub type HRESULT = i32;

#[cfg(windows)]
#[link(name = "ole32")]
unsafe extern "system" {
    pub fn CoInitializeEx(pvReserved: *const c_void, dwCoInit: u32) -> HRESULT;
    pub fn CoUninitialize();
    pub fn CoCreateInstance(
        rclsid: *const GUID,
        pUnkOuter: *mut c_void,
        dwClsContext: u32,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT;
    pub fn CoTaskMemFree(pv: *mut c_void);
}

/// A NUL-terminated UTF-16 buffer for a Win32 `W` entry point.
pub fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a UTF-16 buffer up to its first NUL.
///
/// Lossy: a lone surrogate in a window title (which happens, titles are not validated by the OS)
/// must become a replacement character rather than failing the whole foreground lookup.
pub fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_struct_is_the_size_sendinput_expects() {
        // SendInput rejects the entire call when cbSize disagrees, so this is not a style check.
        assert_eq!(std::mem::size_of::<INPUT>() as i32, INPUT_SIZE);
        assert_eq!(std::mem::align_of::<INPUT>(), 8);
    }

    /// The shell reads `cbSize` to decide which version of the record it was handed, so a wrong
    /// layout is not a compile error and not a runtime error either - it is a shell reading the
    /// wrong eight bytes as the icon handle.
    #[test]
    fn the_tray_icon_record_is_the_size_the_shell_expects() {
        assert_eq!(std::mem::size_of::<NOTIFYICONDATAW>(), 976);
        assert_eq!(std::mem::align_of::<NOTIFYICONDATAW>(), 8);
        assert_eq!(NOTIFYICONDATAW::default().cbSize, 976);
    }

    #[test]
    fn the_icon_and_bitmap_headers_match_their_documented_layouts() {
        assert_eq!(std::mem::size_of::<ICONINFO>(), 32);
        // `biSize` is how GDI tells a V4/V5 header from this one.
        assert_eq!(std::mem::size_of::<BITMAPINFOHEADER>(), 40);
    }

    #[test]
    fn hook_structs_match_the_documented_layouts() {
        assert_eq!(std::mem::size_of::<KBDLLHOOKSTRUCT>(), 24);
        assert_eq!(std::mem::size_of::<MSLLHOOKSTRUCT>(), 32);
        assert_eq!(std::mem::size_of::<MOUSEINPUT>(), 32);
        assert_eq!(std::mem::size_of::<KEYBDINPUT>(), 24);
        assert_eq!(std::mem::size_of::<MSG>(), 48);
    }

    /// `Process32FirstW` fails outright when `dwSize` is not the struct's own size, so the
    /// constructor filling it in is load-bearing rather than tidiness.
    #[test]
    fn a_process_entry_carries_its_own_size() {
        let entry = PROCESSENTRY32W::default();
        assert_eq!(entry.dwSize as usize, std::mem::size_of::<PROCESSENTRY32W>());
        assert_eq!(std::mem::size_of::<PROCESSENTRY32W>(), 568);
    }

    /// `GetOpenFileNameW` validates `lStructSize` against its own layout and refuses the call if
    /// it disagrees, so the hand-written struct's size is pinned.
    #[test]
    fn an_open_file_name_carries_its_own_size() {
        let dialog = OPENFILENAMEW::default();
        assert_eq!(dialog.lStructSize as usize, std::mem::size_of::<OPENFILENAMEW>());
        assert_eq!(std::mem::size_of::<OPENFILENAMEW>(), 152);
    }

    #[test]
    fn extra_info_is_pointer_sized() {
        // u32 here would shift `time` and `dwExtraInfo` and break the injection marker.
        assert_eq!(
            std::mem::size_of::<usize>(),
            std::mem::size_of::<*const u8>()
        );
        assert_eq!(
            std::mem::offset_of!(KBDLLHOOKSTRUCT, dwExtraInfo),
            16,
            "the marker must land where Windows writes it"
        );
    }

    #[test]
    fn wide_strings_round_trip_including_astral_characters() {
        for text in ["", "cmd.exe", "C:\\Program Files\\x\\y.exe", "日本語", "emoji 🎮 here"] {
            let wide = to_wide(text);
            assert_eq!(*wide.last().unwrap(), 0, "must be NUL terminated");
            assert_eq!(from_wide(&wide), text);
        }
    }

    #[test]
    fn from_wide_stops_at_the_first_nul_and_tolerates_a_lone_surrogate() {
        assert_eq!(from_wide(&[0x41, 0x42, 0, 0x43, 0x44]), "AB");
        assert_eq!(from_wide(&[0x41, 0x42]), "AB", "no terminator is still readable");
        // A lone high surrogate: lossy decoding, not a panic.
        assert_eq!(from_wide(&[0xD800, 0x41, 0]).chars().count(), 2);
    }
}

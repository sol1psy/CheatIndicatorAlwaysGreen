#![windows_subsystem = "windows"]
use std::{
    ffi::c_void,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::{
    Controls::*,
    Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    System::{
        Diagnostics::{Debug::*, ToolHelp::*},
        LibraryLoader::*,
        Memory::*,
        Threading::*,
    },
    UI::WindowsAndMessaging::*,
};

const RVA: usize = 0xbc910;
const LEN: usize = 763;
const COLOR: usize = 0x2c0;
const HISTORY: usize = 0x2b0;
const ORIGINAL: &[u8; LEN] = include_bytes!("region.bin");
const PATCH: [u8; 13] = [
    0x66, 0xc7, 0x01, 0x00, 0xff, 0xc6, 0x41, 0x02, 0x00, 0x48, 0x89, 0xc8, 0xc3,
];
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn name(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|x| *x == 0).unwrap_or(s.len())])
}
fn error(s: &str) -> String {
    format!("{s} (Windows {})", unsafe { GetLastError() })
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
unsafe fn snapshot(flags: u32, pid: u32) -> Result<Handle, String> {
    for _ in 0..4 {
        let h = CreateToolhelp32Snapshot(flags, pid);
        if h != INVALID_HANDLE_VALUE {
            return Ok(Handle(h));
        }
        if GetLastError() != ERROR_BAD_LENGTH {
            break;
        }
    }
    Err(error("Cannot inspect processes"))
}
#[derive(Clone)]
struct Target {
    pid: u32,
    base: usize,
}
unsafe fn find_process(exe: &str) -> Result<Option<u32>, String> {
    let snap = snapshot(TH32CS_SNAPPROCESS, 0)?;
    let mut p: PROCESSENTRY32W = zeroed();
    p.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut ids = vec![];
    let mut ok = Process32FirstW(snap.0, &mut p);
    while ok != 0 {
        if name(&p.szExeFile).eq_ignore_ascii_case(exe) {
            ids.push(p.th32ProcessID)
        }
        ok = Process32NextW(snap.0, &mut p)
    }
    if ids.len() > 1 {
        return Err("Multiple games open. Keep one instance.".into());
    }
    Ok(ids.first().copied())
}
struct ProcessState {
    pid: Option<u32>,
    module: Result<Option<Target>, String>,
}
unsafe fn target() -> Result<ProcessState, String> {
    let Some(pid) = find_process("GeometryDash.exe")? else {
        return Ok(ProcessState {
            pid: None,
            module: Ok(None),
        });
    };
    let module = (|| -> Result<Option<Target>, String> {
        let mods = snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)?;
        let mut m: MODULEENTRY32W = zeroed();
        m.dwSize = size_of::<MODULEENTRY32W>() as u32;
        let mut found = None;
        let mut ok = Module32FirstW(mods.0, &mut m);
        while ok != 0 {
            if name(&m.szModule).eq_ignore_ascii_case("hackpro.dll") {
                if found.is_some() {
                    return Err("Multiple hackpro modules found.".into());
                }
                found = Some(Target {
                    pid,
                    base: m.modBaseAddr as usize,
                })
            }
            ok = Module32NextW(mods.0, &mut m)
        }
        Ok(found)
    })();
    Ok(ProcessState {
        pid: Some(pid),
        module,
    })
}
unsafe fn open(t: &Target, write: bool) -> Result<Handle, String> {
    let access = PROCESS_QUERY_INFORMATION
        | PROCESS_VM_READ
        | if write {
            PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_SUSPEND_RESUME
        } else {
            0
        };
    let h = OpenProcess(access, 0, t.pid);
    if h.is_null() {
        Err(error("Access denied; try running as administrator"))
    } else {
        Ok(Handle(h))
    }
}
unsafe fn read(h: HANDLE, addr: usize) -> Result<[u8; LEN], String> {
    let mut b = [0; LEN];
    let mut n = 0;
    if ReadProcessMemory(h, addr as _, b.as_mut_ptr() as _, b.len(), &mut n) == 0 || n != b.len() {
        Err(error("Cannot read module"))
    } else {
        Ok(b)
    }
}
fn patched() -> [u8; LEN] {
    let mut b = *ORIGINAL;
    b[COLOR..COLOR + PATCH.len()].copy_from_slice(&PATCH);
    b[..3].copy_from_slice(&[0x31, 0xc0, 0xc3]);
    b[HISTORY..HISTORY + 3].copy_from_slice(&[0x31, 0xc0, 0xc3]);
    b
}
fn classify(b: &[u8; LEN]) -> Result<bool, String> {
    if b == ORIGINAL {
        Ok(false)
    } else if *b == patched() {
        Ok(true)
    } else {
        Err("Unexpected code at patch location.".into())
    }
}
#[link(name = "ntdll")]
extern "system" {
    fn NtSuspendProcess(h: HANDLE) -> i32;
    fn NtResumeProcess(h: HANDLE) -> i32;
}
struct Freeze(HANDLE);
impl Drop for Freeze {
    fn drop(&mut self) {
        unsafe {
            NtResumeProcess(self.0);
        }
    }
}
unsafe fn toggle(t: &Target, was_on: bool) -> Result<(), String> {
    let h = open(t, true)?;
    if NtSuspendProcess(h.0) < 0 {
        return Err("Cannot pause game safely.".into());
    }
    let _f = Freeze(h.0);
    let current = read(h.0, t.base + RVA)?;
    if classify(&current)? != was_on {
        return Err("State changed. Please try again.".into());
    }
    let desired = if was_on { *ORIGINAL } else { patched() };
    let addr = (t.base + RVA) as *mut c_void;
    let mut old = 0;
    if VirtualProtectEx(h.0, addr, LEN, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
        return Err(error("Cannot unlock memory"));
    }
    let mut n = 0;
    let wrote = WriteProcessMemory(h.0, addr, desired.as_ptr() as _, LEN, &mut n) != 0 && n == LEN;
    let verified = wrote
        && read(h.0, t.base + RVA)
            .map(|b| b == desired)
            .unwrap_or(false);
    let mut rollback_ok = true;
    if !verified {
        n = 0;
        rollback_ok = WriteProcessMemory(h.0, addr, current.as_ptr() as _, LEN, &mut n) != 0
            && n == LEN
            && read(h.0, t.base + RVA)
                .map(|b| b == current)
                .unwrap_or(false);
    }
    let flushed = FlushInstructionCache(h.0, addr, LEN) != 0;
    let mut unused = 0;
    let restored = VirtualProtectEx(h.0, addr, LEN, old, &mut unused) != 0;
    if !rollback_ok {
        return Err("Write failed; rollback failed. Restart the game.".into());
    }
    if !verified {
        return Err("Write failed; original state restored.".into());
    }
    if !flushed || !restored {
        return Err("Bytes changed, but memory cleanup failed. Restart game.".into());
    }
    Ok(())
}
struct App {
    game: HWND,
    module: HWND,
    status: HWND,
    button: HWND,
    font: HFONT,
    brush: HBRUSH,
    target: Option<Target>,
    on: bool,
}
unsafe fn layout(hwnd: HWND, a: &mut App, dpi: u32) {
    let px = |n: i32| ((n as i64 * dpi as i64 + 48) / 96) as i32;
    let font = CreateFontW(
        -px(15),
        0,
        0,
        0,
        400,
        0,
        0,
        0,
        1,
        0,
        0,
        5,
        0,
        wide("Segoe UI").as_ptr(),
    );
    let old_font = a.font;
    a.font = font;
    for (h, y, height) in [
        (a.game, 18, 22),
        (a.module, 46, 22),
        (a.status, 74, 42),
        (a.button, 121, 36),
    ] {
        SetWindowPos(
            h,
            null_mut(),
            px(20),
            px(y),
            px(302),
            px(height),
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        SendMessageW(h, WM_SETFONT, font as usize, 1);
    }
    DeleteObject(old_font as _);
    InvalidateRect(hwnd, null(), 1);
}
unsafe fn set(h: HWND, s: &str) {
    let mut current = vec![0u16; GetWindowTextLengthW(h) as usize + 1];
    GetWindowTextW(h, current.as_mut_ptr(), current.len() as i32);
    if name(&current) != s {
        SetWindowTextW(h, wide(s).as_ptr());
    }
}
unsafe fn refresh(a: &mut App) {
    let old_on = a.on;
    let old_enabled = IsWindowEnabled(a.button) != 0;
    a.target = None;
    a.on = false;
    let mut enabled = false;
    match target() {
        Ok(ProcessState { pid: None, .. }) => {
            set(a.game, "Game     Not running");
            set(a.module, "Mod       Not loaded");
            set(a.status, "Patch     Unavailable");
        }
        Ok(ProcessState {
            pid: Some(pid),
            module,
        }) => {
            set(a.game, &format!("Game     Running · {pid}"));
            match module {
                Ok(Some(t)) => {
                    set(a.module, "Mod       hackpro.dll loaded");
                    match open(&t, false)
                        .and_then(|h| read(h.0, t.base + RVA))
                        .and_then(|b| classify(&b))
                    {
                        Ok(on) => {
                            a.on = on;
                            enabled = true;
                            a.target = Some(t);
                            set(a.status, if on { "Patch     ON" } else { "Patch     OFF" });
                        }
                        Err(e) => {
                            set(a.status, &e);
                        }
                    }
                }
                Ok(None) => {
                    set(a.module, "Mod       Not loaded");
                    set(a.status, "Patch     Unavailable");
                }
                Err(_) => {
                    set(a.module, "Mod       Checking...");
                    set(a.status, "Patch     Unavailable");
                }
            }
        }
        Err(e) => {
            set(a.game, "Game     Unable to inspect");
            set(a.module, "Mod       Unknown");
            set(a.status, &e);
        }
    }
    set(a.button, if a.on { "Turn off" } else { "Turn on" });
    if old_enabled != enabled {
        EnableWindow(a.button, enabled as i32);
    }
    if old_on != a.on || old_enabled != enabled {
        InvalidateRect(a.button, null(), 0);
    }
}
unsafe fn label(parent: HWND, text: &str, y: i32, height: i32, font: HFONT) -> HWND {
    let h = CreateWindowExW(
        0,
        wide("STATIC").as_ptr(),
        wide(text).as_ptr(),
        WS_CHILD | WS_VISIBLE,
        20,
        y,
        302,
        height,
        parent,
        null_mut(),
        null_mut(),
        null(),
    );
    SendMessageW(h, WM_SETFONT, font as usize, 1);
    h
}
unsafe extern "system" fn wnd(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    match msg {
        WM_CREATE => {
            let font = CreateFontW(
                -15,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                0,
                wide("Segoe UI").as_ptr(),
            );
            let brush = CreateSolidBrush(0x001c1c1c);
            let game = label(hwnd, "Game     Checking...", 18, 22, font);
            let module = label(hwnd, "", 46, 22, font);
            let status = label(hwnd, "", 74, 42, font);
            let button = CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide("Turn on").as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
                20,
                121,
                302,
                36,
                hwnd,
                1usize as _,
                null_mut(),
                null(),
            );
            let a = Box::into_raw(Box::new(App {
                game,
                module,
                status,
                button,
                font,
                brush,
                target: None,
                on: false,
            }));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, a as isize);
            let dark: i32 = 1;
            DwmSetWindowAttribute(hwnd, 20, &dark as *const _ as _, 4);
            layout(hwnd, &mut *a, GetDpiForWindow(hwnd));
            SetTimer(hwnd, 1, 250, None);
            refresh(&mut *a);
            return 0;
        }
        WM_DPICHANGED if !ptr.is_null() => {
            let rect = &*(lp as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            layout(hwnd, &mut *ptr, (wp & 0xffff) as u32);
            return 0;
        }
        WM_TIMER if !ptr.is_null() => {
            refresh(&mut *ptr);
            return 0;
        }
        WM_COMMAND if wp & 0xffff == 1 && !ptr.is_null() => {
            let a = &mut *ptr;
            if let Some(t) = a.target.clone() {
                let result = toggle(&t, a.on);
                refresh(a);
                if let Err(e) = result {
                    MessageBoxW(
                        hwnd,
                        wide(&e).as_ptr(),
                        wide("Patch status").as_ptr(),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
            return 0;
        }
        WM_CTLCOLORSTATIC if !ptr.is_null() => {
            SetTextColor(wp as _, 0x00dedede);
            SetBkColor(wp as _, 0x001c1c1c);
            return (*ptr).brush as isize;
        }
        WM_ERASEBKGND if !ptr.is_null() => {
            let mut r = zeroed();
            GetClientRect(hwnd, &mut r);
            FillRect(wp as _, &r, (*ptr).brush);
            return 1;
        }
        WM_DRAWITEM if !ptr.is_null() => {
            let d = &*(lp as *const DRAWITEMSTRUCT);
            let disabled = IsWindowEnabled(d.hwndItem) == 0;
            let color = if disabled {
                0x00303030
            } else if (*ptr).on {
                if d.itemState & ODS_SELECTED != 0 {
                    0x00333391
                } else {
                    0x004747b5
                }
            } else if d.itemState & ODS_SELECTED != 0 {
                0x004b6840
            } else {
                0x004b813d
            };
            let brush = CreateSolidBrush(color);
            FillRect(d.hDC, &d.rcItem, brush);
            DeleteObject(brush as _);
            SelectObject(d.hDC, (*ptr).font as _);
            SetBkMode(d.hDC, TRANSPARENT as i32);
            SetTextColor(d.hDC, if disabled { 0x00808080 } else { 0x00ffffff });
            let text = wide(if (*ptr).on { "Turn off" } else { "Turn on" });
            let mut r = d.rcItem;
            DrawTextW(
                d.hDC,
                text.as_ptr(),
                -1,
                &mut r,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            if d.itemState & ODS_FOCUS != 0 {
                InflateRect(&mut r, -4, -4);
                DrawFocusRect(d.hDC, &r);
            }
            return 1;
        }
        WM_DESTROY => {
            KillTimer(hwnd, 1);
            if !ptr.is_null() {
                let a = Box::from_raw(ptr);
                DeleteObject(a.font as _);
                DeleteObject(a.brush as _);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            PostQuitMessage(0);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}
fn main() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let dpi = GetDpiForSystem();
        let px = |n: i32| ((n as i64 * dpi as i64 + 48) / 96) as i32;
        let instance = GetModuleHandleW(null());
        let class = wide("GDIndicatorStudy");
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(wnd);
        wc.hInstance = instance;
        wc.lpszClassName = class.as_ptr();
        wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        RegisterClassW(&wc);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: px(342),
            bottom: px(178),
        };
        AdjustWindowRectExForDpi(&mut r, style, 0, 0, dpi);
        let mut work_area: RECT = zeroed();
        if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work_area as *mut _ as _, 0) == 0 {
            work_area.right = GetSystemMetrics(SM_CXSCREEN);
            work_area.bottom = GetSystemMetrics(SM_CYSCREEN);
        }
        let width = r.right - r.left;
        let height = r.bottom - r.top;
        let x = work_area.left + ((work_area.right - work_area.left - width) / 2).max(0);
        let y = work_area.top + ((work_area.bottom - work_area.top - height) / 2).max(0);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("GD · Cheat Indicator Patch").as_ptr(),
            style,
            x,
            y,
            width,
            height,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        ShowWindow(hwnd, SW_SHOW);
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

//! Windows-only pieces of the launch, through plain Win32 calls. The names are shared with the
//! native launcher (packaging/launcher, canvas.exe): it sets the same AppUserModelID, checks the
//! same mutex, and finds a running app by the same window property.

#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;

type HANDLE = *mut c_void;
type HWND = *mut c_void;
type BOOL = i32;
type LPARAM = isize;

const APP_ID: &str = "OwenPalmer.CanvasMCP"; // one taskbar button for the launcher and the app
const MUTEX: &str = "Local\\OwenPalmer.CanvasMCP.App"; // held while the app runs
const WINDOW_PROP: &str = "OwenPalmer.CanvasMCP.App"; // on the app's window, so another launch can find it
const ERROR_ALREADY_EXISTS: u32 = 183;
const SW_RESTORE: i32 = 9;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(attrs: *mut c_void, owner: BOOL, name: *const u16) -> HANDLE;
    fn GetLastError() -> u32;
    fn GetCurrentProcessId() -> u32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(f: unsafe extern "system" fn(HWND, LPARAM) -> BOOL, l: LPARAM) -> BOOL;
    fn GetPropW(h: HWND, name: *const u16) -> HANDLE;
    fn SetPropW(h: HWND, name: *const u16, v: HANDLE) -> BOOL;
    fn IsIconic(h: HWND) -> BOOL;
    fn IsWindowVisible(h: HWND) -> BOOL;
    fn ShowWindow(h: HWND, cmd: i32) -> BOOL;
    fn SetForegroundWindow(h: HWND) -> BOOL;
    fn GetWindowThreadProcessId(h: HWND, pid: *mut u32) -> u32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn SetCurrentProcessExplicitAppUserModelID(id: *const u16) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Give this process's windows the app's taskbar identity. Call before any window exists.
pub fn set_app_id() {
    let id = wide(APP_ID);
    unsafe {
        SetCurrentProcessExplicitAppUserModelID(id.as_ptr());
    }
}

/// Take the app's mutex (held for the life of the process); false if another instance has it.
pub fn take_single_instance() -> bool {
    let name = wide(MUTEX);
    unsafe {
        let h = CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
        let err = GetLastError();
        // no mutex at all: run anyway rather than refuse to start
        h.is_null() || err != ERROR_ALREADY_EXISTS
    }
}

unsafe extern "system" fn find_marked(h: HWND, out: LPARAM) -> BOOL {
    let prop = wide(WINDOW_PROP);
    unsafe {
        if !GetPropW(h, prop.as_ptr()).is_null() {
            *(out as *mut HWND) = h;
            return 0;
        }
    }
    1
}

/// Restore and bring forward the running app's window; false if there isn't one (yet).
pub fn focus_existing() -> bool {
    let mut found: HWND = std::ptr::null_mut();
    unsafe {
        EnumWindows(find_marked, &mut found as *mut HWND as LPARAM);
        if found.is_null() {
            return false;
        }
        if IsIconic(found) != 0 {
            ShowWindow(found, SW_RESTORE);
        }
        SetForegroundWindow(found); // allowed: this process was just launched by the user
    }
    true
}

unsafe extern "system" fn mark_own(h: HWND, _: LPARAM) -> BOOL {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(h, &mut pid);
        if pid == GetCurrentProcessId() && IsWindowVisible(h) != 0 {
            let prop = wide(WINDOW_PROP);
            SetPropW(h, prop.as_ptr(), 1 as HANDLE);
        }
    }
    1
}

/// Mark this process's window so a second launch can find and focus it.
pub fn mark_window() {
    unsafe {
        EnumWindows(mark_own, 0);
    }
}

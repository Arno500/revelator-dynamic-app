//! Notification-area (system tray) icon: closing the window hides it and the app keeps
//! running (the mic's preset button keeps working); "Quit" in the tray menu really exits.

use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Set by the tray's Quit item so the close request that follows is not turned into "hide".
pub static QUITTING: AtomicBool = AtomicBool::new(false);

fn icon() -> Option<Icon> {
    const N: u32 = 32;
    Icon::from_rgba(crate::icon::rgba(N), N, N).ok()
}

#[link(name = "user32")]
extern "system" {
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(attrs: *const u8, initial_owner: i32, name: *const u16) -> isize;
    fn GetLastError() -> u32;
}
#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: extern "system" fn(isize, isize) -> i32, lparam: isize) -> i32;
    fn GetPropW(hwnd: isize, name: *const u16) -> isize;
    fn SetPropW(hwnd: isize, name: *const u16, data: isize) -> i32;
}
const ERROR_ALREADY_EXISTS: u32 = 183;

/// Window property marking this app's main window (its title changes with the active slot,
/// so a second instance cannot find it by title).
fn marker() -> Vec<u16> {
    "RevelatorControl.MainWindow"
        .encode_utf16()
        .chain(Some(0))
        .collect()
}

/// Mark `hwnd` as the main window, for [`acquire_single_instance`] in a later launch.
pub fn mark_main_window(hwnd: isize) {
    // SAFETY: NUL-terminated UTF-16 name that outlives the call; our own window.
    unsafe {
        SetPropW(hwnd, marker().as_ptr(), 1);
    }
}

/// The running instance's main window, if any.
fn find_main_window() -> isize {
    extern "system" fn each(hwnd: isize, out: isize) -> i32 {
        // SAFETY: `out` points at the `found` below for the duration of EnumWindows.
        unsafe {
            if GetPropW(hwnd, marker().as_ptr()) != 0 {
                *(out as *mut isize) = hwnd;
                return 0; // stop
            }
        }
        1
    }
    let mut found: isize = 0;
    // SAFETY: the callback only writes `found`, which outlives the call.
    unsafe {
        EnumWindows(each, &mut found as *mut isize as isize);
    }
    found
}

/// Single-instance guard. Returns `true` when this is the first instance. Otherwise `action`
/// (from a jump list task) is handed to the running instance, or without one its window is
/// shown/restored (it may be hidden in the tray), and `false` is returned: the caller should
/// exit.
pub fn acquire_single_instance(action: Option<crate::model::Action>) -> bool {
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() };
    let name = wide("Local\\RevelatorControl.SingleInstance");
    // SAFETY: plain Win32 calls with NUL-terminated UTF-16 strings that outlive the calls.
    unsafe {
        // The handle is intentionally leaked: it must live as long as the process.
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if h != 0 && GetLastError() != ERROR_ALREADY_EXISTS {
            return true;
        }
        if h == 0 {
            return true; // cannot tell; do not block startup
        }
        if action.is_some_and(crate::taskbar::send_to_running) {
            return false;
        }
        let hwnd = find_main_window();
        if hwnd != 0 {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
        false
    }
}

const SW_HIDE: i32 = 0;
const SW_SHOW: i32 = 5;
const SW_RESTORE: i32 = 9;

/// Hide the window. Done with Win32 directly because egui does not run frames for a hidden
/// window, so a `ViewportCommand` sent from the tray would never be processed.
pub fn hide(hwnd: isize) {
    // SAFETY: plain Win32 call on this app's own window handle.
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
}

fn show(ctx: &egui::Context, hwnd: isize) {
    // SAFETY: as above.
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }
    ctx.request_repaint();
}

/// Create the tray icon. Returns `None` when it cannot be created (the window then simply
/// closes the app as before). Keep the returned value alive for as long as the app runs.
pub fn create(ctx: &egui::Context, hwnd: isize) -> Option<TrayIcon> {
    let menu = Menu::new();
    let open = MenuItem::new("Open Revelator Control", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[&open, &quit]).ok()?;
    let (open_id, quit_id) = (open.id().clone(), quit.id().clone());

    let c = ctx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == open_id {
            show(&c, hwnd);
        } else if e.id == quit_id {
            QUITTING.store(true, Ordering::SeqCst);
            // A hidden window runs no frames: show it so the close below is processed.
            show(&c, hwnd);
            c.send_viewport_cmd(egui::ViewportCommand::Close);
            c.request_repaint();
        }
    }));
    let c = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = e
        {
            show(&c, hwnd);
        }
    }));

    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Revelator Control (running in background)")
        .with_icon(icon()?)
        .build()
        .ok()
}

//! Live window/taskbar/tray status and quick actions:
//! - the window and tray icons get a badge with the mic's active preset slot (its number on
//!   the slot's colour); the title and tray tooltip name the preset that slot holds;
//! - the thumbnail toolbar (hover the taskbar button) mutes and switches slots;
//! - the jump list (right-click the taskbar button) offers the same as tasks, which launch the
//!   .exe with arguments; a second launch hands its action to the running instance.
//!
//! Done with Win32 directly rather than through egui, because egui runs no frames while the
//! window is minimized or hidden in the tray, which is exactly when this matters. The device
//! thread calls [`Taskbar::set`] whenever its state changes; main-thread work is posted to a
//! hidden message-only window, which winit's message loop always dispatches.

use crate::icon;
use crate::model::{Action, Connection, MicPresets, RemoteFn, UiState};
use crate::shell;
use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use tray_icon::TrayIcon;

/// Base window title; also how the window is introduced to Windows.
pub const TITLE: &str = "Revelator Control";

#[link(name = "user32")]
extern "system" {
    fn CreateIconFromResourceEx(
        bits: *const u8,
        size: u32,
        is_icon: i32,
        version: u32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> isize;
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn SetWindowTextW(hwnd: isize, text: *const u16) -> i32;
    fn RegisterClassW(class: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: isize,
        menu: isize,
        instance: isize,
        param: *const u8,
    ) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn CallWindowProcW(prev: isize, hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn RegisterWindowMessageW(name: *const u16) -> u32;
    fn FindWindowExW(parent: isize, after: isize, class: *const u16, name: *const u16) -> isize;
}
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

#[repr(C)]
struct WndClassW {
    style: u32,
    wnd_proc: extern "system" fn(isize, u32, usize, isize) -> isize,
    cls_extra: i32,
    wnd_extra: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu_name: *const u16,
    class_name: *const u16,
}

const WM_SETICON: u32 = 0x0080;
const WM_COMMAND: u32 = 0x0111;
const ICON_SMALL: usize = 0;
const ICON_BIG: usize = 1;
const SM_CXICON: i32 = 11;
const SM_CXSMICON: i32 = 49;
const HWND_MESSAGE: isize = -3;
const GWLP_WNDPROC: i32 = -4;
/// Control window: show [`PENDING`] (tray, thumbnail buttons, jump list).
const WM_STATUS: u32 = 0x8000 + 1; // WM_APP + 1
/// Control window: perform `Action::from_code(wparam)` (sent by a second launch).
const WM_REMOTE: u32 = 0x8000 + 2;
const CONTROL_CLASS: &str = "RevelatorControl.Control";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// What the taskbar shows for a state.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// Active slot (0-based); `None` unless a mic is connected.
    pub slot: Option<usize>,
    pub muted: bool,
    /// What each slot holds (`None`: empty or not read yet).
    pub names: Vec<Option<String>>,
    pub title: String,
}

pub fn status_of(st: &UiState) -> Status {
    let connected = st.connection == Connection::Connected;
    let slot = connected
        .then(|| st.values.get("mic.slot"))
        .flatten()
        .map(|v| (v.round().max(0.0) as usize).min(icon::SLOTS - 1));
    let names: Vec<Option<String>> = (0..icon::SLOTS)
        .map(|i| {
            st.mic_presets
                .as_ref()
                .and_then(|p| p.slots.get(i))
                .map(|s| s.trim())
                .filter(|s| !s.is_empty() && MicPresets::defined(s))
                .map(str::to_string)
        })
        .collect();
    let title = match slot {
        None => TITLE.to_string(),
        Some(n) => match &names[n] {
            Some(name) => format!("{TITLE} \u{2013} {}: {name}", n + 1),
            None => format!("{TITLE} \u{2013} Slot {}", n + 1),
        },
    };
    let muted = connected && st.values.get("mic.mute").is_some_and(|v| *v >= 0.5);
    Status {
        slot,
        muted,
        names,
        title,
    }
}

// ------------------------------------------------- main-thread side

thread_local! {
    /// The tray icon (not `Send`), updated on the main thread.
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    /// The last status shown, to re-add thumbnail buttons and skip unchanged jump lists.
    static SHOWN: RefCell<Option<Status>> = const { RefCell::new(None) };
}
/// Latest status, handed from any thread to the main thread.
static PENDING: Mutex<Option<Status>> = Mutex::new(None);
/// Performs actions (jump list, thumbnail buttons, second launches).
static REMOTE: Mutex<Option<RemoteFn>> = Mutex::new(None);
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static MAIN_OLD_PROC: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

fn run(a: Action) {
    let r = REMOTE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(r) = r {
        r(a);
    }
}

fn show_thumbs(st: &Status) {
    let hwnd = MAIN_HWND.load(Ordering::Relaxed);
    shell::update_thumb_buttons(
        hwnd,
        &shell::Thumbs {
            slot: st.slot,
            muted: st.muted,
            names: &st.names,
        },
    );
}

/// Show `st` on the tray icon, the thumbnail toolbar and the jump list (main thread).
fn apply(st: Status) {
    TRAY.with(|t| {
        if let Some(t) = t.borrow().as_ref() {
            // SAFETY: plain Win32 query.
            let n = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
            if let Ok(i) = tray_icon::Icon::from_rgba(icon::rgba_slot(n, st.slot), n, n) {
                let _ = t.set_icon(Some(i));
            }
            let tip = match (st.slot, st.muted) {
                (None, _) => format!("{TITLE} (running in background)"),
                (Some(_), false) => st.title.clone(),
                (Some(_), true) => format!("{} (muted)", st.title),
            };
            let _ = t.set_tooltip(Some(tip));
        }
    });
    show_thumbs(&st);
    let names_changed = SHOWN.with(|s| s.borrow().as_ref().map(|p| &p.names) != Some(&st.names));
    if names_changed {
        shell::set_jump_list(&st.names);
    }
    SHOWN.with(|s| *s.borrow_mut() = Some(st));
}

/// The hidden control window's procedure.
extern "system" fn control_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    match msg {
        WM_STATUS => {
            let st = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(st) = st {
                apply(st);
            }
            0
        }
        WM_REMOTE => {
            if let Some(a) = Action::from_code(wparam) {
                run(a);
            }
            0
        }
        // SAFETY: default handling for every other message.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Subclass of the app window: thumbnail button clicks, and re-adding the buttons whenever
/// the taskbar (re)creates the window's button (after hiding to the tray, Explorer restarts).
extern "system" fn main_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    if msg == WM_COMMAND && (wparam >> 16) & 0xffff == shell::THBN_CLICKED {
        if let Some(a) = Action::from_code(wparam & 0xffff) {
            run(a);
        }
        return 0;
    }
    let created = TASKBAR_CREATED.load(Ordering::Relaxed);
    if created != 0 && msg == created {
        shell::taskbar_button_created();
        SHOWN.with(|s| {
            if let Some(st) = s.borrow().as_ref() {
                show_thumbs(st);
            }
        });
    }
    // SAFETY: chain to the window's original procedure.
    unsafe {
        CallWindowProcW(
            MAIN_OLD_PROC.load(Ordering::Relaxed),
            hwnd,
            msg,
            wparam,
            lparam,
        )
    }
}

fn create_control_window() -> isize {
    let name = wide(CONTROL_CLASS);
    // SAFETY: plain Win32 calls; the class name outlives them (the class keeps a copy).
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class = WndClassW {
            style: 0,
            wnd_proc: control_proc,
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: std::ptr::null(),
            class_name: name.as_ptr(),
        };
        RegisterClassW(&class);
        CreateWindowExW(
            0,
            name.as_ptr(),
            name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            0,
            instance,
            std::ptr::null(),
        )
    }
}

/// From a second launch: hand `a` to the running instance. False if none was found.
pub fn send_to_running(a: Action) -> bool {
    let class = wide(CONTROL_CLASS);
    // SAFETY: plain Win32 calls; no pointers are posted.
    unsafe {
        let w = FindWindowExW(HWND_MESSAGE, 0, class.as_ptr(), std::ptr::null());
        w != 0 && PostMessageW(w, WM_REMOTE, a.code(), 0) != 0
    }
}

// ------------------------------------------------- shared handle

pub struct Taskbar {
    hwnd: isize,
    /// `[small, big]` HICONs for the plain icon (index 0) and each slot (1..). Created once
    /// and never destroyed: posted messages may still refer to them, and they are few.
    icons: Vec<[isize; 2]>,
    shown: Mutex<Option<Status>>,
    /// To the title thread. `SetWindowTextW` waits for the UI thread, so it must not run
    /// on the device thread while that holds the state lock the UI may be waiting for.
    titles: Mutex<Sender<String>>,
    control: isize,
}

impl Taskbar {
    /// Call on the main thread, which owns the window and `tray`. `remote` performs the
    /// actions of the jump list, the thumbnail buttons and second launches.
    pub fn new(hwnd: isize, tray: Option<TrayIcon>, remote: RemoteFn) -> Self {
        shell::init();
        *REMOTE.lock().unwrap_or_else(|e| e.into_inner()) = Some(remote);
        TRAY.with(|t| *t.borrow_mut() = tray);
        MAIN_HWND.store(hwnd, Ordering::Relaxed);
        // SAFETY: registering a message name; subclassing our own window on its thread.
        unsafe {
            TASKBAR_CREATED.store(
                RegisterWindowMessageW(wide("TaskbarButtonCreated").as_ptr()),
                Ordering::Relaxed,
            );
            let old = SetWindowLongPtrW(
                hwnd,
                GWLP_WNDPROC,
                main_proc as extern "system" fn(isize, u32, usize, isize) -> isize as usize
                    as isize,
            );
            MAIN_OLD_PROC.store(old, Ordering::Relaxed);
        }

        // SAFETY: plain Win32 query.
        let (small, big) = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CXICON)) };
        let make = |n: i32, slot: Option<usize>| -> isize {
            let n = n.clamp(16, 256);
            let bits = icon::dib(n as u32, slot);
            // SAFETY: `bits` is a complete icon image of `bits.len()` bytes.
            unsafe {
                CreateIconFromResourceEx(bits.as_ptr(), bits.len() as u32, 1, 0x0003_0000, n, n, 0)
            }
        };
        let icons = std::iter::once(None)
            .chain((0..icon::SLOTS).map(Some))
            .map(|s| [make(small, s), make(big, s)])
            .collect();
        let (tx, rx) = mpsc::channel::<String>();
        let _ = std::thread::Builder::new()
            .name("taskbar-title".into())
            .spawn(move || {
                while let Ok(mut t) = rx.recv() {
                    // only the latest title matters
                    while let Ok(newer) = rx.try_recv() {
                        t = newer;
                    }
                    let w = wide(&t);
                    // SAFETY: NUL-terminated UTF-16 string that outlives the call.
                    unsafe {
                        SetWindowTextW(hwnd, w.as_ptr());
                    }
                }
            });
        Taskbar {
            hwnd,
            icons,
            shown: Mutex::new(None),
            titles: Mutex::new(tx),
            control: create_control_window(),
        }
    }

    /// Show `status`. Cheap when unchanged; safe from any thread and never blocks on the UI.
    pub fn set(&self, status: Status) {
        let mut shown = self.shown.lock().unwrap_or_else(|e| e.into_inner());
        if shown.as_ref() == Some(&status) {
            return;
        }
        let old_slot = shown.as_ref().map(|s| s.slot);
        let old_title = shown.as_ref().map(|s| s.title.clone());
        if old_slot != Some(status.slot) {
            self.apply_icon(status.slot.map_or(0, |s| s + 1));
        }
        if old_title.as_ref() != Some(&status.title) {
            let _ = self
                .titles
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .send(status.title.clone());
        }
        if self.control != 0 {
            *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(status.clone());
            // SAFETY: posting a plain message to our own window; no pointers are passed.
            unsafe {
                PostMessageW(self.control, WM_STATUS, 0, 0);
            }
        }
        *shown = Some(status);
    }

    fn apply_icon(&self, idx: usize) {
        let [small, big] = self.icons[idx.min(self.icons.len() - 1)];
        if small == 0 || big == 0 {
            return;
        }
        // SAFETY: posting WM_SETICON with icon handles owned by (and outliving) this object.
        unsafe {
            PostMessageW(self.hwnd, WM_SETICON, ICON_SMALL, small);
            PostMessageW(self.hwnd, WM_SETICON, ICON_BIG, big);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(slot: f32, names: &[&str]) -> UiState {
        let mut st = UiState {
            connection: Connection::Connected,
            ..UiState::default()
        };
        st.values.insert("mic.slot".into(), slot);
        if !names.is_empty() {
            st.mic_presets = Some(MicPresets {
                slots: names.iter().map(|s| s.to_string()).collect(),
                user: vec![],
            });
        }
        st
    }

    #[test]
    fn title_names_the_preset_held_by_the_active_slot() {
        let s = status_of(&connected(1.0, &["Voice", "Podcast", "(empty)", "x"]));
        assert_eq!(s.slot, Some(1));
        assert_eq!(s.title, "Revelator Control \u{2013} 2: Podcast");
        assert_eq!(s.names[2], None);
        let s = status_of(&connected(2.0, &["Voice", "Podcast", "(empty)", "x"]));
        assert_eq!(s.title, "Revelator Control \u{2013} Slot 3");
        let s = status_of(&connected(0.0, &[]));
        assert_eq!(s.title, "Revelator Control \u{2013} Slot 1");
    }

    #[test]
    fn plain_while_disconnected() {
        let mut st = connected(3.0, &["a", "b", "c", "d"]);
        st.values.insert("mic.mute".into(), 1.0);
        st.connection = Connection::Disconnected;
        let s = status_of(&st);
        assert_eq!((s.slot, s.muted, s.title.as_str()), (None, false, TITLE));
    }

    #[test]
    fn actions_round_trip_through_args_and_codes() {
        for a in [Action::Slot(0), Action::Slot(3), Action::ToggleMute] {
            let args: Vec<String> = a.args().split(' ').map(String::from).collect();
            assert_eq!(Action::from_args(&args), Some(a));
            assert_eq!(Action::from_code(a.code()), Some(a));
        }
        assert_eq!(Action::from_args(&["--slot".into(), "5".into()]), None);
        assert_eq!(Action::from_code(0), None);
    }
}

//! Windows shell integration, over raw COM (no extra dependencies):
//! - jump list tasks (right-click on the taskbar button, or on a Start menu entry): switch
//!   slot / toggle mute. They are in-memory shell links run as `revelator-gui.exe --slot 2`
//!   etc.; Windows keeps the list itself, the app writes no files;
//! - thumbnail toolbar buttons (under the window preview when hovering the taskbar button).
//!
//! Everything here runs on the main (UI) thread, which owns the window and the COM objects.

use crate::icon::{self, Glyph};
use crate::model::Action;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;

type Obj = *mut c_void;

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);

const CLSID_TASKBAR_LIST: Guid = Guid(
    0x56FD_F344,
    0xFD6D,
    0x11D0,
    [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90],
);
const IID_ITASKBAR_LIST3: Guid = Guid(
    0xEA1A_FB91,
    0x9E28,
    0x4B86,
    [0x90, 0xE9, 0x9E, 0x9F, 0x8A, 0x5E, 0xEF, 0xAF],
);
const CLSID_DESTINATION_LIST: Guid = Guid(
    0x77F1_0CF0,
    0x3DB5,
    0x4966,
    [0xB5, 0x20, 0xB7, 0xC5, 0x4F, 0xD3, 0x5E, 0xD6],
);
const IID_ICUSTOM_DESTINATION_LIST: Guid = Guid(
    0x6332_DEBF,
    0x87B5,
    0x4670,
    [0x90, 0xC0, 0x5E, 0x57, 0xB4, 0x08, 0xA4, 0x9E],
);
const CLSID_ENUMERABLE_OBJECT_COLLECTION: Guid = Guid(
    0x2D34_68C1,
    0x36A7,
    0x43B6,
    [0xAC, 0x24, 0xD3, 0xF0, 0x2F, 0xD9, 0x60, 0x7A],
);
const IID_IOBJECT_COLLECTION: Guid = Guid(
    0x5632_B1A4,
    0xE38A,
    0x400A,
    [0x92, 0x8A, 0xD4, 0xCD, 0x63, 0x23, 0x02, 0x95],
);
const IID_IOBJECT_ARRAY: Guid = Guid(
    0x92CA_9DCD,
    0x5622,
    0x4BBA,
    [0xA8, 0x05, 0x5E, 0x9F, 0x54, 0x1B, 0xD8, 0xC9],
);
const CLSID_SHELL_LINK: Guid = Guid(
    0x0002_1401,
    0x0000,
    0x0000,
    [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
);
const IID_ISHELL_LINK_W: Guid = Guid(
    0x0002_14F9,
    0x0000,
    0x0000,
    [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
);
const IID_IPROPERTY_STORE: Guid = Guid(
    0x886D_8EEB,
    0x8CF2,
    0x4446,
    [0x8D, 0x02, 0xCD, 0xBA, 0x1D, 0xBD, 0xCF, 0x99],
);

#[repr(C)]
struct PropertyKey {
    fmtid: Guid,
    pid: u32,
}
/// PKEY_Title: the text a jump list task shows.
const PKEY_TITLE: PropertyKey = PropertyKey {
    fmtid: Guid(
        0xF29F_85E0,
        0x4FF9,
        0x1068,
        [0xAB, 0x91, 0x08, 0x00, 0x2B, 0x27, 0xB3, 0xD9],
    ),
    pid: 2,
};
#[repr(C)]
struct PropVariant {
    vt: u16,
    reserved: [u16; 3],
    ptr: *const u16,
    _pad: usize,
}
const VT_LPWSTR: u16 = 31;

#[repr(C)]
struct ThumbButton {
    mask: u32,
    id: u32,
    bitmap: u32,
    icon: isize,
    tip: [u16; 260],
    flags: u32,
}
const THB_ICON: u32 = 0x2;
const THB_TOOLTIP: u32 = 0x4;
const THB_FLAGS: u32 = 0x8;
const THBF_ENABLED: u32 = 0x0;
const THBF_DISABLED: u32 = 0x1;
/// HIWORD of WM_COMMAND's wParam for a thumbnail button click.
pub const THBN_CLICKED: usize = 0x1800;

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *const c_void, flags: u32) -> i32;
    fn CoCreateInstance(
        clsid: *const Guid,
        outer: Obj,
        ctx: u32,
        iid: *const Guid,
        out: *mut Obj,
    ) -> i32;
}
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
    fn GetSystemMetrics(index: i32) -> i32;
}
const CLSCTX_INPROC_SERVER: u32 = 1;
const COINIT_APARTMENTTHREADED: u32 = 2;
const SM_CXSMICON: i32 = 49;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Method `i` of a COM object's vtable.
unsafe fn method(o: Obj, i: usize) -> *const c_void {
    let vtbl = *(o as *const *const *const c_void);
    *vtbl.add(i)
}

unsafe fn release(o: Obj) {
    if !o.is_null() {
        let f: extern "system" fn(Obj) -> u32 = std::mem::transmute(method(o, 2));
        f(o);
    }
}

unsafe fn create(clsid: &Guid, iid: &Guid) -> Option<Obj> {
    let mut o: Obj = std::ptr::null_mut();
    let hr = CoCreateInstance(
        clsid,
        std::ptr::null_mut(),
        CLSCTX_INPROC_SERVER,
        iid,
        &mut o,
    );
    (hr >= 0 && !o.is_null()).then_some(o)
}

unsafe fn query(o: Obj, iid: &Guid) -> Option<Obj> {
    let f: extern "system" fn(Obj, *const Guid, *mut Obj) -> i32 =
        std::mem::transmute(method(o, 0));
    let mut out: Obj = std::ptr::null_mut();
    (f(o, iid, &mut out) >= 0 && !out.is_null()).then_some(out)
}

/// Initialise COM on this (the main) thread. Harmless if already done.
pub fn init() {
    // SAFETY: plain COM call; a "changed mode" refusal leaves the existing apartment in place.
    unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED);
    }
}

// ---------------------------------------------------------------- jump list

/// One jump list task: an in-memory shell link to this .exe with `args`.
unsafe fn task_link(
    exe: &[u16],
    args: &str,
    title: &str,
    tip: &str,
    icon_index: i32,
) -> Option<Obj> {
    let link = create(&CLSID_SHELL_LINK, &IID_ISHELL_LINK_W)?;
    type SetStr = extern "system" fn(Obj, *const u16) -> i32;
    let set_path: SetStr = std::mem::transmute(method(link, 20));
    let set_args: SetStr = std::mem::transmute(method(link, 11));
    let set_desc: SetStr = std::mem::transmute(method(link, 7));
    let set_icon: extern "system" fn(Obj, *const u16, i32) -> i32 =
        std::mem::transmute(method(link, 17));
    set_path(link, exe.as_ptr());
    set_args(link, wide(args).as_ptr());
    set_desc(link, wide(tip).as_ptr());
    set_icon(link, exe.as_ptr(), icon_index);
    let Some(props) = query(link, &IID_IPROPERTY_STORE) else {
        release(link);
        return None;
    };
    let title = wide(title);
    let value = PropVariant {
        vt: VT_LPWSTR,
        reserved: [0; 3],
        ptr: title.as_ptr(),
        _pad: 0,
    };
    let set_value: extern "system" fn(Obj, *const PropertyKey, *const PropVariant) -> i32 =
        std::mem::transmute(method(props, 6));
    let commit: extern "system" fn(Obj) -> i32 = std::mem::transmute(method(props, 7));
    let ok = set_value(props, &PKEY_TITLE, &value) >= 0 && commit(props) >= 0;
    release(props);
    if ok {
        Some(link)
    } else {
        release(link);
        None
    }
}

/// (Re)build the jump list tasks; `names` are what the slots hold (`None`: unknown/empty).
pub fn set_jump_list(names: &[Option<String>]) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let exe = wide(&exe.to_string_lossy());
    // SAFETY: COM calls with the documented vtable layouts; every object is released.
    unsafe {
        let Some(list) = create(&CLSID_DESTINATION_LIST, &IID_ICUSTOM_DESTINATION_LIST) else {
            return;
        };
        let begin: extern "system" fn(Obj, *mut u32, *const Guid, *mut Obj) -> i32 =
            std::mem::transmute(method(list, 4));
        let add_tasks: extern "system" fn(Obj, Obj) -> i32 = std::mem::transmute(method(list, 7));
        let commit: extern "system" fn(Obj) -> i32 = std::mem::transmute(method(list, 8));
        let abort: extern "system" fn(Obj) -> i32 = std::mem::transmute(method(list, 11));
        let mut min_slots = 0u32;
        let mut removed: Obj = std::ptr::null_mut();
        if begin(list, &mut min_slots, &IID_IOBJECT_ARRAY, &mut removed) < 0 {
            release(list);
            return;
        }
        release(removed);
        let Some(tasks) = create(&CLSID_ENUMERABLE_OBJECT_COLLECTION, &IID_IOBJECT_COLLECTION)
        else {
            abort(list);
            release(list);
            return;
        };
        let add: extern "system" fn(Obj, Obj) -> i32 = std::mem::transmute(method(tasks, 5));
        for i in 0..icon::SLOTS {
            let title = match names.get(i).and_then(|n| n.as_deref()) {
                Some(n) => format!("Slot {}: {n}", i + 1),
                None => format!("Slot {}", i + 1),
            };
            let tip = format!("Switch the mic to preset slot {}", i + 1);
            // icon groups 2..=5 of the .exe (see build.rs) are the slot glyphs
            if let Some(l) = task_link(&exe, &Action::Slot(i).args(), &title, &tip, i as i32 + 1) {
                add(tasks, l);
                release(l);
            }
        }
        if let Some(l) = task_link(
            &exe,
            &Action::ToggleMute.args(),
            "Mute / unmute the mic",
            "Toggle the microphone's mute",
            0,
        ) {
            add(tasks, l);
            release(l);
        }
        // IObjectCollection derives from IObjectArray: the same pointer serves as one.
        if add_tasks(list, tasks) >= 0 {
            commit(list);
        } else {
            abort(list);
        }
        release(tasks);
        release(list);
    }
}

// ------------------------------------------------------- thumbnail toolbar

thread_local! {
    static TASKBAR_LIST: Cell<Obj> = const { Cell::new(std::ptr::null_mut()) };
    /// Buttons can be added once per taskbar button; afterwards they are only updated.
    static BUTTONS_ADDED: Cell<bool> = const { Cell::new(false) };
    /// Glyph icons, created once (the taskbar keeps referring to them).
    static GLYPHS: RefCell<HashMap<Glyph, isize>> = RefCell::new(HashMap::new());
}

fn taskbar_list() -> Option<Obj> {
    TASKBAR_LIST.with(|t| {
        if t.get().is_null() {
            // SAFETY: COM creation + ITaskbarList::HrInit (vtable slot 3).
            unsafe {
                let o = create(&CLSID_TASKBAR_LIST, &IID_ITASKBAR_LIST3)?;
                let init: extern "system" fn(Obj) -> i32 = std::mem::transmute(method(o, 3));
                if init(o) < 0 {
                    release(o);
                    return None;
                }
                t.set(o);
            }
        }
        Some(t.get())
    })
}

fn glyph_icon(g: Glyph) -> isize {
    GLYPHS.with(|m| {
        *m.borrow_mut().entry(g).or_insert_with(|| {
            // SAFETY: plain Win32 query.
            let n = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64);
            let bits = icon::dib_from_rgba(n as u32, &icon::glyph_rgba(n as u32, g));
            // SAFETY: `bits` is a complete icon image of `bits.len()` bytes.
            unsafe {
                CreateIconFromResourceEx(bits.as_ptr(), bits.len() as u32, 1, 0x0003_0000, n, n, 0)
            }
        })
    })
}

/// The taskbar (re)created the window's button: buttons must be added again.
pub fn taskbar_button_created() {
    BUTTONS_ADDED.with(|a| a.set(false));
}

/// What the thumbnail toolbar shows.
pub struct Thumbs<'a> {
    /// Active slot; `None` when no mic is connected (buttons disabled).
    pub slot: Option<usize>,
    pub muted: bool,
    pub names: &'a [Option<String>],
}

/// Show `t` in the thumbnail toolbar of `hwnd`: [mute] [1] [2] [3] [4]. Button ids are the
/// [`Action::code`]s, delivered back as WM_COMMAND with [`THBN_CLICKED`].
pub fn update_thumb_buttons(hwnd: isize, t: &Thumbs) {
    let Some(list) = taskbar_list() else {
        return;
    };
    let connected = t.slot.is_some();
    let button = |a: Action, g: Glyph, tip: String| {
        let mut b = ThumbButton {
            mask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
            id: a.code() as u32,
            bitmap: 0,
            icon: glyph_icon(g),
            tip: [0; 260],
            flags: if connected {
                THBF_ENABLED
            } else {
                THBF_DISABLED
            },
        };
        for (d, c) in b.tip.iter_mut().zip(tip.encode_utf16().take(259)) {
            *d = c;
        }
        b
    };
    let mut buttons = vec![button(
        Action::ToggleMute,
        Glyph::Mic { muted: t.muted },
        if !connected {
            "Mute (no mic connected)".into()
        } else if t.muted {
            "Unmute the mic".into()
        } else {
            "Mute the mic".into()
        },
    )];
    for i in 0..icon::SLOTS {
        let active = t.slot == Some(i);
        let name = t.names.get(i).and_then(|n| n.as_deref());
        let mut tip = match name {
            Some(n) => format!("Slot {}: {n}", i + 1),
            None => format!("Slot {}", i + 1),
        };
        if active {
            tip.push_str(" (active)");
        } else if !connected {
            tip.push_str(" (no mic connected)");
        }
        buttons.push(button(
            Action::Slot(i),
            Glyph::Slot { slot: i, active },
            tip,
        ));
    }
    // SAFETY: ITaskbarList3::ThumbBarAddButtons / ThumbBarUpdateButtons (vtable slots 15/16)
    // with a valid array of `buttons.len()` THUMBBUTTONs.
    unsafe {
        type Thumb = extern "system" fn(Obj, isize, u32, *const ThumbButton) -> i32;
        let add: Thumb = std::mem::transmute(method(list, 15));
        let update: Thumb = std::mem::transmute(method(list, 16));
        let n = buttons.len() as u32;
        let added = BUTTONS_ADDED.with(|a| a.get());
        if !added && add(list, hwnd, n, buttons.as_ptr()) >= 0 {
            BUTTONS_ADDED.with(|a| a.set(true));
        } else if update(list, hwnd, n, buttons.as_ptr()) >= 0 {
            BUTTONS_ADDED.with(|a| a.set(true));
        }
    }
}

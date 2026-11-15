//! "Start on boot": a per-user Run entry (`HKCU\...\CurrentVersion\Run`), which is what
//! Task Manager's "Startup apps" page lists. Task Manager's own enable/disable switch is kept
//! in `Explorer\StartupApproved\Run`; it is honoured when reading and reset when enabling,
//! so toggling in either place gives the same result.

/// Command-line flag used by the Run entry: start hidden in the tray.
pub const MINIMIZED_FLAG: &str = "--minimized";

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const NAME: &str = "Revelator Control";

const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
const KEY_QUERY_VALUE: u32 = 0x0001;
const KEY_SET_VALUE: u32 = 0x0002;
const REG_SZ: u32 = 1;
const REG_BINARY: u32 = 3;
const ERROR_SUCCESS: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(key: isize, sub: *const u16, opts: u32, sam: u32, out: *mut isize) -> i32;
    fn RegCreateKeyExW(
        key: isize,
        sub: *const u16,
        reserved: u32,
        class: *const u16,
        opts: u32,
        sam: u32,
        sec: *const u8,
        out: *mut isize,
        disp: *mut u32,
    ) -> i32;
    fn RegQueryValueExW(
        key: isize,
        name: *const u16,
        reserved: *mut u32,
        ty: *mut u32,
        data: *mut u8,
        len: *mut u32,
    ) -> i32;
    fn RegSetValueExW(
        key: isize,
        name: *const u16,
        reserved: u32,
        ty: u32,
        data: *const u8,
        len: u32,
    ) -> i32;
    fn RegDeleteValueW(key: isize, name: *const u16) -> i32;
    fn RegCloseKey(key: isize) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// An open HKCU key, closed on drop.
struct Key(isize);

impl Key {
    fn open(sub: &str, sam: u32) -> Option<Key> {
        let mut h = 0isize;
        // SAFETY: NUL-terminated path; `h` is written only on success.
        let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide(sub).as_ptr(), 0, sam, &mut h) };
        (r == ERROR_SUCCESS).then_some(Key(h))
    }

    fn create(sub: &str) -> Result<Key, String> {
        let mut h = 0isize;
        // SAFETY: as above; optional pointers are null.
        let r = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(sub).as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                std::ptr::null(),
                &mut h,
                std::ptr::null_mut(),
            )
        };
        if r == ERROR_SUCCESS {
            Ok(Key(h))
        } else {
            Err(format!("cannot open HKCU\\{sub} (error {r})"))
        }
    }

    fn get(&self, name: &str) -> Option<Vec<u8>> {
        let name = wide(name);
        let mut len = 0u32;
        // SAFETY: size query with a null buffer, then a read into a buffer of that size.
        unsafe {
            let r = RegQueryValueExW(
                self.0,
                name.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut len,
            );
            if r != ERROR_SUCCESS {
                return None;
            }
            let mut buf = vec![0u8; len as usize];
            let r = RegQueryValueExW(
                self.0,
                name.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                &mut len,
            );
            buf.truncate(len as usize);
            (r == ERROR_SUCCESS).then_some(buf)
        }
    }

    fn set(&self, name: &str, ty: u32, data: &[u8]) -> Result<(), String> {
        // SAFETY: `data` is valid for `data.len()` bytes.
        let r = unsafe {
            RegSetValueExW(
                self.0,
                wide(name).as_ptr(),
                0,
                ty,
                data.as_ptr(),
                data.len() as u32,
            )
        };
        if r == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("cannot write the startup entry (error {r})"))
        }
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        // SAFETY: NUL-terminated value name.
        let r = unsafe { RegDeleteValueW(self.0, wide(name).as_ptr()) };
        if r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!("cannot remove the startup entry (error {r})"))
        }
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful open/create.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

/// The command the Run entry starts: this executable, hidden in the tray.
fn command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate the app: {e}"))?;
    Ok(format!("\"{}\" {MINIMIZED_FLAG}", exe.display()))
}

/// Start on boot is on: the Run entry exists and Task Manager has not disabled it.
pub fn is_enabled() -> bool {
    let present = Key::open(RUN, KEY_QUERY_VALUE).is_some_and(|k| k.get(NAME).is_some());
    // StartupApproved: first byte 0x02 = enabled, 0x03 = disabled (odd = disabled).
    let disabled = Key::open(APPROVED, KEY_QUERY_VALUE)
        .and_then(|k| k.get(NAME))
        .is_some_and(|v| v.first().is_some_and(|b| b & 1 != 0));
    present && !disabled
}

fn reg_sz(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// If a Run entry exists but starts another executable (the app was rebuilt, updated or
/// moved), point it at this one. Task Manager's enabled/disabled state is left as it is.
pub fn refresh() -> Result<(), String> {
    let Some(run) = Key::open(RUN, KEY_QUERY_VALUE | KEY_SET_VALUE) else {
        return Ok(());
    };
    let Some(old) = run.get(NAME) else {
        return Ok(());
    };
    let cmd = command()?;
    let old: Vec<u16> = old
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .take_while(|&c| c != 0)
        .collect();
    // Paths are case-insensitive on Windows.
    if !String::from_utf16_lossy(&old).eq_ignore_ascii_case(&cmd) {
        run.set(NAME, REG_SZ, &reg_sz(&cmd))?;
    }
    Ok(())
}

/// Turn start on boot on (pointing at this executable) or off.
pub fn set(enabled: bool) -> Result<(), String> {
    let run = Key::create(RUN)?;
    if enabled {
        run.set(NAME, REG_SZ, &reg_sz(&command()?))?;
        // Mark it enabled for Task Manager (clears a previous "Disabled" there).
        let mut approved = [0u8; 12];
        approved[0] = 0x02;
        Key::create(APPROVED)?.set(NAME, REG_BINARY, &approved)?;
    } else {
        run.delete(NAME)?;
        if let Some(k) = Key::open(APPROVED, KEY_SET_VALUE) {
            k.delete(NAME)?;
        }
    }
    Ok(())
}

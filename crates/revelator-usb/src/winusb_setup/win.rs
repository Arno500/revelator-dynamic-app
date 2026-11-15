//! Win32 implementation of the WinUSB interface-GUID setup. Windows only.

use std::mem::size_of;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{
    check_node, check_register, check_unregister, is_vendor_node_id, NodeInfo, SetupError, OUR_GUID,
};

const GUIDS_VALUE: &str = "DeviceInterfaceGUIDs";
const GUID_SINGLE: &str = "DeviceInterfaceGUID";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn os_err(what: &str) -> SetupError {
    // SAFETY: trivial thread-local read.
    SetupError::Os(format!("{what} (Win32 error {})", unsafe {
        GetLastError()
    }))
}

/// Whether the current process token is elevated (running as Administrator).
pub fn is_elevated() -> bool {
    // SAFETY: standard token query; the handle is closed before returning.
    unsafe {
        let mut tok: HANDLE = std::mem::zeroed();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            tok,
            TokenElevation,
            &mut e as *mut _ as *mut _,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        );
        CloseHandle(tok);
        ok != 0 && e.TokenIsElevated != 0
    }
}

/// RAII device-info set containing a single node.
struct DevSet {
    set: HDEVINFO,
    data: SP_DEVINFO_DATA,
}

impl Drop for DevSet {
    fn drop(&mut self) {
        // SAFETY: `set` came from SetupDi* and is destroyed once.
        unsafe { SetupDiDestroyDeviceInfoList(self.set) };
    }
}

fn new_devinfo_data() -> SP_DEVINFO_DATA {
    // SAFETY: plain-old-data struct, zero is a valid initial value.
    let mut d: SP_DEVINFO_DATA = unsafe { std::mem::zeroed() };
    d.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
    d
}

/// Open exactly one present node by instance id. Refuses anything that is not the vendor node.
fn open_node(instance_id: &str) -> Result<DevSet, SetupError> {
    if !is_vendor_node_id(instance_id) {
        return Err(SetupError::WrongNode(instance_id.to_owned()));
    }
    // SAFETY: FFI; pointers are valid for the call, handle is owned by DevSet.
    unsafe {
        let set = SetupDiCreateDeviceInfoList(null(), null_mut());
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return Err(os_err("SetupDiCreateDeviceInfoList failed"));
        }
        let mut ds = DevSet {
            set,
            data: new_devinfo_data(),
        };
        let id = wide(instance_id);
        if SetupDiOpenDeviceInfoW(ds.set, id.as_ptr(), null_mut(), 0, &mut ds.data) == 0 {
            return Err(os_err("SetupDiOpenDeviceInfoW failed (node not present?)"));
        }
        Ok(ds)
    }
}

fn read_service(ds: &DevSet) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut ty = 0u32;
    let mut req = 0u32;
    // SAFETY: buffer size passed in bytes.
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            ds.set,
            &ds.data,
            SPDRP_SERVICE,
            &mut ty,
            buf.as_mut_ptr() as *mut u8,
            (buf.len() * 2) as u32,
            &mut req,
        )
    };
    if ok == 0 {
        return None;
    }
    let s = from_wide(&buf);
    (!s.is_empty()).then_some(s)
}

/// Open the node's `Device Parameters` key. `None` if it cannot be opened with `access`.
fn open_key(ds: &DevSet, access: u32) -> Option<HKEY> {
    // SAFETY: FFI call; returned key must be closed by the caller.
    let k =
        unsafe { SetupDiOpenDevRegKey(ds.set, &ds.data, DICS_FLAG_GLOBAL, 0, DIREG_DEV, access) };
    if k as isize == -1 || k.is_null() {
        None
    } else {
        Some(k)
    }
}

fn query_value(key: HKEY, name: &str) -> Option<(u32, Vec<u8>)> {
    let n = wide(name);
    let mut ty = 0u32;
    let mut len = 0u32;
    // SAFETY: two-step size query then read.
    unsafe {
        if RegQueryValueExW(key, n.as_ptr(), null(), &mut ty, null_mut(), &mut len) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = vec![0u8; len as usize + 2];
        let mut len2 = len + 2;
        if RegQueryValueExW(
            key,
            n.as_ptr(),
            null(),
            &mut ty,
            buf.as_mut_ptr(),
            &mut len2,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        buf.truncate(len2 as usize);
        Some((ty, buf))
    }
}

fn utf16_from_bytes(b: &[u8]) -> Vec<u16> {
    b.as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect()
}

fn read_node(ds: &DevSet, instance_id: &str) -> NodeInfo {
    let mut node = NodeInfo {
        instance_id: instance_id.to_owned(),
        service: read_service(ds),
        guids: vec![],
        single_guid: None,
    };
    if let Some(k) = open_key(ds, KEY_QUERY_VALUE) {
        if let Some((ty, b)) = query_value(k, GUIDS_VALUE) {
            let w = utf16_from_bytes(&b);
            if ty == REG_MULTI_SZ || ty == REG_SZ {
                node.guids = w
                    .split(|&c| c == 0)
                    .filter(|s| !s.is_empty())
                    .map(String::from_utf16_lossy)
                    .collect();
            }
            if node.guids.is_empty() {
                // Value exists but is empty/unparsable: treat as foreign so we never overwrite it.
                node.guids = vec!["<unreadable value>".into()];
            }
        }
        if let Some((_, b)) = query_value(k, GUID_SINGLE) {
            let s = from_wide(&utf16_from_bytes(&b));
            node.single_guid = Some(if s.is_empty() { "<empty>".into() } else { s });
        }
        // SAFETY: key opened above.
        unsafe { RegCloseKey(k) };
    }
    node
}

/// Find the vendor interface node (MI_05 of a supported Dynamic PID) among present USB devices.
pub fn find_vendor_node() -> Result<NodeInfo, SetupError> {
    let ids = enumerate_vendor_ids()?;
    let id = ids.into_iter().next().ok_or(SetupError::NodeNotFound)?;
    let ds = open_node(&id)?;
    Ok(read_node(&ds, &id))
}

fn enumerate_vendor_ids() -> Result<Vec<String>, SetupError> {
    let usb = wide("USB");
    // SAFETY: FFI enumeration; set destroyed at end.
    unsafe {
        let set = SetupDiGetClassDevsW(
            null(),
            usb.as_ptr(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        );
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return Err(os_err("SetupDiGetClassDevsW failed"));
        }
        let mut out = vec![];
        let mut i = 0;
        loop {
            let mut d = new_devinfo_data();
            if SetupDiEnumDeviceInfo(set, i, &mut d) == 0 {
                break;
            }
            i += 1;
            let mut buf = [0u16; 512];
            if SetupDiGetDeviceInstanceIdW(set, &d, buf.as_mut_ptr(), buf.len() as u32, null_mut())
                != 0
            {
                let id = from_wide(&buf);
                if is_vendor_node_id(&id) {
                    out.push(id);
                }
            }
        }
        SetupDiDestroyDeviceInfoList(set);
        Ok(out)
    }
}

/// Restart only this device node: `pnputil /restart-device`, falling back to DIF_PROPERTYCHANGE.
fn restart_node(instance_id: &str) -> Result<(), SetupError> {
    if !is_vendor_node_id(instance_id) {
        return Err(SetupError::WrongNode(instance_id.to_owned()));
    }
    let r = std::process::Command::new("pnputil")
        .args(["/restart-device", instance_id])
        .output();
    if matches!(&r, Ok(o) if o.status.success()) {
        return Ok(());
    }
    restart_via_setupapi(instance_id)
}

fn restart_via_setupapi(instance_id: &str) -> Result<(), SetupError> {
    let ds = open_node(instance_id)?;
    // SAFETY: POD params struct passed with correct sizes.
    unsafe {
        let mut p: SP_PROPCHANGE_PARAMS = std::mem::zeroed();
        p.ClassInstallHeader.cbSize = size_of::<SP_CLASSINSTALL_HEADER>() as u32;
        p.ClassInstallHeader.InstallFunction = DIF_PROPERTYCHANGE;
        p.StateChange = DICS_PROPCHANGE;
        p.Scope = DICS_FLAG_GLOBAL;
        p.HwProfile = 0;
        if SetupDiSetClassInstallParamsW(
            ds.set,
            &ds.data,
            &p.ClassInstallHeader,
            size_of::<SP_PROPCHANGE_PARAMS>() as u32,
        ) == 0
        {
            return Err(os_err("SetupDiSetClassInstallParamsW failed"));
        }
        if SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, ds.set, &ds.data) == 0 {
            return Err(os_err("restarting the device node failed"));
        }
    }
    Ok(())
}

/// Re-read the node and run the safety checks fresh (guards against stale `NodeInfo`).
fn revalidate(node: &NodeInfo) -> Result<(DevSet, NodeInfo), SetupError> {
    if !is_elevated() {
        return Err(SetupError::NotElevated);
    }
    let ds = open_node(&node.instance_id)?;
    let fresh = read_node(&ds, &node.instance_id);
    check_node(&fresh)?;
    Ok((ds, fresh))
}

/// Write `DeviceInterfaceGUIDs = [OUR_GUID]` on the vendor node, then restart that node only.
pub fn register_guid(node: &NodeInfo) -> Result<(), SetupError> {
    let (ds, fresh) = revalidate(node)?;
    if !check_register(&fresh)? {
        return Ok(()); // already ours
    }
    let key = open_key(&ds, KEY_SET_VALUE | KEY_QUERY_VALUE)
        .ok_or_else(|| os_err("could not open the node's device registry key for writing"))?;
    let mut data: Vec<u16> = OUR_GUID.encode_utf16().collect();
    data.extend([0, 0]); // terminator + list terminator
    let name = wide(GUIDS_VALUE);
    // SAFETY: data buffer valid for len bytes.
    let rc = unsafe {
        let rc = RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_MULTI_SZ,
            data.as_ptr() as *const u8,
            (data.len() * 2) as u32,
        );
        RegCloseKey(key);
        rc
    };
    if rc != ERROR_SUCCESS {
        return Err(SetupError::Os(format!(
            "writing {GUIDS_VALUE} failed (Win32 error {rc})"
        )));
    }
    drop(ds);
    restart_node(&fresh.instance_id)
}

/// Delete our `DeviceInterfaceGUIDs` value (only if it is exactly ours), then restart that node only.
pub fn unregister_guid(node: &NodeInfo) -> Result<(), SetupError> {
    let (ds, fresh) = revalidate(node)?;
    if !check_unregister(&fresh)? {
        return Ok(()); // nothing to undo
    }
    let key = open_key(&ds, KEY_SET_VALUE | KEY_QUERY_VALUE)
        .ok_or_else(|| os_err("could not open the node's device registry key for writing"))?;
    let name = wide(GUIDS_VALUE);
    // SAFETY: deleting one named value from the opened key.
    let rc = unsafe {
        let rc = RegDeleteValueW(key, name.as_ptr());
        RegCloseKey(key);
        rc
    };
    if rc != ERROR_SUCCESS {
        return Err(SetupError::Os(format!(
            "deleting {GUIDS_VALUE} failed (Win32 error {rc})"
        )));
    }
    drop(ds);
    restart_node(&fresh.instance_id)
}

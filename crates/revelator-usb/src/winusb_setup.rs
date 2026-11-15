//! One-time, explicit, reversible "register a WinUSB interface GUID on the vendor interface node".
//!
//! Background: the PreSonus driver owns the audio (MI_00) and MIDI (MI_03) interfaces. The vendor
//! interface MI_05 is already bound to Microsoft's inbox WinUSB, but its device node has no
//! `DeviceInterfaceGUIDs` value, so no device interface exists and user-mode libraries (nusb)
//! cannot open it. This module writes that one registry value on that one node and restarts that
//! one node. It never touches any other node, driver or the parent device.
//!
//! The only registry change made is, under
//! `HKLM\SYSTEM\CurrentControlSet\Enum\<MI_05 instance id>\Device Parameters`:
//! `DeviceInterfaceGUIDs` (REG_MULTI_SZ) = [`OUR_GUID`]. Undo deletes exactly that value.
//!
//! Everything platform independent (safety checks, plan text, id parsing) lives at the top and is
//! unit-tested; the Win32 code is in the `win` submodule (Windows only).

use crate::discovery::PRESONUS_VID;

/// Fixed, randomly generated project GUID used as the WinUSB device interface GUID.
pub const OUR_GUID: &str = "{D1A8F3C2-47B6-4E19-9C5D-2F6E8A0B73E4}";

/// The only interface number this module may ever touch (vendor 'Revelator Dynamic CTRL').
pub const VENDOR_MI: u8 = 5;
/// Supported Dynamic PIDs (normal mode).
pub const SUPPORTED_PIDS: [u16; 2] = [0x0D0A, 0x0D04];

/// Snapshot of the vendor interface node, as read from the registry / PnP manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    /// e.g. `USB\VID_194F&PID_0D0A&MI_05\6&1234&0&0005`
    pub instance_id: String,
    /// Bound service name (e.g. `WINUSB`), if any.
    pub service: Option<String>,
    /// Existing REG_MULTI_SZ `DeviceInterfaceGUIDs` entries (empty if value absent).
    pub guids: Vec<String>,
    /// Existing REG_SZ `DeviceInterfaceGUID` (singular), if present.
    pub single_guid: Option<String>,
}

impl NodeInfo {
    pub fn has_any_guid(&self) -> bool {
        !self.guids.is_empty() || self.single_guid.is_some()
    }
    /// True when the plural value is exactly our GUID and no singular value exists.
    pub fn is_ours(&self) -> bool {
        self.single_guid.is_none() && self.guids.len() == 1 && guid_eq(&self.guids[0], OUR_GUID)
    }
    pub fn registry_path(&self) -> String {
        format!(
            "HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Enum\\{}\\Device Parameters",
            self.instance_id
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetupError {
    #[error("this setup step is only available on Windows")]
    Unsupported,
    #[error("administrator rights are required: run this from an elevated terminal (right-click the terminal, 'Run as administrator')")]
    NotElevated,
    #[error(
        "vendor interface node (MI_05 of a Revelator Dynamic) not found; is the mic plugged in?"
    )]
    NodeNotFound,
    #[error("refusing: '{0}' is not MI_05 of VID 194F with a supported Revelator Dynamic PID (0D0A/0D04)")]
    WrongNode(String),
    #[error("refusing: node is bound to service '{0}', not WINUSB; this tool never touches nodes owned by another driver")]
    NotWinUsb(String),
    #[error("refusing: node has no bound service (expected WINUSB)")]
    NoService,
    #[error("refusing: the node already has a DeviceInterfaceGUID(s) value that is not ours ({0}); not overwriting it")]
    ForeignGuid(String),
    #[error("{0}")]
    Os(String),
}

pub fn guid_eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Parse `USB\VID_xxxx&PID_xxxx&MI_xx\...` into (vid, pid, mi).
pub fn parse_instance_id(id: &str) -> Option<(u16, u16, u8)> {
    let mut parts = id.split('\\');
    if !parts.next()?.eq_ignore_ascii_case("USB") {
        return None;
    }
    let hw = parts.next()?.to_ascii_uppercase();
    parts.next()?; // instance part must exist
    let mut it = hw.split('&');
    let vid = u16::from_str_radix(it.next()?.strip_prefix("VID_")?, 16).ok()?;
    let pid = u16::from_str_radix(it.next()?.strip_prefix("PID_")?, 16).ok()?;
    let mi = u8::from_str_radix(it.next()?.strip_prefix("MI_")?, 16).ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((vid, pid, mi))
}

/// True only for MI_05 of VID 194F with a supported Dynamic PID.
pub fn is_vendor_node_id(id: &str) -> bool {
    matches!(parse_instance_id(id), Some((v, p, m)) if v == PRESONUS_VID && SUPPORTED_PIDS.contains(&p) && m == VENDOR_MI)
}

/// Checks common to register and unregister: right node, bound to WINUSB.
pub fn check_node(n: &NodeInfo) -> Result<(), SetupError> {
    if !is_vendor_node_id(&n.instance_id) {
        return Err(SetupError::WrongNode(n.instance_id.clone()));
    }
    match n.service.as_deref() {
        None | Some("") => Err(SetupError::NoService),
        Some(s) if s.eq_ignore_ascii_case("WINUSB") => Ok(()),
        Some(s) => Err(SetupError::NotWinUsb(s.to_owned())),
    }
}

fn foreign_desc(n: &NodeInfo) -> String {
    let mut v: Vec<String> = n.guids.clone();
    if let Some(s) = &n.single_guid {
        v.push(s.clone());
    }
    v.join(", ")
}

/// Safety checks for [`register_guid`]. `Ok(true)` = write needed, `Ok(false)` = already ours.
pub fn check_register(n: &NodeInfo) -> Result<bool, SetupError> {
    check_node(n)?;
    if n.is_ours() {
        return Ok(false);
    }
    if n.has_any_guid() {
        return Err(SetupError::ForeignGuid(foreign_desc(n)));
    }
    Ok(true)
}

/// Safety checks for [`unregister_guid`]. `Ok(true)` = value exists and is ours (delete it),
/// `Ok(false)` = nothing to undo.
pub fn check_unregister(n: &NodeInfo) -> Result<bool, SetupError> {
    check_node(n)?;
    if n.is_ours() {
        return Ok(true);
    }
    if n.has_any_guid() {
        return Err(SetupError::ForeignGuid(foreign_desc(n)));
    }
    Ok(false)
}

/// Human-readable description of exactly what `setup` / `setup --undo` will do.
pub fn plan_text(n: &NodeInfo, undo: bool) -> String {
    let mut s = String::new();
    let action = if undo { "REMOVE" } else { "ADD" };
    s.push_str(&format!(
        "Plan: {action} a WinUSB device interface GUID on the Revelator's vendor interface (one-time setup{})\n\n",
        if undo { ", undo" } else { "" }
    ));
    s.push_str("What will change:\n");
    s.push_str(&format!("  registry key : {}\n", n.registry_path()));
    s.push_str(&format!(
        "  value        : DeviceInterfaceGUIDs (REG_MULTI_SZ) {} {}\n",
        if undo { "deleted, was" } else { "=" },
        OUR_GUID
    ));
    s.push_str(&format!(
        "  device node  : {} (the single node that will be restarted; service {})\n",
        n.instance_id,
        n.service.as_deref().unwrap_or("none")
    ));
    s.push_str("\nWhat is NOT touched:\n");
    s.push_str("  - the PreSonus audio (MI_00) and MIDI (MI_03) interfaces and the PaeDSPUSB driver / Universal Control\n");
    s.push_str("  - the DFU (firmware update) interface MI_06 and the parent USB device\n");
    s.push_str("  - the installed drivers; the node stays bound to Microsoft's WinUSB\n");
    s.push_str("  - no firmware, reset, or device command is sent\n");
    s.push_str("\nRequires: an elevated (Administrator) terminal.\n");
    if undo {
        s.push_str("To re-apply: revelator-cli setup --yes\n");
    } else {
        s.push_str("To undo:     revelator-cli setup --undo --yes\n");
    }
    s.push_str(&match (undo, check_state_note(n, undo)) {
        (_, Some(note)) => format!("\nCurrent state: {note}\n"),
        _ => String::new(),
    });
    s
}

fn check_state_note(n: &NodeInfo, undo: bool) -> Option<String> {
    let r = if undo {
        check_unregister(n)
    } else {
        check_register(n)
    };
    Some(match r {
        Ok(true) => "ready to proceed".into(),
        Ok(false) if undo => "nothing to undo (no DeviceInterfaceGUIDs value present)".into(),
        Ok(false) => "already set up (our GUID is registered); nothing to do".into(),
        Err(e) => format!("WILL REFUSE: {e}"),
    })
}

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::{find_vendor_node, is_elevated, register_guid, unregister_guid};

#[cfg(not(windows))]
pub fn find_vendor_node() -> Result<NodeInfo, SetupError> {
    Err(SetupError::Unsupported)
}
#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}
#[cfg(not(windows))]
pub fn register_guid(_: &NodeInfo) -> Result<(), SetupError> {
    Err(SetupError::Unsupported)
}
#[cfg(not(windows))]
pub fn unregister_guid(_: &NodeInfo) -> Result<(), SetupError> {
    Err(SetupError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "USB\\VID_194F&PID_0D0A&MI_05\\6&2A1B&0&0005";

    fn node() -> NodeInfo {
        NodeInfo {
            instance_id: ID.into(),
            service: Some("WINUSB".into()),
            guids: vec![],
            single_guid: None,
        }
    }

    #[test]
    fn parses_ids() {
        assert_eq!(parse_instance_id(ID), Some((0x194F, 0x0D0A, 5)));
        assert_eq!(parse_instance_id("USB\\VID_194F&PID_0D0A\\ABC"), None);
        assert_eq!(parse_instance_id("HID\\VID_194F&PID_0D0A&MI_05\\x"), None);
        assert!(is_vendor_node_id("usb\\vid_194f&pid_0d04&mi_05\\x"));
        for bad in [
            "USB\\VID_194F&PID_0D0A&MI_00\\x",
            "USB\\VID_194F&PID_0D0A&MI_03\\x",
            "USB\\VID_194F&PID_0D0A&MI_06\\x",
            "USB\\VID_194F&PID_0D0A\\x",
            "USB\\VID_194F&PID_0D06&MI_05\\x",
            "USB\\VID_1234&PID_0D0A&MI_05\\x",
            "USB\\VID_194F&PID_0D0A&MI_05",
        ] {
            assert!(!is_vendor_node_id(bad), "{bad}");
        }
    }

    #[test]
    fn register_happy_path_and_idempotent() {
        assert_eq!(check_register(&node()), Ok(true));
        let mut n = node();
        n.guids = vec![OUR_GUID.to_lowercase()];
        assert_eq!(check_register(&n), Ok(false));
    }

    #[test]
    fn refuses_wrong_nodes() {
        for mi in ["MI_00", "MI_03", "MI_06"] {
            let mut n = node();
            n.instance_id = format!("USB\\VID_194F&PID_0D0A&{mi}\\x");
            assert!(matches!(check_register(&n), Err(SetupError::WrongNode(_))));
            assert!(matches!(
                check_unregister(&n),
                Err(SetupError::WrongNode(_))
            ));
        }
        let mut parent = node();
        parent.instance_id = "USB\\VID_194F&PID_0D0A\\ABC".into();
        assert!(matches!(
            check_register(&parent),
            Err(SetupError::WrongNode(_))
        ));
    }

    #[test]
    fn refuses_other_drivers() {
        let mut n = node();
        n.service = Some("PaeDSPUSB".into());
        assert_eq!(
            check_register(&n),
            Err(SetupError::NotWinUsb("PaeDSPUSB".into()))
        );
        n.service = None;
        assert_eq!(check_register(&n), Err(SetupError::NoService));
        n.service = Some("winusb".into());
        assert_eq!(check_register(&n), Ok(true));
    }

    #[test]
    fn refuses_foreign_guids() {
        let mut n = node();
        n.guids = vec!["{11111111-1111-1111-1111-111111111111}".into()];
        assert!(matches!(
            check_register(&n),
            Err(SetupError::ForeignGuid(_))
        ));
        assert!(matches!(
            check_unregister(&n),
            Err(SetupError::ForeignGuid(_))
        ));
        let mut n = node();
        n.single_guid = Some(OUR_GUID.into());
        assert!(matches!(
            check_register(&n),
            Err(SetupError::ForeignGuid(_))
        ));
        let mut n = node();
        n.guids = vec![
            OUR_GUID.into(),
            "{22222222-2222-2222-2222-222222222222}".into(),
        ];
        assert!(matches!(
            check_register(&n),
            Err(SetupError::ForeignGuid(_))
        ));
    }

    #[test]
    fn unregister_logic() {
        assert_eq!(check_unregister(&node()), Ok(false));
        let mut n = node();
        n.guids = vec![OUR_GUID.into()];
        assert_eq!(check_unregister(&n), Ok(true));
    }

    #[test]
    fn plan_mentions_everything() {
        let p = plan_text(&node(), false);
        for s in [
            ID,
            "DeviceInterfaceGUIDs",
            OUR_GUID,
            "Device Parameters",
            "MI_00",
            "MI_03",
            "MI_06",
            "setup --undo --yes",
            "Administrator",
            "ready to proceed",
        ] {
            assert!(p.contains(s), "missing {s}: {p}");
        }
        let mut n = node();
        n.guids = vec![OUR_GUID.into()];
        let u = plan_text(&n, true);
        assert!(u.contains("REMOVE") && u.contains("setup --yes"));
        let mut bad = node();
        bad.service = Some("X".into());
        assert!(plan_text(&bad, false).contains("WILL REFUSE"));
    }

    #[cfg(not(windows))]
    #[test]
    fn stubs_unsupported() {
        assert_eq!(find_vendor_node(), Err(SetupError::Unsupported));
        assert_eq!(register_guid(&node()), Err(SetupError::Unsupported));
    }
}

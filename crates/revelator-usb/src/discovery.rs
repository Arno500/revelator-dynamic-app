//! Device discovery, classification and read-only descriptor reports.

use std::fmt;

use nusb::DeviceInfo;

use crate::select::{
    parse_config, select_vendor_interface, vendor_candidates, InterfaceDesc, VendorEndpoints,
};

pub const PRESONUS_VID: u16 = 0x194F;

/// Whether this app may talk to a given PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Revelator Dynamic in normal mode.
    Supported,
    /// Revelator Dynamic in "unsupported mode" (0D06/0D0B): reported but refused.
    UnsupportedMode,
    /// Another Revelator / io24 / io44 variant: not supported by this app.
    OtherUnsupported,
}

/// PID table from paedspusb.inf: (pid, name, support).
const KNOWN: &[(u16, &str, Support)] = &[
    (0x0D04, "Revelator Dynamic", Support::Supported),
    (0x0D0A, "Revelator Dynamic (with MIDI)", Support::Supported),
    (
        0x0D06,
        "Revelator Dynamic (unsupported mode)",
        Support::UnsupportedMode,
    ),
    (
        0x0D0B,
        "Revelator Dynamic (unsupported mode)",
        Support::UnsupportedMode,
    ),
    (0x0D01, "Revelator", Support::OtherUnsupported),
    (0x0D08, "Revelator", Support::OtherUnsupported),
    (
        0x0D03,
        "Revelator (unsupported mode)",
        Support::OtherUnsupported,
    ),
    (
        0x0D09,
        "Revelator (unsupported mode)",
        Support::OtherUnsupported,
    ),
    (0x0407, "Revelator io24", Support::OtherUnsupported),
    (0x0422, "Revelator io24", Support::OtherUnsupported),
    (
        0x0406,
        "Revelator io24 (unsupported mode)",
        Support::OtherUnsupported,
    ),
    (
        0x0423,
        "Revelator io24 (unsupported mode)",
        Support::OtherUnsupported,
    ),
    (0x041A, "Revelator io44", Support::OtherUnsupported),
    (0x0424, "Revelator io44", Support::OtherUnsupported),
    (
        0x0419,
        "Revelator io44 (unsupported mode)",
        Support::OtherUnsupported,
    ),
    (
        0x0425,
        "Revelator io44 (unsupported mode)",
        Support::OtherUnsupported,
    ),
];

/// Classify a VID/PID. `None` if it is not a known Revelator-family device.
pub fn classify(vid: u16, pid: u16) -> Option<(&'static str, Support)> {
    if vid != PRESONUS_VID {
        return None;
    }
    KNOWN.iter().find(|k| k.0 == pid).map(|k| (k.1, k.2))
}

/// A known Revelator-family device that is currently attached.
#[derive(Clone)]
pub struct FoundDevice {
    pub info: DeviceInfo,
    pub name: &'static str,
    pub support: Support,
}

/// Enumerate attached known Revelator-family devices.
pub fn find_devices() -> Result<Vec<FoundDevice>, String> {
    let list = nusb::list_devices().map_err(|e| format!("USB enumeration failed: {e}"))?;
    Ok(list
        .filter_map(|info| {
            classify(info.vendor_id(), info.product_id()).map(|(name, support)| FoundDevice {
                info,
                name,
                support,
            })
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct InterfaceSummary {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub string: Option<String>,
}

/// Everything we can learn about one PreSonus (VID 0x194F) device without writing to it.
#[derive(Debug, Clone)]
pub struct DeviceReport {
    pub vid: u16,
    pub pid: u16,
    pub name: Option<&'static str>,
    pub support: Option<Support>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub bus: u8,
    pub address: u8,
    pub speed: Option<String>,
    /// Windows: driver bound to the device as a whole (e.g. `usbccgp`, `WinUSB`).
    pub driver: Option<String>,
    pub instance_id: Option<String>,
    /// Pre-open interface summary (Windows: only for composite devices on usbccgp).
    pub interface_summaries: Vec<InterfaceSummary>,
    /// Full descriptor tree, or the reason it could not be read.
    pub descriptors: Result<Vec<InterfaceDesc>, String>,
    pub vendor_candidates: Vec<VendorEndpoints>,
    pub selected: Option<VendorEndpoints>,
}

/// Dump descriptors of every VID 0x194F device. Returns an empty list when none are attached.
pub fn describe_devices() -> Result<Vec<DeviceReport>, String> {
    let list = nusb::list_devices().map_err(|e| format!("USB enumeration failed: {e}"))?;
    Ok(list
        .filter(|i| i.vendor_id() == PRESONUS_VID)
        .map(|i| report_for(&i))
        .collect())
}

fn report_for(info: &DeviceInfo) -> DeviceReport {
    let cls = classify(info.vendor_id(), info.product_id());
    let descriptors = info
        .open()
        .map_err(|e| format!("could not open device ({e}); no usable driver bound?"))
        .and_then(|dev| {
            dev.active_configuration()
                .map(|c| parse_config(&c))
                .map_err(|e| format!("no active configuration: {e}"))
        });
    let (candidates, selected) = match &descriptors {
        Ok(d) => (vendor_candidates(d), select_vendor_interface(d)),
        Err(_) => (Vec::new(), None),
    };
    #[cfg(target_os = "windows")]
    let (driver, instance_id) = (
        info.driver().map(str::to_owned),
        Some(info.instance_id().to_string_lossy().into_owned()),
    );
    #[cfg(not(target_os = "windows"))]
    let (driver, instance_id) = (None, None);

    DeviceReport {
        vid: info.vendor_id(),
        pid: info.product_id(),
        name: cls.map(|c| c.0),
        support: cls.map(|c| c.1),
        manufacturer: info.manufacturer_string().map(str::to_owned),
        product: info.product_string().map(str::to_owned),
        serial: info.serial_number().map(str::to_owned),
        bus: info.bus_number(),
        address: info.device_address(),
        speed: info.speed().map(|s| format!("{s:?}")),
        driver,
        instance_id,
        interface_summaries: info
            .interfaces()
            .map(|i| InterfaceSummary {
                number: i.interface_number(),
                class: i.class(),
                subclass: i.subclass(),
                protocol: i.protocol(),
                string: i.interface_string().map(str::to_owned),
            })
            .collect(),
        descriptors,
        vendor_candidates: candidates,
        selected,
    }
}

impl fmt::Display for DeviceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Device {:04X}:{:04X} {}",
            self.vid,
            self.pid,
            self.name.unwrap_or("(unrecognised PreSonus device)")
        )?;
        match self.support {
            Some(Support::Supported) => writeln!(f, "  status: supported")?,
            Some(Support::UnsupportedMode) => writeln!(
                f,
                "  status: Revelator Dynamic in UNSUPPORTED MODE - this app refuses to talk to it"
            )?,
            Some(Support::OtherUnsupported) => {
                writeln!(f, "  status: other Revelator, unsupported by this app")?
            }
            None => writeln!(f, "  status: not a known Revelator")?,
        }
        writeln!(
            f,
            "  manufacturer: {}  product: {}  serial: {}",
            self.manufacturer.as_deref().unwrap_or("?"),
            self.product.as_deref().unwrap_or("?"),
            self.serial.as_deref().unwrap_or("?")
        )?;
        writeln!(
            f,
            "  bus {} address {} speed {}",
            self.bus,
            self.address,
            self.speed.as_deref().unwrap_or("?")
        )?;
        if let Some(i) = &self.instance_id {
            writeln!(f, "  instance id: {i}")?;
        }
        if cfg!(target_os = "windows") {
            writeln!(
                f,
                "  driver (device level): {}",
                self.driver.as_deref().unwrap_or("none reported")
            )?;
        }
        if !self.interface_summaries.is_empty() {
            writeln!(f, "  interfaces (pre-open summary):")?;
            for i in &self.interface_summaries {
                writeln!(
                    f,
                    "    #{} class {:02X}/{:02X}/{:02X} {}",
                    i.number,
                    i.class,
                    i.subclass,
                    i.protocol,
                    i.string.as_deref().unwrap_or("")
                )?;
            }
        }
        match &self.descriptors {
            Err(e) => writeln!(f, "  descriptors: unavailable: {e}")?,
            Ok(ifs) => {
                writeln!(f, "  configuration descriptor:")?;
                for i in ifs {
                    for a in &i.alts {
                        writeln!(
                            f,
                            "    interface {} alt {} class {:02X}/{:02X}/{:02X}{}",
                            i.number,
                            a.alt,
                            a.class,
                            a.subclass,
                            a.protocol,
                            if a.class == 0xFF {
                                " (vendor-specific)"
                            } else {
                                ""
                            }
                        )?;
                        for e in &a.endpoints {
                            writeln!(
                                f,
                                "      ep 0x{:02X} {} {:?} maxpkt {} interval {}",
                                e.address,
                                if e.is_in() { "IN " } else { "OUT" },
                                e.kind,
                                e.max_packet,
                                e.interval
                            )?;
                        }
                    }
                }
                match &self.selected {
                    Some(s) => writeln!(
                        f,
                        "  selected vendor bulk interface: {} alt {} (OUT 0x{:02X}, IN 0x{:02X}); {} candidate(s)",
                        s.interface,
                        s.alt,
                        s.out_ep,
                        s.in_ep,
                        self.vendor_candidates.len()
                    )?,
                    None => writeln!(f, "  no vendor-specific interface with a bulk IN+OUT pair found")?,
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_pids() {
        assert_eq!(classify(0x194F, 0x0D04).unwrap().1, Support::Supported);
        assert_eq!(classify(0x194F, 0x0D0A).unwrap().1, Support::Supported);
        assert_eq!(
            classify(0x194F, 0x0D06).unwrap().1,
            Support::UnsupportedMode
        );
        assert_eq!(
            classify(0x194F, 0x0D0B).unwrap().1,
            Support::UnsupportedMode
        );
        assert_eq!(
            classify(0x194F, 0x0422).unwrap().1,
            Support::OtherUnsupported
        );
        assert!(classify(0x194F, 0x9999).is_none());
        assert!(classify(0x1234, 0x0D04).is_none());
    }
}

//! `nusb`-based [`revelator_core::Transport`] for the PreSonus/Fender Revelator family.
//!
//! Safety: this crate only ever performs what the `Transport` trait exposes
//! (bulk OUT/IN frames and vendor IN control requests) plus the standard
//! open-time setup (claim interface, select alt setting, clear halt).
//! Descriptor dumps are read-only.

pub mod audio_rate;
mod discovery;
mod select;
mod transport;
pub mod winusb_setup;

pub use discovery::{
    classify, describe_devices, find_devices, DeviceReport, FoundDevice, InterfaceSummary, Support,
    PRESONUS_VID,
};
pub use select::{
    parse_config, select_vendor_interface, vendor_candidates, AltDesc, EndpointDesc, EpKind,
    InterfaceDesc, VendorEndpoints,
};
pub use transport::{NusbTransport, OpenError};

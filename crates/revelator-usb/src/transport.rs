//! `Transport` implementation over nusb.

use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use nusb::descriptors::Configuration;
use nusb::transfer::{ControlIn, ControlType, Recipient, RequestBuffer, TransferError};
use nusb::{DeviceInfo, Interface};
use revelator_core::{Transport, TransportError};

use crate::discovery::{find_devices, Support};
use crate::select::{parse_config, select_vendor_interface, VendorEndpoints};

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("no Revelator found")]
    NoDevice,
    #[error("{0} is in unsupported mode (PID 0D06/0D0B); switch the mic out of that mode first")]
    UnsupportedMode(String),
    #[error("{0} is not supported by this app (only Revelator Dynamic)")]
    UnsupportedModel(String),
    #[error("USB error: {0}")]
    Usb(String),
    #[error("device has no vendor-specific interface with a bulk IN+OUT pair")]
    NoVendorInterface,
    /// The vendor interface is bound to WinUSB but has no device interface GUID registered.
    #[error("the vendor interface has no WinUSB device interface registered; one-time setup needed: run `revelator-cli setup --yes` from an elevated terminal")]
    InterfaceNotRegistered,
    /// Another process holds the interface (WinUSB allows one opener) or access was denied.
    #[error("the vendor interface is busy or access was denied: close Universal Control and stop the 'PreSonus Hardware Access Service', then retry ({0})")]
    InterfaceBusy(String),
}

/// Map a raw open/claim failure message to a dedicated variant where recognised.
pub(crate) fn classify_open_error(msg: String) -> OpenError {
    let l = msg.to_ascii_lowercase();
    if l.contains("deviceinterfaceguid") {
        OpenError::InterfaceNotRegistered
    } else if l.contains("access is denied")
        || l.contains("access denied")
        || l.contains("permission denied")
        || l.contains("busy")
        || l.contains("in use")
        || l.contains("being used by another process")
        || l.contains("os error 5)")
        || l.contains("os error 32)")
        || l.contains("os error 170)")
    {
        OpenError::InterfaceBusy(msg)
    } else {
        OpenError::Usb(msg)
    }
}

struct ThreadWaker(Thread);
impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Block on `fut` for at most `timeout`; dropping the future cancels the transfer.
fn block_on_timeout<F: Future>(fut: F, timeout: Duration) -> Option<F::Output> {
    let mut fut = std::pin::pin!(fut);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let deadline = Instant::now() + timeout;
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return Some(v);
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        thread::park_timeout(deadline - now);
    }
}

fn map_transfer(e: TransferError) -> TransportError {
    match e {
        TransferError::Disconnected => TransportError::Disconnected,
        other => TransportError::Usb(other.to_string()),
    }
}

pub struct NusbTransport {
    iface: Interface,
    endpoints: VendorEndpoints,
    /// Size of each bulk IN request: one max-size packet, so every transfer completes after a
    /// single packet even when the device sends no zero-length packet after a reply whose
    /// length is a multiple of the packet size.
    read_len: usize,
    /// Bytes received but not yet returned as a frame.
    pending: Vec<u8>,
    description: String,
}

impl NusbTransport {
    /// Open the first attached supported Revelator Dynamic.
    pub fn open_first() -> Result<Self, OpenError> {
        let devs = find_devices().map_err(OpenError::Usb)?;
        if devs.is_empty() {
            return Err(OpenError::NoDevice);
        }
        if let Some(d) = devs.iter().find(|d| d.support == Support::Supported) {
            return Self::open(&d.info);
        }
        let d = &devs[0];
        Err(match d.support {
            Support::UnsupportedMode => OpenError::UnsupportedMode(d.name.to_owned()),
            _ => OpenError::UnsupportedModel(d.name.to_owned()),
        })
    }

    /// Open a specific device. Refuses anything but supported Revelator Dynamic PIDs.
    pub fn open(info: &DeviceInfo) -> Result<Self, OpenError> {
        match crate::discovery::classify(info.vendor_id(), info.product_id()) {
            Some((_, Support::Supported)) => {}
            Some((n, Support::UnsupportedMode)) => {
                return Err(OpenError::UnsupportedMode(n.to_owned()))
            }
            Some((n, _)) => return Err(OpenError::UnsupportedModel(n.to_owned())),
            None => return Err(OpenError::UnsupportedModel("unknown device".to_owned())),
        }
        let dev = info
            .open()
            .map_err(|e| classify_open_error(format!("open failed: {e}")))?;
        let cfg: Configuration = dev
            .active_configuration()
            .map_err(|e| OpenError::Usb(e.to_string()))?;
        let ep =
            select_vendor_interface(&parse_config(&cfg)).ok_or(OpenError::NoVendorInterface)?;
        let iface = dev.claim_interface(ep.interface).map_err(|e| {
            classify_open_error(format!("claim interface {} failed: {e}", ep.interface))
        })?;
        iface
            .set_alt_setting(ep.alt)
            .map_err(|e| OpenError::Usb(format!("set alt setting {} failed: {e}", ep.alt)))?;
        iface
            .clear_halt(ep.out_ep)
            .map_err(|e| OpenError::Usb(format!("clear_halt OUT failed: {e}")))?;
        iface
            .clear_halt(ep.in_ep)
            .map_err(|e| OpenError::Usb(format!("clear_halt IN failed: {e}")))?;
        let read_len = (ep.in_max_packet as usize).max(64);
        let description = format!(
            "{:04X}:{:04X} interface {} alt {} OUT 0x{:02X} IN 0x{:02X}",
            info.vendor_id(),
            info.product_id(),
            ep.interface,
            ep.alt,
            ep.out_ep,
            ep.in_ep
        );
        Ok(Self {
            iface,
            endpoints: ep,
            read_len,
            pending: Vec::new(),
            description,
        })
    }

    pub fn endpoints(&self) -> VendorEndpoints {
        self.endpoints
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    /// One bulk IN transfer (at most one packet), returned as-is. For tracing.
    pub fn read_raw(&mut self, timeout_ms: u32) -> Result<Vec<u8>, TransportError> {
        let fut = self
            .iface
            .bulk_in(self.endpoints.in_ep, RequestBuffer::new(self.read_len));
        let c = block_on_timeout(fut, Duration::from_millis(timeout_ms as u64))
            .ok_or(TransportError::Timeout)?;
        c.into_result().map_err(map_transfer)
    }
}

/// If `buf` starts with a complete frame (by its u16 length header), the frame length.
/// Bytes that do not start with a frame header (`len u16, 0x01, 0x01|0x81`, `len >= 8`) are
/// returned as-is, so the caller drops them and the next read starts on a fresh frame.
pub(crate) fn complete_frame_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < 2 {
        return None;
    }
    let declared = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    let bad_magic = buf.len() >= 4 && (buf[2] != 0x01 || !matches!(buf[3], 0x01 | 0x81));
    if declared < 8 || bad_magic {
        return Some(buf.len());
    }
    (buf.len() >= declared).then_some(declared)
}

impl Transport for NusbTransport {
    fn write_frame(&mut self, frame: &[u8], timeout_ms: u32) -> Result<(), TransportError> {
        let fut = self.iface.bulk_out(self.endpoints.out_ep, frame.to_vec());
        let c = block_on_timeout(fut, Duration::from_millis(timeout_ms as u64))
            .ok_or(TransportError::Timeout)?;
        c.status.map_err(map_transfer)
    }

    fn read_frame(&mut self, timeout_ms: u32) -> Result<Vec<u8>, TransportError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        loop {
            if let Some(n) = complete_frame_len(&self.pending) {
                return Ok(self.pending.drain(..n).collect());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let chunk = if left.is_zero() {
                Err(TransportError::Timeout)
            } else {
                self.read_raw(left.as_millis().max(1) as u32)
            };
            match chunk {
                Ok(c) => self.pending.extend_from_slice(&c),
                Err(e) => {
                    // A partly received frame cannot be completed reliably (the cancelled
                    // transfer may have lost bytes): drop it so later frames stay aligned.
                    self.pending.clear();
                    return Err(e);
                }
            }
        }
    }

    fn vendor_in(
        &mut self,
        request: u8,
        value: u16,
        len: usize,
    ) -> Result<Vec<u8>, TransportError> {
        let fut = self.iface.control_in(ControlIn {
            control_type: ControlType::Vendor,
            recipient: Recipient::Interface,
            request,
            value,
            index: self.endpoints.interface as u16,
            length: len as u16,
        });
        let c =
            block_on_timeout(fut, Duration::from_millis(2000)).ok_or(TransportError::Timeout)?;
        c.into_result().map_err(map_transfer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_open_errors() {
        assert!(matches!(
            classify_open_error(
                "claim interface 5 failed: Could not find DeviceInterfaceGUIDs in registry".into()
            ),
            OpenError::InterfaceNotRegistered
        ));
        for m in [
            "open failed: Access is denied. (os error 5)",
            "claim interface 5 failed: The process cannot access the file because it is being used by another process. (os error 32)",
            "claim interface 5 failed: Device or resource busy",
        ] {
            assert!(matches!(classify_open_error(m.into()), OpenError::InterfaceBusy(_)), "{m}");
        }
        assert!(matches!(
            classify_open_error("set alt setting 1 failed: x".into()),
            OpenError::Usb(_)
        ));
    }

    #[test]
    fn frame_reassembly_lengths() {
        assert_eq!(complete_frame_len(&[]), None);
        assert_eq!(complete_frame_len(&[0x10]), None);
        // 0x0800 = 2048 declared, only 512 buffered
        let mut b = vec![0u8; 512];
        b[0] = 0x00;
        b[1] = 0x08;
        b[2] = 0x01;
        b[3] = 0x81;
        assert_eq!(complete_frame_len(&b), None);
        b.resize(2048 + 5, 0);
        assert_eq!(complete_frame_len(&b), Some(2048));
        // nonsense header: hand back what we have
        assert_eq!(complete_frame_len(&[3, 0, 9, 9]), Some(4));
        // a length that looks fine but no frame magic (middle of a lost frame)
        assert_eq!(complete_frame_len(&[0x10, 0, 0x55, 0x81, 0, 0]), Some(6));
        assert_eq!(complete_frame_len(&[0x10, 0, 0x01, 0x81, 0, 0]), None);
    }
}

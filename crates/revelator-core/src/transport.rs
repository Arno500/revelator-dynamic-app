//! Abstraction over the USB vendor interface so core logic is testable with a mock.
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("device disconnected")]
    Disconnected,
    #[error("timeout")]
    Timeout,
    #[error("usb error: {0}")]
    Usb(String),
}

/// One framed bulk exchange plus the vendor control queries.
pub trait Transport: Send {
    /// Write one complete frame to bulk OUT.
    fn write_frame(&mut self, frame: &[u8], timeout_ms: u32) -> Result<(), TransportError>;
    /// Read one complete frame from bulk IN (blocks up to timeout).
    fn read_frame(&mut self, timeout_ms: u32) -> Result<Vec<u8>, TransportError>;
    /// Vendor IN control request on the private interface (bmRequestType 0xC1).
    fn vendor_in(&mut self, request: u8, value: u16, len: usize)
        -> Result<Vec<u8>, TransportError>;
}

//! A simulated Revelator for development and tests without hardware.
//!
//! The mock speaks the same hypothesized protocol as [`crate::frame`]: it answers `GetP Appl JaSt`
//! with a 503-slot snapshot and stores Para/Pari writes into the slot of the matching
//! [`crate::params::ParamDef`]. Frames not on the safety allow-list are dropped (never answered).

use crate::frame::{self, tag, JAST_BLOB_SIZE};
use crate::params;
use crate::safety::Command;
use crate::transport::{Transport, TransportError};
use std::collections::VecDeque;

/// Number of f32 slots in the mock snapshot (a 2040-byte JaSt reply).
pub const MOCK_SLOTS: usize = 503;

pub struct MockTransport {
    slots: Vec<f32>,
    replies: VecDeque<Vec<u8>>,
    /// Every frame written by the host, in order.
    pub sent: Vec<Vec<u8>>,
    pub protocol_version: u16,
    pub max_command_len: u32,
    pub max_response_len: u32,
    /// When true every call fails with `Disconnected`.
    pub disconnected: bool,
    /// Contents of the four preset slots (answered through MemP reads).
    pub presets: [Vec<u8>; 16],
    /// Contents of the four button slots (`Stat 0..=3`).
    pub stat: [Vec<u8>; 4],
    /// Answer every accepted `SetP` with the bare 8-byte ack the real mic sends (default true).
    pub ack_setp: bool,
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl MockTransport {
    /// Fresh device with every parameter at its default (preamp gain 22 dB, etc.).
    pub fn new() -> Self {
        let mut slots = vec![0.0; MOCK_SLOTS];
        for p in params::ALL {
            slots[p.slot as usize] = p.default;
        }
        // Device-level state as an idle Revelator Dynamic reports it (slots 42..=48).
        slots[crate::device::slots::PRESET] = f32::from_bits(0);
        slots[43] = f32::from_bits(u32::MAX);
        slots[crate::device::slots::FLAGS] = f32::from_bits(0x81);
        slots[crate::device::slots::KNOB] = 0.7;
        slots[46] = 0.263;
        slots[47] = 0.0;
        slots[48] = 22.0;
        Self {
            slots,
            replies: VecDeque::new(),
            sent: Vec::new(),
            protocol_version: 1,
            max_command_len: 0x800,
            max_response_len: 0x800,
            disconnected: false,
            presets: Default::default(),
            stat: Default::default(),
            ack_setp: true,
        }
    }

    /// Change a slot as if a physical knob/button had been used.
    pub fn set_slot(&mut self, slot: usize, v: f32) {
        self.slots[slot] = v;
    }

    pub fn slot(&self, slot: usize) -> f32 {
        self.slots[slot]
    }

    /// Reflect a device-level write in the state, like the real mic does.
    fn apply_appl(&mut self, rb: crate::appl::Readback, t: u32, bits: u32) {
        use crate::appl::Readback;
        let as_int = if t == tag::PARI {
            bits
        } else {
            f32::from_bits(bits) as u32
        };
        match rb {
            Readback::F32(i) => {
                self.slots[i] = if t == tag::PARI {
                    bits as i32 as f32
                } else {
                    f32::from_bits(bits)
                }
            }
            Readback::I32(i) => self.slots[i] = f32::from_bits(as_int),
            Readback::Bit(i, b) => {
                let old = self.slots[i].to_bits();
                let new = if as_int != 0 {
                    old | (1 << b)
                } else {
                    old & !(1 << b)
                };
                self.slots[i] = f32::from_bits(new);
            }
            Readback::None => {}
        }
    }

    fn mem(&mut self, addr: (u32, u32)) -> Option<&mut Vec<u8>> {
        match addr {
            (tag::PRSM, n @ 0..=15) => Some(&mut self.presets[n as usize]),
            (tag::STAT, n @ 0..=3) => Some(&mut self.stat[n as usize]),
            _ => None,
        }
    }

    fn check_alive(&self) -> Result<(), TransportError> {
        if self.disconnected {
            Err(TransportError::Disconnected)
        } else {
            Ok(())
        }
    }
}

impl Transport for MockTransport {
    fn write_frame(&mut self, frame_bytes: &[u8], _timeout_ms: u32) -> Result<(), TransportError> {
        self.check_alive()?;
        self.sent.push(frame_bytes.to_vec());
        let Ok(f) = frame::decode_frame(frame_bytes) else {
            return Ok(());
        };
        let Ok(c) = Command::parse(f.payload) else {
            return Ok(());
        };
        if !c.is_allowed() {
            return Ok(());
        }
        let uid = f.uid;
        if c.op == tag::GETP && c.blob_tag == Some(tag::JAST) {
            // Like the Dynamic: the reply carries as many slots as the requested size holds.
            let size = c.blob_size.unwrap_or(JAST_BLOB_SIZE);
            let n = (size.saturating_sub(8) as usize / 4).min(MOCK_SLOTS);
            let data: Vec<u8> = self.slots[..n]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            self.replies.push_back(frame::build_reply(
                uid,
                tag::APPL,
                0,
                tag::JAST,
                size,
                &data,
            ));
        } else if c.op == tag::GETP && c.blob_tag == Some(tag::MEMP) {
            let (addr, p) = (c.mem_addr.unwrap(), f.payload);
            let id = u16::from_le_bytes([p[28], p[29]]);
            let off = u32::from_le_bytes([p[30], p[31], p[32], p[33]]) as usize;
            let all = self.mem(addr).map(|v| v.clone()).unwrap_or_default();
            let chunk = &all[off.min(all.len())..all.len().min(off + frame::MEMP_CHUNK_MAX)];
            let more = off + chunk.len() < all.len();
            let blob = frame::memp_reply_blob(addr, id, off as u32, more, all.len() as u32, chunk);
            self.replies.push_back(frame::build_reply(
                uid,
                tag::APPL,
                0,
                tag::MEMP,
                frame::MEMP_BLOB_SIZE,
                &blob,
            ));
        } else if c.op == tag::SETP && c.blob_tag == Some(tag::MEMP) {
            // Store a preset slot chunk by chunk.
            if let (Some(addr), Some(ch)) = (c.mem_addr, c.mem_chunk) {
                let data = &f.payload[41..41 + ch.len];
                // like the real firmware: only the user presets and button slots take writes
                if crate::safety::mem_writable(addr) {
                    if let Some(slot) = self.mem(addr) {
                        if ch.offset == 0 {
                            slot.clear();
                        }
                        slot.extend_from_slice(data);
                    }
                }
            }
            if self.ack_setp {
                self.replies.push_back(vec![8, 0, 0x01, 0x81, uid, 0, 0, 0]);
            }
        } else if c.op == tag::SETP {
            if let (Some(id), Some(bits), Some(t)) = (c.param_id, c.value_bits, c.blob_tag) {
                if c.block == tag::APPL {
                    if let Some(p) = crate::appl::ALL.iter().find(|p| p.x == id) {
                        self.apply_appl(p.readback, t, bits);
                    }
                }
            }
            // Like the real mic (seen with `trace-write`): SetP is applied and acknowledged with
            // a bare 8-byte frame `08 00 01 81 <uid> 00 00 00`.
            if self.ack_setp {
                self.replies.push_back(vec![8, 0, 0x01, 0x81, uid, 0, 0, 0]);
            }
        }
        Ok(())
    }

    fn read_frame(&mut self, _timeout_ms: u32) -> Result<Vec<u8>, TransportError> {
        self.check_alive()?;
        self.replies.pop_front().ok_or(TransportError::Timeout)
    }

    fn vendor_in(
        &mut self,
        request: u8,
        value: u16,
        len: usize,
    ) -> Result<Vec<u8>, TransportError> {
        self.check_alive()?;
        let mut out = match (request, value) {
            (0x00, _) => self.protocol_version.to_le_bytes().to_vec(),
            (0x01, 0) => self.max_command_len.to_le_bytes().to_vec(),
            (0x01, 1) => self.max_response_len.to_le_bytes().to_vec(),
            _ => return Err(TransportError::Usb("unsupported request".into())),
        };
        out.truncate(len);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jast_reply_is_2040_bytes() {
        let mut m = MockTransport::new();
        m.write_frame(&frame::get_state(3), 100).unwrap();
        let r = m.read_frame(100).unwrap();
        assert_eq!(r.len(), 2040);
        assert_eq!(frame::parse_reply(&r).unwrap().uid, 3);
    }

    #[test]
    fn disallowed_frames_get_no_reply() {
        let mut m = MockTransport::new();
        m.write_frame(
            &frame::set_param(1, 999, 0, 1, frame::ParamValue::F32(1.0)),
            100,
        )
        .unwrap();
        assert!(matches!(m.read_frame(100), Err(TransportError::Timeout)));
    }

    #[test]
    fn disconnect() {
        let mut m = MockTransport::new();
        m.disconnected = true;
        assert!(matches!(
            m.vendor_in(0, 0, 2),
            Err(TransportError::Disconnected)
        ));
    }
}

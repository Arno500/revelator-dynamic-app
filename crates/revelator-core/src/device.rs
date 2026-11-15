//! High-level device API over any [`Transport`].
//!
//! ```no_run
//! # use revelator_core::{Device, Transport};
//! # fn demo<T: Transport>(transport: T) -> Result<(), revelator_core::DeviceError> {
//! let mut dev = Device::open(transport)?;
//! let state = dev.read_state()?;
//! let gain = revelator_core::appl::find("preamp.gain").unwrap();
//! println!("gain = {:?}", gain.readback.read(&state));
//! dev.set_appl(gain, 30.0)?; // safety check, SetP, readback
//! # Ok(()) }
//! ```

use crate::frame::{self, tag, FrameError, ParamValue, Reply, UidGen, DATA_START};
use crate::params::{Kind, ParamDef};
use crate::safety::{block, Command, SafetyContext, SafetyError};
use crate::transport::{Transport, TransportError};
use thiserror::Error;

const TIMEOUT_MS: u32 = 1000;
/// Wait after a Voice FX model select (`VoFx`) before its blobs (see `send_dsp_acks`).
pub const VOFX_SWITCH_WAIT: std::time::Duration = std::time::Duration::from_millis(200);
/// Maximum number of unrelated frames skipped while waiting for a reply.
const MAX_SKIPPED: usize = 16;

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Safety(#[from] SafetyError),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("value {value} out of range [{min}, {max}] for {key}")]
    OutOfRange {
        key: &'static str,
        value: f32,
        min: f32,
        max: f32,
    },
}

/// Add which step failed to a transport error; disconnects stay typed so callers can detect them.
fn with_step(step: &str, e: TransportError) -> DeviceError {
    match e {
        TransportError::Disconnected => DeviceError::Transport(e),
        other => DeviceError::Protocol(format!("{step}: {other}")),
    }
}

impl DeviceError {
    /// The mic did not answer in time (plain, or wrapped with the step that was waiting).
    pub fn is_timeout(&self) -> bool {
        match self {
            DeviceError::Transport(TransportError::Timeout) => true,
            DeviceError::Protocol(m) => m.ends_with(": timeout"),
            _ => false,
        }
    }
}

/// Information gathered by [`Device::open`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    pub protocol_version: u16,
    /// Maximum command frame length the device accepts.
    pub max_command_len: u32,
    /// Maximum response frame length the device sends.
    pub max_response_len: u32,
}

/// Decoded JaSt state snapshot: a flat array of f32 "slots".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    floats: Vec<f32>,
}

impl State {
    /// Build from raw slot values.
    pub fn from_floats(floats: Vec<f32>) -> Self {
        Self { floats }
    }

    /// All slots (slot 0 is the f32 at frame offset [`DATA_START`]).
    pub fn floats(&self) -> &[f32] {
        &self.floats
    }

    pub fn len(&self) -> usize {
        self.floats.len()
    }

    pub fn is_empty(&self) -> bool {
        self.floats.is_empty()
    }

    pub fn slot(&self, i: usize) -> Option<f32> {
        self.floats.get(i).copied()
    }

    /// Slot reinterpreted as raw bits (for bitfield slots such as Slot 42 in the io24 notes).
    pub fn slot_bits(&self, i: usize) -> Option<u32> {
        self.slot(i).map(f32::to_bits)
    }

    /// Value of a parameter, via its (hypothesized) `slot`.
    pub fn value(&self, p: &ParamDef) -> Option<f32> {
        self.slot(p.slot as usize)
    }
}

/// State slots confirmed on a real Revelator Dynamic (firmware as shipped, PID 0D0A) by
/// watching the raw state while using the mic's physical controls.
pub mod slots {
    /// Active preset, integer 0..=3 (preset button cycles 0,1,2,3,0).
    pub const PRESET: usize = 42;
    /// Flag word (integer); idle value seen: 0x81.
    pub const FLAGS: usize = 44;
    /// Bit in [`FLAGS`] set while the mic is muted.
    pub const FLAG_MUTE: u32 = 1 << 3;
    /// The flag word packs the firmware's boolean state variables in the order of its own table
    /// (embedded in UC's DLL with the firmware image, file offset 0x399150): input1HighPassFilter,
    /// input2HighPassFilter, muteMode, input1Mute, input2Mute, hpOutputMute, mainOutputMute,
    /// usbStatus, channelLink. That puts input1Mute on bit 3 (confirmed) and explains the idle
    /// 0x81 (HPF + USB connected). Bits from that order not yet confirmed on hardware:
    pub const FLAG_MUTE_MODE: u32 = 1 << 2;
    pub const FLAG_HP_MUTE: u32 = 1 << 5;
    /// Knob position, f32 0.0..=1.0 in 0.01 steps. What it controls (headphone level, mix,
    /// gain) is not yet confirmed.
    pub const KNOB: usize = 45;
}

/// The hardware controls decoded from verified slots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Controls {
    /// Active preset, 0-based.
    pub preset: u32,
    pub muted: bool,
    /// Raw flag word (slot 44), for the bits not yet identified.
    pub flags: u32,
    /// Knob position 0..1.
    pub knob: f32,
}

impl State {
    /// Decode the verified hardware controls; `None` if the state is too short.
    pub fn controls(&self) -> Option<Controls> {
        let flags = self.slot_bits(slots::FLAGS)?;
        Some(Controls {
            preset: self.slot_bits(slots::PRESET)?,
            muted: flags & slots::FLAG_MUTE != 0,
            flags,
            knob: self.slot(slots::KNOB)?,
        })
    }
}

/// A connected Revelator.
pub struct Device<T: Transport> {
    transport: T,
    uids: UidGen,
    info: DeviceInfo,
    ctx: SafetyContext,
    /// Set when a state read after a write only got an answer with the full-size request.
    pub full_state_fallback: bool,
    /// Whether the mic acknowledges `SetP` (learnt from the first write; the real one does).
    acks: Option<bool>,
    /// The last raw ack frame seen (diagnostics for slot writes).
    last_ack: Option<Vec<u8>>,
}

impl<T: Transport> Device<T> {
    /// Query protocol version (`vendor_in(0x00, 0, 2)`) and max command/response lengths
    /// (`vendor_in(0x01, 0|1, 4)`), and check they are sane.
    pub fn open(mut transport: T) -> Result<Self, DeviceError> {
        let v = transport.vendor_in(0x00, 0, 2)?;
        if v.len() < 2 {
            return Err(DeviceError::Protocol("short protocol version reply".into()));
        }
        let protocol_version = u16::from_le_bytes([v[0], v[1]]);
        let mut len = |value| -> Result<u32, DeviceError> {
            let b = transport.vendor_in(0x01, value, 4)?;
            if b.len() < 4 {
                return Err(DeviceError::Protocol("short max-length reply".into()));
            }
            Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let max_command_len = len(0)?;
        let max_response_len = len(1)?;
        // Safety rule: refuse a device that cannot carry our largest command / smallest reply.
        if (max_command_len as usize) < 0x30 {
            return Err(DeviceError::Protocol(format!(
                "max command length {max_command_len} too small"
            )));
        }
        if (max_response_len as usize) < DATA_START {
            return Err(DeviceError::Protocol(format!(
                "max response length {max_response_len} too small"
            )));
        }
        Ok(Self {
            transport,
            uids: UidGen::new(),
            info: DeviceInfo {
                protocol_version,
                max_command_len,
                max_response_len,
            },
            ctx: SafetyContext::default(),
            full_state_fallback: false,
            acks: None,
            last_ack: None,
        })
    }

    pub fn info(&self) -> DeviceInfo {
        self.info
    }

    /// Tell the safety layer the current sample rate (needed to allow Voice FX Delay).
    pub fn set_sample_rate(&mut self, hz: Option<u32>) {
        self.ctx.sample_rate = hz;
    }

    /// Access the underlying transport (e.g. to inspect a mock in tests).
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Safety-check, send, and wait for the matching reply.
    /// Safety-check and send a `SetP`. The real Revelator Dynamic acknowledges it with a bare
    /// 8-byte frame, which is read here; it carries no result, so callers confirm through the
    /// state.
    fn send(&mut self, frame_bytes: &[u8]) -> Result<(), DeviceError> {
        let f = frame::decode_frame(frame_bytes)?;
        Command::check_payload(f.payload, &self.ctx)?;
        if frame_bytes.len() > self.info.max_command_len as usize {
            return Err(DeviceError::Protocol(
                "command exceeds device max length".into(),
            ));
        }
        let uid = f.uid;
        self.transport
            .write_frame(frame_bytes, TIMEOUT_MS)
            .map_err(|e| with_step(&format!("sending SetP (uid {uid})"), e))?;
        self.take_ack(uid)
    }

    /// The mic answers every `SetP` with a bare 8-byte ack (`08 00 01 81 <uid> 00 00 00`). Read
    /// it, so acks do not pile up in the mic's reply channel and stall later commands. A missing
    /// ack is not an error (the first write decides whether this mic sends them at all).
    fn take_ack(&mut self, uid: u8) -> Result<(), DeviceError> {
        if self.acks == Some(false) {
            return Ok(());
        }
        let wait = if self.acks == Some(true) { 500 } else { 250 };
        match self.transport.read_frame(wait) {
            Ok(raw) => {
                if raw.len() >= 5 && raw[3] == 0x81 && raw[4] == uid {
                    self.acks = Some(true);
                }
                self.last_ack = Some(raw);
                Ok(())
            }
            Err(TransportError::Timeout) => {
                if self.acks.is_none() {
                    self.acks = Some(false);
                }
                Ok(())
            }
            Err(e) => Err(with_step(
                &format!("reading the ack of SetP (uid {uid})"),
                e,
            )),
        }
    }

    fn exchange(&mut self, frame_bytes: &[u8]) -> Result<Reply, DeviceError> {
        let f = frame::decode_frame(frame_bytes)?;
        Command::check_payload(f.payload, &self.ctx)?;
        if frame_bytes.len() > self.info.max_command_len as usize {
            return Err(DeviceError::Protocol(
                "command exceeds device max length".into(),
            ));
        }
        let uid = f.uid;
        let op = frame::fourcc_str(u32::from_le_bytes([
            f.payload[0],
            f.payload[1],
            f.payload[2],
            f.payload[3],
        ]));
        self.transport
            .write_frame(frame_bytes, TIMEOUT_MS)
            .map_err(|e| with_step(&format!("sending {op} (uid {uid})"), e))?;
        for _ in 0..MAX_SKIPPED {
            let raw = self.transport.read_frame(TIMEOUT_MS).map_err(|e| {
                with_step(
                    &format!("{op} (uid {uid}) was sent, but waiting for the reply failed"),
                    e,
                )
            })?;
            match frame::parse_reply(&raw) {
                Ok(r) if r.uid == uid => return Ok(r),
                // unrelated / unparsable frame: keep waiting
                _ => continue,
            }
        }
        Err(DeviceError::Protocol("no matching reply".into()))
    }

    /// Read back a DSP block blob (`GetP block 0 [tag,size,0,0]`) and return the reply data.
    pub fn get_dsp(
        &mut self,
        block: u32,
        blob_tag: u32,
        size: u32,
    ) -> Result<Vec<u8>, DeviceError> {
        let uid = self.uids.next_uid();
        let r = self.exchange(&frame::get_dsp(uid, block, blob_tag, size))?;
        Ok(r.data)
    }

    /// Fetch and decode the state snapshot (the short one-packet form, slots 0..=52; use
    /// [`Device::read_state_sized`] with [`frame::JAST_BLOB_SIZE`] for the full one).
    pub fn read_state(&mut self) -> Result<State, DeviceError> {
        self.read_state_sized(frame::STATE_POLL_SIZE)
    }

    /// [`Device::read_state`] with an explicit JaSt blob size (the Dynamic answers both
    /// 0x7EC and 0xDC with different reply lengths).
    pub fn read_state_sized(&mut self, blob_size: u32) -> Result<State, DeviceError> {
        let uid = self.uids.next_uid();
        let r = self.exchange(&frame::get_state_sized(uid, blob_size))?;
        match r.blob {
            Some((t, _)) if t == tag::JAST => Ok(State { floats: r.floats() }),
            other => Err(DeviceError::Protocol(format!(
                "unexpected JaSt reply blob {other:?}"
            ))),
        }
    }

    /// Read a device-level parameter (see [`crate::appl`]) from its confirmed state slot.
    /// `None` if the parameter has no known state location.
    pub fn get_appl(&mut self, p: &crate::appl::ApplParam) -> Result<Option<f32>, DeviceError> {
        Ok(p.readback.read(&self.read_state()?))
    }

    /// Write a device-level parameter (see [`crate::appl`]) and read it back from the state.
    /// Returns the readback value, or `None` if the parameter has no known state location.
    pub fn set_appl(
        &mut self,
        p: &crate::appl::ApplParam,
        value: f32,
    ) -> Result<Option<f32>, DeviceError> {
        self.send_appl(p, value)?;
        Ok(p.readback.read(&self.read_state_after_write()?))
    }

    /// The state read that confirms a write. The mic needs a moment after a `SetP` (a read sent
    /// straight away can go unanswered on real hardware), so wait briefly, retry the short
    /// request, then fall back to the full-size one. Read-only commands only.
    pub fn read_state_after_write(&mut self) -> Result<State, DeviceError> {
        let mut last = None;
        for attempt in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            let r = if attempt < 2 {
                self.read_state()
            } else {
                self.read_state_sized(frame::JAST_BLOB_SIZE).inspect(|_| {
                    self.full_state_fallback = true;
                })
            };
            match r {
                Ok(s) => return Ok(s),
                Err(e) if e.is_timeout() => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| DeviceError::Protocol("no state reply".into())))
    }

    /// [`Device::set_appl`] without the state read afterwards.
    pub fn send_appl(&mut self, p: &crate::appl::ApplParam, value: f32) -> Result<(), DeviceError> {
        if !value.is_finite() || value < p.min || value > p.max {
            return Err(DeviceError::OutOfRange {
                key: p.key,
                value,
                min: p.min,
                max: p.max,
            });
        }
        let v = match p.kind {
            Kind::F32 => ParamValue::F32(value),
            Kind::I32 => ParamValue::I32(value.round() as i32),
        };
        let uid = self.uids.next_uid();
        self.send(&frame::set_appl(uid, p.x, p.y, v))
    }

    /// Send DSP blobs (from [`crate::fx::FxState::writes`]) to processing channel `channel`.
    /// The `SetP` ack carries no result and the state reply carries no DSP values, so there is
    /// nothing to confirm.
    pub fn send_dsp(
        &mut self,
        channel: u32,
        writes: &[crate::fx::DspWrite],
    ) -> Result<(), DeviceError> {
        self.send_dsp_acks(channel, writes).map(|_| ())
    }

    /// [`Device::send_dsp`], returning the raw ack frame the mic gave each write (empty if none).
    pub fn send_dsp_acks(
        &mut self,
        channel: u32,
        writes: &[crate::fx::DspWrite],
    ) -> Result<Vec<Vec<u8>>, DeviceError> {
        let mut acks = Vec::new();
        for (i, w) in writes.iter().enumerate() {
            let uid = self.uids.next_uid();
            let index = if w.fixed_index0 { 0 } else { channel };
            self.last_ack = None;
            self.send(&frame::set_blob(uid, w.block, index, &w.blob))?;
            acks.push(self.last_ack.take().unwrap_or_default());
            if w.blob[0..4] == frame::tag::VOFX.to_le_bytes() && i + 1 < writes.len() {
                // The Voice FX manager (firmware 0x41b44) only schedules the model switch; it
                // happens later in the audio path, and until then every blob goes to the OLD
                // effect instance (forwarder 0x3cbdc), so it would be lost. UC likewise sends the
                // model's blobs on a later tick. Give the switch time to complete.
                std::thread::sleep(VOFX_SWITCH_WAIT);
            }
        }
        Ok(acks)
    }

    /// Send stream-mixer writes (from [`crate::mixer::MixerState::writes`]). Not answered by
    /// the device and not in the state reply.
    pub fn send_mixer(&mut self, writes: &[crate::mixer::MixWrite]) -> Result<(), DeviceError> {
        for w in writes {
            let uid = self.uids.next_uid();
            self.send(&w.frame(uid))?;
        }
        Ok(())
    }

    /// Select the Voice FX model through the dedicated `VoFx` command. Delay is refused at
    /// 88.2/96 kHz (and when the sample rate is unknown); see [`Device::set_sample_rate`].
    pub fn set_voicefx_model(&mut self, model: u32) -> Result<(), DeviceError> {
        let uid = self.uids.next_uid();
        self.send(&frame::set_voicefx_model(uid, block::VOICE_FX, model))?;
        Ok(())
    }

    /// Read the raw contents of one of the mic's memory areas through the chunked MemP transfer
    /// UC uses (`GetP Appl 0 [MemP, addr0, addr1]`, then follow-up requests while the device
    /// says more data follows). Read-only; the area must pass [`crate::safety::mem_readable`].
    pub fn read_mem(&mut self, addr: (u32, u32)) -> Result<Vec<u8>, DeviceError> {
        const MAX_CHUNKS: usize = 64;
        let id = u16::from(self.uids.next_uid());
        let mut out = Vec::new();
        for _ in 0..MAX_CHUNKS {
            let uid = self.uids.next_uid();
            let offset = out.len() as u32;
            let r = self.exchange(&frame::memp_read(uid, addr.0, addr.1, id, offset))?;
            let c = frame::MemChunk::from_reply(&r).ok_or_else(|| {
                DeviceError::Protocol(format!(
                    "unexpected reply to the preset read: blob {:?}, {} data bytes",
                    r.blob.map(|(t, n)| (frame::fourcc_str(t), n)),
                    r.data.len()
                ))
            })?;
            if c.addr != addr || c.offset != offset {
                return Err(DeviceError::Protocol(format!(
                    "preset read: asked for {}/{} at offset {offset}, got {}/{} at offset {}",
                    frame::fourcc_str(addr.0),
                    addr.1,
                    frame::fourcc_str(c.addr.0),
                    c.addr.1,
                    c.offset
                )));
            }
            out.extend_from_slice(&c.data);
            let complete = c.total > 0 && out.len() >= c.total as usize;
            if !c.more || complete || c.data.is_empty() {
                return Ok(out);
            }
        }
        Err(DeviceError::Protocol(format!(
            "preset read: still more data after {MAX_CHUNKS} chunks"
        )))
    }

    /// Preset `n` (0..=15): 0..=7 are the factory presets (read-only), 8..=15 the user presets.
    pub fn read_preset(&mut self, n: u32) -> Result<Vec<u8>, DeviceError> {
        self.read_mem((tag::PRSM, n))
    }

    /// Button slot `n` (0..=3): the preset the physical preset button selects at that position.
    pub fn read_button_slot(&mut self, n: u32) -> Result<Vec<u8>, DeviceError> {
        self.read_mem((tag::STAT, n))
    }
}
impl<T: Transport> Device<T> {
    /// Store `doc` (a UBJSON preset document, see [`crate::presetdoc`]) in a writable memory
    /// area (`(PrsM, 8..=15)` user presets or `(Stat, 0..=3)` button slots) with UC's chunked
    /// MemP write, then read it back and compare. Nothing is sent unless the document decodes and
    /// fits; a mismatch afterwards is reported as an error.
    pub fn write_mem(&mut self, addr: (u32, u32), doc: &[u8]) -> Result<(), DeviceError> {
        if !crate::safety::mem_writable(addr) {
            return Err(DeviceError::Protocol(format!(
                "{}/{} is not writable (PrsM 0-7 are factory presets)",
                frame::fourcc_str(addr.0),
                addr.1
            )));
        }
        crate::ubjson::decode(doc)
            .map_err(|e| DeviceError::Protocol(format!("not a valid preset document: {e}")))?;
        if doc.len() > crate::safety::MAX_PRESET_BYTES as usize {
            return Err(DeviceError::Protocol(format!(
                "preset document too large ({} bytes)",
                doc.len()
            )));
        }
        let before = self.read_mem(addr).ok();
        // UC leaves the transfer id of a write at 0 (@0x1800528c0).
        let id = 0u16;
        let mut acks = Vec::new();
        let mut offset = 0;
        while offset < doc.len() {
            let end = (offset + frame::MEMP_CHUNK_MAX).min(doc.len());
            let uid = self.uids.next_uid();
            self.send(&frame::memp_write(
                uid,
                addr,
                id,
                offset as u32,
                doc.len() as u32,
                &doc[offset..end],
            ))?;
            offset = end;
            acks.push(match &self.last_ack {
                Some(a) => a
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                None => "none".into(),
            });
        }
        // The mic may need a moment before it shows the new contents: read back up to three
        // times, waiting longer each time.
        let mut back = Vec::new();
        for wait in [150u64, 600, 1500] {
            std::thread::sleep(std::time::Duration::from_millis(wait));
            back = self.read_mem(addr)?;
            if back == doc {
                return Ok(());
            }
        }
        let how = if before.as_deref() == Some(back.as_slice()) {
            "the area is unchanged: the mic ignored the write"
        } else {
            "the area changed, but not to what was written"
        };
        Err(DeviceError::Protocol(format!(
            "{}/{} reads back differently after the write ({} bytes written, {} read): {how} (acks: {})",
            frame::fourcc_str(addr.0),
            addr.1,
            doc.len(),
            back.len(),
            acks.join(" | ")
        )))
    }

    /// User preset `n` (0..=7), stored at `(PrsM, 8 + n)`.
    pub fn write_user_preset(&mut self, n: u32, doc: &[u8]) -> Result<(), DeviceError> {
        self.write_mem((tag::PRSM, 8 + n), doc)
    }

    /// Button slot `n` (0..=3), stored at `(Stat, n)`.
    pub fn write_button_slot(&mut self, n: u32, doc: &[u8]) -> Result<(), DeviceError> {
        self.write_mem((tag::STAT, n), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockTransport;

    #[test]
    fn writes_a_preset_slot_and_verifies_it() {
        let mut m = MockTransport::new();
        m.stat[1] = b"{iaSib}".to_vec();
        let mut dev = Device::open(m).unwrap();
        // two chunks: a document bigger than one MemP chunk
        let mut val = b"Si\x7f".to_vec();
        val.extend(std::iter::repeat_n(b'x', 127));
        let mut doc = b"{".to_vec();
        for n in 0..20u8 {
            doc.extend_from_slice(&[b'i', 2, b'k', b'0' + (n % 10)]);
            doc.extend_from_slice(&val);
        }
        doc.push(b'}');
        assert!(doc.len() > frame::MEMP_CHUNK_MAX);
        dev.write_button_slot(1, &doc).unwrap();
        assert_eq!(dev.read_button_slot(1).unwrap(), doc);
        dev.write_user_preset(2, &doc).unwrap();
        assert_eq!(dev.read_preset(10).unwrap(), doc);
        // refused before anything is sent: no such slot, factory presets, not a document
        assert!(dev.write_button_slot(4, b"{}").is_err());
        assert!(dev.write_mem((tag::PRSM, 3), b"{}").is_err());
        assert!(dev.write_user_preset(0, b"not ubjson").is_err());
    }

    #[test]
    fn setp_acks_are_consumed() {
        // The real mic acknowledges every SetP (08 00 01 81 uid ...); unread acks used to stall it.
        for acks in [true, false] {
            let mut m = MockTransport::new();
            m.ack_setp = acks;
            let mut d = Device::open(m).unwrap();
            let p = crate::appl::find("mic.mute").unwrap();
            for v in [1.0, 0.0, 1.0] {
                assert_eq!(d.set_appl(p, v).unwrap(), Some(v));
            }
            assert_eq!(d.acks, Some(acks));
        }
    }

    #[test]
    fn reads_a_chunked_preset() {
        let mut m = MockTransport::new();
        let big: Vec<u8> = (0..5000u32).map(|i| b'a' + (i % 26) as u8).collect();
        m.presets[2] = big.clone();
        m.presets[0] = b"{\"name\":\"x\"}".to_vec();
        let mut dev = Device::open(m).unwrap();
        assert_eq!(dev.read_preset(2).unwrap(), big);
        assert_eq!(dev.read_preset(0).unwrap(), b"{\"name\":\"x\"}");
        assert!(dev.read_preset(1).unwrap().is_empty());
        // there is no preset 16: refused before anything is sent
        assert!(matches!(dev.read_preset(16), Err(DeviceError::Safety(_))));
    }

    /// Slots 40..=49 as captured from a real Revelator Dynamic (raw-state 0xDC), muted variant.
    #[test]
    fn decodes_captured_controls() {
        let mut bits = vec![0u32; 53];
        bits[40] = 3;
        bits[41] = 4;
        bits[42] = 2;
        bits[43] = 0xFFFF_FFFF;
        bits[44] = 0x89;
        bits[45] = 0x3F66_6668; // 0.90
        bits[48] = 0x41EB_333B;
        let st = State::from_floats(bits.into_iter().map(f32::from_bits).collect());
        let c = st.controls().unwrap();
        assert_eq!(c.preset, 2);
        assert!(c.muted);
        assert_eq!(c.flags, 0x89);
        assert!((c.knob - 0.90).abs() < 1e-6);
        // unmuted idle flags
        let mut f = st.floats().to_vec();
        f[44] = f32::from_bits(0x81);
        assert!(!State::from_floats(f).controls().unwrap().muted);
        // too short
        assert_eq!(State::from_floats(vec![0.0; 10]).controls(), None);
    }

    #[test]
    fn set_appl_roundtrip_on_mock() {
        let mut d = Device::open(MockTransport::new()).unwrap();
        let gain = crate::appl::find("preamp.gain").unwrap();
        assert_eq!(d.set_appl(gain, 35.5).unwrap(), Some(35.5));
        let mute = crate::appl::find("mic.mute").unwrap();
        assert_eq!(d.set_appl(mute, 1.0).unwrap(), Some(1.0));
        assert!(d.read_state().unwrap().controls().unwrap().muted);
        assert_eq!(d.set_appl(mute, 0.0).unwrap(), Some(0.0));
        let preset = crate::appl::find("preset").unwrap();
        assert_eq!(d.set_appl(preset, 2.0).unwrap(), Some(2.0));
        assert!(matches!(
            d.set_appl(gain, 61.0),
            Err(DeviceError::OutOfRange { .. })
        ));
    }

    #[test]
    fn open_reads_info() {
        let d = Device::open(MockTransport::new()).unwrap();
        assert_eq!(d.info().protocol_version, 1);
        assert!(d.info().max_response_len >= 2040);
    }

    #[test]
    fn open_rejects_tiny_limits() {
        let mut m = MockTransport::new();
        m.max_command_len = 8;
        assert!(matches!(Device::open(m), Err(DeviceError::Protocol(_))));
    }

    #[test]
    fn out_of_range_is_not_sent() {
        let mut d = Device::open(MockTransport::new()).unwrap();
        let gain = crate::appl::find("preamp.gain").unwrap();
        for v in [99.0, -1.0, f32::NAN] {
            assert!(matches!(
                d.set_appl(gain, v),
                Err(DeviceError::OutOfRange { .. })
            ));
        }
        assert!(d.transport_mut().sent.is_empty());
    }

    #[test]
    fn mute_goes_as_pari() {
        let mut d = Device::open(MockTransport::new()).unwrap();
        let mute = crate::appl::find("mic.mute").unwrap();
        d.set_appl(mute, 1.0).unwrap();
        let sent = &d.transport_mut().sent;
        let c = Command::parse(frame::decode_frame(&sent[0]).unwrap().payload).unwrap();
        assert_eq!(c.blob_tag, Some(tag::PARI));
    }

    #[test]
    fn delay_blocked_by_safety_before_sending() {
        let mut d = Device::open(MockTransport::new()).unwrap();
        d.set_sample_rate(Some(96_000));
        assert!(matches!(
            d.set_voicefx_model(5),
            Err(DeviceError::Safety(_))
        ));
        assert!(d.transport_mut().sent.is_empty());
        d.set_sample_rate(Some(48_000));
        d.set_voicefx_model(5).unwrap();
        assert_eq!(d.transport_mut().sent.len(), 1);
    }

    #[test]
    fn knob_turn_shows_in_state() {
        let mut d = Device::open(MockTransport::new()).unwrap();
        let hp = crate::appl::find("headphones.volume").unwrap();
        d.transport_mut().set_slot(slots::KNOB, 0.25);
        assert_eq!(d.get_appl(hp).unwrap(), Some(0.25));
    }
}

//! Allow-list guard. Every outgoing frame is parsed back into a [`Command`] and must pass
//! [`Command::is_allowed`] and [`Command::check`] before it reaches the transport.
//!
//! Only `SetP`/`GetP` on known blocks and tags are permitted. Anything else (DFU, reset,
//! bootloader, firmware or EEPROM writes, unknown tags) is rejected by construction because it
//! is simply not on the list.

use crate::frame::{tag, PARAM_BLOB_SIZE};
use thiserror::Error;

/// Block numbers (the `block` field of SetP).
pub mod block {
    pub const MIXER: u32 = 100;
    /// Fat Channel / input processing. Number is a guess.
    pub const FAT_CHANNEL: u32 = 101;
    pub const VOICE_FX: u32 = 201;
    pub const REVERB: u32 = 202;
}

/// Voice FX hardware model number for Delay (io24 notes: "hardware model 5").
pub const VOICEFX_MODEL_DELAY: u32 = 5;
/// `fxmodel` parameter id in block 201 (io24 notes).
pub const FXMODEL_PARAM_ID: u32 = 450;

/// USB product ids we refuse to talk to: bootloader/DFU and the "unsupported mode" PIDs.
pub const REFUSED_PIDS: &[u16] = &[0x0D06, 0x0D0B, 0x0405];
pub const VENDOR_ID: u16 = 0x194F;
/// Normal-mode Revelator Dynamic PIDs.
pub const SUPPORTED_PIDS: &[u16] = &[0x0D04, 0x0D0A];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SafetyError {
    #[error("command not on the allow-list: {0}")]
    NotAllowed(String),
    #[error("malformed command payload")]
    Malformed,
    #[error("Delay Voice FX is blocked at 88.2/96 kHz (firmware crash into bootloader); sample rate: {0}")]
    DelayAtHighRate(String),
    #[error("device is in bootloader/unsupported mode (pid {0:#06x})")]
    RefusedDevice(u16),
}

/// Refuse bootloader / unsupported-mode devices. Unknown PIDs are also refused.
pub fn check_usb_id(vid: u16, pid: u16) -> Result<(), SafetyError> {
    if vid == VENDOR_ID && SUPPORTED_PIDS.contains(&pid) {
        Ok(())
    } else {
        Err(SafetyError::RefusedDevice(pid))
    }
}

/// Runtime context for context-dependent rules.
#[derive(Debug, Clone, Copy, Default)]
pub struct SafetyContext {
    /// Current sample rate in Hz, if known. Unknown is treated as unsafe for Delay.
    pub sample_rate: Option<u32>,
}

/// Memory areas the mic serves through the MemP transfer, from its firmware's handlers (read
/// handler at file offset 0x36c70c, write handler at 0x36c53c in UC's embedded firmware image):
/// `(PrsM, 0..=7)` are the factory presets (read-only), `(PrsM, 8..=15)` the user presets, and
/// `(Stat, 0..=3)` the four presets of the physical button.
pub fn mem_readable(a: (u32, u32)) -> bool {
    matches!(a, (tag::PRSM, 0..=15) | (tag::STAT, 0..=3))
}

/// The writable part of [`mem_readable`]: the user presets and the button slots.
pub fn mem_writable(a: (u32, u32)) -> bool {
    matches!(a, (tag::PRSM, 8..=15) | (tag::STAT, 0..=3))
}

/// Largest preset document a slot write may carry (factory slots are about 850 bytes).
pub const MAX_PRESET_BYTES: u32 = 4096;

/// The routing part of a payload: `op block index blob_tag ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub op: u32,
    pub block: u32,
    pub index: u32,
    pub blob_tag: Option<u32>,
    pub blob_size: Option<u32>,
    /// Length of the blob in bytes (payload beyond the 12-byte routing header).
    pub blob_len: usize,
    /// Para/Pari parameter id.
    pub param_id: Option<u32>,
    /// Raw 32-bit value (Para/Pari value, or VoFx model).
    pub value_bits: Option<u32>,
    /// MemP memory address `(addr0, addr1)`, e.g. `(PrsM, slot)`.
    pub mem_addr: Option<(u32, u32)>,
    /// MemP chunk fields `(offset, more, len, total)` and whether the data starts with `{`.
    pub mem_chunk: Option<MemChunkInfo>,
    /// Para/Pari field before the parameter id (`y`, the channel, for `Appl` writes).
    pub sub_index: Option<u32>,
}

/// The framing fields of a MemP write chunk (see `frame::memp_write`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemChunkInfo {
    pub offset: u32,
    pub more: bool,
    pub len: usize,
    pub total: u32,
    pub first_byte: u8,
}

fn rd(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
}

impl Command {
    /// Parse a frame payload (without the 8-byte frame header).
    pub fn parse(payload: &[u8]) -> Result<Self, SafetyError> {
        let (op, block, index) = match (rd(payload, 0), rd(payload, 4), rd(payload, 8)) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => return Err(SafetyError::Malformed),
        };
        let blob_tag = rd(payload, 12);
        let blob_size = rd(payload, 16);
        let scalar = matches!(blob_tag, Some(t) if t == tag::PARA || t == tag::PARI);
        let sub_index = if scalar { rd(payload, 20) } else { None };
        let (param_id, value_bits) = match blob_tag {
            Some(t) if t == tag::PARA || t == tag::PARI => (rd(payload, 24), rd(payload, 28)),
            Some(t) if t == tag::VOFX => (None, rd(payload, 24)),
            _ => (None, None),
        };
        let mem_addr = match blob_tag {
            Some(tag::MEMP) => rd(payload, 20).zip(rd(payload, 24)),
            _ => None,
        };
        // MemP blob inside the payload: offset at +30, more at +34, len at +35, total at +37,
        // data from +41 (blob offsets 0x12, 0x16, 0x17, 0x19, 0x1D after the 12-byte header
        // and the 8-byte blob header).
        let mem_chunk = match blob_tag {
            Some(tag::MEMP) => (|| {
                Some(MemChunkInfo {
                    offset: rd(payload, 30)?,
                    more: *payload.get(34)? != 0,
                    len: u16::from_le_bytes([*payload.get(35)?, *payload.get(36)?]) as usize,
                    total: rd(payload, 37)?,
                    first_byte: *payload.get(41)?,
                })
            })(),
            _ => None,
        };
        Ok(Self {
            op,
            block,
            index,
            blob_tag,
            blob_size,
            blob_len: payload.len().saturating_sub(12),
            param_id,
            value_bits,
            mem_addr,
            mem_chunk,
            sub_index,
        })
    }

    /// Static allow-list check.
    pub fn is_allowed(&self) -> bool {
        match self.op {
            tag::GETP => match (self.block, self.blob_tag) {
                (tag::APPL, Some(tag::JAST)) => self.index == 0,
                // Read a preset area: `GetP Appl 0 [MemP, 0x7EC, PrsM 0..=15 | Stat 0..=3, ...]`, sent as
                // the whole blob like UC does.
                (tag::APPL, Some(tag::MEMP)) => {
                    self.index == 0
                        && self.blob_size == Some(crate::frame::MEMP_BLOB_SIZE)
                        && self.blob_len == crate::frame::MEMP_BLOB_SIZE as usize
                        && self.mem_addr.is_some_and(mem_readable)
                }
                // Diagnostic read-back of a DSP block (Voice FX 201 / reverb 202).
                (201 | 202, Some(tag::VOFX)) => self.index == 0,
                (201 | 202, Some(t)) if t == crate::fx::tag::VRVB => self.index == 0,
                _ => false,
            },
            // Store a user preset or button slot: `SetP Appl 0 [MemP, 0x7EC, PrsM 8..=15 | Stat 0..=3, ...]`, one chunk of a
            // UBJSON document (starts with `{`), small, and consistent with its own framing.
            tag::SETP if self.block == tag::APPL && self.blob_tag == Some(tag::MEMP) => {
                self.index == 0
                    && self.blob_size == Some(crate::frame::MEMP_BLOB_SIZE)
                    && self.blob_len == crate::frame::MEMP_BLOB_SIZE as usize
                    && self.mem_addr.is_some_and(mem_writable)
                    && matches!(self.mem_chunk, Some(c)
                        if c.total >= 2
                            && c.total <= MAX_PRESET_BYTES
                            && c.len > 0
                            && c.len <= crate::frame::MEMP_CHUNK_MAX
                            && c.offset as u64 + c.len as u64 <= c.total as u64
                            && c.more == ((c.offset as usize + c.len) < c.total as usize)
                            && (c.offset > 0 || c.first_byte == b'{'))
            }
            tag::SETP if self.block == tag::APPL => {
                // Device-level writes: only Para/Pari scalars to an allow-listed selector, on
                // channel 0, with the parameter's own type and a value inside its range.
                self.index == 0
                    && self.sub_index == Some(0)
                    && self.blob_size == Some(PARAM_BLOB_SIZE)
                    && self.blob_len == PARAM_BLOB_SIZE as usize
                    && self.appl_value_ok()
            }
            tag::SETP if self.block == crate::mixer::BLOCK => {
                // Stream mixer crosspoint gains and bus mono switches (see `crate::mixer`).
                self.blob_size == Some(PARAM_BLOB_SIZE)
                    && self.blob_len == PARAM_BLOB_SIZE as usize
                    && match (
                        self.blob_tag,
                        self.sub_index,
                        self.param_id,
                        self.value_bits,
                    ) {
                        (Some(t), Some(y), Some(x), Some(bits)) => {
                            crate::mixer::write_ok(self.index, t, y, x, bits)
                        }
                        _ => false,
                    }
            }
            tag::SETP if self.block == crate::mixer::FX_BLOCK => {
                // Mic -> reverb send crosspoint, UC's literal address `SetP 0 0` (see `mixer`).
                self.blob_size == Some(PARAM_BLOB_SIZE)
                    && self.blob_len == PARAM_BLOB_SIZE as usize
                    && match (
                        self.blob_tag,
                        self.sub_index,
                        self.param_id,
                        self.value_bits,
                    ) {
                        (Some(t), Some(y), Some(x), Some(bits)) => {
                            crate::mixer::fx_write_ok(self.index, t, y, x, bits)
                        }
                        _ => false,
                    }
            }
            tag::SETP if self.block == crate::fx::blk::OPT => {
                // "Swap EQ/comp order": Pari x=0 y=0, value 0 or 1
                self.index <= 1
                    && self.blob_tag == Some(tag::PARI)
                    && self.blob_size == Some(PARAM_BLOB_SIZE)
                    && self.blob_len == PARAM_BLOB_SIZE as usize
                    && self.param_id == Some(0)
                    && matches!(self.value_bits, Some(0 | 1))
            }
            tag::SETP if self.block == block::VOICE_FX && self.blob_tag == Some(tag::VOFX) => {
                // model select: UC's six Voice FX models (0..=5)
                self.index == 0
                    && self.blob_size == Some(16)
                    && self.blob_len == 16
                    && matches!(self.value_bits, Some(0..=VOICEFX_MODEL_DELAY))
            }
            // No `VFxO` / `Setu` from the host: both are firmware-internal (see `fx::tag`), and a
            // host `Setu` can tear Voice FX down. `allowed_for` refuses them on every block.
            tag::SETP
                if crate::fx::blk::ALL.contains(&self.block)
                    && !matches!(self.blob_tag, Some(t) if t == tag::PARA || t == tag::PARI) =>
            {
                // DSP blocks: only the blob kinds UC's engines send, with their exact sizes, to
                // processing channel 0 or 1.
                self.index <= 1
                    && match (self.blob_tag, self.blob_size) {
                        (Some(t), Some(sz)) => {
                            crate::fx::tag::allowed_for(self.block, t)
                                && crate::fx::tag::size_of(t) == Some(sz)
                                && self.blob_len == sz as usize
                        }
                        _ => false,
                    }
            }
            _ => false,
        }
    }

    /// `Appl` scalar: known selector `x`, matching Para/Pari type, finite value in range.
    fn appl_value_ok(&self) -> bool {
        let (Some(x), Some(bits), Some(t)) = (self.param_id, self.value_bits, self.blob_tag) else {
            return false;
        };
        let Some(p) = crate::appl::ALL.iter().find(|p| p.x == x) else {
            return false;
        };
        let v = match (p.kind, t) {
            (crate::params::Kind::F32, tag::PARA) => f32::from_bits(bits),
            (crate::params::Kind::I32, tag::PARI) => bits as i32 as f32,
            _ => return false,
        };
        v.is_finite() && v >= p.min && v <= p.max
    }

    /// Full check: allow-list plus context-dependent hazards.
    pub fn check(&self, ctx: &SafetyContext) -> Result<(), SafetyError> {
        if !self.is_allowed() {
            return Err(SafetyError::NotAllowed(format!(
                "{} block={} tag={}",
                crate::frame::fourcc_str(self.op),
                self.block,
                self.blob_tag
                    .map(crate::frame::fourcc_str)
                    .unwrap_or_default()
            )));
        }
        if self.op == tag::SETP && self.block == block::VOICE_FX {
            let selects_delay = match self.blob_tag {
                Some(tag::VOFX) => self.value_bits == Some(VOICEFX_MODEL_DELAY),
                // the echo/delay engine's parameter blob counts as using Delay
                Some(crate::fx::tag::VECH) => true,
                Some(tag::PARI) => {
                    self.param_id == Some(FXMODEL_PARAM_ID)
                        && self.value_bits == Some(VOICEFX_MODEL_DELAY)
                }
                Some(tag::PARA) => {
                    self.param_id == Some(FXMODEL_PARAM_ID)
                        && self.value_bits.map(f32::from_bits) == Some(VOICEFX_MODEL_DELAY as f32)
                }
                _ => false,
            };
            if selects_delay {
                match ctx.sample_rate {
                    Some(r) if r < 88_200 => {}
                    Some(r) => return Err(SafetyError::DelayAtHighRate(r.to_string())),
                    None => return Err(SafetyError::DelayAtHighRate("unknown".into())),
                }
            }
        }
        Ok(())
    }

    /// Parse a payload and run the full check.
    pub fn check_payload(payload: &[u8], ctx: &SafetyContext) -> Result<Self, SafetyError> {
        let c = Self::parse(payload)?;
        c.check(ctx)?;
        if c.op == tag::SETP
            && crate::fx::blk::ALL.contains(&c.block)
            && !crate::fx::blob_values_ok(&payload[12..])
        {
            return Err(SafetyError::NotAllowed(format!(
                "DSP blob {} with a non-finite value or an unstable filter",
                c.blob_tag.map(crate::frame::fourcc_str).unwrap_or_default()
            )));
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::*;

    fn cmd(frame: &[u8]) -> Command {
        Command::parse(decode_frame(frame).unwrap().payload).unwrap()
    }

    #[test]
    fn allows_known_commands() {
        assert!(cmd(&get_state(1)).is_allowed());
        assert!(cmd(&set_appl(1, 3, 0, ParamValue::F32(30.0))).is_allowed());
        // the io24 numeric-block scalar writes were never confirmed: refused
        assert!(!cmd(&set_param(1, 101, 0, 5, ParamValue::F32(0.5))).is_allowed());
        // block 100 is the stream mixer: only crosspoint gains / mono (see `mixer`)
        assert!(cmd(&set_scalar(1, 100, 0, 5, 0, ParamValue::F32(-6.0))).is_allowed());
        assert!(!cmd(&set_scalar(1, 100, 0, 6, 0, ParamValue::F32(-6.0))).is_allowed());
        assert!(!cmd(&set_scalar(1, 100, 3, 5, 0, ParamValue::F32(-6.0))).is_allowed());
        assert!(!cmd(&set_scalar(1, 100, 0, 5, 0, ParamValue::F32(30.0))).is_allowed());
        assert!(cmd(&set_scalar(1, 100, 2, 8, 0, ParamValue::I32(1))).is_allowed());
        assert!(!cmd(&set_scalar(1, 100, 2, 8, 0, ParamValue::I32(2))).is_allowed());
        // block 0: only the mic -> reverb send crosspoint (index 0, y 0, x 1, Para)
        assert!(cmd(&set_scalar(1, 0, 0, 1, 0, ParamValue::F32(-20.0))).is_allowed());
        assert!(cmd(&set_scalar(1, 0, 0, 1, 0, ParamValue::F32(-145.0))).is_allowed());
        assert!(!cmd(&set_scalar(1, 0, 0, 1, 0, ParamValue::F32(21.0))).is_allowed());
        assert!(!cmd(&set_scalar(1, 0, 0, 0, 0, ParamValue::F32(-20.0))).is_allowed());
        assert!(!cmd(&set_scalar(1, 0, 1, 1, 0, ParamValue::F32(-20.0))).is_allowed());
        assert!(!cmd(&set_scalar(1, 0, 0, 1, 1, ParamValue::F32(-20.0))).is_allowed());
        assert!(!cmd(&set_scalar(1, 0, 0, 1, 0, ParamValue::I32(1))).is_allowed());
        assert!(!cmd(&set_blob(1, 0, 0, &[0u8; 20])).is_allowed());
        assert!(!cmd(&set_param(1, 202, 0, 5, ParamValue::I32(1))).is_allowed());
        assert!(!cmd(&set_param(1, 201, 0, FXMODEL_PARAM_ID, ParamValue::I32(2))).is_allowed());
    }

    #[test]
    fn preset_reads_only() {
        use crate::frame::memp_read;
        for slot in 0..=15 {
            assert!(cmd(&memp_read(1, tag::PRSM, slot, 7, 0)).is_allowed());
            assert!(cmd(&memp_read(1, tag::PRSM, slot, 7, 0x7CE)).is_allowed());
        }
        for slot in 0..=3 {
            assert!(cmd(&memp_read(1, tag::STAT, slot, 7, 0)).is_allowed());
        }
        // other slots / addresses
        assert!(!cmd(&memp_read(1, tag::PRSM, 16, 7, 0)).is_allowed());
        assert!(!cmd(&memp_read(1, tag::STAT, 4, 7, 0)).is_allowed());
        assert!(!cmd(&memp_read(1, tag::JAST, 1, 7, 0)).is_allowed());
        // the same blob as a write is refused
        let mut f = memp_read(1, tag::PRSM, 0, 7, 0);
        f[8..12].copy_from_slice(&tag::SETP.to_le_bytes());
        assert!(!cmd(&f).is_allowed());
        // truncated blob
        let f = memp_read(1, tag::PRSM, 0, 7, 0);
        let short = crate::frame::encode_frame(1, &f[8..100]).unwrap();
        assert!(!cmd(&short).is_allowed());
        // the old (wrong) layout: block MemP
        let mut f = memp_read(1, tag::PRSM, 0, 7, 0);
        f[12..16].copy_from_slice(&tag::MEMP.to_le_bytes());
        assert!(!cmd(&f).is_allowed());
    }

    #[test]
    fn appl_writes_limited_to_allow_list() {
        // preamp gain (x=3) and mute (x=7) are allowed
        assert!(cmd(&set_appl(1, 3, 0, ParamValue::F32(30.0))).is_allowed());
        assert!(cmd(&set_appl(1, 7, 0, ParamValue::I32(1))).is_allowed());
        // presetButtonMode (x=17) and unknown selectors are not
        // presetButtonMode 0..2, muteButtonMode 0..1
        assert!(cmd(&set_appl(1, 17, 0, ParamValue::I32(2))).is_allowed());
        assert!(!cmd(&set_appl(1, 17, 0, ParamValue::I32(3))).is_allowed());
        assert!(cmd(&set_appl(1, 8, 0, ParamValue::I32(0))).is_allowed());
        assert!(!cmd(&set_appl(1, 8, 0, ParamValue::I32(2))).is_allowed());
        // phonesSrc (x=11): not exposed
        assert!(!cmd(&set_appl(1, 11, 0, ParamValue::I32(0))).is_allowed());
        assert!(!cmd(&set_appl(1, 999, 0, ParamValue::F32(0.0))).is_allowed());
        // Appl SetP with a non-zero block index is refused
        let mut f = set_appl(1, 3, 0, ParamValue::F32(1.0));
        f[16] = 1;
        assert!(!cmd(&f).is_allowed());
        // channel 1, wrong type, out of range, NaN
        assert!(!cmd(&set_appl(1, 3, 1, ParamValue::F32(30.0))).is_allowed());
        assert!(!cmd(&set_appl(1, 3, 0, ParamValue::I32(30))).is_allowed());
        assert!(!cmd(&set_appl(1, 7, 0, ParamValue::F32(1.0))).is_allowed());
        assert!(!cmd(&set_appl(1, 3, 0, ParamValue::F32(60.5))).is_allowed());
        assert!(!cmd(&set_appl(1, 3, 0, ParamValue::F32(f32::NAN))).is_allowed());
        assert!(!cmd(&set_appl(1, 16, 0, ParamValue::I32(4))).is_allowed());
        assert!(!cmd(&set_appl(1, 7, 0, ParamValue::I32(-1))).is_allowed());
        assert!(!cmd(&set_appl(1, 1, 0, ParamValue::F32(1.5))).is_allowed());
    }

    #[test]
    fn rejects_unknown_block_tag_op() {
        assert!(!cmd(&set_param(1, 999, 0, 5, ParamValue::F32(0.5))).is_allowed());
        let mut p = Vec::new();
        for x in [fourcc(b"DFU!"), 0, 0, 0] {
            p.extend_from_slice(&u32::to_le_bytes(x));
        }
        let c = Command::parse(&p).unwrap();
        assert!(!c.is_allowed());
        assert!(matches!(
            c.check(&SafetyContext::default()),
            Err(SafetyError::NotAllowed(_))
        ));
        // SetP with an unknown blob tag
        let mut p = Vec::new();
        for x in [tag::SETP, 100, 0, fourcc(b"Rset"), 16, 0, 0, 0] {
            p.extend_from_slice(&u32::to_le_bytes(x));
        }
        assert!(!Command::parse(&p).unwrap().is_allowed());
        assert_eq!(Command::parse(&[1, 2]), Err(SafetyError::Malformed));
    }

    #[test]
    fn dsp_blobs_limited_to_known_blocks_tags_sizes() {
        use crate::fx::{self, blk, FxState};
        let st = FxState::default();
        for w in st.all_writes(48_000.0) {
            assert!(
                cmd(&set_blob(1, w.block, 0, &w.blob)).is_allowed(),
                "{}",
                fx::describe(&w)
            );
            // wrong channel
            assert!(!cmd(&set_blob(1, w.block, 7, &w.blob)).is_allowed());
            // truncated
            assert!(!cmd(&set_blob(1, w.block, 0, &w.blob[..w.blob.len() - 4])).is_allowed());
        }
        // NaN coefficient / unstable pole refused by the value check
        let eq = st.writes(fx::Engine::Eq, 48_000.0);
        let bq = eq
            .iter()
            .find(|w| w.blob[0..4] == fx::tag::BQDF.to_le_bytes())
            .unwrap();
        let ctx = SafetyContext::default();
        let ok = set_blob(1, bq.block, 0, &bq.blob);
        assert!(Command::check_payload(decode_frame(&ok).unwrap().payload, &ctx).is_ok());
        for (off, v) in [(20, f32::NAN), (28, -1.5f32), (20, 2.5)] {
            let mut b = bq.blob.clone();
            b[off..off + 4].copy_from_slice(&v.to_le_bytes());
            let f = set_blob(1, bq.block, 0, &b);
            assert!(
                Command::check_payload(decode_frame(&f).unwrap().payload, &ctx).is_err(),
                "{off} {v}"
            );
        }
        // right blob, wrong block
        let lim = st.writes(fx::Engine::Limiter, 48_000.0).remove(0);
        assert!(!cmd(&set_blob(1, blk::EQ, 0, &lim.blob)).is_allowed());
        // unknown tag on a DSP block
        let mut bad = lim.blob.clone();
        bad[0..4].copy_from_slice(&fourcc(b"FRst").to_le_bytes());
        assert!(!cmd(&set_blob(1, blk::LIMITER, 0, &bad)).is_allowed());
    }

    #[test]
    fn echo_blob_needs_known_low_rate() {
        use crate::fx::FxState;
        let mut st = FxState::default();
        st.set("voicefx.model", 5.0).unwrap();
        let w = st.writes(crate::fx::Engine::VoiceFx, 48_000.0);
        let vech = cmd(&set_blob(1, w[1].block, 0, &w[1].blob));
        assert_eq!(w[1].blob[0..4], crate::fx::tag::VECH.to_le_bytes());
        assert!(vech.is_allowed());
        assert!(vech.check(&SafetyContext { sample_rate: None }).is_err());
        assert!(vech
            .check(&SafetyContext {
                sample_rate: Some(96_000)
            })
            .is_err());
        assert!(vech
            .check(&SafetyContext {
                sample_rate: Some(48_000)
            })
            .is_ok());
        let sel = cmd(&set_blob(1, w[0].block, 0, &w[0].blob));
        assert!(sel.check(&SafetyContext { sample_rate: None }).is_err());
        // VoFx must go to index 0
        assert!(!cmd(&set_blob(1, w[0].block, 1, &w[0].blob)).is_allowed());
    }

    #[test]
    fn every_model_write_is_allowed() {
        use crate::fx::{Engine, FxState};
        let ctx = SafetyContext {
            sample_rate: Some(48_000),
        };
        for (key, models) in [("eq.model", 0..=2), ("voicefx.model", 0..=5)] {
            for m in models {
                let mut st = FxState::default();
                st.set(key, m as f32).unwrap();
                st.set("eq.swap", 1.0).unwrap();
                for e in [Engine::Eq, Engine::VoiceFx, Engine::Order] {
                    for w in st.writes(e, 48_000.0) {
                        let c = cmd(&set_blob(1, w.block, 0, &w.blob));
                        assert!(c.check(&ctx).is_ok(), "{key}={m} {e:?} {:?}", &w.blob[0..4]);
                    }
                }
            }
        }
        // opt: only Pari x=0 with 0/1
        let mut st = FxState::default();
        st.set("eq.swap", 1.0).unwrap();
        let mut b = st.writes(Engine::Order, 48_000.0).remove(0).blob;
        b[16..20].copy_from_slice(&2u32.to_le_bytes());
        assert!(!cmd(&set_blob(1, crate::fx::blk::OPT, 0, &b)).is_allowed());
    }

    /// Every parameter at its min, max and default, for every model and rate, produces blobs
    /// that pass the full check including the value/stability check.
    #[test]
    fn value_check_accepts_every_legit_setting() {
        use crate::fx::{self, Engine, FxState, MULTI_RATES, PARAMS};
        let ctx = SafetyContext {
            sample_rate: Some(48_000),
        };
        let engines = [
            Engine::Order,
            Engine::Hpf,
            Engine::Gate,
            Engine::Comp,
            Engine::Eq,
            Engine::Limiter,
            Engine::Reverb,
            Engine::VoiceFx,
        ];
        let mut n = 0;
        for p in PARAMS {
            for v in [p.min, p.max, p.default] {
                for (mk, models) in [
                    ("eq.model", 0..=2),
                    ("voicefx.model", 0..=5),
                    ("comp.model", 0..=2),
                ] {
                    for m in models {
                        let mut st = FxState::default();
                        st.set(mk, m as f32).unwrap();
                        st.set(p.key, v).unwrap();
                        for fs in MULTI_RATES {
                            for e in engines {
                                for w in st.writes(e, fs) {
                                    let f = set_blob(1, w.block, 0, &w.blob);
                                    let r = Command::check_payload(
                                        decode_frame(&f).unwrap().payload,
                                        &ctx,
                                    );
                                    assert!(
                                        r.is_ok(),
                                        "{}={v} {mk}={m} fs={fs}: {} {r:?}",
                                        p.key,
                                        fx::describe(&w)
                                    );
                                    n += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(n > 10_000);
    }

    #[test]
    fn rejects_wrong_blob_length() {
        let f = set_appl(1, 3, 0, ParamValue::F32(30.0));
        let mut payload = decode_frame(&f).unwrap().payload.to_vec();
        payload.push(0);
        assert!(!Command::parse(&payload).unwrap().is_allowed());
    }

    #[test]
    fn delay_blocked_at_high_rates() {
        let f = set_voicefx_model(1, block::VOICE_FX, VOICEFX_MODEL_DELAY);
        let c = cmd(&f);
        assert!(c.is_allowed());
        for r in [88_200, 96_000] {
            assert!(c
                .check(&SafetyContext {
                    sample_rate: Some(r)
                })
                .is_err());
        }
        assert!(c.check(&SafetyContext { sample_rate: None }).is_err());
        assert!(c
            .check(&SafetyContext {
                sample_rate: Some(48_000)
            })
            .is_ok());
        // other models fine at 96k
        let ok = cmd(&set_voicefx_model(1, block::VOICE_FX, 2));
        assert!(ok
            .check(&SafetyContext {
                sample_rate: Some(96_000)
            })
            .is_ok());
        // no such model
        assert!(!cmd(&set_voicefx_model(1, block::VOICE_FX, 6)).is_allowed());
    }

    #[test]
    fn preset_slot_writes_are_tightly_checked() {
        use crate::frame::memp_write;
        let doc = b"{iaSib}".to_vec();
        let ok = |slot, off, total, data: &[u8]| {
            cmd(&memp_write(1, (tag::PRSM, slot), 7, off, total, data)).is_allowed()
        };
        let n = doc.len() as u32;
        // user presets 8..=15 of PrsM
        assert!(ok(8, 0, n, &doc));
        assert!(ok(15, 0, n, &doc));
        // the factory presets are read-only, and there is no PrsM 16
        assert!(!ok(0, 0, n, &doc));
        assert!(!ok(7, 0, n, &doc));
        assert!(!ok(16, 0, n, &doc));
        // the four button slots
        let stat =
            |i, d: &[u8]| cmd(&memp_write(1, (tag::STAT, i), 7, 0, d.len() as u32, d)).is_allowed();
        assert!(stat(0, &doc) && stat(3, &doc));
        assert!(!stat(4, &doc));
        // other memory, empty or oversized or inconsistent chunks, no document
        assert!(!cmd(&memp_write(1, (tag::JAST, 1), 7, 0, n, &doc)).is_allowed());
        assert!(!ok(8, 0, n, &[]));
        assert!(!ok(8, 0, MAX_PRESET_BYTES + 1, &doc));
        assert!(!ok(8, 0, n - 1, &doc));
        assert!(!ok(8, 0, n, b"x{ab"));
        // a later chunk need not start with `{`
        assert!(ok(9, 5, 5 + 3, b"abc"));
        // the first chunk of a two-chunk document: more = 1
        let big = vec![b'{'; 3000];
        assert!(ok(10, 0, 3000, &big[..crate::frame::MEMP_CHUNK_MAX]));
    }

    #[test]
    fn usb_ids() {
        assert!(check_usb_id(0x194F, 0x0D04).is_ok());
        assert!(check_usb_id(0x194F, 0x0D0A).is_ok());
        assert!(check_usb_id(0x194F, 0x0D06).is_err());
        assert!(check_usb_id(0x194F, 0x0405).is_err());
        assert!(check_usb_id(0x1234, 0x0D04).is_err());
    }
}

//! Wire framing: FourCC tags, frame encode/decode, command builders and reply parsing.
//!
//! Frame layout: `u16 LE total length (payload + 8)`, `0x01`, `dir`, `uid`, `0,0,0`, payload.
//! `dir` is `0x01` for host commands and `0x81` for device replies (seen on a Revelator Dynamic).
//! Payload layout: `op(u32) block(u32) block_index(u32) blob...`, all little-endian.
//! FourCCs are the ASCII bytes reversed on the wire, i.e. `u32::from_be_bytes(b"GetP")`
//! stored little-endian.

use thiserror::Error;

/// Build a FourCC value, e.g. `fourcc(b"GetP") == 0x47657450`.
pub const fn fourcc(b: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*b)
}

/// Render a FourCC value back to text (non-printable bytes become `?`).
pub fn fourcc_str(v: u32) -> String {
    v.to_be_bytes()
        .iter()
        .map(|&c| {
            if (0x20..0x7f).contains(&c) {
                c as char
            } else {
                '?'
            }
        })
        .collect()
}

/// Known FourCC constants.
pub mod tag {
    use super::fourcc;
    pub const GETP: u32 = fourcc(b"GetP");
    pub const SETP: u32 = fourcc(b"SetP");
    pub const RPLY: u32 = fourcc(b"Rply");
    pub const APPL: u32 = fourcc(b"Appl");
    pub const JAST: u32 = fourcc(b"JaSt");
    pub const MEMP: u32 = fourcc(b"MemP");
    pub const PARA: u32 = fourcc(b"Para");
    pub const PARI: u32 = fourcc(b"Pari");
    pub const STAT: u32 = fourcc(b"Stat");
    pub const PRSM: u32 = fourcc(b"PrsM");
    pub const VOFX: u32 = fourcc(b"VoFx");
}

pub const HEADER_LEN: usize = 8;
/// Header byte 3 of a host-to-device frame.
pub const DIR_COMMAND: u8 = 0x01;
/// Header byte 3 of a device-to-host frame.
pub const DIR_REPLY: u8 = 0x81;
/// Offset of the first f32 value in a full JaSt reply frame (header included).
pub const DATA_START: usize = 0x1C;
/// Size field of the JaSt request blob.
pub const JAST_BLOB_SIZE: u32 = 0x7EC;
/// JaSt size used for polling: the Dynamic answers with a 240-byte reply (one USB packet)
/// holding slots 0..=52, which covers every slot in use (42..=48). Verified on hardware.
pub const STATE_POLL_SIZE: u32 = 0xDC;
/// Size field of a Para/Pari scalar blob.
pub const PARAM_BLOB_SIZE: u32 = 0x14;
/// Size of a MemP (chunked memory transfer) blob; UC always sends the whole blob.
pub const MEMP_BLOB_SIZE: u32 = 0x7EC;
/// Offset of the data bytes inside a MemP blob (after tag and size).
const MEMP_DATA: usize = 0x1D;
/// Data bytes a MemP chunk can carry.
pub const MEMP_CHUNK_MAX: usize = 0x7CE;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame too short ({0} bytes)")]
    TooShort(usize),
    #[error("length field {declared} does not match frame size {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("bad header magic")]
    BadMagic,
    #[error("payload too large")]
    TooLarge,
    #[error("not a reply (tag {0})")]
    NotReply(String),
}

/// A decoded frame borrowing its payload.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub uid: u8,
    pub payload: &'a [u8],
}

/// Wrap `payload` in a frame header.
pub fn encode_frame(uid: u8, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    encode_frame_dir(uid, DIR_COMMAND, payload)
}

fn encode_frame_dir(uid: u8, dir: u8, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    let total = payload.len() + HEADER_LEN;
    let total16 = u16::try_from(total).map_err(|_| FrameError::TooLarge)?;
    let mut v = Vec::with_capacity(total);
    v.extend_from_slice(&total16.to_le_bytes());
    v.extend_from_slice(&[0x01, dir, uid, 0, 0, 0]);
    v.extend_from_slice(payload);
    Ok(v)
}

/// Split a frame into uid + payload, validating length and magic (either direction).
pub fn decode_frame(bytes: &[u8]) -> Result<Frame<'_>, FrameError> {
    if bytes.len() < HEADER_LEN {
        return Err(FrameError::TooShort(bytes.len()));
    }
    let declared = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    if declared != bytes.len() {
        return Err(FrameError::LengthMismatch {
            declared,
            actual: bytes.len(),
        });
    }
    if bytes[2] != 1 || !(bytes[3] == DIR_COMMAND || bytes[3] == DIR_REPLY) {
        return Err(FrameError::BadMagic);
    }
    Ok(Frame {
        uid: bytes[4],
        payload: &bytes[HEADER_LEN..],
    })
}

/// Unique-id generator: 1..=255, wrapping, never 0.
#[derive(Debug, Default, Clone)]
pub struct UidGen(u8);

impl UidGen {
    pub fn new() -> Self {
        Self(0)
    }
    pub fn next_uid(&mut self) -> u8 {
        self.0 = if self.0 == 255 { 1 } else { self.0 + 1 };
        self.0
    }
}

/// A scalar value for a Para (f32) or Pari (i32) write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamValue {
    F32(f32),
    I32(i32),
}

fn put(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn finish(uid: u8, payload: Vec<u8>) -> Vec<u8> {
    encode_frame(uid, &payload).expect("builder payloads are small")
}

/// `GetP Appl 0` + blob `[JaSt, 0x7EC, 0, 0]`: request the full state snapshot.
pub fn get_state(uid: u8) -> Vec<u8> {
    get_state_sized(uid, JAST_BLOB_SIZE)
}

/// [`get_state`] with an explicit JaSt blob size field (for probing; the right value for the
/// Revelator Dynamic is not yet confirmed).
pub fn get_state_sized(uid: u8, blob_size: u32) -> Vec<u8> {
    let mut p = Vec::new();
    for x in [tag::GETP, tag::APPL, 0, tag::JAST, blob_size, 0, 0] {
        put(&mut p, x);
    }
    finish(uid, p)
}

/// `GetP block 0 [tag, size, 0, 0]`: read back a DSP block's stored blob (diagnostic).
pub fn get_dsp(uid: u8, block: u32, blob_tag: u32, blob_size: u32) -> Vec<u8> {
    let mut p = Vec::new();
    for x in [tag::GETP, block, 0, blob_tag, blob_size, 0, 0] {
        put(&mut p, x);
    }
    finish(uid, p)
}

/// `SetP block block_index` + blob `[Para|Pari, 0x14, index, param_id, value]`.
/// The blob `index` field is set equal to `block_index`.
pub fn set_param(
    uid: u8,
    block: u32,
    block_index: u32,
    param_id: u32,
    value: ParamValue,
) -> Vec<u8> {
    let mut p = Vec::new();
    let (t, bits) = match value {
        ParamValue::F32(f) => (tag::PARA, f.to_bits()),
        ParamValue::I32(i) => (tag::PARI, i as u32),
    };
    for x in [
        tag::SETP,
        block,
        block_index,
        t,
        PARAM_BLOB_SIZE,
        block_index,
        param_id,
        bits,
    ] {
        put(&mut p, x);
    }
    finish(uid, p)
}

/// `SetP Appl 0` + blob `[Para|Pari, 0x14, y, x, value]`: write a device-level parameter
/// (layout from Universal Control's dspusbdevice.dll; see `crate::appl`).
pub fn set_appl(uid: u8, x: u32, y: u32, value: ParamValue) -> Vec<u8> {
    set_scalar(uid, tag::APPL, 0, x, y, value)
}

/// `SetP block index` + blob `[Para|Pari, 0x14, y, x, value]`: UC's scalar senders
/// (0x180054920 float, 0x1800548a0 int) for a binding `{block, index, x, y}`.
pub fn set_scalar(uid: u8, block: u32, index: u32, x: u32, y: u32, value: ParamValue) -> Vec<u8> {
    let mut p = Vec::new();
    let (t, bits) = match value {
        ParamValue::F32(f) => (tag::PARA, f.to_bits()),
        ParamValue::I32(i) => (tag::PARI, i as u32),
    };
    for v in [tag::SETP, block, index, t, PARAM_BLOB_SIZE, y, x, bits] {
        put(&mut p, v);
    }
    finish(uid, p)
}

/// `SetP <block> <index> <blob>`: send a raw DSP blob (already holding its tag and size) to a
/// processing block. Layout from UC's frame builder (0x180055a60).
pub fn set_blob(uid: u8, block: u32, index: u32, blob: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(12 + blob.len());
    for v in [tag::SETP, block, index] {
        put(&mut p, v);
    }
    p.extend_from_slice(blob);
    finish(uid, p)
}

/// `SetP 201 0 [VoFx, 16, 0, model]`: select the Voice FX model. (Layout from the io24 notes;
/// unverified on the Dynamic.)
pub fn set_voicefx_model(uid: u8, block: u32, model: u32) -> Vec<u8> {
    let mut p = Vec::new();
    for x in [tag::SETP, block, 0, tag::VOFX, 16, 0, model] {
        put(&mut p, x);
    }
    finish(uid, p)
}

/// `GetP Appl 0` + MemP blob: ask for the chunk of memory `(addr0, addr1)` starting at
/// `offset` (e.g. preset slot n is `(PrsM, n)`). `id` is the host's transfer number; UC
/// increments it per transfer and repeats it in follow-up requests. Layout from UC's
/// dspusbdevice.dll (@0x180052bd3 first request, @0x180052f10 follow-ups):
/// `[MemP, 0x7EC, addr0, addr1, u16 id, u32 offset, u8 more=0, u16 len=0, u32 total=0, zeros]`.
pub fn memp_read(uid: u8, addr0: u32, addr1: u32, id: u16, offset: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(12 + MEMP_BLOB_SIZE as usize);
    for v in [
        tag::GETP,
        tag::APPL,
        0,
        tag::MEMP,
        MEMP_BLOB_SIZE,
        addr0,
        addr1,
    ] {
        put(&mut p, v);
    }
    p.extend_from_slice(&id.to_le_bytes());
    p.extend_from_slice(&offset.to_le_bytes());
    p.resize(12 + MEMP_BLOB_SIZE as usize, 0);
    finish(uid, p)
}

/// `SetP Appl 0` + MemP blob: write one chunk (`data`, at most [`MEMP_CHUNK_MAX`] bytes) of the
/// `total`-byte object at `offset` to memory `(addr0, addr1)`. Same blob layout as a read reply
/// (UC's writer, @0x1800528c0): `[MemP, 0x7EC, addr0, addr1, u16 id, u32 offset, u8 more,
/// u16 len, u32 total, data, zeros]`, always the whole 0x7EC blob.
pub fn memp_write(
    uid: u8,
    addr: (u32, u32),
    id: u16,
    offset: u32,
    total: u32,
    data: &[u8],
) -> Vec<u8> {
    let more = (offset as usize + data.len()) < total as usize;
    let mut p = Vec::with_capacity(12 + MEMP_BLOB_SIZE as usize);
    for v in [tag::SETP, tag::APPL, 0, tag::MEMP, MEMP_BLOB_SIZE] {
        put(&mut p, v);
    }
    p.extend_from_slice(&memp_reply_blob(addr, id, offset, more, total, data));
    finish(uid, p)
}

/// One chunk of a MemP transfer, as the device returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemChunk {
    pub addr: (u32, u32),
    pub id: u16,
    pub offset: u32,
    /// The device has more data after this chunk.
    pub more: bool,
    /// Total size of the transferred object (0 when the device leaves it out).
    pub total: u32,
    pub data: Vec<u8>,
}

impl MemChunk {
    /// Decode the MemP blob of a reply (`r.data` starts right after the blob tag and size).
    pub fn from_reply(r: &Reply) -> Option<Self> {
        let (t, _) = r.blob?;
        if t != tag::MEMP {
            return None;
        }
        // offsets inside the blob, minus the 8-byte tag/size header
        let d = &r.data;
        let u16_at = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32_at(b, 0));
        let len = u16_at(0x0F)? as usize;
        let start = MEMP_DATA - 8;
        if len > MEMP_CHUNK_MAX {
            return None;
        }
        Some(Self {
            addr: (u32_at(0)?, u32_at(4)?),
            id: u16_at(8)?,
            offset: u32_at(0x0A)?,
            more: *d.get(0x0E)? == 1,
            total: u32_at(0x11)?,
            data: d.get(start..start + len)?.to_vec(),
        })
    }
}

/// Blob bytes of a MemP reply chunk (for the mock device and tests).
pub fn memp_reply_blob(
    addr: (u32, u32),
    id: u16,
    offset: u32,
    more: bool,
    total: u32,
    data: &[u8],
) -> Vec<u8> {
    let mut b = Vec::with_capacity(MEMP_BLOB_SIZE as usize - 8);
    b.extend_from_slice(&addr.0.to_le_bytes());
    b.extend_from_slice(&addr.1.to_le_bytes());
    b.extend_from_slice(&id.to_le_bytes());
    b.extend_from_slice(&offset.to_le_bytes());
    b.push(more as u8);
    b.extend_from_slice(&(data.len() as u16).to_le_bytes());
    b.extend_from_slice(&total.to_le_bytes());
    b.extend_from_slice(data);
    b.resize(MEMP_BLOB_SIZE as usize - 8, 0);
    b
}

/// A parsed device reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub uid: u8,
    pub block: u32,
    pub index: u32,
    /// Blob tag and declared size, when the reply carries a blob.
    pub blob: Option<(u32, u32)>,
    /// Bytes following the blob header (for JaSt: the f32 snapshot, starting at [`DATA_START`]).
    pub data: Vec<u8>,
}

impl Reply {
    /// Little-endian f32 values in `data`.
    #[allow(clippy::chunks_exact_to_as_chunks)]
    pub fn floats(&self) -> Vec<f32> {
        self.data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Strip the header, verify the `Rply` tag and split out block/index/blob/data.
pub fn parse_reply(bytes: &[u8]) -> Result<Reply, FrameError> {
    let f = decode_frame(bytes)?;
    if f.payload.len() < 12 {
        return Err(FrameError::TooShort(bytes.len()));
    }
    let t = u32_at(f.payload, 0);
    if t != tag::RPLY {
        return Err(FrameError::NotReply(fourcc_str(t)));
    }
    let (blob, data) = if f.payload.len() >= 20 {
        (
            Some((u32_at(f.payload, 12), u32_at(f.payload, 16))),
            f.payload[20..].to_vec(),
        )
    } else {
        (None, Vec::new())
    };
    Ok(Reply {
        uid: f.uid,
        block: u32_at(f.payload, 4),
        index: u32_at(f.payload, 8),
        blob,
        data,
    })
}

/// Build a reply frame as the device sends it (used by the mock device).
pub fn build_reply(
    uid: u8,
    block: u32,
    index: u32,
    blob_tag: u32,
    blob_size: u32,
    data: &[u8],
) -> Vec<u8> {
    let mut p = Vec::new();
    for x in [tag::RPLY, block, index, blob_tag, blob_size] {
        put(&mut p, x);
    }
    p.extend_from_slice(data);
    encode_frame_dir(uid, DIR_REPLY, &p).expect("reply payloads are small")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_values() {
        assert_eq!(tag::GETP, 0x47657450);
        assert_eq!(tag::SETP, 0x53657450);
        assert_eq!(tag::RPLY, 0x52706C79);
        assert_eq!(fourcc_str(tag::JAST), "JaSt");
        // wire order is reversed ASCII
        assert_eq!(tag::GETP.to_le_bytes(), *b"PteG");
    }

    #[test]
    fn golden_get_state() {
        let f = get_state(7);
        let mut want = vec![0x24, 0x00, 0x01, 0x01, 0x07, 0, 0, 0];
        want.extend_from_slice(b"PteG");
        want.extend_from_slice(b"lppA");
        want.extend_from_slice(&[0, 0, 0, 0]);
        want.extend_from_slice(b"tSaJ");
        want.extend_from_slice(&[0xEC, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(f, want);
    }

    #[test]
    fn golden_set_param_f32() {
        let f = set_param(1, 100, 2, 0x15, ParamValue::F32(1.0));
        let mut want = vec![0x28, 0, 1, 1, 1, 0, 0, 0];
        want.extend_from_slice(b"PteS");
        want.extend_from_slice(&[100, 0, 0, 0, 2, 0, 0, 0]);
        want.extend_from_slice(b"araP");
        want.extend_from_slice(&[0x14, 0, 0, 0, 2, 0, 0, 0, 0x15, 0, 0, 0, 0, 0, 0x80, 0x3F]);
        assert_eq!(f, want);
    }

    #[test]
    fn golden_set_appl() {
        // preamp gain 30 dB on channel 0: SetP Appl 0 [Para, 0x14, y=0, x=3, 30.0]
        let f = set_appl(5, 3, 0, ParamValue::F32(30.0));
        let mut want = vec![0x28, 0, 1, 1, 5, 0, 0, 0];
        want.extend_from_slice(b"PteS");
        want.extend_from_slice(b"lppA");
        want.extend_from_slice(&[0, 0, 0, 0]);
        want.extend_from_slice(b"araP");
        want.extend_from_slice(&[0x14, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0]);
        want.extend_from_slice(&30.0f32.to_le_bytes());
        assert_eq!(f, want);
    }

    #[test]
    fn set_param_i32_uses_pari() {
        let f = set_param(1, 100, 0, 3, ParamValue::I32(-1));
        let d = decode_frame(&f).unwrap();
        assert_eq!(u32_at(d.payload, 12), tag::PARI);
        assert_eq!(&d.payload[28..32], &[0xFF; 4]);
    }

    #[test]
    fn uid_wraps_and_skips_zero() {
        let mut g = UidGen::new();
        assert_eq!(g.next_uid(), 1);
        for _ in 0..253 {
            g.next_uid();
        }
        assert_eq!(g.next_uid(), 255);
        assert_eq!(g.next_uid(), 1);
    }

    #[test]
    fn decode_errors() {
        assert_eq!(decode_frame(&[1, 2]), Err(FrameError::TooShort(2)));
        assert!(matches!(
            decode_frame(&[9, 0, 1, 1, 1, 0, 0, 0]),
            Err(FrameError::LengthMismatch { .. })
        ));
        assert_eq!(
            decode_frame(&[8, 0, 2, 1, 1, 0, 0, 0]),
            Err(FrameError::BadMagic)
        );
    }

    #[test]
    fn reply_roundtrip() {
        let mut data = Vec::new();
        for f in [1.5f32, -2.0] {
            data.extend_from_slice(&f.to_le_bytes());
        }
        let r = build_reply(9, tag::APPL, 0, tag::JAST, JAST_BLOB_SIZE, &data);
        assert_eq!(&r[0..2], &[(DATA_START + 8) as u8, 0]);
        // floats start at DATA_START
        assert_eq!(&r[DATA_START..DATA_START + 4], &1.5f32.to_le_bytes());
        let p = parse_reply(&r).unwrap();
        assert_eq!(p.uid, 9);
        assert_eq!(p.blob, Some((tag::JAST, JAST_BLOB_SIZE)));
        assert_eq!(p.floats(), vec![1.5, -2.0]);
        // non-reply rejected
        assert!(matches!(
            parse_reply(&get_state(1)),
            Err(FrameError::NotReply(_))
        ));
    }

    /// Head of a real reply captured from a Revelator Dynamic (GetP Appl JaSt 0xDC, uid 2).
    #[test]
    fn parses_captured_hardware_reply() {
        let mut r = vec![0xF0, 0x00, 0x01, 0x81, 0x02, 0, 0, 0];
        r.extend_from_slice(b"ylpR");
        r.extend_from_slice(b"lppA");
        r.extend_from_slice(&[0, 0, 0, 0]);
        r.extend_from_slice(b"tSaJ");
        r.extend_from_slice(&[0xDC, 0, 0, 0]);
        r.resize(0xF0, 0);
        let p = parse_reply(&r).unwrap();
        assert_eq!(p.uid, 2);
        assert_eq!(p.block, tag::APPL);
        assert_eq!(p.blob, Some((tag::JAST, 0xDC)));
        assert_eq!(p.data.len(), 0xF0 - DATA_START);
    }
}

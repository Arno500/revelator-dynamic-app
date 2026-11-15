//! DSP processing blocks, ported from Universal Control's `hwaccess/dspusbdevice.dll`.
//!
//! The Revelator's DSP does not take knob values. UC runs one "engine" per block on the PC: it
//! turns the user parameters (frequency, Q, threshold, ...) into low-level DSP data (biquad
//! coefficients, linear gains, time constants) and sends that as a blob:
//!
//! `SetP <block> <processing channel> [tag, size, engine id, fields...]`
//!
//! `block` is the object id (`eq  `, `comp`, `gate`, `lim `, `filt` as FourCCs), the engine id
//! is 0, and the processing channel is 0 on the single-input Dynamic. Every blob carries the
//! whole block state, so a change to one parameter resends the block. See
//! `docs/uc-dll-notes.md` ("DSP blocks") for the addresses of each routine ported here.
//!
//! Everything here is pure: no I/O.

/// Block (object) ids: the `block` field of the SetP routing header.
pub mod blk {
    use crate::frame::fourcc;
    pub const EQ: u32 = fourcc(b"eq  ");
    pub const COMP: u32 = fourcc(b"comp");
    pub const GATE: u32 = fourcc(b"gate");
    pub const LIMITER: u32 = fourcc(b"lim ");
    pub const HPF: u32 = fourcc(b"filt");
    /// Voice FX (numeric block id, not a FourCC).
    pub const VOICE_FX: u32 = 201;
    pub const REVERB: u32 = 202;
    /// EQ/compressor selector (`opt `): only the "swap EQ/comp order" switch goes to the device.
    pub const OPT: u32 = fourcc(b"opt ");
    pub const ALL: [u32; 7] = [EQ, COMP, GATE, LIMITER, HPF, VOICE_FX, REVERB];
}

/// Blob tags sent to those blocks, with their fixed sizes.
pub mod tag {
    use crate::frame::fourcc;
    /// One biquad at the current sample rate.
    pub const BQDF: u32 = fourcc(b"Bqdf");
    /// One biquad precomputed for each supported sample rate.
    pub const MBDF: u32 = fourcc(b"MBdf");
    pub const CPXT: u32 = fourcc(b"cpxt");
    pub const GATE: u32 = fourcc(b"gate");
    pub const LIM: u32 = fourcc(b"lim ");
    /// Integer parameter write (`Pari`), used for the `opt ` block.
    pub const PARI_TAG: u32 = fourcc(b"Pari");
    /// Voice FX echo/delay (model 5).
    pub const VECH: u32 = fourcc(b"vech");
    /// Voice FX doubler (model 0).
    pub const VDOG: u32 = fourcc(b"vdog");
    pub const VRVB: u32 = fourcc(b"vrvb");
    /// Voice FX model select.
    pub const VOFX: u32 = fourcc(b"VoFx");
    /// 7-coefficient (3rd-order) filter at the current rate (passive/vintage EQ low/high shelf).
    pub const LFDF: u32 = fourcc(b"Lfdf");
    /// `Lfdf` precomputed for each supported rate.
    pub const MLFF: u32 = fourcc(b"MLff");
    /// Voice FX detune (model 1).
    pub const MAY4: u32 = fourcc(b"may4");
    /// Voice FX vocoder parameters (model 2).
    pub const BOTA: u32 = fourcc(b"bota");
    /// Voice FX vocoder filter bank (model 2).
    pub const INIA: u32 = fourcc(b"inia");
    /// Voice FX ring modulator (model 3).
    pub const BOTB: u32 = fourcc(b"botb");
    /// Voice FX robot (model 4).
    pub const BOTC: u32 = fourcc(b"botc");
    /// Engine setup (sample rate, block size, format). Never sent by us: UC's
    /// `[Setu, 0x1C, fs, 128, 1, 1, 0]` (0x180048a10) only reaches its PC-side engine, and the
    /// firmware sets its own engines up (`[Setu, 0x1C, fs, block, 2, 2, fmt]` from its
    /// audio-format function). A host `Setu` can tear Voice FX down. Kept for decoding only.
    pub const SETU: u32 = fourcc(b"Setu");
    pub const SETU_SIZE: u32 = 0x1C;
    pub const LFDF_SIZE: u32 = 0x2C;
    pub const MLFF_SIZE: u32 = 0x294;
    pub const MAY4_SIZE: u32 = 0x24;
    pub const BOT_SIZE: u32 = 0x28;
    pub const INIA_SIZE: u32 = 0x1C4;
    pub const VECH_SIZE: u32 = 0x1C;
    pub const VDOG_SIZE: u32 = 0x20;
    pub const VRVB_SIZE: u32 = 0x38;
    pub const VOFX_SIZE: u32 = 0x10;
    /// Firmware-internal Voice FX half-rate switch (`[VFxO, 0xC, 1]` when fs > 48 kHz halves the
    /// engines' block size and rate). Not an enable, and UC never sends it; neither do we.
    pub const VFXO: u32 = fourcc(b"VFxO");
    pub const VFXO_SIZE: u32 = 0xC;
    pub const BQDF_SIZE: u32 = 0x24;
    pub const MBDF_SIZE: u32 = 0x1F4;
    pub const CPXT_SIZE: u32 = 0x40;
    pub const GATE_SIZE: u32 = 0x48;
    pub const LIM_SIZE: u32 = 0x18;

    /// Size for a tag, if the tag is one we send.
    pub fn size_of(t: u32) -> Option<u32> {
        match t {
            BQDF => Some(BQDF_SIZE),
            MBDF => Some(MBDF_SIZE),
            CPXT => Some(CPXT_SIZE),
            GATE => Some(GATE_SIZE),
            LIM => Some(LIM_SIZE),
            VECH => Some(VECH_SIZE),
            VDOG => Some(VDOG_SIZE),
            VRVB => Some(VRVB_SIZE),
            VOFX => Some(VOFX_SIZE),
            LFDF => Some(LFDF_SIZE),
            MLFF => Some(MLFF_SIZE),
            MAY4 => Some(MAY4_SIZE),
            BOTA | BOTB | BOTC => Some(BOT_SIZE),
            INIA => Some(INIA_SIZE),
            _ => None,
        }
    }

    /// Which tags a block accepts from us.
    pub fn allowed_for(block: u32, t: u32) -> bool {
        use super::blk;
        match block {
            blk::EQ => matches!(t, BQDF | MBDF | LFDF | MLFF),
            blk::HPF => matches!(t, BQDF | MBDF),
            blk::COMP => matches!(t, CPXT | MBDF),
            blk::GATE => matches!(t, GATE | MBDF),
            blk::LIMITER => t == LIM,
            blk::VOICE_FX => matches!(
                t,
                VECH | VDOG | BQDF | MBDF | VOFX | MAY4 | BOTA | BOTB | BOTC | INIA
            ),
            blk::REVERB => t == VRVB,
            _ => false,
        }
    }
}

/// Sample rates UC precomputes in `MBdf` (the DSP picks the one it runs at).
pub const MULTI_RATES: [f32; 4] = [44_100.0, 48_000.0, 88_200.0, 96_000.0];
/// The engine id UC assigns with its `Targ` message (always 0).
pub const ENGINE_ID: u32 = 0;
/// UC's floor for "-inf" linear gains (10^(-144/20)).
const TINY: f32 = 6.309_573e-8;

/// One write: a blob for `block`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DspWrite {
    pub block: u32,
    pub blob: Vec<u8>,
    /// Send to block index 0 instead of the processing channel (the `VoFx` model select).
    pub fixed_index0: bool,
}

struct Blob(Vec<u8>);

impl Blob {
    fn new(tag: u32, size: u32) -> Self {
        let mut b = Blob(Vec::with_capacity(size as usize));
        b.u(tag);
        b.u(size);
        b
    }
    fn u(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn f(&mut self, v: f32) -> &mut Self {
        self.u(v.to_bits())
    }
    fn coefs(&mut self, c: &Coefs) -> &mut Self {
        for &x in c {
            self.f(x as f32);
        }
        self
    }
    fn fs(&mut self, c: &[f32]) -> &mut Self {
        for &x in c {
            self.f(x);
        }
        self
    }
    fn done(mut self) -> Vec<u8> {
        let size = u32::from_le_bytes([self.0[4], self.0[5], self.0[6], self.0[7]]) as usize;
        debug_assert!(self.0.len() <= size, "blob overflow");
        self.0.resize(size, 0);
        self.0
    }
}

// ---------------------------------------------------------------------------------------------
// Filter designer (UC 0x180012f50; float twin 0x1800113a0)
// ---------------------------------------------------------------------------------------------

/// Normalized biquad as the DSP wants it: `[b0, -a1, b1, -a2, b2]` (a0 = 1).
pub type Coefs = [f64; 5];
pub const IDENTITY: Coefs = [1.0, 0.0, 0.0, 0.0, 0.0];

/// Filter kinds used by the engines (UC's internal numbering; 32 kinds exist, see [`design`]).
pub mod kind {
    /// No filter.
    pub const OFF: i32 = -1;
    /// 2nd-order high-pass, bilinear (HPF block).
    pub const HPF2: i32 = 3;
    /// Peaking EQ, bilinear, A = 10^(dB/20) (parametric EQ bands).
    pub const PEAK: i32 = 6;
    /// Band-pass, constant 0 dB peak (key filters).
    pub const BANDPASS: i32 = 7;
    /// Low shelf (RBJ).
    pub const LOW_SHELF: i32 = 8;
    /// High shelf (RBJ).
    pub const HIGH_SHELF: i32 = 9;
    /// Peaking EQ (RBJ, alpha = w/2Q) (Tube compressor side chain).
    pub const PEAK_RBJ: i32 = 11;
}

/// Port of UC's biquad designer. `freq` in Hz, `gain_db` in dB.
pub fn design(kind: i32, fs: f64, freq: f64, q: f64, gain_db: f64) -> Coefs {
    use std::f64::consts::PI;
    let t = PI / fs; // UC +0xb8
                     // first-order and bilinear kinds
    let one_pole = |b0: f64, b1: f64, na1: f64| [b0, na1, b1, 0.0, 0.0];
    match kind {
        0 | 1 => {
            let k = (freq * t).tan();
            let inv = 1.0 / (k + 1.0);
            let na1 = (1.0 - k) * inv;
            return if kind == 0 {
                one_pole(k * inv, k * inv, na1)
            } else {
                one_pole(inv, -inv, na1)
            };
        }
        2 | 3 => {
            let k = (freq * t).tan();
            let k2 = k * k;
            let a0 = k / q + k2 + 1.0;
            let na1 = 2.0 * (1.0 - k2) / a0;
            let na2 = (k / q - 1.0 - k2) / a0;
            return if kind == 2 {
                [k2 / a0, na1, 2.0 * k2 / a0, na2, k2 / a0]
            } else {
                [1.0 / a0, na1, -2.0 / a0, na2, 1.0 / a0]
            };
        }
        4 | 15 => {
            let g = if kind == 15 { gain_db / 3.0 } else { gain_db };
            let a = 10f64.powf(g * 0.05);
            let w = freq * t;
            let (d8, c0) = if a > 1.0 {
                (1.0 / w.tan(), 1.0 / (w / a).tan())
            } else {
                (1.0 / (w * a).tan(), 1.0 / w.tan())
            };
            let e8 = 1.0 / (c0 + 1.0);
            return one_pole((d8 + 1.0) * a * e8, (1.0 - d8) * a * e8, (c0 - 1.0) * e8);
        }
        5 | 16 => {
            let g = if kind == 16 { gain_db / 3.0 } else { gain_db };
            let a = 10f64.powf(g * 0.05);
            let (c0, d8) = if a >= 1.0 {
                let x = ((freq * a / fs).min(0.499) * PI).tan();
                (1.0 / x, 1.0 / (freq * t).tan())
            } else {
                let x = ((freq / (a * fs)).min(0.499) * PI).tan();
                (1.0 / (freq * t).tan(), 1.0 / x)
            };
            let e8 = 1.0 / (c0 + 1.0);
            return one_pole((d8 + 1.0) * e8, (1.0 - d8) * e8, (c0 - 1.0) * e8);
        }
        6 => {
            let a = 10f64.powf(gain_db * 0.05);
            let k = (freq * t).tan();
            let k2 = k * k;
            let p = a.sqrt() * k / q;
            let a0 = p / a + k2 + 1.0;
            return [
                (1.0 + p + k2) / a0,
                2.0 * (1.0 - k2) / a0,
                2.0 * (k2 - 1.0) / a0,
                (p / a - 1.0 - k2) / a0,
                (k2 - p + 1.0) / a0,
            ];
        }
        26 | 27 => {
            let d8 = (PI * (0.5 - freq / fs)).tan();
            let inv = 1.0 / (d8 + 1.0);
            let na1 = (d8 - 1.0) * inv;
            return if kind == 26 {
                one_pole(d8 * inv, -d8 * inv, na1)
            } else {
                one_pole(inv, inv, na1)
            };
        }
        7..=12 | 17..=25 | 28..=31 => {}
        _ => return IDENTITY,
    }
    // RBJ cookbook kinds
    let w = 2.0 * freq * t;
    let (sn, cs) = w.sin_cos();
    let alpha = 0.5 / q * sn;
    let rbj_inv = 1.0 / (alpha + 1.0);
    match kind {
        7 | 21 => [
            alpha * rbj_inv,
            2.0 * cs * rbj_inv,
            0.0,
            (alpha - 1.0) * rbj_inv,
            -alpha * rbj_inv,
        ],
        10 => [
            (1.0 - alpha) * rbj_inv,
            2.0 * cs * rbj_inv,
            -2.0 * cs * rbj_inv,
            (alpha - 1.0) * rbj_inv,
            1.0,
        ],
        12 => [
            rbj_inv,
            2.0 * cs * rbj_inv,
            -2.0 * cs * rbj_inv,
            (alpha - 1.0) * rbj_inv,
            rbj_inv,
        ],
        19 | 22 | 24 => {
            let b = (1.0 - cs) * 0.5 * rbj_inv;
            [
                b,
                2.0 * cs * rbj_inv,
                (1.0 - cs) * rbj_inv,
                (alpha - 1.0) * rbj_inv,
                b,
            ]
        }
        20 | 23 | 25 => {
            let b = (1.0 + cs) * 0.5 * rbj_inv;
            [
                b,
                2.0 * cs * rbj_inv,
                -(1.0 + cs) * rbj_inv,
                (alpha - 1.0) * rbj_inv,
                b,
            ]
        }
        11 => {
            let al = 0.5 * w / q; // UC uses w/(2Q) here, not sin(w)/(2Q)
            let a = 10f64.powf(gain_db * 0.025);
            let inv = a / (al + a);
            [
                (1.0 + a * al) * inv,
                2.0 * cs * inv,
                -2.0 * cs * inv,
                -(1.0 - al / a) * inv,
                (1.0 - a * al) * inv,
            ]
        }
        _ => {
            // shelves: 8/17/28/30 low, 9/18/29/31 high; some kinds scale the gain
            let g = match kind {
                17 | 18 => gain_db * 0.5,
                28 | 29 => gain_db / 3.0,
                30 | 31 => gain_db * 0.25,
                _ => gain_db,
            };
            let a = 10f64.powf(g * 0.025);
            let bs = a.sqrt() / q * sn; // 2*sqrt(A)*alpha
            let (ap, am) = (a + 1.0, a - 1.0);
            if matches!(kind, 8 | 17 | 28 | 30) {
                let inv = 1.0 / (ap + am * cs + bs);
                [
                    a * (ap - am * cs + bs) * inv,
                    (2.0 * cs * ap + 2.0 * am) * inv,
                    2.0 * a * (am - ap * cs) * inv,
                    -(ap + am * cs - bs) * inv,
                    a * (ap - am * cs - bs) * inv,
                ]
            } else {
                let inv = 1.0 / (ap - am * cs + bs);
                [
                    a * (ap + am * cs + bs) * inv,
                    -2.0 * (am - ap * cs) * inv,
                    -2.0 * a * (am + ap * cs) * inv,
                    -(ap - am * cs - bs) * inv,
                    a * (ap + am * cs - bs) * inv,
                ]
            }
        }
    }
}

/// Parameters of one filter, as UC hands them to the designer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterSpec {
    pub kind: i32,
    pub freq: f32,
    pub q: f32,
    pub gain: f32,
}

impl FilterSpec {
    pub fn coefs(&self, fs: f64) -> Coefs {
        design(
            self.kind,
            fs,
            self.freq as f64,
            self.q as f64,
            self.gain as f64,
        )
    }
}

/// `MBdf` (UC 0x18000cc70): one filter for each rate in [`MULTI_RATES`]. `None` (or a spec with
/// a non-positive frequency or Q) sends pass-through entries. `scale_b` multiplies b0/b1/b2.
fn mbdf(band: u32, spec: Option<FilterSpec>, scale_b: f64) -> Vec<u8> {
    let mut b = Blob::new(tag::MBDF, tag::MBDF_SIZE);
    b.u(ENGINE_ID).u(band);
    for rate in MULTI_RATES {
        let mut c = match spec {
            Some(s) if s.freq > 0.0 && s.q > 0.0 => s.coefs(rate as f64),
            _ => IDENTITY,
        };
        if spec.is_some() {
            c[0] *= scale_b;
            c[2] *= scale_b;
            c[4] *= scale_b;
        }
        b.coefs(&c).f(rate);
    }
    let mut v = b.done();
    v[0x1F0..0x1F4].copy_from_slice(&(MULTI_RATES.len() as u32).to_le_bytes());
    v
}

fn bqdf(band: u32, c: &Coefs) -> Vec<u8> {
    let mut b = Blob::new(tag::BQDF, tag::BQDF_SIZE);
    b.u(ENGINE_ID).u(band).coefs(c);
    b.done()
}

fn bqdf32(band: u32, c: &[f32; 5]) -> Vec<u8> {
    let mut b = Blob::new(tag::BQDF, tag::BQDF_SIZE);
    b.u(ENGINE_ID).u(band).fs(c);
    b.done()
}

/// `MBdf` from a designer taking the sample period (the passive/vintage EQ bands).
fn mbdf_with(band: u32, design: impl Fn(f32) -> [f32; 5]) -> Vec<u8> {
    let mut b = Blob::new(tag::MBDF, tag::MBDF_SIZE);
    b.u(ENGINE_ID).u(band);
    for rate in MULTI_RATES {
        b.fs(&design(1.0 / rate)).f(rate);
    }
    let mut v = b.done();
    v[0x1F0..0x1F4].copy_from_slice(&(MULTI_RATES.len() as u32).to_le_bytes());
    v
}

fn lfdf(c: &[f32; 7]) -> Vec<u8> {
    let mut b = Blob::new(tag::LFDF, tag::LFDF_SIZE);
    b.u(ENGINE_ID).u(0).fs(c);
    b.done()
}

/// `MLff` (passive/vintage EQ send): the 7-coefficient filter for each rate, 32-byte entries.
fn mlff(design: impl Fn(f32) -> [f32; 7]) -> Vec<u8> {
    let mut b = Blob::new(tag::MLFF, tag::MLFF_SIZE);
    b.u(ENGINE_ID).u(0);
    for rate in MULTI_RATES {
        b.fs(&design(1.0 / rate)).f(rate);
    }
    let mut v = b.done();
    v[0x290..0x294].copy_from_slice(&(MULTI_RATES.len() as u32).to_le_bytes());
    v
}

fn db_gain_inv(v: f32) -> f32 {
    // threshold-style: 10^(-dB/20), saturating above +144
    if v > 144.0 {
        TINY
    } else {
        10f32.powf(-0.05 * v)
    }
}

fn db_gain(v: f32) -> f32 {
    if v < -144.0 {
        TINY
    } else {
        10f32.powf(0.05 * v)
    }
}

fn on(v: f32) -> u32 {
    (v >= 1.0) as u32
}

/// Key (side-chain) filter shared by gate and compressors: band-pass when the frequency is above
/// 40 Hz, off otherwise.
fn key_filter(freq: f32, q: f32, gain: f32) -> FilterSpec {
    FilterSpec {
        kind: if freq > 40.001 {
            kind::BANDPASS
        } else {
            kind::OFF
        },
        freq: freq.max(40.0),
        q,
        gain,
    }
}

// ---------------------------------------------------------------------------------------------
// Parameter model
// ---------------------------------------------------------------------------------------------

/// Which engine a parameter feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    Gate,
    Comp,
    Eq,
    Hpf,
    Limiter,
    Reverb,
    VoiceFx,
    /// EQ/compressor order switch (`opt ` block).
    Order,
}

/// One user-facing DSP parameter (ranges and defaults from UC's parameter tables).
#[derive(Debug, Clone, Copy)]
pub struct FxParam {
    pub key: &'static str,
    pub label: &'static str,
    pub engine: Engine,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: &'static str,
}

const fn p(
    key: &'static str,
    label: &'static str,
    engine: Engine,
    min: f32,
    max: f32,
    default: f32,
    unit: &'static str,
) -> FxParam {
    FxParam {
        key,
        label,
        engine,
        min,
        max,
        default,
        unit,
    }
}

use Engine::*;

/// Every DSP parameter we can set. Models: `comp.model` 0 standard, 1 tube, 2 FET;
/// `eq.model` 0 parametric, 1 passive, 2 vintage; `voicefx.model` 0 doubler, 1 detune,
/// 2 vocoder, 3 ring modulator, 4 robot, 5 echo.
pub const PARAMS: &[FxParam] = &[
    p(
        "hpf.freq",
        "High-pass frequency (24 = off)",
        Hpf,
        24.0,
        1000.0,
        24.0,
        "Hz",
    ),
    p("gate.on", "Gate on", Gate, 0.0, 1.0, 0.0, "on/off"),
    p(
        "gate.expander",
        "Expander mode",
        Gate,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p(
        "gate.keylisten",
        "Key listen",
        Gate,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p(
        "gate.keyfilter",
        "Key filter (40 or less = off)",
        Gate,
        20.0,
        16000.0,
        20.0,
        "Hz",
    ),
    p("gate.threshold", "Threshold", Gate, -84.0, 0.0, -84.0, "dB"),
    p("gate.range", "Range", Gate, -84.0, 0.0, -84.0, "dB"),
    p("gate.attack", "Attack", Gate, 0.00002, 0.5, 0.005, "s"),
    p("gate.release", "Release", Gate, 0.05, 2.0, 0.7, "s"),
    p(
        "comp.model",
        "Compressor model: 0 standard, 1 tube, 2 FET",
        Comp,
        0.0,
        2.0,
        0.0,
        "",
    ),
    p(
        "comp.on",
        "Compressor on (all models)",
        Comp,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p(
        "comp.keylisten",
        "Key listen (all models)",
        Comp,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p(
        "comp.keyfilter",
        "Key filter (40 or less = off; all models)",
        Comp,
        20.0,
        16000.0,
        20.0,
        "Hz",
    ),
    p(
        "comp.threshold",
        "Standard: threshold",
        Comp,
        -56.0,
        0.0,
        0.0,
        "dB",
    ),
    p("comp.ratio", "Standard: ratio", Comp, 1.0, 20.0, 2.0, ":1"),
    p(
        "comp.attack",
        "Standard: attack",
        Comp,
        0.0002,
        0.15,
        0.02,
        "s",
    ),
    p(
        "comp.release",
        "Standard: release",
        Comp,
        0.0025,
        0.9,
        0.15,
        "s",
    ),
    p(
        "comp.gain",
        "Standard: makeup gain",
        Comp,
        0.0,
        28.0,
        0.0,
        "dB",
    ),
    p(
        "comp.softknee",
        "Standard: soft knee",
        Comp,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p(
        "comp.auto",
        "Standard: auto attack/release",
        Comp,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p(
        "comp.tube.mode",
        "Tube: 0 compress, 1 limit",
        Comp,
        0.0,
        1.0,
        0.0,
        "",
    ),
    p("comp.tube.gain", "Tube: gain", Comp, 0.0, 100.0, 40.0, ""),
    p(
        "comp.tube.peak",
        "Tube: peak reduction",
        Comp,
        0.0,
        100.0,
        0.0,
        "",
    ),
    p(
        "comp.fet.input",
        "FET: input",
        Comp,
        -56.0,
        0.0,
        -43.0,
        "dB",
    ),
    p(
        "comp.fet.output",
        "FET: output",
        Comp,
        -56.0,
        0.0,
        0.0,
        "dB",
    ),
    p(
        "comp.fet.attack",
        "FET: attack",
        Comp,
        0.000021,
        0.0008,
        0.0001,
        "s",
    ),
    p(
        "comp.fet.release",
        "FET: release",
        Comp,
        0.05,
        1.1,
        0.25,
        "s",
    ),
    p(
        "comp.fet.ratio",
        "FET: ratio 0..4 = 4, 8, 12, 20, all",
        Comp,
        0.0,
        4.0,
        0.0,
        "",
    ),
    p(
        "eq.swap",
        "Swap EQ/compressor order",
        Order,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p(
        "eq.model",
        "EQ model: 0 parametric, 1 passive, 2 vintage",
        Eq,
        0.0,
        2.0,
        0.0,
        "",
    ),
    p("eq.on", "EQ on (all models)", Eq, 0.0, 1.0, 0.0, "on/off"),
    p("eq.low.on", "Low band on", Eq, 0.0, 1.0, 1.0, "on/off"),
    p(
        "eq.low.shelf",
        "Low band: shelf instead of peak",
        Eq,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p("eq.low.gain", "Low band gain", Eq, -15.0, 15.0, 0.0, "dB"),
    p(
        "eq.low.freq",
        "Low band frequency",
        Eq,
        36.0,
        18000.0,
        130.0,
        "Hz",
    ),
    p("eq.low.q", "Low band Q", Eq, 0.1, 10.0, 0.6, ""),
    p(
        "eq.lowmid.on",
        "Low-mid band on",
        Eq,
        0.0,
        1.0,
        1.0,
        "on/off",
    ),
    p(
        "eq.lowmid.gain",
        "Low-mid band gain",
        Eq,
        -15.0,
        15.0,
        0.0,
        "dB",
    ),
    p(
        "eq.lowmid.freq",
        "Low-mid band frequency",
        Eq,
        36.0,
        18000.0,
        320.0,
        "Hz",
    ),
    p("eq.lowmid.q", "Low-mid band Q", Eq, 0.1, 10.0, 0.6, ""),
    p(
        "eq.himid.on",
        "High-mid band on",
        Eq,
        0.0,
        1.0,
        1.0,
        "on/off",
    ),
    p(
        "eq.himid.gain",
        "High-mid band gain",
        Eq,
        -15.0,
        15.0,
        0.0,
        "dB",
    ),
    p(
        "eq.himid.freq",
        "High-mid band frequency",
        Eq,
        36.0,
        18000.0,
        1400.0,
        "Hz",
    ),
    p("eq.himid.q", "High-mid band Q", Eq, 0.1, 10.0, 0.6, ""),
    p("eq.high.on", "High band on", Eq, 0.0, 1.0, 1.0, "on/off"),
    p(
        "eq.high.shelf",
        "High band: shelf instead of peak",
        Eq,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p("eq.high.gain", "High band gain", Eq, -15.0, 15.0, 0.0, "dB"),
    p(
        "eq.high.freq",
        "High band frequency",
        Eq,
        36.0,
        18000.0,
        5000.0,
        "Hz",
    ),
    p("eq.high.q", "High band Q", Eq, 0.1, 10.0, 0.6, ""),
    p(
        "eq.passive.lowfreq",
        "Passive: low frequency 0..3 = 20, 30, 60, 100 Hz",
        Eq,
        0.0,
        3.0,
        0.0,
        "",
    ),
    p(
        "eq.passive.lowboost",
        "Passive: low boost",
        Eq,
        0.0,
        10.0,
        0.0,
        "",
    ),
    p(
        "eq.passive.lowatten",
        "Passive: low attenuation",
        Eq,
        0.0,
        10.0,
        0.0,
        "",
    ),
    p(
        "eq.passive.highfreq",
        "Passive: high boost frequency 0..6 = 3, 4, 5, 8, 10, 12, 16 kHz",
        Eq,
        0.0,
        6.0,
        4.0,
        "",
    ),
    p(
        "eq.passive.highboost",
        "Passive: high boost",
        Eq,
        0.0,
        10.0,
        1.5,
        "",
    ),
    p(
        "eq.passive.bandwidth",
        "Passive: high boost bandwidth",
        Eq,
        0.0,
        10.0,
        10.0,
        "",
    ),
    p(
        "eq.passive.attenfreq",
        "Passive: high attenuation frequency 0..2 = 5, 10, 20 kHz",
        Eq,
        0.0,
        2.0,
        2.0,
        "",
    ),
    p(
        "eq.passive.highatten",
        "Passive: high attenuation",
        Eq,
        0.0,
        10.0,
        1.5,
        "",
    ),
    p(
        "eq.vintage.lowfreq",
        "Vintage: low frequency 0..3 = 35, 60, 110, 220 Hz",
        Eq,
        0.0,
        3.0,
        1.0,
        "",
    ),
    p(
        "eq.vintage.lowgain",
        "Vintage: low gain",
        Eq,
        -16.0,
        16.0,
        0.0,
        "dB",
    ),
    p(
        "eq.vintage.lowmidfreq",
        "Vintage: low-mid frequency 0..2 = 360, 700, 1600 Hz",
        Eq,
        0.0,
        2.0,
        2.0,
        "",
    ),
    p(
        "eq.vintage.lowmidgain",
        "Vintage: low-mid gain",
        Eq,
        -16.0,
        16.0,
        0.0,
        "dB",
    ),
    p(
        "eq.vintage.himidfreq",
        "Vintage: high-mid frequency 0..2 = 3.2, 4.8, 7.2 kHz",
        Eq,
        0.0,
        2.0,
        2.0,
        "",
    ),
    p(
        "eq.vintage.himidgain",
        "Vintage: high-mid gain",
        Eq,
        -16.0,
        16.0,
        0.0,
        "dB",
    ),
    p(
        "eq.vintage.highgain",
        "Vintage: high gain",
        Eq,
        -16.0,
        16.0,
        0.0,
        "dB",
    ),
    p("limiter.on", "Limiter on", Limiter, 0.0, 1.0, 0.0, "on/off"),
    p(
        "limiter.threshold",
        "Limiter threshold",
        Limiter,
        -28.0,
        0.0,
        0.0,
        "dB",
    ),
    p("reverb.on", "Reverb on", Reverb, 0.0, 1.0, 0.0, "on/off"),
    p("reverb.size", "Reverb size", Reverb, 0.0, 1.0, 0.15, ""),
    p("reverb.mix", "Reverb wet/dry", Reverb, 0.0, 1.0, 0.5, ""),
    p(
        "reverb.hpf",
        "Reverb high-pass (0.1 or less = off)",
        Reverb,
        0.0,
        500.0,
        200.0,
        "Hz",
    ),
    p(
        "reverb.predelay",
        "Reverb pre-delay",
        Reverb,
        0.0001,
        0.25,
        0.02,
        "s",
    ),
    p(
        "voicefx.model",
        "Voice FX model: 0 doubler, 1 detune, 2 vocoder, 3 ring mod, 4 robot, 5 echo",
        VoiceFx,
        0.0,
        5.0,
        0.0,
        "",
    ),
    p(
        "voicefx.on",
        "Voice FX on",
        VoiceFx,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p(
        "voicefx.mix",
        "Voice FX wet/dry",
        VoiceFx,
        0.0,
        1.0,
        0.5,
        "",
    ),
    p(
        "voicefx.doubler.lows",
        "Doubler: lows",
        VoiceFx,
        0.0,
        1.0,
        0.5,
        "",
    ),
    p(
        "voicefx.doubler.width",
        "Doubler: width",
        VoiceFx,
        0.0,
        1.0,
        0.5,
        "",
    ),
    p(
        "voicefx.echo.time",
        "Echo: time",
        VoiceFx,
        0.0001,
        0.25,
        0.125,
        "s",
    ),
    p(
        "voicefx.echo.feedback",
        "Echo: feedback",
        VoiceFx,
        0.0,
        1.0,
        0.5,
        "",
    ),
    p(
        "voicefx.detune.amount",
        "Detune: amount 0..8",
        VoiceFx,
        0.0,
        8.0,
        4.0,
        "",
    ),
    p(
        "voicefx.vocoder.volume",
        "Vocoder: volume",
        VoiceFx,
        0.0,
        1.0,
        1.0,
        "",
    ),
    p(
        "voicefx.vocoder.carrier",
        "Vocoder: carrier type 0..2",
        VoiceFx,
        0.0,
        2.0,
        1.0,
        "",
    ),
    p(
        "voicefx.vocoder.carrierfreq",
        "Vocoder: carrier frequency",
        VoiceFx,
        50.0,
        500.0,
        80.0,
        "Hz",
    ),
    p(
        "voicefx.ringmod.freq",
        "Ring mod: frequency",
        VoiceFx,
        0.1,
        2000.0,
        30.0,
        "Hz",
    ),
    p(
        "voicefx.ringmod.sub",
        "Ring mod: sub carrier on",
        VoiceFx,
        0.0,
        1.0,
        0.0,
        "on/off",
    ),
    p(
        "voicefx.ringmod.subfreq",
        "Ring mod: sub carrier frequency",
        VoiceFx,
        0.1,
        2000.0,
        50.0,
        "Hz",
    ),
    p(
        "voicefx.ringmod.dist",
        "Ring mod: distortion",
        VoiceFx,
        0.0,
        1.0,
        0.5,
        "",
    ),
    p(
        "voicefx.ringmod.volume",
        "Ring mod: volume",
        VoiceFx,
        0.0,
        1.0,
        1.0,
        "",
    ),
    p(
        "voicefx.robot.pitch",
        "Robot: pitch",
        VoiceFx,
        0.0,
        1.0,
        0.5,
        "",
    ),
    p(
        "voicefx.robot.regen",
        "Robot: regeneration",
        VoiceFx,
        0.0,
        1.0,
        0.5,
        "",
    ),
    p(
        "voicefx.robot.damping",
        "Robot: damping",
        VoiceFx,
        0.0,
        1.0,
        0.5,
        "",
    ),
    p(
        "voicefx.robot.dist",
        "Robot: distortion",
        VoiceFx,
        0.0,
        1.0,
        0.5,
        "",
    ),
    p(
        "voicefx.robot.volume",
        "Robot: volume",
        VoiceFx,
        0.0,
        1.0,
        1.0,
        "",
    ),
];

pub fn find(key: &str) -> Option<&'static FxParam> {
    PARAMS.iter().find(|p| p.key == key)
}

pub const EQ_BANDS: [&str; 4] = ["low", "lowmid", "himid", "high"];

/// Current values of all DSP parameters (the device cannot be asked for them, so the app keeps
/// its own copy).
#[derive(Debug, Clone, PartialEq)]
pub struct FxState {
    vals: Vec<f32>,
}

impl Default for FxState {
    fn default() -> Self {
        Self {
            vals: PARAMS.iter().map(|p| p.default).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FxError {
    Unknown(String),
    Range {
        key: &'static str,
        min: f32,
        max: f32,
    },
    NotPorted(&'static str),
}

impl std::fmt::Display for FxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FxError::Unknown(k) => write!(f, "unknown DSP parameter '{k}'"),
            FxError::Range { key, min, max } => write!(f, "{key} must be between {min} and {max}"),
            FxError::NotPorted(what) => write!(f, "{what} is not supported yet"),
        }
    }
}

impl std::error::Error for FxError {}

impl FxState {
    pub fn get(&self, key: &str) -> f32 {
        match PARAMS.iter().position(|p| p.key == key) {
            Some(i) => self.vals[i],
            None => panic!("no DSP parameter {key}"),
        }
    }

    /// Set a value (range-checked). Returns the engine to resend.
    pub fn set(&mut self, key: &str, v: f32) -> Result<Engine, FxError> {
        let i = PARAMS
            .iter()
            .position(|p| p.key == key)
            .ok_or_else(|| FxError::Unknown(key.into()))?;
        let p = &PARAMS[i];
        if !(v >= p.min && v <= p.max) {
            return Err(FxError::Range {
                key: p.key,
                min: p.min,
                max: p.max,
            });
        }
        self.vals[i] = v;
        Ok(p.engine)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static FxParam, f32)> + '_ {
        PARAMS.iter().zip(self.vals.iter().copied())
    }

    /// `key=value` lines (the state file format).
    pub fn to_text(&self) -> String {
        self.iter()
            .map(|(p, v)| format!("{}={}\n", p.key, v))
            .collect()
    }

    /// Parse `key=value` lines. Unknown keys and bad values are skipped; missing keys keep their
    /// defaults.
    pub fn from_text(s: &str) -> Self {
        let mut st = Self::default();
        for line in s.lines() {
            if let Some((k, v)) = line.split_once('=') {
                if let Ok(v) = v.trim().parse::<f32>() {
                    let _ = st.set(k.trim(), v);
                }
            }
        }
        st
    }

    /// All writes for one engine at sample rate `fs`.
    pub fn writes(&self, e: Engine, fs: f32) -> Vec<DspWrite> {
        let fs64 = fs as f64;
        let w = |block, blobs: Vec<Vec<u8>>| {
            blobs
                .into_iter()
                .map(|blob| {
                    let fixed_index0 = blob[0..4] == tag::VOFX.to_le_bytes();
                    DspWrite {
                        block,
                        blob,
                        fixed_index0,
                    }
                })
                .collect()
        };
        match e {
            Gate => w(blk::GATE, self.gate(fs)),
            Limiter => w(blk::LIMITER, vec![self.limiter(fs)]),
            Hpf => w(blk::HPF, self.hpf(fs64)),
            Eq => w(blk::EQ, self.eq(fs64)),
            Comp => w(blk::COMP, self.comp(fs64)),
            Reverb => w(blk::REVERB, vec![self.reverb(fs64)]),
            VoiceFx => w(blk::VOICE_FX, self.voicefx(fs64)),
            Order => w(blk::OPT, vec![self.order()]),
        }
    }

    /// Writes for every engine. No `Setu`: like UC, we never send one over USB (see
    /// [`tag::SETU`]); the firmware sets its engines up itself.
    pub fn all_writes(&self, fs: f32) -> Vec<DspWrite> {
        [Order, Hpf, Gate, Comp, Eq, Limiter, Reverb, VoiceFx]
            .into_iter()
            .flat_map(|e| self.writes(e, fs))
            .collect()
    }

    /// Gate (UC engine vtable 0x18043c678: setParam 0x18001f420, send 0x18001f870).
    fn gate(&self, fs: f32) -> Vec<Vec<u8>> {
        let g = |k: &str| self.get(k);
        let key = key_filter(g("gate.keyfilter"), 8.0, 1.0);
        let c = key.coefs(fs as f64);
        let att = g("gate.attack").max(0.00002);
        let rel = g("gate.release").max(0.0001);
        let tau = std::f32::consts::TAU;
        let att_coef = (-tau / att / (fs * 0.25)).exp();
        let m = if g("gate.expander") >= 1.0 { 1.0 } else { 0.5 };
        let rel_coef = (-tau / (m * rel) / (fs * 0.25)).exp();
        let hold = ((m * rel * fs * 0.5).ceil() as i32).max(1);
        let mut b = Blob::new(tag::GATE, tag::GATE_SIZE);
        b.u(ENGINE_ID).coefs(&c);
        b.f(db_gain(g("gate.range")))
            .f(db_gain_inv(g("gate.threshold")));
        b.f(att).f(rel).f(att_coef).f(rel_coef).u(hold as u32);
        b.u(on(g("gate.on")))
            .u(on(g("gate.expander")))
            .u(on(g("gate.keylisten")));
        vec![mbdf(0, Some(key), 1.0), b.done()]
    }

    /// Limiter (vtable 0x18043c768: setParam 0x18001ff30, send 0x180020140).
    fn limiter(&self, fs: f32) -> Vec<u8> {
        let mut b = Blob::new(tag::LIM, tag::LIM_SIZE);
        b.u(ENGINE_ID).u(on(self.get("limiter.on")));
        b.f(db_gain_inv(self.get("limiter.threshold")));
        b.f((-15.707_964 / fs).exp());
        b.done()
    }

    /// High-pass (vtable 0x18043beb8: one band, kind 3, Q 0.7071; base send 0x180016d60, which
    /// numbers the Bqdf band from 1 and the MBdf band from 0).
    fn hpf(&self, fs: f64) -> Vec<Vec<u8>> {
        let f = self.get("hpf.freq");
        let spec = (f > 24.001).then_some(FilterSpec {
            kind: kind::HPF2,
            freq: f,
            q: std::f32::consts::FRAC_1_SQRT_2,
            gain: 0.0,
        });
        let c = spec.map(|s| s.coefs(fs)).unwrap_or(IDENTITY);
        vec![mbdf(0, spec, 1.0), bqdf(1, &c)]
    }

    /// Parametric EQ, 4 bands (vtable 0x18043c220: setParam 0x180016a10, send 0x180019e30).
    /// A band is a peak (kind 6); "shelf" turns the low band into a low shelf (kind 8) and the
    /// high band into a high shelf (kind 9).
    fn eq(&self, fs: f64) -> Vec<Vec<u8>> {
        match self.get("eq.model").round() as i32 {
            1 => return self.passive_eq(fs as f32),
            2 => return self.vintage_eq(fs as f32),
            _ => {}
        }
        let all_on = self.get("eq.on") >= 1.0;
        let mut out = Vec::new();
        for (i, name) in EQ_BANDS.iter().enumerate() {
            let g = |s: &str| self.get(&format!("eq.{name}.{s}"));
            let shelf = match i {
                0 => self.get("eq.low.shelf") >= 1.0,
                3 => self.get("eq.high.shelf") >= 1.0,
                _ => false,
            };
            let k = match (i, shelf) {
                (0, true) => kind::LOW_SHELF,
                (3, true) => kind::HIGH_SHELF,
                _ => kind::PEAK,
            };
            let spec = (all_on && g("on") >= 1.0).then_some(FilterSpec {
                kind: k,
                freq: g("freq"),
                q: g("q"),
                gain: g("gain"),
            });
            let c = spec.map(|s| s.coefs(fs)).unwrap_or(IDENTITY);
            out.push(mbdf(i as u32, spec, 1.0));
            out.push(bqdf(i as u32, &c));
        }
        out
    }

    /// Passive EQ (vtable 0x18043c400: setParam 0x18001ab10, send 0x18001ace0). Low section:
    /// `Bqdf`/`MBdf` band 1; high boost + attenuation: one 3rd-order `Lfdf`/`MLff`. Bands 2 and 3
    /// (used by the parametric EQ) are cleared. EQ off zeroes the boosts and cuts.
    fn passive_eq(&self, fs: f32) -> Vec<Vec<u8>> {
        use crate::fx_analog::{passive_high, passive_low};
        let g = |k: &str| self.get(&format!("eq.passive.{k}"));
        let i = |k: &str| g(k).round() as i32;
        let on = self.get("eq.on") >= 1.0;
        let z = |v: f32| if on { v } else { 0.0 };
        let (lb, la, hb, ha) = (
            z(g("lowboost")),
            z(g("lowatten")),
            z(g("highboost")),
            z(g("highatten")),
        );
        let (lf, hf, af, bw) = (i("lowfreq"), i("highfreq"), i("attenfreq"), g("bandwidth"));
        let high = |t: f32| passive_high(hf, hb, bw, af, ha, t);
        let low = |t: f32| passive_low(lf, lb, la, t);
        vec![
            bqdf(2, &IDENTITY),
            bqdf(3, &IDENTITY),
            mbdf(2, None, 1.0),
            mbdf(3, None, 1.0),
            lfdf(&high(1.0 / fs)),
            mlff(high),
            bqdf32(1, &low(1.0 / fs)),
            mbdf_with(1, low),
        ]
    }

    /// Vintage EQ (vtable 0x18043c508: setParam 0x18001cd10, send 0x18001ced0): low shelf as a
    /// 3rd-order `Lfdf`/`MLff`, high shelf band 1, low-mid band 2, high-mid band 3.
    fn vintage_eq(&self, fs: f32) -> Vec<Vec<u8>> {
        use crate::fx_analog::{vintage_high, vintage_himid, vintage_lomid, vintage_low};
        let g = |k: &str| self.get(&format!("eq.vintage.{k}"));
        let i = |k: &str| g(k).round() as i32;
        let on = self.get("eq.on") >= 1.0;
        let z = |v: f32| if on { v } else { 0.0 };
        let (lf, lmf, hmf) = (i("lowfreq"), i("lowmidfreq"), i("himidfreq"));
        let (lg, lmg, hmg, hg) = (
            z(g("lowgain")),
            z(g("lowmidgain")),
            z(g("himidgain")),
            z(g("highgain")),
        );
        let t = 1.0 / fs;
        vec![
            lfdf(&vintage_low(lf, lg, t)),
            mlff(|t| vintage_low(lf, lg, t)),
            bqdf32(1, &vintage_high(hg, t)),
            mbdf_with(1, |t| vintage_high(hg, t)),
            bqdf32(2, &vintage_lomid(lmf, lmg, t)),
            mbdf_with(2, |t| vintage_lomid(lmf, lmg, t)),
            bqdf32(3, &vintage_himid(hmf, hmg, t)),
            mbdf_with(3, |t| vintage_himid(hmf, hmg, t)),
        ]
    }

    /// "Swap EQ/Comp Order" (UC 0x1800493f0): `SetP opt  <ch> [Pari, 0x14, 0, 0, swap]`. The EQ
    /// and compressor model selections never reach the device (UC swaps engines on the PC).
    fn order(&self) -> Vec<u8> {
        let mut b = Blob::new(tag::PARI_TAG, 0x14);
        b.u(0).u(0).u(on(self.get("eq.swap")));
        b.done()
    }

    /// Reverb, block 202 (vtable 0x18043c9a8: setParam 0x180020dc0, send 0x180020e60).
    fn reverb(&self, fs: f64) -> Vec<u8> {
        let g = |k: &str| self.get(k);
        let hp = g("reverb.hpf");
        let c = if (hp as f64) > 0.1 {
            design(kind::HPF2, fs, hp as f64, 0.7f32 as f64, 0.0)
        } else {
            IDENTITY
        };
        let pre = g("reverb.predelay");
        let mut b = Blob::new(tag::VRVB, tag::VRVB_SIZE);
        // The firmware's per-channel code runs the reverb whenever the sample rate allows it and
        // only scales it by `mix`; the `on` word is not looked at there (firmware 0x4acd6 /
        // 0x2054e). So "off" has to be a zero wet amount.
        let wet = if g("reverb.on") >= 0.5 {
            g("reverb.mix")
        } else {
            0.0
        };
        b.u(ENGINE_ID).u(on(g("reverb.on"))).f(wet);
        b.u(((pre as f64) > 0.0001) as u32)
            .f(pre)
            .f(g("reverb.size"))
            .f(hp)
            .coefs(&c);
        b.done()
    }

    /// Voice FX, block 201: model select (`VoFx`, block index 0) followed by the model's blobs,
    /// exactly UC's sequence. On/off travels in each model blob's `on` word.
    fn voicefx(&self, fs: f64) -> Vec<Vec<u8>> {
        let g = |k: &str| self.get(k);
        let model = g("voicefx.model").round() as u32;
        let mut sel = Blob::new(tag::VOFX, tag::VOFX_SIZE);
        sel.u(0).u(model);
        let vfx_on = on(g("voicefx.on"));
        let mut out = vec![sel.done()];
        let mix = g("voicefx.mix");
        match model {
            1 => {
                // Detune (vtable 0x18043cc70: setParam 0x180023d20, send 0x180023db0): a 6 kHz
                // low-pass, then `may4` with the detune step mapped through [-8..-1, 0].
                let spec = FilterSpec {
                    kind: 2,
                    freq: 6000.0,
                    q: 0.7,
                    gain: 0.0,
                };
                out.push(bqdf(0, &spec.coefs(fs)));
                out.push(mbdf(0, Some(spec), 1.0));
                const STEPS: [i32; 9] = [-8, -7, -6, -5, -4, -3, -2, -1, 0];
                let idx = ((g("voicefx.detune.amount") + 0.5) as usize).min(8);
                let mut b = Blob::new(tag::MAY4, tag::MAY4_SIZE);
                b.u(ENGINE_ID)
                    .u(vfx_on)
                    .u(STEPS[idx] as u32)
                    .u(0)
                    .u(0)
                    .u(0)
                    .f(mix);
                out.push(b.done());
            }
            2 => {
                // Vocoder (vtable 0x18043d140: setParam 0x180023350, send 0x180023430): a
                // 10-band analysis bank (`inia`, plus every filter as `MBdf` bands 0..21), then
                // the parameters (`bota`).
                const HP: [f32; 10] = [
                    85.0, 141.0, 230.0, 378.0, 622.0, 1024.0, 1680.0, 2730.0, 4450.0, 7250.0,
                ];
                const LP: [f32; 10] = [
                    141.0, 230.0, 378.0, 622.0, 1024.0, 1690.0, 2750.0, 4482.0, 7300.0, 11900.0,
                ];
                let spec = |kind, freq, q| FilterSpec {
                    kind,
                    freq,
                    q,
                    gain: 0.0,
                };
                let mut bank = Blob::new(tag::INIA, tag::INIA_SIZE);
                bank.u(ENGINE_ID);
                for (i, (&h, &l)) in HP.iter().zip(&LP).enumerate() {
                    out.push(mbdf(i as u32, Some(spec(kind::HPF2, h, 2.0)), 1.0));
                    out.push(mbdf(i as u32 + 10, Some(spec(2, l, 2.0)), 1.0));
                }
                for &h in &HP {
                    bank.coefs(&spec(kind::HPF2, h, 2.0).coefs(fs));
                }
                for &l in &LP {
                    bank.coefs(&spec(2, l, 2.0).coefs(fs));
                }
                let lp600 = spec(2, 600.0, 0.7);
                let hp2500 = spec(kind::HPF2, 2500.0, 0.7);
                bank.coefs(&lp600.coefs(fs)).coefs(&hp2500.coefs(fs));
                out.push(mbdf(20, Some(lp600), 1.0));
                out.push(mbdf(21, Some(hp2500), 1.0));
                out.push(bank.done());
                let mut b = Blob::new(tag::BOTA, tag::BOT_SIZE);
                b.u(ENGINE_ID)
                    .u(vfx_on)
                    .u(g("voicefx.vocoder.carrier").round() as u32);
                b.f(g("voicefx.vocoder.carrierfreq"))
                    .f(g("voicefx.vocoder.volume"));
                b.u(1).u(1).f(mix);
                out.push(b.done());
            }
            3 => {
                // Ring modulator (vtable 0x18043cdd0: setParam 0x180023a00, send 0x180023b20)
                let mut b = Blob::new(tag::BOTB, tag::BOT_SIZE);
                b.u(ENGINE_ID).u(vfx_on).f(g("voicefx.ringmod.freq"));
                b.f(g("voicefx.ringmod.dist"))
                    .f(g("voicefx.ringmod.volume"));
                b.u(on(g("voicefx.ringmod.sub")))
                    .f(g("voicefx.ringmod.subfreq"))
                    .f(mix);
                out.push(b.done());
            }
            4 => {
                // Robot (vtable 0x18043d090: setParam 0x180023b70, send 0x180023c70)
                let pitch = (g("voicefx.robot.pitch") * 1300.0 + 250.5) as i32;
                let mut b = Blob::new(tag::BOTC, tag::BOT_SIZE);
                b.u(ENGINE_ID).u(vfx_on).u(pitch as u32);
                b.f(g("voicefx.robot.regen") * 0.35 + 0.5);
                b.f(g("voicefx.robot.damping") * 0.6 + 0.3);
                b.f(g("voicefx.robot.dist"))
                    .f(g("voicefx.robot.volume"))
                    .f(mix);
                out.push(b.done());
            }
            5 => {
                // Echo (vtable 0x18043c8f8: setParam 0x180020f90, send 0x180021030)
                let mut b = Blob::new(tag::VECH, tag::VECH_SIZE);
                b.u(ENGINE_ID).u(on(g("voicefx.on"))).f(g("voicefx.mix"));
                b.f(g("voicefx.echo.feedback") * 0.5)
                    .f(g("voicefx.echo.time"));
                out.push(b.done());
            }
            _ => {
                // Doubler (vtable 0x18043cfe0: setParam 0x180023060, send 0x180023100): two
                // low shelves (600 / 550 Hz, Q 0.7, lows x 12 dB), then the parameters.
                let lows = g("voicefx.doubler.lows");
                for (band, f) in [(0u32, 600.0f32), (1, 550.0)] {
                    let spec = FilterSpec {
                        kind: kind::LOW_SHELF,
                        freq: f,
                        q: 0.7,
                        gain: lows * 12.0,
                    };
                    out.push(bqdf(band, &spec.coefs(fs)));
                    out.push(mbdf(band, Some(spec), 1.0));
                }
                let mut b = Blob::new(tag::VDOG, tag::VDOG_SIZE);
                b.u(ENGINE_ID)
                    .u(on(g("voicefx.on")))
                    .f(lows)
                    .f(g("voicefx.doubler.width"))
                    .f(lows);
                b.f(g("voicefx.mix"));
                out.push(b.done());
            }
        }
        out
    }

    /// Compressor: one DSP block, three host-side models that all send `cpxt`.
    fn comp(&self, fs: f64) -> Vec<Vec<u8>> {
        let g = |k: &str| self.get(k);
        let model = g("comp.model").round() as i32;
        let comp_on = on(g("comp.on"));
        let keylisten = on(g("comp.keylisten"));
        let keyf = g("comp.keyfilter");
        let q = std::f32::consts::FRAC_1_SQRT_2;
        // (key spec, b scale, att, rel, 1-1/ratio, knee, threshold, makeup)
        let (key, scale, att, rel, ratio, knee, thr, makeup): (
            Option<FilterSpec>,
            f64,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
        ) = match model {
            1 => {
                // Tube (vtable 0x18043b9d8: setParam 0x18000fc00, send 0x18000feb0)
                let peak = g("comp.tube.peak");
                let k = if g("comp.tube.mode") >= 1.0 {
                    1.0 / 12.0
                } else {
                    1.0 / 18.0
                };
                let r = if peak > 0.0 {
                    1.0 - 1.0 / (peak * k + 2.5)
                } else {
                    1.0
                };
                let (key, scale) = if keyf >= 40.001 {
                    (Some(key_filter(keyf, q, 0.0)), 1.0)
                } else if keylisten == 1 {
                    (None, 1.0)
                } else {
                    // fixed 15 kHz emphasis in the side chain, b coefficients halved
                    (
                        Some(FilterSpec {
                            kind: kind::PEAK_RBJ,
                            freq: 15_000.0,
                            q: 0.5,
                            gain: 7.0,
                        }),
                        0.5,
                    )
                };
                (
                    key,
                    scale,
                    peak * 0.0005 + 0.02,
                    0.38,
                    r,
                    3.0,
                    1.0 - peak * 0.5,
                    db_gain((g("comp.tube.gain") - 40.0) * 0.5),
                )
            }
            2 => {
                // FET (vtable 0x18043b928: setParam 0x18000f580, send 0x18000f970)
                const R: [f32; 5] = [6.5, 9.9, 14.0, 22.0, 20.0];
                const T: [f32; 5] = [3.75, 2.0, 1.1, 0.0, 2.0];
                const U: [f32; 5] = [-3.0, -1.7, -1.2, 0.0, -1.7];
                let ri = ((g("comp.fet.ratio") + 0.5) as usize).min(4);
                let x = -40.0 - g("comp.fet.input");
                let o = g("comp.fet.output");
                let thr = x * x * 0.007_861 + x * 1.52 + 6.13 - T[ri];
                let mk = (o * 0.005_727 + 1.435) * o + 6.591 - thr + 4.0 + U[ri];
                let att = if ri == 4 { 5.0 } else { 1.0 } * g("comp.fet.attack");
                let key = key_filter(keyf, q, 0.0);
                (
                    Some(key),
                    1.0,
                    att,
                    g("comp.fet.release") * 10.0,
                    1.0 - 1.0 / R[ri],
                    2.5,
                    thr,
                    db_gain(mk),
                )
            }
            _ => {
                // Standard (vtable 0x18043ba88: setParam 0x18000ef40, send 0x18000f3e0)
                let (att, rel) = if g("comp.auto") >= 1.0 {
                    (0.01, 0.15)
                } else {
                    (g("comp.attack"), g("comp.release"))
                };
                let ratio = g("comp.ratio");
                let r = if ratio > 0.0 { 1.0 - 1.0 / ratio } else { 0.0 };
                let knee = if g("comp.softknee") >= 1.0 { 3.0 } else { 0.01 };
                (
                    Some(key_filter(keyf, q, 0.0)),
                    1.0,
                    att,
                    rel,
                    r,
                    knee,
                    g("comp.threshold"),
                    db_gain(g("comp.gain")),
                )
            }
        };
        let mut c = match key {
            Some(s) if s.kind != kind::OFF => s.coefs(fs),
            _ => IDENTITY,
        };
        c[0] *= scale;
        c[2] *= scale;
        c[4] *= scale;
        let mut b = Blob::new(tag::CPXT, tag::CPXT_SIZE);
        b.u(ENGINE_ID).coefs(&c);
        b.f(att)
            .f(rel)
            .f(ratio)
            .f(knee)
            .f(thr)
            .f(makeup)
            .u(comp_on)
            .u(keylisten);
        let key_mb = match key {
            Some(s) if s.kind != kind::OFF => Some(s),
            _ => None,
        };
        vec![mbdf(0, key_mb, scale), b.done()]
    }
}

/// Magnitude response (dB) at `f` Hz of the current-rate filters in `writes` (`Bqdf` biquads
/// and 3rd-order `Lfdf` sections, in series), as the device would apply them at `fs`. The
/// multi-rate tables (`MBdf`/`MLff`) are copies of the same filters and are skipped.
pub fn response_db(writes: &[DspWrite], fs: f32, f: f32) -> f32 {
    let w = 2.0 * std::f64::consts::PI * f as f64 / fs as f64;
    let mut db = 0.0f64;
    for wr in writes {
        let b = &wr.blob;
        let t = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let n = match t {
            tag::BQDF => 5,
            tag::LFDF => 7,
            _ => continue,
        };
        let c: Vec<f64> = (0..n)
            .map(|i| {
                let o = 16 + 4 * i;
                f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as f64
            })
            .collect();
        // c = [b0, -a1, b1, -a2, b2 (, -a3, b3)]
        let (mut nr, mut ni, mut dr, mut di) = (c[0], 0.0, 1.0, 0.0);
        for k in 1..=(n - 1) / 2 {
            let (cs, sn) = ((k as f64 * w).cos(), (k as f64 * w).sin());
            nr += c[2 * k] * cs;
            ni -= c[2 * k] * sn;
            dr -= c[2 * k - 1] * cs;
            di += c[2 * k - 1] * sn;
        }
        db += 10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10();
    }
    db as f32
}

/// Denominator `[1, a1, .., an]` is stable (every pole strictly inside the unit circle), by
/// the Schur-Cohn step-down recursion.
fn denominator_stable(mut a: Vec<f64>) -> bool {
    for m in (1..a.len()).rev() {
        let k = a[m];
        if !k.is_finite() || k.abs() >= 1.0 {
            return false;
        }
        let d = 1.0 - k * k;
        a = (0..m).map(|i| (a[i] - k * a[m - i]) / d).collect();
    }
    true
}

/// Filter sections inside a DSP blob: `(offset of the first coefficient, order, count, stride)`.
fn filter_sections(t: u32) -> Option<(usize, usize, usize, usize)> {
    let n = MULTI_RATES.len();
    match t {
        tag::BQDF => Some((16, 2, 1, 0)),
        tag::MBDF => Some((16, 2, n, 24)),
        tag::LFDF => Some((16, 3, 1, 0)),
        tag::MLFF => Some((16, 3, n, 32)),
        _ => None,
    }
}

/// Value check for an outgoing DSP blob (tag and size already allow-listed): no 32-bit field
/// is a NaN/Inf, every filter coefficient is finite and every filter section is stable,
/// so a bad design can never make the mic oscillate or blast noise.
pub fn blob_values_ok(blob: &[u8]) -> bool {
    if blob.len() < 8 {
        return false;
    }
    let word = |o: usize| f32::from_le_bytes([blob[o], blob[o + 1], blob[o + 2], blob[o + 3]]);
    // Integer fields share the blob with floats; a small negative integer (e.g. a detune step
    // of -1) has a NaN bit pattern, while a NaN/Inf from arithmetic (0x7FC00000, 0xFF800000..)
    // is never a small integer.
    let field_ok = |o: usize| {
        let w = word(o);
        w.is_finite() || (w.to_bits() as i32).unsigned_abs() < 0x1_0000
    };
    if !(8..blob.len() - 3).step_by(4).all(field_ok) {
        return false;
    }
    let t = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
    let Some((first, order, count, stride)) = filter_sections(t) else {
        return true;
    };
    (0..count).all(|s| {
        let base = first + s * stride;
        // c = [b0, -a1, b1, -a2, b2 (, -a3, b3)]
        let mut a = vec![1.0f64];
        for k in 1..=order {
            let o = base + 4 * (2 * k - 1);
            if o + 4 > blob.len() {
                return false;
            }
            a.push(-(word(o) as f64));
        }
        denominator_stable(a)
    })
}

/// Default DSP state file, shared by the CLI and the GUI:
/// `%APPDATA%\revelator-control\dsp-state.txt`.
pub fn default_state_path() -> std::path::PathBuf {
    match std::env::var_os("APPDATA") {
        Some(d) => std::path::PathBuf::from(d)
            .join("revelator-control")
            .join("dsp-state.txt"),
        None => std::path::PathBuf::from("revelator-dsp-state.txt"),
    }
}

/// Load a state file (missing or unreadable file = defaults).
pub fn load_state(path: &std::path::Path) -> FxState {
    std::fs::read_to_string(path)
        .map(|s| FxState::from_text(&s))
        .unwrap_or_default()
}

/// Save a state file, creating its directory.
pub fn save_state(path: &std::path::Path, st: &FxState) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
    }
    std::fs::write(path, st.to_text()).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Short text for a tag, for logs.
pub fn describe(w: &DspWrite) -> String {
    let t = u32::from_le_bytes([w.blob[0], w.blob[1], w.blob[2], w.blob[3]]);
    format!(
        "{} {} ({} bytes)",
        match w.block {
            blk::VOICE_FX => "voicefx".to_string(),
            blk::REVERB => "reverb".to_string(),
            b => crate::frame::fourcc_str(b),
        },
        crate::frame::fourcc_str(t),
        w.blob.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mag_db(c: &Coefs, fs: f64, f: f64) -> f64 {
        // H(z) with b0,b1,b2 / (1 - na1 z^-1 - na2 z^-2)
        let w = 2.0 * std::f64::consts::PI * f / fs;
        let z1 = (w.cos(), -w.sin());
        let z2 = ((2.0 * w).cos(), -(2.0 * w).sin());
        let num = (c[0] + c[2] * z1.0 + c[4] * z2.0, c[2] * z1.1 + c[4] * z2.1);
        let den = (1.0 - c[1] * z1.0 - c[3] * z2.0, -c[1] * z1.1 - c[3] * z2.1);
        let n = (num.0 * num.0 + num.1 * num.1).sqrt();
        let d = (den.0 * den.0 + den.1 * den.1).sqrt();
        20.0 * (n / d).log10()
    }

    #[test]
    fn designer_shapes() {
        let fs = 48_000.0;
        // peak +6 dB at 1 kHz
        let c = design(kind::PEAK, fs, 1000.0, 1.0, 6.0);
        assert!(
            (mag_db(&c, fs, 1000.0) - 6.0).abs() < 0.05,
            "{}",
            mag_db(&c, fs, 1000.0)
        );
        assert!(mag_db(&c, fs, 50.0).abs() < 0.2);
        // HPF: -3 dB at cutoff, deep cut an octave+ below
        let c = design(kind::HPF2, fs, 100.0, std::f64::consts::FRAC_1_SQRT_2, 0.0);
        assert!((mag_db(&c, fs, 100.0) + 3.01).abs() < 0.05);
        assert!(mag_db(&c, fs, 25.0) < -20.0);
        assert!(mag_db(&c, fs, 5000.0).abs() < 0.01);
        // band-pass: 0 dB at centre
        let c = design(kind::BANDPASS, fs, 1200.0, 8.0, 1.0);
        assert!(mag_db(&c, fs, 1200.0).abs() < 0.01);
        // shelves: +6 dB far below/above
        let c = design(kind::LOW_SHELF, fs, 200.0, 0.7, 6.0);
        assert!((mag_db(&c, fs, 20.0) - 6.0).abs() < 0.1);
        assert!(mag_db(&c, fs, 10_000.0).abs() < 0.1);
        let c = design(kind::HIGH_SHELF, fs, 5000.0, 0.7, -6.0);
        assert!((mag_db(&c, fs, 20_000.0) + 6.0).abs() < 0.3);
        assert!(mag_db(&c, fs, 100.0).abs() < 0.1);
        assert_eq!(design(kind::OFF, fs, 1000.0, 1.0, 0.0), IDENTITY);
    }

    #[test]
    fn blob_sizes_and_headers() {
        let st = FxState::default();
        let mut states = vec![st.clone()];
        for m in 1..=5 {
            let mut s = st.clone();
            s.set("voicefx.model", m as f32).unwrap();
            states.push(s);
        }
        for m in 1..=2 {
            let mut s = st.clone();
            s.set("eq.model", m as f32).unwrap();
            states.push(s);
        }
        for (e, st) in [Gate, Comp, Hpf, Limiter, Reverb]
            .into_iter()
            .map(|e| (e, st.clone()))
            .chain(
                states
                    .iter()
                    .flat_map(|s| [(Eq, s.clone()), (VoiceFx, s.clone())]),
            )
        {
            for w in st.writes(e, 48_000.0) {
                let t = u32::from_le_bytes(w.blob[0..4].try_into().unwrap());
                let size = u32::from_le_bytes(w.blob[4..8].try_into().unwrap());
                assert_eq!(Some(size), tag::size_of(t), "{}", describe(&w));
                assert_eq!(w.blob.len(), size as usize);
                assert!(tag::allowed_for(w.block, t), "{}", describe(&w));
                assert_eq!(&w.blob[8..12], &[0, 0, 0, 0]); // engine id
                assert_eq!(w.fixed_index0, t == tag::VOFX);
            }
        }
        for model in [0.0, 1.0, 2.0] {
            let mut s = FxState::default();
            s.set("comp.model", model).unwrap();
            assert_eq!(s.writes(Comp, 48_000.0).len(), 2);
        }
    }

    #[test]
    fn golden_limiter_and_gate() {
        let mut st = FxState::default();
        st.set("limiter.on", 1.0).unwrap();
        st.set("limiter.threshold", -6.0).unwrap();
        let b = st.limiter(48_000.0);
        let f = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        assert_eq!(&b[0..4], b" mil");
        assert_eq!(u32::from_le_bytes(b[12..16].try_into().unwrap()), 1);
        assert!((f(16) - 1.995_262).abs() < 1e-4); // 10^(6/20)
        assert!((f(20) - (-15.707_964f32 / 48_000.0).exp()).abs() < 1e-7);

        st.set("gate.on", 1.0).unwrap();
        st.set("gate.threshold", -40.0).unwrap();
        let g = &st.gate(48_000.0)[1];
        let f = |o: usize| f32::from_le_bytes(g[o..o + 4].try_into().unwrap());
        let u = |o: usize| u32::from_le_bytes(g[o..o + 4].try_into().unwrap());
        assert_eq!(f(0x0C), 1.0); // key filter off: identity
        assert!((f(0x20) - 10f32.powf(-84.0 / 20.0)).abs() < 1e-9); // range
        assert!((f(0x24) - 100.0).abs() < 1e-3); // 1/threshold
        assert_eq!(u(0x38), 8400); // hold: ceil(0.5*0.7*48000*0.5)
        assert_eq!(u(0x3C), 1);
    }

    #[test]
    fn voicefx_sequence_matches_uc() {
        // UC sends `VoFx` and then the model's blobs; never `Setu` or `VFxO`.
        for b in [blk::VOICE_FX, blk::REVERB, blk::EQ] {
            assert!(!tag::allowed_for(b, tag::SETU));
            assert!(!tag::allowed_for(b, tag::VFXO));
        }
        for m in 0..=5 {
            let mut st = FxState::default();
            st.set("voicefx.model", m as f32).unwrap();
            st.set("voicefx.on", 1.0).unwrap();
            let w = st.writes(VoiceFx, 48_000.0);
            assert_eq!(w[0].blob[0..4], tag::VOFX.to_le_bytes());
            assert_eq!(u32::from_le_bytes(w[0].blob[12..16].try_into().unwrap()), m);
            assert!(w[1..].iter().all(|x| !x.fixed_index0));
        }
        for w in FxState::default().all_writes(48_000.0) {
            let t = &w.blob[0..4];
            assert!(t != tag::SETU.to_le_bytes() && t != tag::VFXO.to_le_bytes());
        }
    }

    #[test]
    fn voicefx_models_layout() {
        let mut s = FxState::default();
        let u = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let f = |b: &[u8], o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        s.set("voicefx.on", 1.0).unwrap();
        s.set("voicefx.model", 1.0).unwrap();
        let w = s.writes(VoiceFx, 48_000.0);
        let may4 = &w.last().unwrap().blob;
        assert_eq!(u(may4, 12), 1);
        assert_eq!(u(may4, 16) as i32, -4); // default step 4
        s.set("voicefx.model", 2.0).unwrap();
        let w = s.writes(VoiceFx, 48_000.0);
        assert_eq!(w.len(), 1 + 22 + 2); // VoFx, 22 MBdf, inia, bota
        let bota = &w.last().unwrap().blob;
        assert_eq!((u(bota, 16), f(bota, 20), f(bota, 24)), (1, 80.0, 1.0));
        assert_eq!((u(bota, 28), u(bota, 32), f(bota, 36)), (1, 1, 0.5));
        s.set("voicefx.model", 4.0).unwrap();
        let botc = s.writes(VoiceFx, 48_000.0).pop().unwrap().blob;
        assert_eq!(u(&botc, 16), 900); // 0.5 * 1300 + 250.5
        assert!((f(&botc, 20) - 0.675).abs() < 1e-6 && (f(&botc, 24) - 0.6).abs() < 1e-6);
    }

    #[test]
    fn response_of_sent_filters() {
        let mut s = FxState::default();
        let r = |s: &FxState, f| response_db(&s.writes(Eq, 48_000.0), 48_000.0, f);
        assert!(r(&s, 1000.0).abs() < 1e-3); // EQ off = identity
        s.set("eq.on", 1.0).unwrap();
        s.set("eq.lowmid.freq", 1000.0).unwrap();
        s.set("eq.lowmid.gain", 6.0).unwrap();
        assert!((r(&s, 1000.0) - 6.0).abs() < 0.05, "{}", r(&s, 1000.0));
        assert!(r(&s, 20.0).abs() < 0.3);
        s.set("eq.model", 2.0).unwrap();
        s.set("eq.vintage.lowgain", 12.0).unwrap();
        assert!(r(&s, 40.0) > 6.0, "{}", r(&s, 40.0));
        assert!(r(&s, 5000.0).abs() < 1.0, "{}", r(&s, 5000.0));
        s.set("eq.model", 1.0).unwrap();
        s.set("eq.passive.lowboost", 10.0).unwrap();
        assert!(r(&s, 30.0) > 3.0, "{}", r(&s, 30.0));
        s.set("hpf.freq", 200.0).unwrap();
        let hp = s.writes(Hpf, 48_000.0);
        assert!(response_db(&hp, 48_000.0, 50.0) < -12.0);
    }

    #[test]
    fn eq_models_and_order() {
        let mut s = FxState::default();
        s.set("eq.model", 1.0).unwrap();
        let tags: Vec<u32> = s
            .writes(Eq, 48_000.0)
            .iter()
            .map(|w| u32::from_le_bytes(w.blob[0..4].try_into().unwrap()))
            .collect();
        use tag::*;
        assert_eq!(tags, [BQDF, BQDF, MBDF, MBDF, LFDF, MLFF, BQDF, MBDF]);
        s.set("eq.model", 2.0).unwrap();
        let w = s.writes(Eq, 48_000.0);
        let ml = &w[1].blob;
        assert_eq!(u32::from_le_bytes(ml[0x290..0x294].try_into().unwrap()), 4);
        assert_eq!(
            f32::from_le_bytes(ml[16 + 28..16 + 32].try_into().unwrap()),
            44_100.0
        );
        s.set("eq.swap", 1.0).unwrap();
        let o = s.writes(Order, 48_000.0);
        assert_eq!(o[0].block, blk::OPT);
        assert_eq!(&o[0].blob[0..4], &tag::PARI_TAG.to_le_bytes());
        assert_eq!(o[0].blob.len(), 0x14);
        assert_eq!(&o[0].blob[16..20], &1u32.to_le_bytes());
    }

    #[test]
    fn mbdf_layout() {
        let spec = FilterSpec {
            kind: kind::PEAK,
            freq: 1000.0,
            q: 1.0,
            gain: 3.0,
        };
        let m = mbdf(2, Some(spec), 1.0);
        assert_eq!(m.len(), 0x1F4);
        assert_eq!(u32::from_le_bytes(m[12..16].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(m[0x1F0..0x1F4].try_into().unwrap()), 4);
        for (k, rate) in MULTI_RATES.iter().enumerate() {
            let o = 0x10 + 24 * k;
            assert_eq!(
                f32::from_le_bytes(m[o + 20..o + 24].try_into().unwrap()),
                *rate
            );
            let b0 = f32::from_le_bytes(m[o..o + 4].try_into().unwrap());
            assert!((b0 as f64 - spec.coefs(*rate as f64)[0]).abs() < 1e-6);
        }
        let off = mbdf(0, None, 1.0);
        assert_eq!(f32::from_le_bytes(off[0x10..0x14].try_into().unwrap()), 1.0);
    }

    #[test]
    fn state_text_roundtrip_and_ranges() {
        let mut s = FxState::default();
        s.set("eq.low.gain", 4.5).unwrap();
        assert!(s.set("eq.low.gain", 40.0).is_err());
        assert!(s.set("nope", 1.0).is_err());
        assert!(s.set("voicefx.model", 6.0).is_err());
        s.set("voicefx.model", 5.0).unwrap();
        let w = s.writes(VoiceFx, 48_000.0);
        assert_eq!(w.len(), 2); // VoFx, Vech
        assert_eq!(&w[0].blob[12..16], &5u32.to_le_bytes());
        let back = FxState::from_text(&s.to_text());
        assert_eq!(back, s);
    }
}

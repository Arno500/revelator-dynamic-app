//! Typed parameter table for the Revelator Dynamic.
//!
//! All block numbers, block indices, parameter ids and state slots live in this one file so they
//! are easy to correct once the real device has been probed. Every entry carries a
//! [`Confidence`]. Keys follow the UC `.scene`/`.channel` naming where one exists
//! (`preampgain`, `gate.threshold`, `eq.eqfreq1`, ...), so preset import maps 1:1.
//!
//! Values in the API are in user units (dB, Hz, seconds, 0/1 flags). The wire value is assumed
//! to equal the user value; [`Scale`] only describes how to map to a normalized 0..1 slider.

use crate::safety::block;

/// How sure we are about a table entry's block/id/slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Confirmed on real Dynamic hardware.
    Verified,
    /// Taken from the public Revelator io24 protocol notes; same family, not yet confirmed.
    FromIo24Notes,
    /// Placeholder invented to make the table complete.
    Guess,
}

/// Wire type of the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `Para` tag, f32.
    F32,
    /// `Pari` tag, i32 (flags, enums).
    I32,
}

/// Mapping between user value and normalized 0..1 slider position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    Linear,
    /// Linear in dB between min and max (identical to `Linear` numerically; kept distinct for UI).
    Db,
    /// Logarithmic (frequencies, times). `min` must be > 0.
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Preamp,
    Monitor,
    Presets,
    Mixer,
    FatChannel,
    VoiceFx,
    Reverb,
}

/// One parameter definition.
#[derive(Debug, Clone, Copy)]
pub struct ParamDef {
    /// Stable key, e.g. `"preampgain"`, `"mic.volume"`, `"eq.eqfreq2"`.
    pub key: &'static str,
    pub label: &'static str,
    pub group: Group,
    /// SetP block number.
    pub block: u32,
    /// SetP block index (e.g. mixer strip).
    pub index: u32,
    /// Para/Pari parameter id.
    pub id: u32,
    pub kind: Kind,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// `"dB"`, `"Hz"`, `"s"`, `""` (unitless), `"on/off"` ...
    pub unit: &'static str,
    pub scale: Scale,
    pub confidence: Confidence,
    /// Index of this parameter's f32 in the JaSt snapshot (see `State`). Assigned automatically
    /// from the table position; a hypothesis like everything else.
    pub slot: u16,
}

impl ParamDef {
    /// Clamp into `[min, max]`.
    pub fn clamp(&self, v: f32) -> f32 {
        v.clamp(self.min, self.max)
    }

    /// True for on/off style flags.
    pub fn is_toggle(&self) -> bool {
        self.kind == Kind::I32 && self.min == 0.0 && self.max == 1.0
    }

    /// User value -> 0..1.
    pub fn to_normalized(&self, v: f32) -> f32 {
        let v = self.clamp(v);
        let n = match self.scale {
            Scale::Linear | Scale::Db => (v - self.min) / (self.max - self.min),
            Scale::Log => (v / self.min).ln() / (self.max / self.min).ln(),
        };
        n.clamp(0.0, 1.0)
    }

    /// 0..1 -> user value.
    pub fn from_normalized(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        match self.scale {
            Scale::Linear | Scale::Db => self.min + n * (self.max - self.min),
            Scale::Log => self.min * (self.max / self.min).powf(n),
        }
    }
}

/// Look up by key.
pub fn find(key: &str) -> Option<&'static ParamDef> {
    ALL.iter().find(|p| p.key == key)
}

/// Look up by wire address.
pub fn find_by_wire(block: u32, index: u32, id: u32) -> Option<&'static ParamDef> {
    ALL.iter()
        .find(|p| p.block == block && p.index == index && p.id == id)
}

/// Iterate a group.
pub fn group(g: Group) -> impl Iterator<Item = &'static ParamDef> {
    ALL.iter().filter(move |p| p.group == g)
}

// ---------------------------------------------------------------------------------------------
// Table construction
// ---------------------------------------------------------------------------------------------

use Confidence::{FromIo24Notes as Io, Guess as Gs};
use Group as G;
use Kind::{F32, I32};
use Scale::{Db, Linear, Log};

/// First JaSt float slot assigned to table entries (guess).
const SLOT_BASE: u16 = 100;

#[allow(clippy::too_many_arguments)]
const fn p(
    key: &'static str,
    label: &'static str,
    group: Group,
    block: u32,
    index: u32,
    id: u32,
    kind: Kind,
    min: f32,
    max: f32,
    default: f32,
    unit: &'static str,
    scale: Scale,
    confidence: Confidence,
) -> ParamDef {
    ParamDef {
        key,
        label,
        group,
        block,
        index,
        id,
        kind,
        min,
        max,
        default,
        unit,
        scale,
        confidence,
        slot: 0,
    }
}

const MX: u32 = block::MIXER;
const FC: u32 = block::FAT_CHANNEL;
const VFX: u32 = block::VOICE_FX;
const RVB: u32 = block::REVERB;

// Mixer input strips: index 0..3 = Mic, Playback, Virtual A, Virtual B.
macro_rules! strip {
    ($k:literal, $l:literal, $i:expr) => {
        [
            p(
                concat!($k, ".volume"),
                concat!($l, " level to Main"),
                G::Mixer,
                MX,
                $i,
                20,
                F32,
                -96.0,
                10.0,
                0.0,
                "dB",
                Db,
                Gs,
            ),
            p(
                concat!($k, ".aux1"),
                concat!($l, " level to Mix A"),
                G::Mixer,
                MX,
                $i,
                21,
                F32,
                -96.0,
                10.0,
                -96.0,
                "dB",
                Db,
                Gs,
            ),
            p(
                concat!($k, ".aux2"),
                concat!($l, " level to Mix B"),
                G::Mixer,
                MX,
                $i,
                22,
                F32,
                -96.0,
                10.0,
                -96.0,
                "dB",
                Db,
                Gs,
            ),
            p(
                concat!($k, ".mute"),
                concat!($l, " mute"),
                G::Mixer,
                MX,
                $i,
                23,
                I32,
                0.0,
                1.0,
                0.0,
                "on/off",
                Linear,
                Gs,
            ),
            p(
                concat!($k, ".solo"),
                concat!($l, " solo"),
                G::Mixer,
                MX,
                $i,
                24,
                I32,
                0.0,
                1.0,
                0.0,
                "on/off",
                Linear,
                Gs,
            ),
        ]
    };
}

macro_rules! eq_band {
    ($n:literal, $base:expr, $f:expr, $q:expr) => {
        [
            p(
                concat!("eq.eqgain", $n),
                concat!("EQ band ", $n, " gain"),
                G::FatChannel,
                FC,
                0,
                $base,
                F32,
                -18.0,
                18.0,
                0.0,
                "dB",
                Db,
                Gs,
            ),
            p(
                concat!("eq.eqq", $n),
                concat!("EQ band ", $n, " Q"),
                G::FatChannel,
                FC,
                0,
                $base + 1,
                F32,
                0.1,
                10.0,
                $q,
                "",
                Log,
                Gs,
            ),
            p(
                concat!("eq.eqfreq", $n),
                concat!("EQ band ", $n, " frequency"),
                G::FatChannel,
                FC,
                0,
                $base + 2,
                F32,
                20.0,
                20000.0,
                $f,
                "Hz",
                Log,
                Gs,
            ),
            p(
                concat!("eq.eqbandon", $n),
                concat!("EQ band ", $n, " on"),
                G::FatChannel,
                FC,
                0,
                $base + 3,
                I32,
                0.0,
                1.0,
                1.0,
                "on/off",
                Linear,
                Gs,
            ),
        ]
    };
}

const MIC: [ParamDef; 5] = strip!("mic", "Mic", 0);
const PLAYBACK: [ParamDef; 5] = strip!("playback", "Playback", 1);
const VIRT_A: [ParamDef; 5] = strip!("virtuala", "Virtual A", 2);
const VIRT_B: [ParamDef; 5] = strip!("virtualb", "Virtual B", 3);

// Mixer masters: index 4 = Main, 5 = Mix A, 6 = Mix B.
const MASTERS: [ParamDef; 6] = [
    p(
        "main.volume",
        "Main level",
        G::Mixer,
        MX,
        4,
        20,
        F32,
        -96.0,
        10.0,
        0.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "main.mute",
        "Main mute",
        G::Mixer,
        MX,
        4,
        23,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "mixa.volume",
        "Mix A level",
        G::Mixer,
        MX,
        5,
        20,
        F32,
        -96.0,
        10.0,
        0.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "mixa.mute",
        "Mix A mute",
        G::Mixer,
        MX,
        5,
        23,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "mixb.volume",
        "Mix B level",
        G::Mixer,
        MX,
        6,
        20,
        F32,
        -96.0,
        10.0,
        0.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "mixb.mute",
        "Mix B mute",
        G::Mixer,
        MX,
        6,
        23,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
];

// Device-level controls. Block 100 index 7 = "global" is a guess; presets ids from io24 notes.
const GLOBAL: [ParamDef; 9] = [
    p(
        "preampgain",
        "Preamp gain",
        G::Preamp,
        MX,
        7,
        1,
        F32,
        0.0,
        60.0,
        22.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "micmute",
        "Mic mute",
        G::Preamp,
        MX,
        7,
        2,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "hpVolume",
        "Headphone volume",
        G::Monitor,
        MX,
        7,
        3,
        F32,
        -96.0,
        0.0,
        -12.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "mainVolume",
        "Main volume",
        G::Monitor,
        MX,
        7,
        4,
        F32,
        -96.0,
        0.0,
        0.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "monitorBlend",
        "Monitor blend (mic <-> playback)",
        G::Monitor,
        MX,
        7,
        5,
        F32,
        0.0,
        1.0,
        0.5,
        "",
        Linear,
        Gs,
    ),
    p(
        "presetSlot",
        "Active preset slot",
        G::Presets,
        MX,
        0,
        150,
        I32,
        0.0,
        3.0,
        0.0,
        "",
        Linear,
        Io,
    ),
    p(
        "presetButtonMode",
        "Preset button mode",
        G::Presets,
        MX,
        7,
        6,
        I32,
        0.0,
        3.0,
        0.0,
        "",
        Linear,
        Gs,
    ),
    p(
        "muteButtonMode",
        "Mute button mode",
        G::Presets,
        MX,
        7,
        7,
        I32,
        0.0,
        3.0,
        0.0,
        "",
        Linear,
        Gs,
    ),
    p(
        "presetHotKey",
        "Preset hot key",
        G::Presets,
        MX,
        0,
        151,
        I32,
        0.0,
        3.0,
        0.0,
        "",
        Linear,
        Io,
    ),
];

// Fat Channel singles. Block 101 is a guess.
const FAT: [ParamDef; 25] = [
    p(
        "filter.hpf",
        "High-pass frequency",
        G::FatChannel,
        FC,
        0,
        300,
        F32,
        20.0,
        500.0,
        24.0,
        "Hz",
        Log,
        Gs,
    ),
    p(
        "gate.on",
        "Gate on",
        G::FatChannel,
        FC,
        0,
        310,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "gate.keylisten",
        "Gate key listen",
        G::FatChannel,
        FC,
        0,
        311,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "gate.expander",
        "Gate expander mode",
        G::FatChannel,
        FC,
        0,
        312,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "gate.keyfilter",
        "Gate key filter",
        G::FatChannel,
        FC,
        0,
        313,
        F32,
        20.0,
        20000.0,
        200.0,
        "Hz",
        Log,
        Gs,
    ),
    p(
        "gate.threshold",
        "Gate threshold",
        G::FatChannel,
        FC,
        0,
        314,
        F32,
        -84.0,
        0.0,
        -60.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "gate.range",
        "Gate range",
        G::FatChannel,
        FC,
        0,
        315,
        F32,
        -96.0,
        0.0,
        -40.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "gate.attack",
        "Gate attack",
        G::FatChannel,
        FC,
        0,
        316,
        F32,
        0.0001,
        0.1,
        0.001,
        "s",
        Log,
        Gs,
    ),
    p(
        "gate.release",
        "Gate release",
        G::FatChannel,
        FC,
        0,
        317,
        F32,
        0.01,
        2.0,
        0.2,
        "s",
        Log,
        Gs,
    ),
    p(
        "comp.on",
        "Compressor on",
        G::FatChannel,
        FC,
        0,
        320,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "comp.softknee",
        "Compressor soft knee",
        G::FatChannel,
        FC,
        0,
        321,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "comp.automode",
        "Compressor auto mode",
        G::FatChannel,
        FC,
        0,
        322,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "comp.threshold",
        "Compressor threshold",
        G::FatChannel,
        FC,
        0,
        323,
        F32,
        -60.0,
        0.0,
        -20.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "comp.ratio",
        "Compressor ratio",
        G::FatChannel,
        FC,
        0,
        324,
        F32,
        1.0,
        20.0,
        2.0,
        ":1",
        Log,
        Gs,
    ),
    p(
        "comp.attack",
        "Compressor attack",
        G::FatChannel,
        FC,
        0,
        325,
        F32,
        0.0001,
        0.1,
        0.001,
        "s",
        Log,
        Gs,
    ),
    p(
        "comp.release",
        "Compressor release",
        G::FatChannel,
        FC,
        0,
        326,
        F32,
        0.01,
        2.0,
        0.15,
        "s",
        Log,
        Gs,
    ),
    p(
        "comp.gain",
        "Compressor makeup gain",
        G::FatChannel,
        FC,
        0,
        327,
        F32,
        0.0,
        24.0,
        0.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "limit.limiteron",
        "Limiter on",
        G::FatChannel,
        FC,
        0,
        340,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "limit.threshold",
        "Limiter threshold",
        G::FatChannel,
        FC,
        0,
        341,
        F32,
        -30.0,
        0.0,
        0.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "eq.eqallon",
        "EQ on",
        G::FatChannel,
        FC,
        0,
        330,
        I32,
        0.0,
        1.0,
        1.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "eq.eqbandop1",
        "EQ band 1 shape (0 bell, 1 shelf)",
        G::FatChannel,
        FC,
        0,
        339,
        I32,
        0.0,
        1.0,
        0.0,
        "",
        Linear,
        Gs,
    ),
    p(
        "eq.eqbandop4",
        "EQ band 4 shape (0 bell, 1 shelf)",
        G::FatChannel,
        FC,
        0,
        438,
        I32,
        0.0,
        1.0,
        0.0,
        "",
        Linear,
        Gs,
    ),
    p(
        "voicefx.mix",
        "Voice FX mix",
        G::VoiceFx,
        MX,
        0,
        4,
        F32,
        0.0,
        1.0,
        0.0,
        "",
        Linear,
        Io,
    ),
    p(
        "voicefx.processingChannel",
        "Voice FX input",
        G::VoiceFx,
        VFX,
        0,
        149,
        I32,
        0.0,
        1.0,
        0.0,
        "",
        Linear,
        Io,
    ),
    p(
        "voicefx.model",
        "Voice FX model",
        G::VoiceFx,
        VFX,
        0,
        450,
        I32,
        0.0,
        10.0,
        0.0,
        "",
        Linear,
        Io,
    ),
];

const EQ1: [ParamDef; 4] = eq_band!(1, 330 + 1, 69.0, 0.7);
const EQ2: [ParamDef; 4] = eq_band!(2, 340 + 10, 240.0, 1.0);
const EQ3: [ParamDef; 4] = eq_band!(3, 360 + 10, 1400.0, 1.0);
const EQ4: [ParamDef; 4] = eq_band!(4, 380 + 10, 8000.0, 0.7);

const VOICEFX_PARAMS: [ParamDef; 4] = [
    p(
        "voicefx.p1",
        "Voice FX parameter 1",
        G::VoiceFx,
        VFX,
        0,
        451,
        F32,
        0.0,
        1.0,
        0.5,
        "",
        Linear,
        Gs,
    ),
    p(
        "voicefx.p2",
        "Voice FX parameter 2",
        G::VoiceFx,
        VFX,
        0,
        452,
        F32,
        0.0,
        1.0,
        0.5,
        "",
        Linear,
        Gs,
    ),
    p(
        "voicefx.p3",
        "Voice FX parameter 3",
        G::VoiceFx,
        VFX,
        0,
        453,
        F32,
        0.0,
        1.0,
        0.5,
        "",
        Linear,
        Gs,
    ),
    p(
        "voicefx.p4",
        "Voice FX parameter 4",
        G::VoiceFx,
        VFX,
        0,
        454,
        F32,
        0.0,
        1.0,
        0.5,
        "",
        Linear,
        Gs,
    ),
];

const REVERB: [ParamDef; 4] = [
    p(
        "reverb.on",
        "Reverb on",
        G::Reverb,
        RVB,
        0,
        1,
        I32,
        0.0,
        1.0,
        0.0,
        "on/off",
        Linear,
        Gs,
    ),
    p(
        "reverb.mix",
        "Reverb level",
        G::Reverb,
        RVB,
        0,
        2,
        F32,
        -96.0,
        0.0,
        -96.0,
        "dB",
        Db,
        Gs,
    ),
    p(
        "reverb.size",
        "Reverb size",
        G::Reverb,
        RVB,
        0,
        3,
        F32,
        0.0,
        1.0,
        0.5,
        "",
        Linear,
        Gs,
    ),
    p(
        "reverb.decay",
        "Reverb decay",
        G::Reverb,
        RVB,
        0,
        4,
        F32,
        0.1,
        10.0,
        1.5,
        "s",
        Log,
        Gs,
    ),
];

const PARTS: &[&[ParamDef]] = &[
    &GLOBAL,
    &MIC,
    &PLAYBACK,
    &VIRT_A,
    &VIRT_B,
    &MASTERS,
    &FAT,
    &EQ1,
    &EQ2,
    &EQ3,
    &EQ4,
    &VOICEFX_PARAMS,
    &REVERB,
];

const fn total(parts: &[&[ParamDef]]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < parts.len() {
        n += parts[i].len();
        i += 1;
    }
    n
}

const N: usize = total(PARTS);

const fn join(parts: &[&[ParamDef]]) -> [ParamDef; N] {
    let mut out = [GLOBAL[0]; N];
    let mut k = 0;
    let mut i = 0;
    while i < parts.len() {
        let mut j = 0;
        while j < parts[i].len() {
            let mut d = parts[i][j];
            d.slot = SLOT_BASE + k as u16;
            out[k] = d;
            k += 1;
            j += 1;
        }
        i += 1;
    }
    out
}

static TABLE: [ParamDef; N] = join(PARTS);

/// The complete parameter table.
pub static ALL: &[ParamDef] = &TABLE;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keys_wire_and_slots_unique() {
        let mut keys = HashSet::new();
        let mut wires = HashSet::new();
        let mut slots = HashSet::new();
        for d in ALL {
            assert!(keys.insert(d.key), "dup key {}", d.key);
            assert!(
                wires.insert((d.block, d.index, d.id)),
                "dup wire {} {:?}",
                d.key,
                (d.block, d.index, d.id)
            );
            assert!(slots.insert(d.slot));
            assert!(d.min < d.max, "{}", d.key);
            assert!(
                d.default >= d.min && d.default <= d.max,
                "default of {}",
                d.key
            );
            if d.scale == Scale::Log {
                assert!(d.min > 0.0);
            }
        }
        assert!(ALL.len() > 80);
    }

    #[test]
    fn lookups() {
        assert_eq!(find("preampgain").unwrap().unit, "dB");
        assert!(find("nope").is_none());
        let d = find("mic.volume").unwrap();
        assert_eq!(
            find_by_wire(d.block, d.index, d.id).unwrap().key,
            "mic.volume"
        );
        assert!(group(Group::Mixer).count() >= 26);
    }

    #[test]
    fn scaling_roundtrip() {
        for d in ALL {
            for n in [0.0, 0.25, 0.5, 1.0] {
                let v = d.from_normalized(n);
                assert!((d.to_normalized(v) - n).abs() < 1e-4, "{}", d.key);
            }
        }
        let f = find("eq.eqfreq1").unwrap();
        // log: geometric mean of 20..20000 is ~632 Hz
        assert!((f.from_normalized(0.5) - 632.4).abs() < 1.0);
        let g = find("preampgain").unwrap();
        assert_eq!(g.from_normalized(0.5), 30.0);
        assert_eq!(g.clamp(100.0), 60.0);
    }
}

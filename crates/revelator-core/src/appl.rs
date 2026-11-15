//! Device-level ("Appl") parameters, with write addresses taken from Universal Control's
//! `hwaccess/dspusbdevice.dll` (see `docs/uc-dll-notes.md`).
//!
//! UC binds each of these to `SetP Appl 0` with blob `[Para|Pari, 0x14, y, x, value]`, where
//! `x` selects the parameter and `y` is the channel (0 on the single-input Dynamic). Floats go
//! as `Para`, everything else as `Pari`. Volumes are sent as a 0..1 position (UC converts its dB
//! value through the curve first); we expose that position directly, the same value the state
//! reports.
//!
//! Readback slots come from the JaSt state; see [`crate::device::slots`].

use crate::params::Kind;

/// Where a parameter shows up in the JaSt state, for readback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readback {
    /// f32 slot.
    F32(usize),
    /// Integer slot (raw bits).
    I32(usize),
    /// One bit of an integer slot.
    Bit(usize, u32),
    /// Not located in the state.
    None,
}

impl Readback {
    /// The value in a state reply (`None` if not located or not in the reply).
    pub fn read(self, st: &crate::device::State) -> Option<f32> {
        match self {
            Readback::F32(i) => st.slot(i).filter(|v| v.is_finite()),
            Readback::I32(i) => st.slot_bits(i).map(|b| b as i32 as f32),
            Readback::Bit(i, bit) => st.slot_bits(i).map(|b| ((b >> bit) & 1) as f32),
            Readback::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ApplParam {
    pub key: &'static str,
    pub label: &'static str,
    /// Parameter selector (blob field after `y`).
    pub x: u32,
    /// Channel.
    pub y: u32,
    pub kind: Kind,
    pub min: f32,
    pub max: f32,
    pub unit: &'static str,
    pub readback: Readback,
}

#[allow(clippy::too_many_arguments)]
const fn a(
    key: &'static str,
    label: &'static str,
    x: u32,
    kind: Kind,
    min: f32,
    max: f32,
    unit: &'static str,
    readback: Readback,
) -> ApplParam {
    ApplParam {
        key,
        label,
        x,
        y: 0,
        kind,
        min,
        max,
        unit,
        readback,
    }
}

/// The writable set. Only these `x` values pass the safety allow-list.
pub const ALL: &[ApplParam] = &[
    a(
        "preamp.gain",
        "Preamp gain (UC preampgain)",
        3,
        Kind::F32,
        0.0,
        60.0,
        "dB",
        Readback::F32(48),
    ),
    a(
        "mic.mute",
        "Mic mute (UC hardwareMute)",
        7,
        Kind::I32,
        0.0,
        1.0,
        "on/off",
        Readback::Bit(44, 3),
    ),
    a(
        "preset",
        "Active preset slot 0..3 (UC activePresetSlotIndex)",
        16,
        Kind::I32,
        0.0,
        3.0,
        "",
        Readback::I32(42),
    ),
    a(
        "headphones.volume",
        "Headphone volume position 0..1 (UC phonesVolume)",
        1,
        Kind::F32,
        0.0,
        1.0,
        "",
        Readback::F32(45),
    ),
    a(
        "headphones.mute",
        "Headphones mute (UC phonesMute)",
        6,
        Kind::I32,
        0.0,
        1.0,
        "on/off",
        // confirmed on hardware by `readback-test`: flag word bit 2 follows this write
        Readback::Bit(44, 2),
    ),
    a(
        "main.volume",
        "Main output volume position 0..1 (UC mainOutVolume)",
        2,
        Kind::F32,
        0.0,
        1.0,
        "",
        Readback::F32(46),
    ),
    a(
        "monitor.blend",
        "Monitor blend -1..1 (UC monitorBlend)",
        10,
        Kind::F32,
        -1.0,
        1.0,
        "",
        Readback::F32(47),
    ),
    // UC processingChannel (id 149, -1..1, default -1 = unassigned), binding @0x180041xxx, y = channel.
    // UC sends it as `Pari`; the firmware's Pari handler maps host x=12 to states 19/20, which
    // forward it as block 0 `Pari` x=12.
    a(
        "fx.channel",
        "Processing channel assigned to the FX engines -1..1 (UC processingChannel)",
        12,
        Kind::I32,
        -1.0,
        1.0,
        "",
        Readback::None,
    ),
    // Appl x=4 is UC param 600. UC's channel handler (dspusbdevice.dll 0x180041f7b..0x180042054)
    // sets it to `dspAmount` (id 148, "DSP Amount", default 1.0) unless `bypassDSP` (id 147) is on,
    // in which case it sends 0. So UC pushes 1.0 here by default; a device that never received it
    // may scale its effects by 0. (The firmware labels +0x10d0 "input 2 gain"; unverified which
    // is right, it did not move any state slot when written earlier.) EXPERIMENTAL probe.
    a(
        "fx.amount",
        "DSP amount 0..1, 0 = bypass (UC dspAmount/bypassDSP, param 600)",
        4,
        Kind::F32,
        0.0,
        1.0,
        "",
        Readback::None,
    ),
    // UC outputDelayBus (id 6, UI 0..4, 0 = off), binding @0x180036f0f x=13. UC sends UI - 1,
    // so -1 = off. The firmware maps host Pari x=13 to its state 23, which drives block 0 Pari
    // x=8 (the output delay line, default 0.25 s). A bus >= 0 switches the delay on, and that
    // tears Voice FX down (they share memory), so the app pushes -1 on connect.
    a(
        "output.delay.bus",
        "Output delay bus: -1 = off, 0..3 = delayed bus (UC outputDelayBus - 1)",
        13,
        Kind::I32,
        -1.0,
        3.0,
        "",
        Readback::None,
    ),
    // UC muteButtonMode (id 20, enum "Phones"/"Mic", default 1), binding @0x1800371bf.
    // Probably the firmware's muteMode, flag bit 2, but its polarity is unknown (idle 0 while
    // the button mutes the mic), so no readback until `readback-test` settles it.
    a(
        "mute.button",
        "Mute button mutes: 0 = headphones, 1 = mic (UC muteButtonMode)",
        8,
        Kind::I32,
        0.0,
        1.0,
        "",
        Readback::None,
    ),
    // UC presetButtonMode (id 8, 0..2, default 2; UC labels Single / Double / Multi),
    // binding @0x180037219.
    a(
        "preset.button",
        "Preset button: 0 = single, 1 = double, 2 = multi (UC presetButtonMode)",
        17,
        Kind::I32,
        0.0,
        2.0,
        "",
        Readback::None,
    ),
];

pub fn find(key: &str) -> Option<&'static ApplParam> {
    ALL.iter().find(|p| p.key == key)
}

/// True if `x` is one of the writable Appl selectors.
pub fn is_writable_x(x: u32) -> bool {
    ALL.iter().any(|p| p.x == x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keys_and_selectors_unique() {
        let mut k = HashSet::new();
        let mut x = HashSet::new();
        for p in ALL {
            assert!(k.insert(p.key), "{}", p.key);
            assert!(x.insert((p.x, p.y)), "{}", p.key);
            assert!(p.min < p.max);
        }
        assert!(is_writable_x(3));
        assert!(is_writable_x(17));
        assert!(!is_writable_x(11)); // phonesSrc: not exposed
    }
}

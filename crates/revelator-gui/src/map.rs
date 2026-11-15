//! The single GUI-id -> device-parameter mapping table (plus unit conversions).
//!
//! Device writes go only through `revelator_core::appl` (the `SetP Appl 0` addresses taken from
//! Universal Control's DLL, read back where the state slot is known) and, for `mixer.*` ids,
//! `revelator_core::mixer` (crosspoint gains, see [`mixer_key`]). GUI parameters with neither
//! are not sent and their controls are disabled outside demo mode (see [`hw_supported`]).
//! The `dsp.*` parameters go through `revelator_core::fx` instead (see `dsp.rs`).

use crate::model::Confidence;
use revelator_core::appl::{self, ApplParam};
use revelator_core::State;

/// How a GUI value relates to the device value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Xf {
    /// Same unit.
    Id,
    /// GUI shows 0..100 %; linear over the device param's `[min, max]`.
    Pct,
}

pub struct Map {
    pub gui: &'static str,
    pub key: &'static str,
    pub xf: Xf,
}

/// GUI parameters with deliberately no device counterpart.
/// - `sys.samplerate`: not a device parameter; forwarded to `Device::set_sample_rate`.
pub const SKIPPED: [&str; 1] = ["sys.samplerate"];

const TABLE: [Map; 7] = [
    Map {
        gui: "mic.gain",
        key: "preamp.gain",
        xf: Xf::Id,
    },
    Map {
        gui: "mic.mute",
        key: "mic.mute",
        xf: Xf::Id,
    },
    Map {
        gui: "mic.hp_volume",
        key: "headphones.volume",
        xf: Xf::Pct,
    },
    Map {
        gui: "mic.monitor_blend",
        key: "monitor.blend",
        xf: Xf::Pct,
    },
    Map {
        gui: "mic.slot",
        key: "preset",
        xf: Xf::Id,
    },
    Map {
        gui: "mic.preset_button_mode",
        key: "preset.button",
        xf: Xf::Id,
    },
    Map {
        gui: "mic.mute_button_mode",
        key: "mute.button",
        xf: Xf::Id,
    },
];

/// Stream-mixer key (`revelator_core::mixer`) behind a GUI mixer id.
pub fn mixer_key(gui: &str) -> Option<String> {
    let rest = gui.strip_prefix("mixer.")?;
    let (head, field) = rest.rsplit_once('.')?;
    if let Some(bus) = head.strip_prefix("out.") {
        let f = match field {
            "vol" => "volume",
            "mute" => "mute",
            _ => return None,
        };
        return matches!(bus, "main" | "mixa" | "mixb").then(|| format!("{bus}.{f}"));
    }
    let src = match head {
        "mic" => "mic",
        "playback" => "playback",
        "va" => "virtuala",
        "vb" => "virtualb",
        "rev" => "reverb",
        _ => return None,
    };
    if src == "mic" {
        // the mic channel's own settings that UC pushes with the mixer
        let k = match field {
            "reverb" => Some("mic.reverb"),
            "bypass" => Some("mic.bypass"),
            "dspamount" => Some("mic.dspamount"),
            "dawpost" => Some("mic.dawpostdsp"),
            _ => None,
        };
        if let Some(k) = k {
            return Some(k.into());
        }
    }
    let f = match field {
        "main" => "volume",
        "mixa" => "aux1",
        "mixb" => "aux2",
        "mute" => "mute",
        "solo" => "solo",
        _ => return None,
    };
    Some(format!("{src}.{f}"))
}

/// GUI mixer id for a stream-mixer key (inverse of [`mixer_key`]).
pub fn mixer_gui_id(key: &str) -> Option<String> {
    let (head, field) = key.split_once('.')?;
    if matches!(head, "main" | "mixa" | "mixb") {
        let f = match field {
            "volume" => "vol",
            "mute" => "mute",
            _ => return None,
        };
        return Some(format!("mixer.out.{head}.{f}"));
    }
    let ch = match head {
        "mic" => "mic",
        "playback" => "playback",
        "virtuala" => "va",
        "virtualb" => "vb",
        "reverb" => "rev",
        _ => return None,
    };
    match key {
        "mic.reverb" => return Some("mixer.mic.reverb".into()),
        "mic.bypass" => return Some("mixer.mic.bypass".into()),
        "mic.dspamount" => return Some("mixer.mic.dspamount".into()),
        "mic.dawpostdsp" => return Some("mixer.mic.dawpost".into()),
        _ => {}
    }
    let f = match field {
        "volume" => "main",
        "aux1" => "mixa",
        "aux2" => "mixb",
        "mute" => "mute",
        "solo" => "solo",
        _ => return None,
    };
    Some(format!("mixer.{ch}.{f}"))
}

/// GUI meter ids (dBFS; the reduction in positive dB) from a state reply. Slots are inferred, see
/// `revelator_core::meters`.
pub fn meters(st: &State) -> std::collections::HashMap<String, f32> {
    use revelator_core::meters as m;
    let mut out = std::collections::HashMap::new();
    let mut put = |id: &str, slots: &[usize]| {
        if let Some(p) = m::peak(st, slots) {
            out.insert(id.to_string(), m::level_db(p));
        }
    };
    put("meter.mic", m::MIC_IN);
    put("meter.ch.mic", m::MIC_MIX);
    put("meter.ch.playback", m::PLAYBACK);
    put("meter.ch.va", m::VIRTUAL_A);
    put("meter.ch.vb", m::VIRTUAL_B);
    put("meter.ch.rev", m::REVERB);
    put("meter.out.main", m::OUT_MAIN);
    put("meter.out.mixa", m::OUT_MIXA);
    put("meter.out.mixb", m::OUT_MIXB);
    if let Some(r) = m::reduction_db(st) {
        out.insert("meter.reduction".to_string(), r);
    }
    out
}

pub fn table() -> &'static [Map] {
    &TABLE
}

pub fn lookup(gui: &str) -> Option<&'static Map> {
    TABLE.iter().find(|e| e.gui == gui)
}

/// Device parameter for a mapped entry (table is validated by tests).
pub fn def(e: &Map) -> &'static ApplParam {
    appl::find(e.key).expect("map table names an appl key")
}

/// True if the real device can take this GUI parameter (mapped, DSP, or the sample rate).
pub fn hw_supported(gui: &str) -> bool {
    lookup(gui).is_some()
        || crate::dsp::param(gui).is_some()
        || mixer_key(gui).is_some()
        || SKIPPED.contains(&gui)
}

pub fn to_core(e: &Map, d: &ApplParam, gui: f32) -> f32 {
    let v = match e.xf {
        Xf::Id => gui,
        Xf::Pct => d.min + gui.clamp(0.0, 100.0) / 100.0 * (d.max - d.min),
    };
    v.clamp(d.min, d.max)
}

pub fn to_gui(e: &Map, d: &ApplParam, core: f32) -> f32 {
    match e.xf {
        Xf::Id => core,
        Xf::Pct => (core - d.min) / (d.max - d.min) * 100.0,
    }
}

/// Device value of `d` in a state reply, from its confirmed slot.
pub fn read(d: &ApplParam, st: &State) -> Option<f32> {
    d.readback.read(st)
}

/// Confidence of a mapped GUI id: the write addresses come from UC's DLL, not yet confirmed
/// on hardware from this app.
pub fn confidence(gui: &str) -> Option<Confidence> {
    (lookup(gui).is_some() || mixer_key(gui).is_some()).then_some(Confidence::Inferred)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{all_specs, spec};
    use std::collections::HashSet;

    #[test]
    fn every_mapped_entry_is_consistent() {
        let mut seen = HashSet::new();
        for e in table() {
            let d = def(e);
            assert!(spec(e.gui).is_some(), "unknown gui id {}", e.gui);
            assert!(seen.insert(d.key), "dup device key {}", d.key);
        }
    }

    #[test]
    fn unmapped_gui_ids_are_not_sent() {
        for s in all_specs() {
            let mapped = lookup(&s.id).is_some();
            let dsp = crate::dsp::param(&s.id).is_some();
            let mix = mixer_key(&s.id);
            let n = [mapped, dsp, mix.is_some()].iter().filter(|b| **b).count();
            assert!(n <= 1, "{}", s.id);
            if let Some(k) = &mix {
                let m = revelator_core::mixer::MixerState::default();
                // GUI defaults are UC's default scene
                assert_eq!(m.get(k), Some(s.default), "{} -> {k}", s.id);
                assert_eq!(mixer_gui_id(k).as_deref(), Some(s.id.as_str()));
            }
            // every GUI parameter now reaches the device
            assert!(hw_supported(&s.id), "{}", s.id);
        }
        assert_eq!(mixer_key("mixer.va.mixb").as_deref(), Some("virtuala.aux2"));
        assert_eq!(
            mixer_key("mixer.out.mixa.vol").as_deref(),
            Some("mixa.volume")
        );
        assert_eq!(mixer_key("mixer.out.mixa.solo"), None);
        assert_eq!(
            mixer_key("mixer.rev.main").as_deref(),
            Some("reverb.volume")
        );
        assert_eq!(mixer_key("mixer.mic.reverb").as_deref(), Some("mic.reverb"));
        assert_eq!(mixer_key("mixer.va.reverb"), None);
    }

    #[test]
    fn meters_come_from_the_state_slots() {
        let mut v = vec![0.0f32; 53];
        v[20..=25].fill(1.0);
        v[6] = 0.1;
        v[8] = 0.5;
        v[9] = 0.25;
        v[14] = 1.0;
        v[20] = 0.5;
        let m = meters(&State::from_floats(v));
        assert!((m["meter.mic"] + 20.0).abs() < 0.01);
        assert!((m["meter.ch.mic"] + 6.02).abs() < 0.01);
        assert_eq!(m["meter.out.main"], 0.0);
        assert_eq!(m["meter.out.mixa"], -96.0);
        assert!((m["meter.reduction"] - 6.02).abs() < 0.01);
        for id in m.keys() {
            assert!(id.starts_with("meter."));
        }
        // slots missing from a short reply are left out, not reported as silence
        let short = meters(&State::from_floats(vec![0.0; 4]));
        assert!(short.contains_key("meter.ch.playback") && !short.contains_key("meter.mic"));
        assert!(meters(&State::from_floats(vec![])).is_empty());
    }

    #[test]
    fn conversions_stay_in_device_range() {
        for e in table() {
            let d = def(e);
            let s = spec(e.gui).unwrap();
            for gui in [s.min, s.default, s.max] {
                let c = to_core(e, d, gui);
                assert!(c >= d.min && c <= d.max, "{} {gui} -> {c}", e.gui);
                assert!((to_gui(e, d, c) - gui.clamp(s.min, s.max)).abs() < 1e-3);
            }
        }
        let e = lookup("mic.hp_volume").unwrap();
        assert_eq!(to_core(e, def(e), 50.0), 0.5);
        let e = lookup("mic.monitor_blend").unwrap();
        assert_eq!(to_core(e, def(e), 0.0), -1.0);
    }
}

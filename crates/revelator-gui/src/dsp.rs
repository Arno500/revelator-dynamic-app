//! GUI side of the DSP engines (`revelator_core::fx`, ported from Universal Control).
//!
//! Every core DSP parameter `<key>` is the GUI parameter `dsp.<key>`, with the core's range and
//! default. Values are in core units, except times: seconds in core, milliseconds in the GUI.
//! The device cannot report these values, so the backend keeps them in the same state file as
//! `revelator-cli dsp` and sends whole engine blocks.

use crate::model::{Confidence, Spec};
use revelator_core::fx::{self, Engine, FxParam, FxState};

pub const PREFIX: &str = "dsp.";

pub const EQ_MODELS: [&str; 3] = ["Parametric", "Passive", "Vintage"];
pub const COMP_MODELS: [&str; 3] = ["Standard", "Tube", "FET"];
pub const VOICEFX_MODELS: [&str; 6] = [
    "Doubler",
    "Detune",
    "Vocoder",
    "Ring modulator",
    "Robot",
    "Echo",
];
/// `voicefx.model` value of the echo (Delay) effect: needs a known sample rate below 88.2 kHz.
pub const ECHO_MODEL: f32 = 5.0;
pub const EQ_BANDS: [(&str, &str); 4] = [
    ("low", "Low"),
    ("lowmid", "Low-mid"),
    ("himid", "High-mid"),
    ("high", "High"),
];

pub fn id(key: &str) -> String {
    format!("{PREFIX}{key}")
}

/// Core parameter behind a GUI id (`None` for non-DSP ids).
pub fn param(id: &str) -> Option<&'static FxParam> {
    fx::find(id.strip_prefix(PREFIX)?)
}

fn scale(p: &FxParam) -> f32 {
    if p.unit == "s" {
        1000.0
    } else {
        1.0
    }
}

pub fn to_core(p: &FxParam, gui: f32) -> f32 {
    (gui / scale(p)).clamp(p.min, p.max)
}

pub fn to_gui(p: &FxParam, core: f32) -> f32 {
    core * scale(p)
}

fn unit(p: &FxParam) -> &'static str {
    match p.unit {
        "s" => " ms",
        "dB" => " dB",
        "Hz" => " Hz",
        ":1" => " :1",
        _ => "",
    }
}

/// Short control label: the core label without its "Model: " prefix and "(all models)" note.
fn label(p: &FxParam) -> String {
    if p.key.ends_with(".model") {
        return "Model".into();
    }
    let l = p
        .label
        .replace(" (all models)", "")
        .replace("; all models", "");
    let l = match l.split_once(": ") {
        Some((head, rest)) if !head.contains(' ') || head.len() <= 10 => rest.to_string(),
        _ => l,
    };
    // Notes such as "(40 or less = off)" are explained in the tooltips; they would make the
    // label too wide for its column.
    let l = match l.find(" (") {
        Some(i) => l[..i].to_string(),
        None => l,
    };
    let mut c = l.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => l,
    }
}

pub fn specs() -> Vec<Spec> {
    fx::PARAMS
        .iter()
        .map(|p| {
            let k = scale(p);
            Spec {
                id: id(p.key),
                label: label(p),
                min: p.min * k,
                max: p.max * k,
                default: p.default * k,
                unit: unit(p),
                // Ported from the UC DLL and unit-tested, but not yet confirmed on hardware.
                confidence: Confidence::Inferred,
            }
        })
        .collect()
}

/// Core state from GUI values (`get` returns a GUI value by GUI id).
pub fn state_from(get: impl Fn(&str) -> f32) -> FxState {
    let mut st = FxState::default();
    for p in fx::PARAMS {
        let _ = st.set(p.key, to_core(p, get(&id(p.key))));
    }
    st
}

/// GUI `(id, value)` pairs for a core state.
pub fn gui_values(st: &FxState) -> impl Iterator<Item = (String, f32)> + '_ {
    st.iter().map(|(p, v)| (id(p.key), to_gui(p, v)))
}

/// Every engine, in the order a full resend uses.
pub const ENGINES: [Engine; 8] = [
    Engine::Order,
    Engine::Hpf,
    Engine::Gate,
    Engine::Comp,
    Engine::Eq,
    Engine::Limiter,
    Engine::Reverb,
    Engine::VoiceFx,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_core_param_has_a_gui_spec_in_range() {
        let s = specs();
        assert_eq!(s.len(), fx::PARAMS.len());
        for (sp, p) in s.iter().zip(fx::PARAMS) {
            assert_eq!(param(&sp.id).unwrap().key, p.key);
            assert!(sp.min <= sp.default && sp.default <= sp.max, "{}", sp.id);
            assert!(
                !sp.label.is_empty() && !sp.label.contains(": "),
                "{}",
                sp.label
            );
            let back = to_core(p, to_gui(p, p.default));
            assert!(
                (back - p.default).abs() <= p.default.abs() * 1e-5,
                "{}",
                p.key
            );
        }
        let a = param("dsp.gate.attack").unwrap();
        assert_eq!(to_gui(a, 0.005), 5.0);
        assert!(param("mic.gain").is_none());
    }

    #[test]
    fn state_roundtrip() {
        let mut st = FxState::default();
        st.set("comp.attack", 0.01).unwrap();
        st.set("voicefx.model", 4.0).unwrap();
        let vals: std::collections::HashMap<_, _> = gui_values(&st).collect();
        assert_eq!(vals["dsp.comp.attack"], 10.0);
        let back = state_from(|id| vals[id]);
        assert_eq!(back.get("voicefx.model"), 4.0);
        assert!((back.get("comp.attack") - 0.01).abs() < 1e-7);
    }
}

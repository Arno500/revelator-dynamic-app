//! Fat Channel profiles: ready-made starting points for common ways of using a desktop mic.
//!
//! Each profile is a *complete* Fat Channel state: it starts from a neutral baseline for every
//! Fat Channel parameter (high-pass, gate/expander, the three compressor models, the three EQ
//! models, EQ/comp order, limiter) and then applies the profile's own settings, so applying one
//! never leaves settings from the previous profile behind. Values use the GUI model's units
//! (dB, Hz, ms, ratio); keys are the DSP keys without the `dsp.` prefix. They are tuned for a
//! dynamic mic used close (~5-15 cm); after applying, adjust gain/threshold to taste.

use crate::model::{spec, Model, ParamId};
use revelator_core::fx::{self, Engine};
use std::sync::OnceLock;

pub struct Profile {
    pub name: &'static str,
    /// One emoji shown on the profile button.
    pub icon: &'static str,
    pub blurb: &'static str,
    /// `(dsp key, GUI value)` on top of the baseline.
    pub settings: &'static [(&'static str, f32)],
}

/// Settings every profile starts from: all sections off, flat EQ bands (band 1 a low shelf and
/// band 4 a high shelf, like the editor), standard compressor, parametric EQ, normal order.
const BASELINE: &[(&str, f32)] = &[
    ("comp.model", 0.0),
    ("eq.model", 0.0),
    ("eq.swap", 0.0),
    // 24 Hz is the core's "off" for the high-pass
    ("hpf.freq", 24.0),
    ("gate.on", 0.0),
    ("comp.on", 0.0),
    ("eq.on", 0.0),
    ("limiter.on", 0.0),
    ("eq.low.shelf", 1.0),
    ("eq.high.shelf", 1.0),
    ("eq.low.freq", 100.0),
    ("eq.lowmid.freq", 400.0),
    ("eq.himid.freq", 2500.0),
    ("eq.high.freq", 8000.0),
    ("eq.low.q", 0.9),
    ("eq.lowmid.q", 0.9),
    ("eq.himid.q", 0.9),
    ("eq.high.q", 0.9),
];

pub static PROFILES: &[Profile] = &[
    Profile {
        name: "Meetings",
        icon: "\u{1F465}",
        blurb: "Video calls (Teams, Zoom, Meet): clear, even speech that survives codecs. A gentle expander instead of a hard gate keeps words from being clipped; cuts desk thump and room rumble.",
        settings: &[
            ("hpf.freq", 100.0),
            ("gate.on", 1.0),
            ("gate.expander", 1.0),
            ("gate.threshold", -52.0),
            ("gate.range", -18.0),
            ("gate.attack", 3.0),
            ("gate.release", 250.0),
            ("comp.on", 1.0),
            ("comp.threshold", -22.0),
            ("comp.ratio", 3.0),
            ("comp.attack", 8.0),
            ("comp.release", 150.0),
            ("comp.gain", 4.0),
            ("comp.softknee", 1.0),
            ("eq.on", 1.0),
            ("eq.low.freq", 120.0),
            ("eq.low.gain", -2.0),
            ("eq.low.q", 0.7),
            ("eq.lowmid.freq", 300.0),
            ("eq.lowmid.gain", -2.0),
            ("eq.lowmid.q", 1.0),
            ("eq.himid.freq", 3000.0),
            ("eq.himid.gain", 2.5),
            ("eq.high.freq", 9000.0),
            ("eq.high.gain", 1.0),
            ("eq.high.q", 0.7),
            ("limiter.on", 1.0),
            ("limiter.threshold", -3.0),
        ],
    },
    Profile {
        name: "Gaming",
        icon: "\u{1F3AE}",
        blurb: "Voice chat over loud games: tight gate whose key filter ignores low thumps (keyboard, desk), firm compression so you cut through.",
        settings: &[
            ("hpf.freq", 120.0),
            ("gate.on", 1.0),
            ("gate.threshold", -44.0),
            ("gate.range", -40.0),
            ("gate.attack", 1.0),
            ("gate.release", 120.0),
            ("gate.keyfilter", 300.0),
            ("comp.on", 1.0),
            ("comp.threshold", -20.0),
            ("comp.ratio", 4.0),
            ("comp.attack", 5.0),
            ("comp.release", 100.0),
            ("comp.gain", 5.0),
            ("eq.on", 1.0),
            ("eq.low.freq", 150.0),
            ("eq.low.gain", -3.0),
            ("eq.low.q", 0.7),
            ("eq.lowmid.freq", 400.0),
            ("eq.lowmid.gain", -2.0),
            ("eq.lowmid.q", 1.0),
            ("eq.himid.freq", 3500.0),
            ("eq.himid.gain", 3.0),
            ("eq.himid.q", 1.0),
            ("eq.high.q", 0.7),
            ("limiter.on", 1.0),
            ("limiter.threshold", -3.0),
        ],
    },
    Profile {
        name: "Streaming",
        icon: "\u{1F4E1}",
        blurb: "Live streams and broadcast: punchy 'radio' voice. FET compressor for fast, heavy levelling, vintage EQ for body and presence, so you stay consistent while moving.",
        settings: &[
            ("hpf.freq", 80.0),
            ("gate.on", 1.0),
            ("gate.threshold", -52.0),
            ("gate.range", -30.0),
            ("gate.attack", 2.0),
            ("gate.release", 150.0),
            ("comp.model", 2.0),
            ("comp.on", 1.0),
            ("comp.fet.input", -28.0),
            ("comp.fet.output", -6.0),
            ("comp.fet.attack", 0.2),
            ("comp.fet.release", 150.0),
            ("comp.fet.ratio", 1.0),
            ("eq.model", 2.0),
            ("eq.on", 1.0),
            ("eq.vintage.lowfreq", 1.0),
            ("eq.vintage.lowgain", 3.0),
            ("eq.vintage.lowmidfreq", 0.0),
            ("eq.vintage.lowmidgain", -3.0),
            ("eq.vintage.himidfreq", 1.0),
            ("eq.vintage.himidgain", 3.0),
            ("eq.vintage.highgain", 3.0),
            ("limiter.on", 1.0),
            ("limiter.threshold", -1.5),
        ],
    },
    Profile {
        name: "Podcast",
        icon: "\u{1F3A7}",
        blurb: "Recording speech and voice-over: warm and full. Tube compressor for smooth, natural levelling, passive EQ for low-end body and a touch of air, gentle expander so breaths stay natural.",
        settings: &[
            ("hpf.freq", 80.0),
            ("gate.on", 1.0),
            ("gate.expander", 1.0),
            ("gate.threshold", -60.0),
            ("gate.range", -18.0),
            ("gate.attack", 3.0),
            ("gate.release", 250.0),
            ("comp.model", 1.0),
            ("comp.on", 1.0),
            ("comp.tube.mode", 0.0),
            ("comp.tube.gain", 45.0),
            ("comp.tube.peak", 40.0),
            ("eq.model", 1.0),
            ("eq.on", 1.0),
            ("eq.passive.lowfreq", 3.0),
            ("eq.passive.lowboost", 2.5),
            ("eq.passive.lowatten", 0.0),
            ("eq.passive.highfreq", 2.0),
            ("eq.passive.highboost", 3.0),
            ("eq.passive.bandwidth", 6.0),
            ("eq.passive.attenfreq", 2.0),
            ("eq.passive.highatten", 0.0),
            ("limiter.on", 1.0),
            ("limiter.threshold", -2.0),
        ],
    },
    Profile {
        name: "Noisy room",
        icon: "\u{1F50A}",
        blurb: "Untreated or noisy space (open office, fan, street): strong high-pass, deep gate keyed from the voice range, compressor that ignores rumble, reduced low-mid boom.",
        settings: &[
            ("hpf.freq", 150.0),
            ("gate.on", 1.0),
            ("gate.threshold", -38.0),
            ("gate.range", -45.0),
            ("gate.attack", 1.0),
            ("gate.release", 100.0),
            ("gate.keyfilter", 400.0),
            ("comp.on", 1.0),
            ("comp.threshold", -20.0),
            ("comp.ratio", 3.0),
            ("comp.attack", 6.0),
            ("comp.release", 120.0),
            ("comp.gain", 5.0),
            ("comp.keyfilter", 200.0),
            ("eq.on", 1.0),
            ("eq.low.freq", 180.0),
            ("eq.low.gain", -4.0),
            ("eq.low.q", 0.7),
            ("eq.lowmid.freq", 450.0),
            ("eq.lowmid.gain", -2.5),
            ("eq.lowmid.q", 1.0),
            ("eq.himid.freq", 3200.0),
            ("eq.himid.gain", 2.5),
            ("eq.himid.q", 1.0),
            ("eq.high.freq", 7500.0),
            ("eq.high.gain", -1.0),
            ("eq.high.q", 0.7),
            ("limiter.on", 1.0),
            ("limiter.threshold", -3.0),
        ],
    },
    Profile {
        name: "Singing / Music",
        icon: "\u{1F3A4}",
        blurb: "Vocals and instruments: no gate, light and slow compression with a soft knee and the EQ before the compressor, gentle air on top.",
        settings: &[
            ("hpf.freq", 60.0),
            ("eq.swap", 1.0),
            ("comp.on", 1.0),
            ("comp.threshold", -18.0),
            ("comp.ratio", 2.5),
            ("comp.attack", 20.0),
            ("comp.release", 200.0),
            ("comp.gain", 3.0),
            ("comp.softknee", 1.0),
            ("eq.on", 1.0),
            ("eq.low.q", 0.7),
            ("eq.lowmid.freq", 300.0),
            ("eq.lowmid.gain", -1.5),
            ("eq.lowmid.q", 1.0),
            ("eq.himid.freq", 3000.0),
            ("eq.himid.gain", 1.5),
            ("eq.high.freq", 12000.0),
            ("eq.high.gain", 2.5),
            ("eq.high.q", 0.7),
            ("limiter.on", 1.0),
            ("limiter.threshold", -1.0),
        ],
    },
    Profile {
        name: "Flat / Clean",
        icon: "\u{2728}",
        blurb: "Neutral: only a low rumble filter and a safety limiter. A good base for your own tweaks.",
        settings: &[
            ("hpf.freq", 60.0),
            ("limiter.on", 1.0),
            ("limiter.threshold", -1.0),
        ],
    },
];

/// Engines that make up the Fat Channel.
fn is_fat(e: Engine) -> bool {
    matches!(
        e,
        Engine::Hpf | Engine::Gate | Engine::Comp | Engine::Eq | Engine::Limiter | Engine::Order
    )
}

fn full_values(p: &Profile) -> Vec<(ParamId, f32)> {
    // Every Fat Channel parameter at its default, then the baseline, then the profile.
    let mut v: Vec<(ParamId, f32)> = fx::PARAMS
        .iter()
        .filter(|f| is_fat(f.engine))
        .filter_map(|f| {
            let id = crate::dsp::id(f.key);
            let d = spec(&id)?.default;
            Some((id, d))
        })
        .collect();
    for (k, x) in BASELINE.iter().chain(p.settings) {
        let id = crate::dsp::id(k);
        match v.iter_mut().find(|(i, _)| *i == id) {
            Some(slot) => slot.1 = *x,
            None => v.push((id, *x)),
        }
    }
    v
}

impl Profile {
    /// Every Fat Channel parameter this profile sets, in GUI ids and units.
    pub fn values(&self) -> &'static [(ParamId, f32)] {
        static CACHE: OnceLock<Vec<Vec<(ParamId, f32)>>> = OnceLock::new();
        let all = CACHE.get_or_init(|| PROFILES.iter().map(full_values).collect());
        match PROFILES.iter().position(|p| std::ptr::eq(p, self)) {
            Some(i) => &all[i],
            None => &[],
        }
    }

    /// True when the model's current Fat Channel state equals this profile (so the button can
    /// light up, and un-light as soon as the user tweaks anything).
    pub fn matches(&self, m: &Model) -> bool {
        self.values()
            .iter()
            .all(|(id, v)| (m.get(id) - v).abs() < 1e-3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_values_are_known_and_in_range() {
        for p in PROFILES {
            for (k, _) in p.settings {
                assert!(fx::find(k).is_some(), "{}: unknown key {k}", p.name);
            }
            for (id, v) in p.values() {
                let s = spec(id).unwrap_or_else(|| panic!("{}: unknown param {id}", p.name));
                assert!(
                    (s.min..=s.max).contains(v),
                    "{}: {id} = {v} outside {}..{}",
                    p.name,
                    s.min,
                    s.max
                );
            }
        }
    }

    #[test]
    fn names_unique_and_nonempty() {
        let names: HashSet<_> = PROFILES.iter().map(|p| p.name).collect();
        assert_eq!(names.len(), PROFILES.len());
        assert!(PROFILES
            .iter()
            .all(|p| !p.blurb.is_empty() && !p.icon.is_empty()));
    }

    #[test]
    fn every_profile_sets_every_fat_channel_param_so_nothing_lingers() {
        let fat = fx::PARAMS.iter().filter(|f| is_fat(f.engine)).count();
        for p in PROFILES {
            let ids: HashSet<_> = p.values().iter().map(|(i, _)| i.clone()).collect();
            assert_eq!(ids.len(), fat, "{}", p.name);
            assert_eq!(p.values().len(), fat, "{}", p.name);
        }
    }

    #[test]
    fn matches_after_apply_and_breaks_on_tweak() {
        let mut m = Model::default();
        let now = std::time::Instant::now();
        let p = &PROFILES[0];
        assert!(!p.matches(&m));
        for (id, v) in p.values() {
            m.set_local(id, *v, now).unwrap();
        }
        assert!(p.matches(&m));
        m.set_local("dsp.comp.ratio", 9.0, now).unwrap();
        assert!(!p.matches(&m));
    }
}

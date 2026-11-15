//! The preset document UC keeps in each of the mic's four preset slots (UBJSON, see
//! [`crate::ubjson`]), mapped to and from the app's DSP settings ([`crate::fx::FxState`]).
//!
//! Layout seen in the four factory slots read from a Revelator Dynamic: `preset_name`,
//! `icon_id`, `opt{swapcompeq}`, `filter{hpf}`, `gate{...}`, `limit{...}`, `voicefxopt`,
//! `eq{__classid, eqallon, eqgain1..4, eqq1..4, eqfreq1..4, eqbandon1..4, eqbandop1,4}`,
//! `comp{__classid, on, softknee, automode, threshold, ratio, attack, release, gain, keyfilter,
//! keylisten}`, `voicefx{on, __classid, feedback, mix, time}`. Values are in the units of the UI
//! (dB, Hz, seconds).
//!
//! Only what is understood is mapped. Everything else in the document (names, icon, class ids,
//! the Voice FX group) is kept exactly as it was in the slot, so storing never invents data:
//! [`to_doc`] needs the slot's current document as its base. The EQ (parametric, passive,
//! vintage) and compressor (standard, tube, FET) groups are written for the selected model, and
//! [`apply`] reads them back by class id, together with the Voice FX group (doubler, detune,
//! robot, echo).
//!
//! The mic ships with eight factory presets of this kind ([`FACTORY`], taken from the firmware
//! image inside UC's DLL); four of them sit in its slots 0..=3 (checked byte for byte).

use crate::fx::{Engine, FxState, EQ_BANDS};
use crate::ubjson::Value;

/// The eight factory presets of the Revelator Dynamic's firmware (UBJSON documents).
pub const FACTORY: [(&str, &[u8]); 8] = [
    ("Broadcast Basic", include_bytes!("factory/0.bin")),
    ("Broadcast Light", include_bytes!("factory/1.bin")),
    ("Broadcast Radio", include_bytes!("factory/2.bin")),
    ("Broadcast Clear", include_bytes!("factory/3.bin")),
    ("Reverb", include_bytes!("factory/4.bin")),
    ("Delay", include_bytes!("factory/5.bin")),
    ("Witness Protection", include_bytes!("factory/6.bin")),
    ("Computron 4000", include_bytes!("factory/7.bin")),
];

/// Longest preset name UC keeps for a slot (the app's own limit, see the GUI).
pub const MAX_NAME: usize = 16;

/// `(group, document key, DSP parameter key, integer?)` for every mapped value.
fn table() -> Vec<(&'static str, String, String, bool)> {
    let mut t: Vec<(&'static str, String, String, bool)> = Vec::new();
    let mut add = |g: &'static str, d: &str, k: &str, int: bool| {
        t.push((g, d.to_string(), k.to_string(), int));
    };
    add("opt", "swapcompeq", "eq.swap", true);
    add("filter", "hpf", "hpf.freq", false);
    for (d, k, int) in [
        ("on", "on", true),
        ("keylisten", "keylisten", true),
        ("expander", "expander", true),
        ("keyfilter", "keyfilter", false),
        ("threshold", "threshold", false),
        ("range", "range", false),
        ("attack", "attack", false),
        ("release", "release", false),
    ] {
        add("gate", d, &format!("gate.{k}"), int);
    }
    add("limit", "limiteron", "limiter.on", true);
    add("limit", "threshold", "limiter.threshold", false);
    add("eq", "eqallon", "eq.on", true);
    for (i, band) in EQ_BANDS.iter().enumerate() {
        let n = i + 1;
        add(
            "eq",
            &format!("eqgain{n}"),
            &format!("eq.{band}.gain"),
            false,
        );
        add("eq", &format!("eqq{n}"), &format!("eq.{band}.q"), false);
        add(
            "eq",
            &format!("eqfreq{n}"),
            &format!("eq.{band}.freq"),
            false,
        );
        add(
            "eq",
            &format!("eqbandon{n}"),
            &format!("eq.{band}.on"),
            true,
        );
    }
    // band shape: 1 = shelf (the factory presets use it on the outer bands of Broadcast)
    add("eq", "eqbandop1", "eq.low.shelf", true);
    add("eq", "eqbandop4", "eq.high.shelf", true);
    for (d, k, int) in [
        ("on", "on", true),
        ("softknee", "softknee", true),
        ("automode", "auto", true),
        ("threshold", "threshold", false),
        ("ratio", "ratio", false),
        ("attack", "attack", false),
        ("release", "release", false),
        ("gain", "gain", false),
        ("keyfilter", "keyfilter", false),
        ("keylisten", "keylisten", true),
    ] {
        add("comp", d, &format!("comp.{k}"), int);
    }
    t
}

/// The document `fx` describes, built on `base` (the slot's current document). `name` replaces
/// `preset_name` when not empty. The EQ and compressor groups are rebuilt for the selected
/// models (their fields differ per model).
pub fn to_doc(fx: &FxState, name: &str, base: &Value) -> Result<Value, String> {
    if !matches!(base, Value::Object(_)) {
        return Err("the slot does not hold a preset document".into());
    }
    let eq_model = fx.get("eq.model").round() as usize;
    let comp_model = fx.get("comp.model").round() as usize;
    let mut doc = base.clone();
    ensure_class(&mut doc, "eq", EQ_CLASSES[eq_model.min(2)]);
    ensure_class(&mut doc, "comp", COMP_CLASSES[comp_model.min(2)]);
    let val = |v: f32, int: bool| {
        if int {
            Value::Int(v.round() as i64)
        } else {
            Value::Float(v as f64)
        }
    };
    for (group, dkey, fkey, int) in table() {
        // fields of the parametric EQ / standard compressor only make sense for those models
        let keep = match group {
            "eq" => eq_model == 0 || dkey == "eqallon",
            "comp" => comp_model == 0 || matches!(dkey.as_str(), "on" | "keyfilter" | "keylisten"),
            _ => true,
        };
        if !keep {
            continue;
        }
        let v = val(fx.get(&fkey), int);
        match doc.get_mut(group) {
            Some(g) => g.put(&dkey, v),
            None => {
                let mut g = Value::Object(Vec::new());
                g.put(&dkey, v);
                doc.put(group, g);
            }
        }
    }
    for (group, fields) in [
        ("eq", EQ_MODEL_FIELDS[eq_model.min(2)]),
        ("comp", COMP_MODEL_FIELDS[comp_model.min(2)]),
    ] {
        for (dkey, fkey, int) in fields {
            let v = val(fx.get(fkey), *int);
            if let Some(g) = doc.get_mut(group) {
                g.put(dkey, v);
            }
        }
    }
    let name: String = name.trim().chars().take(MAX_NAME).collect();
    if !name.is_empty() {
        doc.put("preset_name", Value::Str(name));
    }
    Ok(doc)
}

/// Make `group` an object of class `cls` (replacing it when it is another model's).
fn ensure_class(doc: &mut Value, group: &str, cls: &str) {
    if class_of(doc, group) != Some(cls) {
        doc.put(
            group,
            Value::Object(vec![("__classid".into(), Value::Str(cls.into()))]),
        );
    }
}

const CLS_EQ_PARAMETRIC: &str = "{A0A8A068-14F0-4B04-BB6F-AF8329D0E8EE}";
const CLS_EQ_PASSIVE: &str = "{C0730CBB-5135-4558-9222-C40BDBA036ED}";
const CLS_EQ_VINTAGE: &str = "{E1C5E024-C5CD-473C-B08A-6EC177812E01}";
const CLS_COMP_TUBE: &str = "{7F8A4262-D377-48E3-9D48-15D82C400A71}";
/// Class id for each `eq.model` / `comp.model` value.
const EQ_CLASSES: [&str; 3] = [CLS_EQ_PARAMETRIC, CLS_EQ_PASSIVE, CLS_EQ_VINTAGE];
const COMP_CLASSES: [&str; 3] = [CLS_COMP_STANDARD, CLS_COMP_TUBE, CLS_COMP_FET];

type Field = (&'static str, &'static str, bool);
/// Extra `(document key, DSP key, integer?)` fields per EQ model (parametric: the main table).
const EQ_MODEL_FIELDS: [&[Field]; 3] = [
    &[],
    &[
        ("bboost", "eq.passive.lowboost", false),
        ("batten", "eq.passive.lowatten", false),
        ("bfreq", "eq.passive.lowfreq", true),
        ("mboost", "eq.passive.highboost", false),
        ("bbwidth", "eq.passive.bandwidth", false),
        ("mfreq", "eq.passive.highfreq", true),
        ("hatten", "eq.passive.highatten", false),
        ("hsfreq", "eq.passive.attenfreq", true),
    ],
    &[
        ("lowgain", "eq.vintage.lowgain", false),
        ("lowfreq", "eq.vintage.lowfreq", true),
        ("lowmidgain", "eq.vintage.lowmidgain", false),
        ("lowmidfreq", "eq.vintage.lowmidfreq", true),
        ("himidgain", "eq.vintage.himidgain", false),
        ("himidfreq", "eq.vintage.himidfreq", true),
        ("higain", "eq.vintage.highgain", false),
    ],
];
/// Extra fields per compressor model (standard: the main table).
const COMP_MODEL_FIELDS: [&[Field]; 3] = [
    &[],
    &[
        ("mode", "comp.tube.mode", true),
        ("gain", "comp.tube.gain", false),
        ("peak", "comp.tube.peak", false),
    ],
    &[
        ("input", "comp.fet.input", false),
        ("output", "comp.fet.output", false),
        ("attack", "comp.fet.attack", false),
        ("release", "comp.fet.release", false),
        ("ratio", "comp.fet.ratio", true),
    ],
];

/// Class ids that tell the document's effect and compressor models apart.
const CLS_COMP_STANDARD: &str = "{870D04F7-212E-4F9C-ADBB-39A97216433F}";
const CLS_COMP_FET: &str = "{1F831EC1-B8AC-4EE9-AD53-54227AF53D58}";
const CLS_FX_DOUBLER: &str = "{66A10093-D461-4CAC-A80C-91F6A1BB37E5}";
const CLS_FX_ECHO: &str = "{98A527BA-2D6E-4B35-BB26-251EC081A067}";
const CLS_FX_DETUNE: &str = "{509018B0-0DE1-4D26-9DDF-D782476D3C98}";
const CLS_FX_ROBOT: &str = "{491FED97-6761-4FF6-91E1-E155891291AA}";

fn class_of<'a>(doc: &'a Value, group: &str) -> Option<&'a str> {
    match doc.get(group)?.get("__classid") {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

/// Apply the understood values of `doc` to `fx`, recording the keys it set. Missing or
/// out-of-range values are skipped.
fn apply_inner(doc: &Value, fx: &mut FxState, touched: &mut Vec<String>) {
    let mut put = |fx: &mut FxState, key: &str, v: f64| {
        if fx.set(key, v as f32).is_ok() {
            touched.push(key.to_string());
        }
    };
    for (group, dkey, fkey, _) in table() {
        if let Some(v) = doc
            .get(group)
            .and_then(|g| g.get(&dkey))
            .and_then(Value::as_f64)
        {
            put(fx, &fkey, v);
        }
    }
    let num = |g: &str, k: &str| doc.get(g).and_then(|g| g.get(k)).and_then(Value::as_f64);
    if let Some(m) = EQ_CLASSES
        .iter()
        .position(|c| class_of(doc, "eq") == Some(*c))
    {
        put(fx, "eq.model", m as f64);
        for (d, k, _) in EQ_MODEL_FIELDS[m] {
            if let Some(v) = num("eq", d) {
                put(fx, k, v);
            }
        }
    }
    if let Some(m) = COMP_CLASSES
        .iter()
        .position(|c| class_of(doc, "comp") == Some(*c))
    {
        put(fx, "comp.model", m as f64);
        for (d, k, _) in COMP_MODEL_FIELDS[m] {
            if let Some(v) = num("comp", d) {
                put(fx, k, v);
            }
        }
    }
    let (model, fields): (Option<f64>, &[(&str, &str)]) = match class_of(doc, "voicefx") {
        Some(CLS_FX_DOUBLER) => (
            Some(0.0),
            &[
                ("lows", "voicefx.doubler.lows"),
                ("width", "voicefx.doubler.width"),
            ],
        ),
        Some(CLS_FX_DETUNE) => (Some(1.0), &[("detune", "voicefx.detune.amount")]),
        Some(CLS_FX_ROBOT) => (
            Some(4.0),
            &[
                ("ctune", "voicefx.robot.pitch"),
                ("cfb", "voicefx.robot.regen"),
                ("cdamp", "voicefx.robot.damping"),
                ("cdist", "voicefx.robot.dist"),
                ("cvol", "voicefx.robot.volume"),
            ],
        ),
        Some(CLS_FX_ECHO) => (
            Some(5.0),
            &[
                ("time", "voicefx.echo.time"),
                ("feedback", "voicefx.echo.feedback"),
            ],
        ),
        _ => (None, &[]),
    };
    if let Some(m) = model {
        put(fx, "voicefx.model", m);
        for (d, k) in fields {
            if let Some(v) = num("voicefx", d) {
                put(fx, k, v);
            }
        }
        if let Some(v) = num("voicefx", "on") {
            put(fx, "voicefx.on", v);
        }
        if let Some(v) = num("voicefx", "mix").or_else(|| num("voicefx", "cmix")) {
            put(fx, "voicefx.mix", v);
        }
    }
}

/// Apply the understood values of `doc` to `fx`. Returns the engines that changed (to resend)
/// and how many values were applied.
pub fn apply(doc: &Value, fx: &mut FxState) -> (Vec<Engine>, usize) {
    let before = fx.clone();
    let mut touched = Vec::new();
    apply_inner(doc, fx, &mut touched);
    let mut engines = Vec::new();
    for k in &touched {
        if before.get(k) != fx.get(k) {
            if let Some(p) = crate::fx::find(k) {
                if !engines.contains(&p.engine) {
                    engines.push(p.engine);
                }
            }
        }
    }
    (engines, touched.len())
}

/// The `(DSP parameter key, value)` pairs a document sets (for loading it through the GUI).
pub fn values(doc: &Value) -> Vec<(String, f32)> {
    let mut fx = FxState::default();
    let mut touched = Vec::new();
    apply_inner(doc, &mut fx, &mut touched);
    let mut out: Vec<(String, f32)> = Vec::new();
    for k in touched {
        let v = fx.get(&k);
        match out.iter_mut().find(|(n, _)| *n == k) {
            Some(e) => e.1 = v,
            None => out.push((k, v)),
        }
    }
    out
}

/// Name stored in a document, if any.
pub fn name_of(doc: &Value) -> Option<&str> {
    match doc.get("preset_name") {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ubjson;

    fn base() -> Value {
        Value::Object(vec![
            ("preset_name".into(), Value::Str("Broadcast Basic".into())),
            ("icon_id".into(), Value::Str("broadcastbasic".into())),
            (
                "gate".into(),
                Value::Object(vec![("on".into(), Value::Int(1))]),
            ),
            (
                "eq".into(),
                Value::Object(vec![
                    ("__classid".into(), Value::Str("{X}".into())),
                    ("eqbandop1".into(), Value::Int(1)),
                ]),
            ),
            (
                "voicefx".into(),
                Value::Object(vec![("mix".into(), Value::Float(0.5))]),
            ),
        ])
    }

    #[test]
    fn stores_values_and_keeps_the_rest() {
        let mut fx = FxState::default();
        fx.set("gate.threshold", -40.0).unwrap();
        fx.set("eq.himid.gain", 3.5).unwrap();
        fx.set("comp.on", 1.0).unwrap();
        let doc = to_doc(&fx, "My setup", &base()).unwrap();
        assert_eq!(name_of(&doc), Some("My setup"));
        // kept as it was
        assert_eq!(
            doc.get("icon_id"),
            Some(&Value::Str("broadcastbasic".into()))
        );
        // the band shape now follows the DSP setting (default: peak = 0)
        assert_eq!(
            doc.get("eq").unwrap().get("eqbandop1"),
            Some(&Value::Int(0))
        );
        assert_eq!(
            doc.get("eq").unwrap().get("__classid"),
            Some(&Value::Str(CLS_EQ_PARAMETRIC.into()))
        );
        assert_eq!(
            doc.get("voicefx").unwrap().get("mix"),
            Some(&Value::Float(0.5))
        );
        assert_eq!(
            doc.get("gate").unwrap().get("threshold"),
            Some(&Value::Float(-40.0))
        );
        assert_eq!(doc.get("comp").unwrap().get("on"), Some(&Value::Int(1)));
        // survives the binary round trip and applies back
        let bytes = ubjson::encode(&doc).unwrap();
        let back = ubjson::decode(&bytes).unwrap();
        let mut other = FxState::default();
        let (engines, n) = apply(&back, &mut other);
        assert!(n > 20);
        assert!(engines.contains(&Engine::Gate) && engines.contains(&Engine::Comp));
        assert_eq!(other.get("gate.threshold"), -40.0);
        assert_eq!(other.get("eq.himid.gain"), 3.5);
        assert_eq!(other.get("comp.on"), 1.0);
    }

    #[test]
    fn factory_presets_decode_and_apply() {
        for (name, bytes) in FACTORY {
            let doc = ubjson::decode(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(name_of(&doc), Some(name));
            let vals = values(&doc);
            assert!(vals.len() > 25, "{name}: {}", vals.len());
            for (k, v) in &vals {
                let p = crate::fx::find(k).unwrap();
                assert!(*v >= p.min && *v <= p.max, "{name}: {k}={v}");
            }
        }
        let get = |name: &str, key: &str| {
            let (_, b) = FACTORY.iter().find(|(n, _)| *n == name).unwrap();
            values(&ubjson::decode(b).unwrap())
                .into_iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
        };
        assert_eq!(get("Delay", "voicefx.model"), Some(5.0));
        assert_eq!(get("Delay", "voicefx.on"), Some(1.0));
        assert_eq!(get("Reverb", "voicefx.model"), Some(0.0));
        assert_eq!(get("Witness Protection", "voicefx.model"), Some(1.0));
        assert_eq!(get("Witness Protection", "comp.model"), Some(2.0));
        assert_eq!(get("Computron 4000", "voicefx.model"), Some(4.0));
        assert_eq!(get("Broadcast Basic", "eq.high.shelf"), Some(1.0));
    }

    #[test]
    fn other_models_round_trip() {
        let mut fx = FxState::default();
        fx.set("eq.model", 2.0).unwrap();
        fx.set("eq.vintage.lowgain", 4.0).unwrap();
        fx.set("comp.model", 1.0).unwrap();
        fx.set("comp.tube.gain", 70.0).unwrap();
        let doc = to_doc(&fx, "", &base()).unwrap();
        assert_eq!(class_of(&doc, "eq"), Some(CLS_EQ_VINTAGE));
        assert_eq!(class_of(&doc, "comp"), Some(CLS_COMP_TUBE));
        assert_eq!(
            doc.get("eq").unwrap().get("lowgain"),
            Some(&Value::Float(4.0))
        );
        assert!(doc.get("eq").unwrap().get("eqgain1").is_none());
        let back = ubjson::decode(&ubjson::encode(&doc).unwrap()).unwrap();
        let mut other = FxState::default();
        apply(&back, &mut other);
        assert_eq!(other.get("eq.model"), 2.0);
        assert_eq!(other.get("eq.vintage.lowgain"), 4.0);
        assert_eq!(other.get("comp.model"), 1.0);
        assert_eq!(other.get("comp.tube.gain"), 70.0);
    }

    #[test]
    fn refuses_what_the_document_cannot_describe() {
        assert!(to_doc(&FxState::default(), "", &Value::Null).is_err());
        // an empty name keeps the old one
        let d = to_doc(&FxState::default(), "  ", &base()).unwrap();
        assert_eq!(name_of(&d), Some("Broadcast Basic"));
    }
}

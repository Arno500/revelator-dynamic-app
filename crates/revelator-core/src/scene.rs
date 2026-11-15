//! Reading and writing Universal Control `.scene` and `.channel` files.
//!
//! Both are plain JSON with an unusual layout (tab indentation and `"key": ` followed by a
//! newline before the opening brace). [`to_uc_string`] reproduces that layout; parsing is
//! ordinary JSON. Unknown keys are kept in `extra` maps so files round-trip.

use crate::dsp::{BandKind, EqBand};
use crate::params::{self, ParamDef};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SceneError {
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("expected a JSON object at the top level")]
    NotObject,
}

/// Serialize JSON in UC's layout: tabs, and `"key": \n{` for nested objects.
pub fn to_uc_string(v: &Value) -> String {
    let mut s = String::new();
    write_value(v, 0, &mut s);
    s.push('\n');
    s
}

fn write_value(v: &Value, depth: usize, out: &mut String) {
    match v {
        Value::Object(m) => {
            let ind = "\t".repeat(depth);
            out.push_str("{\n");
            let n = m.len();
            for (i, (k, val)) in m.iter().enumerate() {
                out.push_str(&ind);
                out.push('\t');
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push_str(": ");
                if val.is_object() {
                    out.push('\n');
                    out.push_str(&ind);
                    out.push('\t');
                }
                write_value(val, depth + 1, out);
                if i + 1 < n {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&ind);
            out.push('}');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

fn parse_object(text: &str) -> Result<Map<String, Value>, SceneError> {
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))?;
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(SceneError::NotObject),
    }
}

/// One mixer strip / bus entry of a scene.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SceneChannel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linkmaster: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solo: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lr: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assign_aux1: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assign_aux2: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aux1: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aux2: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preampgain: Option<f64>,
    /// Everything else (`pan`, `FXA`, `mono`, ...), preserved verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `.scene` file: sections (`line`, `return`, `fxreturn`, `aux`, `main`) of named channels.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    /// `(section, [(channel id, channel)])` in file order.
    pub sections: Vec<(String, Vec<(String, SceneChannel)>)>,
    /// Non-object top-level entries, preserved.
    pub extra: Map<String, Value>,
}

impl Scene {
    pub fn parse(text: &str) -> Result<Self, SceneError> {
        let top = parse_object(text)?;
        let mut sc = Scene::default();
        for (name, v) in top {
            match v {
                Value::Object(chs) if chs.values().all(Value::is_object) => {
                    let mut list = Vec::new();
                    for (id, cv) in chs {
                        list.push((id, serde_json::from_value::<SceneChannel>(cv)?));
                    }
                    sc.sections.push((name, list));
                }
                other => {
                    sc.extra.insert(name, other);
                }
            }
        }
        Ok(sc)
    }

    pub fn to_value(&self) -> Value {
        let mut top = Map::new();
        for (name, list) in &self.sections {
            let mut m = Map::new();
            for (id, ch) in list {
                m.insert(id.clone(), serde_json::to_value(ch).unwrap_or(Value::Null));
            }
            top.insert(name.clone(), Value::Object(m));
        }
        for (k, v) in &self.extra {
            top.insert(k.clone(), v.clone());
        }
        Value::Object(top)
    }

    /// Serialize in UC's file layout.
    pub fn to_uc_string(&self) -> String {
        to_uc_string(&self.to_value())
    }

    pub fn channel(&self, section: &str, id: &str) -> Option<&SceneChannel> {
        self.sections
            .iter()
            .find(|(s, _)| s == section)?
            .1
            .iter()
            .find(|(i, _)| i == id)
            .map(|(_, c)| c)
    }

    /// Translate the scene to `(parameter, value)` pairs for the mixer part of the table.
    /// Entries with no matching table key are skipped.
    pub fn param_values(&self) -> Vec<(&'static ParamDef, f32)> {
        let mut out = Vec::new();
        let mut push = |key: String, v: Option<f64>| {
            if let (Some(v), Some(p)) = (v, params::find(&key)) {
                out.push((p, p.clamp(v as f32)));
            }
        };
        for (section, list) in &self.sections {
            for (id, ch) in list {
                let prefix = match (section.as_str(), id.as_str()) {
                    ("line", "ch1") => "mic",
                    ("return", "ch1") => "playback",
                    ("return", "ch2") => "virtuala",
                    ("return", "ch3") => "virtualb",
                    ("aux", "ch1") => "mixa",
                    ("aux", "ch2") => "mixb",
                    ("main", "ch1") => "main",
                    _ => continue,
                };
                push(format!("{prefix}.volume"), ch.volume);
                push(format!("{prefix}.mute"), ch.mute.map(f64::from));
                if section == "line" || section == "return" {
                    push(format!("{prefix}.aux1"), ch.aux1);
                    push(format!("{prefix}.aux2"), ch.aux2);
                    push(format!("{prefix}.solo"), ch.solo.map(f64::from));
                }
                if section == "line" {
                    push("preampgain".to_string(), ch.preampgain);
                }
            }
        }
        out
    }
}

/// A `.channel` preset: Fat Channel settings (`filter`, `gate`, `comp`, `limit`, `eq`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChannelPreset {
    /// `(section, entries)` in file order; values are JSON numbers.
    pub sections: Vec<(String, Map<String, Value>)>,
    pub extra: Map<String, Value>,
}

impl ChannelPreset {
    pub fn parse(text: &str) -> Result<Self, SceneError> {
        let mut p = ChannelPreset::default();
        for (name, v) in parse_object(text)? {
            match v {
                Value::Object(m) => p.sections.push((name, m)),
                other => {
                    p.extra.insert(name, other);
                }
            }
        }
        Ok(p)
    }

    pub fn to_value(&self) -> Value {
        let mut top = Map::new();
        for (n, m) in &self.sections {
            top.insert(n.clone(), Value::Object(m.clone()));
        }
        for (k, v) in &self.extra {
            top.insert(k.clone(), v.clone());
        }
        Value::Object(top)
    }

    pub fn to_uc_string(&self) -> String {
        to_uc_string(&self.to_value())
    }

    /// Numeric value `section.key`.
    pub fn get(&self, section: &str, key: &str) -> Option<f64> {
        self.sections
            .iter()
            .find(|(s, _)| s == section)?
            .1
            .get(key)?
            .as_f64()
    }

    /// High-pass cutoff in Hz.
    pub fn hpf(&self) -> Option<f64> {
        self.get("filter", "hpf")
    }

    /// The four EQ bands as DSP bands (band 1 and 4 are shelves when their `eqbandop` is 1).
    pub fn eq_bands(&self) -> Vec<EqBand> {
        (1..=4)
            .filter_map(|n| {
                let g = |k: &str| self.get("eq", &format!("{k}{n}"));
                let shelf = g("eqbandop").unwrap_or(0.0) >= 0.5;
                let kind = match (n, shelf) {
                    (1, true) => BandKind::LowShelf,
                    (4, true) => BandKind::HighShelf,
                    _ => BandKind::Peak,
                };
                Some(EqBand {
                    kind,
                    freq: g("eqfreq")?,
                    gain_db: g("eqgain")?,
                    q: g("eqq")?,
                    enabled: g("eqbandon").unwrap_or(1.0) >= 0.5
                        && self.get("eq", "eqallon").unwrap_or(1.0) >= 0.5,
                })
            })
            .collect()
    }

    /// Translate to `(parameter, value)` pairs (`section.key` must exist in the table).
    pub fn param_values(&self) -> Vec<(&'static ParamDef, f32)> {
        let mut out = Vec::new();
        for (s, m) in &self.sections {
            for (k, v) in m {
                if let (Some(x), Some(p)) = (v.as_f64(), params::find(&format!("{s}.{k}"))) {
                    out.push((p, p.clamp(x as f32)));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENE: &str = "{\n\t\"line\": \n\t{\n\t\t\"ch1\": \n\t\t{\n\t\t\t\"username\": \"Microphone\",\n\t\t\t\"mute\": 0,\n\t\t\t\"volume\": 0.0,\n\t\t\t\"solo\": 0,\n\t\t\t\"aux1\": -6.5,\n\t\t\t\"preampgain\": 22.0,\n\t\t\t\"pan\": 0.5\n\t\t}\n\t},\n\t\"aux\": \n\t{\n\t\t\"ch1\": \n\t\t{\n\t\t\t\"username\": \"Mix A\",\n\t\t\t\"volume\": -3.0,\n\t\t\t\"preset_name\": \"Default\"\n\t\t}\n\t}\n}\n";

    #[test]
    fn scene_parses_and_roundtrips_exactly() {
        let s = Scene::parse(SCENE).unwrap();
        assert_eq!(s.channel("line", "ch1").unwrap().preampgain, Some(22.0));
        assert_eq!(s.channel("line", "ch1").unwrap().extra["pan"], 0.5);
        assert_eq!(s.to_uc_string(), SCENE);
        assert_eq!(Scene::parse(&s.to_uc_string()).unwrap(), s);
    }

    #[test]
    fn scene_to_params() {
        let s = Scene::parse(SCENE).unwrap();
        let v: Vec<_> = s
            .param_values()
            .into_iter()
            .map(|(p, v)| (p.key, v))
            .collect();
        assert!(v.contains(&("preampgain", 22.0)));
        assert!(v.contains(&("mic.aux1", -6.5)));
        assert!(v.contains(&("mixa.volume", -3.0)));
    }

    const CH: &str = "{\n\t\"filter\": \n\t{\n\t\t\"hpf\": 24\n\t},\n\t\"gate\": \n\t{\n\t\t\"on\": 1,\n\t\t\"threshold\": -29.5\n\t},\n\t\"eq\": \n\t{\n\t\t\"eqallon\": 1,\n\t\t\"eqgain1\": 4.0,\n\t\t\"eqq1\": 0.6,\n\t\t\"eqfreq1\": 69.1,\n\t\t\"eqbandon1\": 1,\n\t\t\"eqbandop1\": 1,\n\t\t\"eqgain2\": -5.0,\n\t\t\"eqq2\": 4,\n\t\t\"eqfreq2\": 241.0,\n\t\t\"eqbandon2\": 1\n\t}\n}\n";

    #[test]
    fn channel_preset() {
        let c = ChannelPreset::parse(CH).unwrap();
        assert_eq!(c.hpf(), Some(24.0));
        let bands = c.eq_bands();
        assert_eq!(bands.len(), 2); // bands 3/4 missing
        assert_eq!(bands[0].kind, BandKind::LowShelf);
        assert_eq!(bands[1].kind, BandKind::Peak);
        let keys: Vec<_> = c.param_values().iter().map(|(p, _)| p.key).collect();
        assert!(keys.contains(&"gate.threshold") && keys.contains(&"filter.hpf"));
        assert_eq!(c.to_uc_string(), CH);
    }

    #[test]
    fn rejects_non_object() {
        assert!(matches!(Scene::parse("[1]"), Err(SceneError::NotObject)));
        assert!(Scene::parse("{").is_err());
    }
}

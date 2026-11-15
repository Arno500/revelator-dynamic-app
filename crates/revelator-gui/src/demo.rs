//! Fake device with moving meters so the app runs with no hardware.

use crate::model::*;
use std::collections::HashMap;
use std::time::Instant;

pub struct DemoBackend {
    values: HashMap<ParamId, f32>,
    start: Instant,
    connected: bool,
    /// Values/connection changed since the last `snapshot` (meters are polled separately).
    dirty: bool,
}

const CH_KEYS: [&str; 5] = [
    "meter.ch.mic",
    "meter.ch.playback",
    "meter.ch.va",
    "meter.ch.vb",
    "meter.ch.rev",
];
const OUT_KEYS: [&str; 3] = ["meter.out.main", "meter.out.mixa", "meter.out.mixb"];

/// Update a meter in place; only allocates the first time a key is seen.
fn put(m: &mut HashMap<String, f32>, key: &str, v: f32) {
    match m.get_mut(key) {
        Some(x) => *x = v,
        None => {
            m.insert(key.to_string(), v);
        }
    }
}

impl DemoBackend {
    pub fn new() -> Self {
        DemoBackend {
            values: default_values(),
            start: Instant::now(),
            connected: false,
            dirty: true,
        }
    }

    #[cfg(test)]
    pub fn meters_at(&self, t: f32) -> HashMap<String, f32> {
        let mut m = HashMap::new();
        self.fill_meters(t, &mut m);
        m
    }

    fn fill_meters(&self, t: f32, m: &mut HashMap<String, f32>) {
        let gain = self.values["mic.gain"];
        let muted = self.values["mic.mute"] >= 0.5;
        // Speech-like envelope.
        let env = ((t * 2.3).sin() * 0.5 + 0.5) * ((t * 0.7).sin() * 0.3 + 0.7);
        let mic = if muted {
            DB_FLOOR
        } else {
            -50.0 + env * 40.0 + (gain - 22.0) * 0.3
        };
        let mic = mic.clamp(DB_FLOOR, 0.0);
        put(m, "meter.mic", mic);
        let thresh = self.values["dsp.comp.threshold"];
        let red = if self.values["dsp.comp.on"] >= 0.5 {
            (mic - thresh).max(0.0) * 0.5
        } else {
            0.0
        };
        put(m, "meter.reduction", red);
        for (i, key) in CH_KEYS.iter().enumerate() {
            let base = if i == 0 {
                mic
            } else {
                -30.0 + ((t * (0.9 + i as f32 * 0.4)).sin()) * 12.0
            };
            put(m, key, base.min(0.0));
        }
        for (i, (d, _)) in DESTS.iter().enumerate() {
            let vol = self.values[&out_id(d, "vol")];
            let muted = self.values[&out_id(d, "mute")] >= 0.5;
            let lvl = if muted {
                DB_FLOOR
            } else {
                (mic.max(-40.0) + vol).min(0.0)
            };
            put(m, OUT_KEYS[i], lvl);
        }
    }
}

impl Default for DemoBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for DemoBackend {
    fn name(&self) -> &str {
        "Demo"
    }
    fn connect(&mut self) {
        self.connected = true;
        self.dirty = true;
    }
    fn animated(&self) -> bool {
        true
    }
    fn poll_meters(&mut self, out: &mut HashMap<String, f32>) -> bool {
        self.fill_meters(self.start.elapsed().as_secs_f32(), out);
        true
    }
    fn snapshot(&mut self) -> Option<UiState> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        Some(UiState {
            connection: if self.connected {
                Connection::Connected
            } else {
                Connection::Connecting
            },
            connection_detail: String::new(),
            info: DeviceInfo {
                name: "Revelator Dynamic (demo)".into(),
                firmware: "demo".into(),
                protocol: "demo".into(),
            },
            values: self.values.clone(),
            meters: HashMap::new(),
            last_error: None,
            notice: None,
            mic_presets: None,
        })
    }
    fn set(&mut self, id: &str, v: f32) -> Result<(), String> {
        let s = spec(id).ok_or_else(|| format!("unknown parameter {id}"))?;
        self.values.insert(id.to_string(), v.clamp(s.min, s.max));
        self.dirty = true;
        Ok(())
    }
    fn recall_slot(&mut self, slot: usize) -> Result<(), String> {
        if slot >= SLOTS {
            return Err("bad slot".into());
        }
        self.values.insert("mic.slot".into(), slot as f32);
        self.dirty = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_snapshot() {
        let mut d = DemoBackend::new();
        d.connect();
        d.set("mic.gain", 33.0).unwrap();
        let s = d.snapshot().unwrap();
        assert_eq!(s.connection, Connection::Connected);
        assert_eq!(s.values["mic.gain"], 33.0);
        assert!(d.set("bogus", 1.0).is_err());
    }

    #[test]
    fn slot_recall() {
        let mut d = DemoBackend::new();
        assert!(d.recall_slot(9).is_err());
        d.recall_slot(1).unwrap();
        assert_eq!(d.values["mic.slot"], 1.0);
    }

    #[test]
    fn meters_in_range_and_mute() {
        let mut d = DemoBackend::new();
        for i in 0..50 {
            let m = d.meters_at(i as f32 * 0.37);
            assert!(m["meter.mic"] <= 0.0 && m["meter.mic"] >= DB_FLOOR);
        }
        d.set("mic.mute", 1.0).unwrap();
        assert_eq!(d.meters_at(1.0)["meter.mic"], DB_FLOOR);
    }
}

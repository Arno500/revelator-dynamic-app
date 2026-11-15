//! UI-side data model: parameter table, state snapshot and scene mapping.
//! Self-contained so the GUI works without hardware; `real.rs` adapts core to it.

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub type ParamId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    /// Confirmed on hardware (none yet from this app's own writes).
    #[allow(dead_code)]
    Verified,
    Inferred,
    /// A guessed mapping (none left: every control now has an address from UC's DLL).
    #[allow(dead_code)]
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub id: String,
    pub label: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: &'static str,
    pub confidence: Confidence,
}

pub const CHANNELS: [(&str, &str); 5] = [
    ("mic", "Mic"),
    ("playback", "Playback"),
    ("va", "Virtual A"),
    ("vb", "Virtual B"),
    ("rev", "Reverb"),
];
pub const DESTS: [(&str, &str); 3] = [("main", "Main"), ("mixa", "Mix A"), ("mixb", "Mix B")];
pub const SLOTS: usize = 4;
pub const DB_FLOOR: f32 = -96.0;
pub const SAMPLE_RATES: [u32; 4] = [44100, 48000, 88200, 96000];

pub fn mixer_id(ch: &str, field: &str) -> String {
    format!("mixer.{ch}.{field}")
}
pub fn out_id(dest: &str, field: &str) -> String {
    format!("mixer.out.{dest}.{field}")
}

fn sp(
    id: &str,
    label: &str,
    min: f32,
    max: f32,
    def: f32,
    unit: &'static str,
    c: Confidence,
) -> Spec {
    Spec {
        id: id.into(),
        label: label.into(),
        min,
        max,
        default: def,
        unit,
        confidence: c,
    }
}

fn build_specs() -> Vec<Spec> {
    use Confidence::*;
    let mut v = vec![
        sp("mic.gain", "Preamp gain", 0.0, 60.0, 22.0, " dB", Inferred),
        sp("mic.mute", "Mute", 0.0, 1.0, 0.0, "", Inferred),
        sp(
            "mic.hp_volume",
            "Headphone volume",
            0.0,
            100.0,
            70.0,
            " %",
            Inferred,
        ),
        sp(
            "mic.monitor_blend",
            "Monitor blend",
            0.0,
            100.0,
            0.0,
            " %",
            Inferred,
        ),
        sp(
            "mic.slot",
            "Active preset slot",
            0.0,
            3.0,
            0.0,
            "",
            Inferred,
        ),
        sp(
            "mic.preset_button_mode",
            "Preset button mode",
            0.0,
            2.0,
            2.0,
            "",
            Inferred,
        ),
        sp(
            "mic.mute_button_mode",
            "Mute button mode",
            0.0,
            1.0,
            1.0,
            "",
            Inferred,
        ),
        sp(
            "sys.samplerate",
            "Sample rate",
            44100.0,
            96000.0,
            48000.0,
            " Hz",
            Inferred,
        ),
    ];
    v.extend(crate::dsp::specs());
    for (ch, name) in CHANNELS {
        // UC's default scene: the reverb return starts at -96 dB everywhere
        let main = if ch == "rev" { DB_FLOOR } else { 0.0 };
        v.push(sp(
            &mixer_id(ch, "main"),
            name,
            DB_FLOOR,
            10.0,
            main,
            " dB",
            Inferred,
        ));
        let aux = if ch == "mic" { 0.0 } else { DB_FLOOR };
        v.push(sp(
            &mixer_id(ch, "mixa"),
            name,
            DB_FLOOR,
            10.0,
            aux,
            " dB",
            Inferred,
        ));
        v.push(sp(
            &mixer_id(ch, "mixb"),
            name,
            DB_FLOOR,
            10.0,
            aux,
            " dB",
            Inferred,
        ));
        v.push(sp(
            &mixer_id(ch, "mute"),
            "Mute",
            0.0,
            1.0,
            0.0,
            "",
            Inferred,
        ));
        v.push(sp(
            &mixer_id(ch, "solo"),
            "Solo",
            0.0,
            1.0,
            0.0,
            "",
            Inferred,
        ));
    }
    v.push(sp(
        "mixer.mic.reverb",
        "Reverb level",
        DB_FLOOR,
        10.0,
        DB_FLOOR,
        " dB",
        Inferred,
    ));
    // The mic channel's effect switches (UC `bypassDSP`, `dspAmount`, `dawpostdsp`).
    v.push(sp(
        "mixer.mic.bypass",
        "Bypass Fat Channel",
        0.0,
        1.0,
        0.0,
        "",
        Inferred,
    ));
    v.push(sp(
        "mixer.mic.dspamount",
        "Fat Channel amount",
        0.0,
        1.0,
        1.0,
        "",
        Inferred,
    ));
    v.push(sp(
        "mixer.mic.dawpost",
        "Fat Channel on USB Stream Outputs",
        0.0,
        1.0,
        1.0,
        "",
        Inferred,
    ));
    for (d, name) in DESTS {
        v.push(sp(
            &out_id(d, "vol"),
            name,
            DB_FLOOR,
            10.0,
            0.0,
            " dB",
            Inferred,
        ));
        v.push(sp(&out_id(d, "mute"), "Mute", 0.0, 1.0, 0.0, "", Inferred));
    }
    v
}

fn specs() -> &'static HashMap<String, Spec> {
    static S: OnceLock<HashMap<String, Spec>> = OnceLock::new();
    S.get_or_init(|| {
        build_specs()
            .into_iter()
            .map(|mut s| {
                // Confidence comes from the core parameter table wherever a mapping exists.
                if let Some(c) = crate::map::confidence(&s.id) {
                    s.confidence = c;
                }
                (s.id.clone(), s)
            })
            .collect()
    })
}

pub fn spec(id: &str) -> Option<&'static Spec> {
    specs().get(id)
}
pub fn all_specs() -> impl Iterator<Item = &'static Spec> {
    specs().values()
}
pub fn default_values() -> HashMap<ParamId, f32> {
    defaults().clone()
}

fn defaults() -> &'static HashMap<ParamId, f32> {
    static D: OnceLock<HashMap<ParamId, f32>> = OnceLock::new();
    D.get_or_init(|| all_specs().map(|s| (s.id.clone(), s.default)).collect())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Connection {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Clone, Debug, Default)]
pub struct DeviceInfo {
    pub name: String,
    pub firmware: String,
    pub protocol: String,
}

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub connection: Connection,
    /// Human readable detail for the connect screen.
    pub connection_detail: String,
    pub info: DeviceInfo,
    pub values: HashMap<ParamId, f32>,
    /// Meter levels in dBFS (reduction is positive dB of gain reduction).
    pub meters: HashMap<String, f32>,
    pub last_error: Option<String>,
    /// A one-off success message from the device thread (delivered once, like errors).
    pub notice: Option<String>,
    /// What the mic's preset areas hold (read on request).
    pub mic_presets: Option<MicPresets>,
}

/// A preset the mic can copy into one of its button slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetSource {
    /// Factory preset 0..8 (built into the firmware).
    Factory(usize),
    /// User preset 0..8 (writable area of the mic).
    User(usize),
}

impl MicPresets {
    /// True when `name` (as listed) stands for a real preset, not an empty/unreadable area.
    pub fn defined(name: &str) -> bool {
        name == "(unnamed)" || !(name.starts_with('(') || name == "...")
    }
}

/// Names of what the mic currently holds, `"(empty)"` / `"(unreadable)"` when there is none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MicPresets {
    /// The four presets the physical button cycles through.
    pub slots: Vec<String>,
    /// The eight user presets.
    pub user: Vec<String>,
}

pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;
/// Called by the backend (from any thread, possibly under its lock: keep it quick and never
/// call back into the backend) with the new state whenever it changes.
pub type StatusHook = std::sync::Arc<dyn Fn(&UiState) + Send + Sync>;

/// Quick actions offered outside the window: jump list tasks, taskbar thumbnail buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Switch the mic to preset slot `n` (0-based).
    Slot(usize),
    ToggleMute,
}

impl Action {
    /// Command-line form (used by the jump list tasks); `Action::from_args` reads it back.
    pub fn args(self) -> String {
        match self {
            Action::Slot(n) => format!("--slot {}", n + 1),
            Action::ToggleMute => "--toggle-mute".into(),
        }
    }
    pub fn from_args(args: &[String]) -> Option<Action> {
        if args.iter().any(|a| a == "--toggle-mute") {
            return Some(Action::ToggleMute);
        }
        let i = args.iter().position(|a| a == "--slot")?;
        let n: usize = args.get(i + 1)?.parse().ok()?;
        (1..=4).contains(&n).then(|| Action::Slot(n - 1))
    }
    /// Compact code for a window message, and back.
    pub fn code(self) -> usize {
        match self {
            Action::Slot(n) => n + 1,
            Action::ToggleMute => 100,
        }
    }
    pub fn from_code(c: usize) -> Option<Action> {
        match c {
            1..=4 => Some(Action::Slot(c - 1)),
            100 => Some(Action::ToggleMute),
            _ => None,
        }
    }
}

/// Performs an [`Action`]; callable from any thread.
pub type RemoteFn = std::sync::Arc<dyn Fn(Action) + Send + Sync>;

pub trait Backend {
    fn name(&self) -> &str;
    /// Begin / retry connecting. Must not block.
    fn connect(&mut self);
    /// Latest state if it changed since the last call, else `None`. Non-blocking.
    fn snapshot(&mut self) -> Option<UiState>;
    /// Register a callback the backend invokes (from any thread) when `snapshot` has news, so the
    /// UI can sleep until then instead of polling.
    fn set_wake(&mut self, _wake: Wake) {}
    /// Register a [`StatusHook`]. Unlike the UI, it also runs while the window is minimized
    /// or hidden. Backends living on the UI thread need not support it.
    fn set_status_hook(&mut self, _hook: StatusHook) {}
    /// A handle performing [`Action`]s from any thread, also while the UI runs no frames.
    /// `None`: the UI performs them itself.
    fn remote(&self) -> Option<RemoteFn> {
        None
    }
    /// Refresh live meter levels in place; returns true if it did (animated backends only).
    fn poll_meters(&mut self, _out: &mut HashMap<String, f32>) -> bool {
        false
    }
    /// True if the backend animates continuously (demo meters) and needs a steady repaint.
    fn animated(&self) -> bool {
        false
    }
    fn set(&mut self, id: &str, v: f32) -> Result<(), String>;
    fn recall_slot(&mut self, slot: usize) -> Result<(), String>;
    /// Load the settings kept in the mic's preset slot into the app (device only).
    fn load_slot(&mut self, _slot: usize) -> Result<(), String> {
        Err("reading the mic's slots needs the real device".into())
    }
    /// Read the names held by the mic's button slots and user presets (device only).
    fn refresh_mic_presets(&mut self) -> Result<(), String> {
        Err("reading the mic's presets needs the real device".into())
    }
    /// Copy a factory/user preset into a button slot (device only).
    fn assign_slot(&mut self, _slot: usize, _src: PresetSource) -> Result<(), String> {
        Err("assigning presets needs the real device".into())
    }
    /// Empty user preset `n` (device only).
    fn delete_user_preset(&mut self, _n: usize) -> Result<(), String> {
        Err("deleting presets needs the real device".into())
    }
    /// Store the current settings in user preset `n` (device only).
    fn save_user_preset(&mut self, _n: usize, _name: &str) -> Result<(), String> {
        Err("storing presets needs the real device".into())
    }
}

/// GUI id of the Voice FX model selector (model 5 = echo/delay).
pub const ECHO_ID: &str = "dsp.voicefx.model";

/// True if the Delay voice FX must not be used at this sample rate (firmware hazard).
pub fn delay_blocked(sample_rate: f32) -> bool {
    sample_rate >= 88_000.0
}

/// UI-side state with optimistic local edits that are not clobbered by stale snapshots.
pub struct Model {
    pub state: UiState,
    /// Device values from the last snapshot, to fall back on when a local edit expires.
    device: HashMap<ParamId, f32>,
    pending: HashMap<ParamId, Instant>,
    pub hold: Duration,
}

impl Default for Model {
    fn default() -> Self {
        let state = UiState {
            values: default_values(),
            ..UiState::default()
        };
        Model {
            state,
            device: HashMap::new(),
            pending: HashMap::new(),
            hold: Duration::from_millis(500),
        }
    }
}

impl Model {
    pub fn get(&self, id: &str) -> f32 {
        self.state
            .values
            .get(id)
            .copied()
            .or_else(|| spec(id).map(|s| s.default))
            .unwrap_or(0.0)
    }
    pub fn on(&self, id: &str) -> bool {
        self.get(id) >= 0.5
    }
    pub fn meter(&self, id: &str) -> f32 {
        self.state.meters.get(id).copied().unwrap_or(DB_FLOOR)
    }
    /// True if the backend reported `id` (or the user just set it), i.e. it is not a default.
    pub fn known(&self, id: &str) -> bool {
        self.device.contains_key(id) || self.pending.contains_key(id)
    }

    /// Validate/clamp and apply a user edit locally. Returns the value to send.
    pub fn set_local(&mut self, id: &str, v: f32, now: Instant) -> Result<f32, String> {
        let s = spec(id).ok_or_else(|| format!("unknown parameter {id}"))?;
        let v = v.clamp(s.min, s.max);
        let echo = crate::dsp::ECHO_MODEL;
        if id == ECHO_ID && v == echo && delay_blocked(self.get("sys.samplerate")) {
            return Err("Echo voice FX is disabled at 88.2/96 kHz (firmware hazard)".into());
        }
        if id == "sys.samplerate" && self.get(ECHO_ID) == echo && delay_blocked(v) {
            return Err("Switch Voice FX away from Echo before using 88.2/96 kHz".into());
        }
        self.state.values.insert(id.to_string(), v);
        self.pending.insert(id.to_string(), now);
        Ok(v)
    }

    pub fn apply_snapshot(&mut self, snap: UiState, now: Instant) {
        let hold = self.hold;
        self.pending.retain(|_, t| now.duration_since(*t) < hold);
        let mut snap = snap;
        self.device = snap.values.clone();
        let mut values = std::mem::take(&mut snap.values);
        for k in self.pending.keys() {
            if let Some(local) = self.state.values.get(k) {
                values.insert(k.clone(), *local);
            }
        }
        if values.len() < defaults().len() {
            for (k, v) in defaults() {
                values.entry(k.clone()).or_insert(*v);
            }
        }
        if snap.meters.is_empty() {
            snap.meters = std::mem::take(&mut self.state.meters); // polled separately
        }
        let keep_err = self.state.last_error.take();
        let last_error = snap.last_error.clone().or(keep_err);
        let keep_notice = self.state.notice.take();
        let notice = snap.notice.clone().or(keep_notice);
        self.state = UiState {
            values,
            last_error,
            notice,
            ..snap
        };
    }

    /// Drop expired local edits; their controls show the device value again. Snapshots only
    /// arrive when something changed, so this must also run without one.
    pub fn expire_pending(&mut self, now: Instant) {
        let hold = self.hold;
        let state = &mut self.state;
        let device = &self.device;
        self.pending.retain(|k, t| {
            if now.duration_since(*t) < hold {
                return true;
            }
            if let Some(v) = device.get(k) {
                state.values.insert(k.clone(), *v);
            }
            false
        });
    }

    /// True while local edits are still shielded from stale snapshots (needs a later repaint).
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn any_solo(&self) -> bool {
        CHANNELS.iter().any(|(c, _)| self.on(&mixer_id(c, "solo")))
    }
    /// Channel is audible in the mixes (not muted, and solo logic permits it).
    pub fn channel_audible(&self, ch: &str) -> bool {
        !self.on(&mixer_id(ch, "mute")) && (!self.any_solo() || self.on(&mixer_id(ch, "solo")))
    }
}

// ---------------------------------------------------------------- scenes

/// Native scene file: flat parameter map.
pub fn scene_to_json(values: &HashMap<ParamId, f32>) -> String {
    let sorted: BTreeMap<_, _> = values
        .iter()
        .filter(|(k, _)| !k.starts_with("sys."))
        .collect();
    serde_json::to_string_pretty(
        &json!({ "format": "revelator-control-scene", "version": 1, "values": sorted }),
    )
    .unwrap_or_default()
}

pub fn scene_from_json(text: &str) -> Result<Vec<(ParamId, f32)>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("invalid scene: {e}"))?;
    let obj = v
        .get("values")
        .and_then(Value::as_object)
        .ok_or("scene has no \"values\"")?;
    Ok(obj
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_f64()? as f32)))
        .filter(|(k, _)| spec(k).is_some())
        .collect())
}

/// Map a Universal Control `.scene` (line/return/aux/main JSON) onto our parameters.
pub fn uc_scene_import(text: &str) -> Result<Vec<(ParamId, f32)>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("invalid UC scene: {e}"))?;
    let f = |p: &[&str]| -> Option<f32> {
        let mut cur = &v;
        for k in p {
            cur = cur.get(*k)?;
        }
        cur.as_f64().map(|x| x as f32)
    };
    let mut out = Vec::new();
    let mut push = |id: String, val: Option<f32>| {
        if let Some(val) = val {
            out.push((id, val));
        }
    };
    let chans = [
        ("mic", "line", "ch1"),
        ("playback", "return", "ch1"),
        ("va", "return", "ch2"),
        ("vb", "return", "ch3"),
        ("rev", "fxreturn", "ch1"),
    ];
    let mut found = false;
    for (ch, grp, n) in chans {
        if v.get(grp).and_then(|g| g.get(n)).is_some() {
            found = true;
        }
        push(mixer_id(ch, "main"), f(&[grp, n, "volume"]));
        push(mixer_id(ch, "mute"), f(&[grp, n, "mute"]));
        push(mixer_id(ch, "solo"), f(&[grp, n, "solo"]));
        push(mixer_id(ch, "mixa"), f(&[grp, n, "aux1"]));
        push(mixer_id(ch, "mixb"), f(&[grp, n, "aux2"]));
    }
    push("mic.gain".into(), f(&["line", "ch1", "preampgain"]));
    push("mixer.mic.reverb".into(), f(&["line", "ch1", "FXA"]));
    for (d, grp, n) in [
        ("main", "main", "ch1"),
        ("mixa", "aux", "ch1"),
        ("mixb", "aux", "ch2"),
    ] {
        push(out_id(d, "vol"), f(&[grp, n, "volume"]));
        push(out_id(d, "mute"), f(&[grp, n, "mute"]));
    }
    if !found {
        return Err("not a Universal Control scene (no line/return channels)".into());
    }
    Ok(out)
}

/// Map a dB level to 0..1 for meter display (-60..0 dB).
pub fn meter_fraction(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_hazard_blocked() {
        let mut m = Model::default();
        let now = Instant::now();
        assert!(m.set_local(ECHO_ID, 5.0, now).is_ok());
        m.set_local(ECHO_ID, 0.0, now).unwrap();
        m.set_local("sys.samplerate", 96000.0, now).unwrap();
        assert!(m.set_local(ECHO_ID, 5.0, now).is_err());
        assert!(m.set_local(ECHO_ID, 4.0, now).is_ok());
        m.set_local("sys.samplerate", 48000.0, now).unwrap();
        m.set_local(ECHO_ID, 5.0, now).unwrap();
        assert!(m.set_local("sys.samplerate", 88200.0, now).is_err());
    }

    #[test]
    fn set_clamps_and_rejects_unknown() {
        let mut m = Model::default();
        let now = Instant::now();
        assert_eq!(m.set_local("mic.gain", 999.0, now).unwrap(), 60.0);
        assert!(m.set_local("nope", 1.0, now).is_err());
    }

    #[test]
    fn snapshot_does_not_clobber_recent_edit() {
        let mut m = Model::default();
        let t0 = Instant::now();
        m.set_local("mic.gain", 40.0, t0).unwrap();
        let mut snap = UiState::default();
        snap.values.insert("mic.gain".into(), 10.0);
        snap.values.insert("mic.hp_volume".into(), 33.0);
        m.apply_snapshot(snap.clone(), t0 + Duration::from_millis(100));
        assert_eq!(m.get("mic.gain"), 40.0);
        assert_eq!(m.get("mic.hp_volume"), 33.0);
        m.apply_snapshot(snap, t0 + Duration::from_secs(2));
        assert_eq!(m.get("mic.gain"), 10.0);
    }

    #[test]
    fn expired_edit_reverts_to_device_value_without_new_snapshot() {
        let mut m = Model::default();
        let t0 = Instant::now();
        let mut snap = UiState::default();
        snap.values.insert("mic.gain".into(), 10.0);
        m.apply_snapshot(snap, t0);
        m.set_local("mic.gain", 40.0, t0).unwrap();
        m.expire_pending(t0 + Duration::from_millis(100));
        assert!(m.has_pending());
        assert_eq!(m.get("mic.gain"), 40.0);
        m.expire_pending(t0 + Duration::from_secs(2));
        assert!(!m.has_pending());
        assert_eq!(m.get("mic.gain"), 10.0);
    }

    #[test]
    fn solo_logic() {
        let mut m = Model::default();
        let now = Instant::now();
        assert!(m.channel_audible("mic"));
        m.set_local(&mixer_id("va", "solo"), 1.0, now).unwrap();
        assert!(!m.channel_audible("mic"));
        assert!(m.channel_audible("va"));
        m.set_local(&mixer_id("va", "mute"), 1.0, now).unwrap();
        assert!(!m.channel_audible("va"));
    }

    #[test]
    fn scene_roundtrip() {
        let mut vals = default_values();
        vals.insert("mic.gain".into(), 31.5);
        let text = scene_to_json(&vals);
        let back = scene_from_json(&text).unwrap();
        assert!(back.contains(&("mic.gain".to_string(), 31.5)));
        assert!(scene_from_json("{}").is_err());
    }

    #[test]
    fn uc_import() {
        let text = r#"{"line":{"ch1":{"mute":1,"volume":-3.0,"solo":0,"aux1":0.0,"aux2":-96.0,"preampgain":22.0}},
            "return":{"ch1":{"volume":0.0,"aux1":-96.0,"aux2":-96.0},"ch2":{"volume":-6.0}},
            "aux":{"ch1":{"volume":-1.0},"ch2":{"mute":1}},"main":{"ch1":{"volume":-2.0}}}"#;
        let r: HashMap<_, _> = uc_scene_import(text).unwrap().into_iter().collect();
        assert_eq!(r["mic.gain"], 22.0);
        assert_eq!(r["mixer.mic.mute"], 1.0);
        assert_eq!(r["mixer.va.main"], -6.0);
        assert_eq!(r["mixer.out.main.vol"], -2.0);
        assert_eq!(r["mixer.out.mixb.mute"], 1.0);
        assert!(uc_scene_import("{}").is_err());
    }

    #[test]
    fn meter_scale() {
        assert_eq!(meter_fraction(-96.0), 0.0);
        assert_eq!(meter_fraction(0.0), 1.0);
        assert!((meter_fraction(-30.0) - 0.5).abs() < 1e-6);
    }
}

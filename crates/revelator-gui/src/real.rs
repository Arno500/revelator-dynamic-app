//! Real-device backend: a dedicated thread owns the device session; the UI talks to it over
//! channels and never blocks on USB. The thread retries opening forever (hotplug/reconnect).
//!
//! The thread is generic over [`Opener`]/[`Session`] so it is testable without hardware.
//! `HwOpener`/`HwSession` adapt `revelator_usb::NusbTransport` + `revelator_core::Device`.

use crate::dsp;
use crate::map;
use crate::model::*;
use revelator_core::fx::{self, Engine, FxState};
use revelator_core::mixer::{self, MixerState};
use revelator_core::{presetdoc, ubjson};
use revelator_core::{Device, DeviceError, Transport, TransportError};
use revelator_usb::{NusbTransport, OpenError};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum SessionError {
    #[allow(dead_code)] // constructed by the hardware session adapter and tests
    Disconnected,
    Other(String),
}

pub struct Reading {
    pub values: HashMap<ParamId, f32>,
    pub meters: HashMap<String, f32>,
}

pub trait Session: Send {
    fn info(&self) -> DeviceInfo;
    fn read(&mut self) -> Result<Reading, SessionError>;
    fn set(&mut self, id: &str, v: f32) -> Result<(), SessionError>;
    fn recall_slot(&mut self, slot: usize) -> Result<(), SessionError>;
    /// Load the DSP settings stored in preset slot `slot` of the mic.
    fn load_slot(&mut self, _slot: usize) -> Result<(), SessionError> {
        Err(SessionError::Other("not supported".into()))
    }
    /// Names held by the button slots and user presets.
    fn mic_presets(&mut self) -> Result<MicPresets, SessionError> {
        Err(SessionError::Other("not supported".into()))
    }
    /// Copy a factory/user preset into a button slot.
    fn assign_slot(&mut self, _slot: usize, _src: PresetSource) -> Result<(), SessionError> {
        Err(SessionError::Other("not supported".into()))
    }
    /// Empty user preset `n`.
    fn delete_user(&mut self, _n: usize) -> Result<(), SessionError> {
        Err(SessionError::Other("not supported".into()))
    }
    /// Store the current DSP settings in user preset `n`.
    fn save_user(&mut self, _n: usize, _name: &str) -> Result<(), SessionError> {
        Err(SessionError::Other("not supported".into()))
    }
    /// Send whatever `set` queued (DSP engine blocks). Called once a burst of commands is handled,
    /// so dragging a slider or applying a profile sends each engine once.
    fn flush(&mut self) -> Result<(), SessionError> {
        Ok(())
    }
    /// Called once after every (re)connection: send the saved settings to the mic, which comes
    /// up with its own defaults (power cycle, unplug, firmware reset).
    fn on_connect(&mut self) -> Result<(), SessionError> {
        Ok(())
    }
}

pub trait Opener: Send {
    /// Try to open the device. `Err` carries a user-facing reason ("plug in ...").
    fn open(&mut self) -> Result<Box<dyn Session>, String>;
}

/// Opens the first attached Revelator Dynamic over nusb and wraps it in a [`HwSession`].
pub struct HwOpener;

/// User-facing text for a USB open failure.
pub fn open_error_message(e: &OpenError) -> String {
    match e {
        OpenError::NoDevice => "Plug in your Revelator Dynamic".into(),
        OpenError::UnsupportedMode(n) => format!(
            "{n} is in an unsupported mode. Switch the mic out of that mode (so it enumerates as a normal Revelator Dynamic) and replug it."
        ),
        OpenError::UnsupportedModel(n) => {
            format!("{n} is not supported: this app only works with the Revelator Dynamic.")
        }
        OpenError::NoVendorInterface => "No usable control interface found on the mic.".into(),
        OpenError::InterfaceNotRegistered => "One-time setup needed: Windows has not exposed the mic's control interface to apps yet.             Open an elevated terminal (Run as administrator) and run: revelator-cli setup --yes             (it changes one registry value on the control interface only; the PreSonus audio driver is not touched; undo with: revelator-cli setup --undo --yes)."
            .into(),
        OpenError::InterfaceBusy(m) => format!(
            "The mic's control interface is busy or access was denied ({m}). Close Universal Control and stop the 'PreSonus Hardware Access Service'             (they can hold the interface exclusively; WinUSB allows one opener at a time), then retry."
        ),
        OpenError::Usb(m) => format!("USB error: {m}"),
    }
}

fn device_error(e: DeviceError) -> SessionError {
    match e {
        DeviceError::Transport(TransportError::Disconnected) => SessionError::Disconnected,
        other => SessionError::Other(other.to_string()),
    }
}

impl Opener for HwOpener {
    fn open(&mut self) -> Result<Box<dyn Session>, String> {
        let transport = NusbTransport::open_first().map_err(|e| open_error_message(&e))?;
        let name = format!("Revelator Dynamic ({})", transport.description());
        let session = HwSession::open(transport, name)
            .map_err(|e| format!("Found the mic but it did not answer: {e}"))?;
        Ok(Box::new(session))
    }
}

/// [`Session`] over a core [`Device`]; generic over the transport so it is testable with the mock.
pub struct HwSession<T: Transport> {
    dev: Device<T>,
    name: String,
    /// Last sample rate the user chose (the device does not report it); `None` = unknown,
    /// which keeps Delay blocked in the core safety layer.
    rate: Option<u32>,
    /// DSP values (the device cannot report them), persisted to `fx_path` when set.
    fx: FxState,
    fx_path: Option<PathBuf>,
    /// Engines changed since the last flush, in first-change order.
    dirty: Vec<Engine>,
    /// Reads the rate Windows uses for the mic's audio endpoints (`None` = not found).
    rate_probe: Box<dyn FnMut() -> Option<u32> + Send>,
    /// True while `rate` comes from Windows (the manual choice is then ignored).
    rate_from_windows: bool,
    /// Set when Echo was just switched off because of a high sample rate (reported once).
    echo_disarmed: bool,
    /// Stream mixer settings (the device cannot report them), persisted to `mix_path`.
    mix: MixerState,
    mix_path: Option<PathBuf>,
    /// Mixer writes last sent this session (`None` = nothing sent yet: send everything).
    mix_sent: Option<Vec<mixer::MixWrite>>,
    mix_dirty: bool,
    /// Last value written to each mic parameter that cannot be read back (shown as-is).
    written: HashMap<String, f32>,
    /// Voice FX model last selected with `VoFx` this session (resent only when it changes).
    vofx_sent: Option<u32>,
}
/// GUI value id: 1 while the sample rate is read from Windows.
pub const RATE_FROM_WINDOWS_ID: &str = "sys.samplerate.windows";

/// Sample rate used to design filters when the user has not said which rate is in use.
const DEFAULT_FS: f32 = 48_000.0;

impl<T: Transport> HwSession<T> {
    /// Keep a copy of `old` (a mic area about to be overwritten) next to the settings file.
    fn backup(&self, tag: &str, old: &[u8]) -> Result<(), SessionError> {
        let Some(dir) = self.fx_path.as_ref().and_then(|p| p.parent()) else {
            return Ok(());
        };
        let dir = dir.join("slot-backups");
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(dir.join(format!("{tag}-{secs}.bin")), old))
            .map_err(|e| SessionError::Other(format!("could not back up {tag} first: {e}")))
    }
    /// Open with the DSP state shared with `revelator-cli dsp` (`%APPDATA%\revelator-control`).
    pub fn open(transport: T, name: String) -> Result<Self, DeviceError> {
        let mut s = Self::open_with_state(transport, name, Some(fx::default_state_path()))?;
        s.mix_path = Some(mixer::default_state_path());
        s.mix = mixer::load_state(&mixer::default_state_path());
        // Like UC, push the whole mixer and the mic channel parameters (DSP amount, pan, DAW
        // post-DSP, ...) on connect; the mic starts with its own defaults otherwise.
        s.mix_dirty = true;
        let _ = s.flush_mixer();
        Ok(s)
    }

    /// `fx_path: None` = keep DSP state in memory only (tests).
    pub fn open_with_state(
        transport: T,
        name: String,
        fx_path: Option<PathBuf>,
    ) -> Result<Self, DeviceError> {
        let fx = fx_path.as_deref().map(fx::load_state).unwrap_or_default();
        Ok(HwSession {
            dev: Device::open(transport)?,
            name,
            rate: None,
            fx,
            fx_path,
            dirty: Vec::new(),
            rate_probe: {
                // Core Audio watcher: updated by endpoint notifications, so this is just a read.
                let mut w = revelator_usb::audio_rate::RateWatcher::start();
                Box::new(move || w.current())
            },
            rate_from_windows: false,
            echo_disarmed: false,
            mix: MixerState::default(),
            mix_path: None,
            mix_sent: None,
            mix_dirty: false,
            written: HashMap::new(),
            vofx_sent: None,
        })
    }

    /// Push the saved DSP settings (the profile in `%APPDATA%\revelator-control`) to the mic.
    /// Does nothing when nothing was ever saved, so a first run keeps the mic's own settings.
    /// The mixer is already sent when the session opens.
    fn push_saved_state(&mut self) -> Result<(), SessionError> {
        if !self.fx_path.as_ref().is_some_and(|p| p.exists()) {
            return Ok(());
        }
        self.probe_rate();
        // Echo (Delay) crashes the firmware at 88.2/96 kHz and needs a known rate: never push
        // it blindly (same rule as `load_slot`).
        let echo_unsafe = self.fx.get("voicefx.model") == dsp::ECHO_MODEL
            && !self.rate.is_some_and(|r| r < 88_200);
        if echo_unsafe {
            let _ = self.fx.set("voicefx.on", 0.0);
            let _ = self.fx.set("voicefx.model", 0.0);
        }
        for e in dsp::ENGINES {
            self.mark(e);
        }
        self.flush()?;
        if echo_unsafe {
            return Err(SessionError::Other(
                "the saved settings use Echo, which needs a known sample rate below 88.2 kHz: Voice FX was left off".into(),
            ));
        }
        Ok(())
    }

    /// Take the latest Windows endpoint rate. A found rate always wins over the manual choice,
    /// so the Delay hazard check uses what the audio engine really runs at.
    fn probe_rate(&mut self) {
        match (self.rate_probe)() {
            Some(hz) => {
                self.rate = Some(hz);
                self.rate_from_windows = true;
                self.dev.set_sample_rate(Some(hz));
                self.disarm_echo_at_high_rate();
            }
            None if self.rate_from_windows => {
                // Endpoint gone: back to unknown until Windows or the user says otherwise.
                self.rate = None;
                self.rate_from_windows = false;
                self.dev.set_sample_rate(None);
            }
            None => {}
        }
    }

    /// Echo (Delay) at 88.2/96 kHz crashes the firmware. Windows switches the rate before we
    /// hear about it, so this cannot prevent the switch; it moves Voice FX off Echo (to the
    /// Doubler, effect off) as soon as the high rate is seen, and sends that right away.
    fn disarm_echo_at_high_rate(&mut self) {
        if self.rate.is_some_and(|r| r >= 88_200) && self.fx.get("voicefx.model") == dsp::ECHO_MODEL
        {
            let _ = self.fx.set("voicefx.on", 0.0);
            let _ = self.fx.set("voicefx.model", 0.0);
            self.mark(Engine::VoiceFx);
            self.echo_disarmed = true;
        }
    }

    /// Send the mixer writes that changed since the last send (all of them the first time).
    fn flush_mixer(&mut self) -> Result<(), SessionError> {
        if !std::mem::take(&mut self.mix_dirty) {
            return Ok(());
        }
        let all = self.mix.writes();
        let changed: Vec<_> = match &self.mix_sent {
            Some(prev) => all.iter().filter(|w| !prev.contains(w)).copied().collect(),
            None => all.clone(),
        };
        self.dev.send_mixer(&changed).map_err(device_error)?;
        self.mix_sent = Some(all);
        if let Some(path) = &self.mix_path {
            mixer::save_state(path, &self.mix).map_err(SessionError::Other)?;
        }
        Ok(())
    }

    /// Track the monitor blend (it scales the mic and the returns on the main bus). Once the
    /// mixer has been sent this session, a blend change (GUI or the mic's knob) resends it.
    fn follow_blend(&mut self, b: f32) {
        if b.is_finite() && (-1.0..=1.0).contains(&b) && b != self.mix.blend {
            self.mix.blend = b;
            if self.mix_sent.is_some() {
                self.mix_dirty = true;
            }
        }
    }

    fn mark(&mut self, e: Engine) {
        if !self.dirty.contains(&e) {
            self.dirty.push(e);
        }
    }

    fn set_dsp(&mut self, p: &'static fx::FxParam, gui: f32) -> Result<(), SessionError> {
        let v = dsp::to_core(p, gui);
        if p.key == "voicefx.model"
            && v == dsp::ECHO_MODEL
            && !self.rate.is_some_and(|r| r < 88_200)
        {
            return Err(SessionError::Other(
                "Echo needs the sample rate: pick 44.1 or 48 kHz as the sample rate in the status bar first (Delay at 88.2/96 kHz crashes the firmware)".into(),
            ));
        }
        if self.fx.get(p.key) == v {
            return Ok(());
        }
        let e = self
            .fx
            .set(p.key, v)
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.mark(e);
        Ok(())
    }
}

impl<T: Transport> Session for HwSession<T> {
    fn info(&self) -> DeviceInfo {
        let i = self.dev.info();
        DeviceInfo {
            name: self.name.clone(),
            firmware: "not reported".into(), // core exposes no firmware version
            protocol: format!("v{}", i.protocol_version),
        }
    }

    fn read(&mut self) -> Result<Reading, SessionError> {
        self.probe_rate();
        if std::mem::take(&mut self.echo_disarmed) {
            self.flush()?;
            return Err(SessionError::Other(
                "Echo was switched off: the sample rate is now 88.2/96 kHz, where Echo (Delay) crashes the mic firmware. Switch the rate back to 44.1/48 kHz before using Echo.".into(),
            ));
        }
        // The mic can take over a second to answer right after a write (seen after a mute):
        // on a timeout, wait and retry before reporting an error.
        let state = match self.dev.read_state() {
            Err(e) if e.is_timeout() => self.dev.read_state_after_write(),
            r => r,
        }
        .map_err(device_error)?;
        let mut values = HashMap::new();
        for e in map::table() {
            let d = map::def(e);
            if let Some(v) = map::read(d, &state) {
                values.insert(e.gui.to_string(), map::to_gui(e, d, v));
            } else if let Some(v) = self.written.get(e.gui) {
                values.insert(e.gui.to_string(), *v);
            }
        }
        if let Some(e) = map::lookup("mic.monitor_blend") {
            if let Some(b) = map::read(map::def(e), &state) {
                self.follow_blend(b);
                self.flush_mixer()?;
            }
        }
        for (k, v, ..) in self.mix.entries() {
            if let Some(id) = map::mixer_gui_id(&k) {
                values.insert(id, v);
            }
        }
        if let Some(r) = self.rate {
            values.insert("sys.samplerate".into(), r as f32);
        }
        if self.rate_from_windows {
            values.insert(RATE_FROM_WINDOWS_ID.into(), 1.0);
        }
        values.extend(dsp::gui_values(&self.fx));
        Ok(Reading {
            values,
            meters: map::meters(&state),
        })
    }

    fn set(&mut self, id: &str, v: f32) -> Result<(), SessionError> {
        if id == "sys.samplerate" {
            // Not a device parameter: feeds the safety context (Delay at 88.2/96 kHz).
            if self.rate_from_windows {
                return Err(SessionError::Other(
                    "the sample rate is read from Windows; change it in Sound settings".into(),
                ));
            }
            let hz = v.round() as u32;
            self.dev.set_sample_rate(Some(hz));
            self.rate = Some(hz);
            return Ok(());
        }
        if let Some(p) = dsp::param(id) {
            return self.set_dsp(p, v);
        }
        if let Some(k) = map::mixer_key(id) {
            if self.mix.get(&k) != Some(v) {
                self.mix
                    .set(&k, v)
                    .map_err(|e| SessionError::Other(e.to_string()))?;
                self.mix_dirty = true;
            }
            return Ok(());
        }
        // Only parameters with a write address from UC's DLL reach the device.
        let e = map::lookup(id).ok_or_else(|| {
            SessionError::Other("this control has no known device address yet".into())
        })?;
        let d = map::def(e);
        let core = map::to_core(e, d, v);
        self.dev.set_appl(d, core).map_err(device_error)?;
        self.written.insert(id.to_string(), v);
        if id == "mic.monitor_blend" && self.mix.blend != core {
            // The blend also scales the main-bus crosspoints (UC does both).
            self.mix.blend = core;
            self.mix_dirty = true;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), SessionError> {
        self.flush_mixer()?;
        if self.dirty.is_empty() {
            return Ok(());
        }
        let fs = self.rate.map_or(DEFAULT_FS, |r| r as f32);
        // Resend in the canonical order (EQ/comp order switch first), like a full push.
        let dirty = std::mem::take(&mut self.dirty);
        for e in dsp::ENGINES.into_iter().filter(|e| dirty.contains(e)) {
            // Exactly UC's blobs: no `Setu` (the firmware sets its engines up itself).
            let mut writes = self.fx.writes(e, fs);
            if e == Engine::VoiceFx {
                // Like UC, `VoFx` only when the model changes: every `VoFx`, even for the same
                // model, rebuilds the engine in the firmware and drops the settings sent with it.
                let model = self.fx.get("voicefx.model").round() as u32;
                if self.vofx_sent == Some(model) {
                    writes.retain(|w| !w.fixed_index0);
                }
            }
            let sent = self.dev.send_dsp(0, &writes);
            if e == Engine::VoiceFx {
                self.vofx_sent = match sent {
                    Ok(()) => Some(self.fx.get("voicefx.model").round() as u32),
                    Err(_) => None,
                };
            }
            sent.map_err(|err| match device_error(err) {
                SessionError::Other(m) => SessionError::Other(format!("DSP {e:?}: {m}")),
                d => d,
            })?;
        }
        if let Some(path) = &self.fx_path {
            fx::save_state(path, &self.fx).map_err(SessionError::Other)?;
        }
        Ok(())
    }

    fn on_connect(&mut self) -> Result<(), SessionError> {
        self.push_saved_state()
    }

    fn recall_slot(&mut self, slot: usize) -> Result<(), SessionError> {
        // Selecting the active slot is the only preset operation core exposes.
        self.set("mic.slot", slot as f32)
    }

    fn mic_presets(&mut self) -> Result<MicPresets, SessionError> {
        let name = |r: Result<Vec<u8>, DeviceError>| match r {
            Err(_) => "(unreadable)".to_string(),
            Ok(b) if b.is_empty() => "(empty)".to_string(),
            Ok(b) => match ubjson::decode(&b) {
                Ok(ubjson::Value::Object(o)) if o.is_empty() => "(empty)".to_string(),
                Ok(d) => presetdoc::name_of(&d).unwrap_or("(unnamed)").to_string(),
                Err(_) => "(not a preset)".to_string(),
            },
        };
        let mut out = MicPresets::default();
        for n in 0..4 {
            let r = self.dev.read_button_slot(n);
            if matches!(r, Err(DeviceError::Transport(TransportError::Disconnected))) {
                return Err(SessionError::Disconnected);
            }
            out.slots.push(name(r));
        }
        for n in 8..16 {
            let r = self.dev.read_preset(n);
            if matches!(r, Err(DeviceError::Transport(TransportError::Disconnected))) {
                return Err(SessionError::Disconnected);
            }
            out.user.push(name(r));
        }
        Ok(out)
    }

    fn assign_slot(&mut self, slot: usize, src: PresetSource) -> Result<(), SessionError> {
        if slot >= 4 {
            return Err(SessionError::Other("bad slot".into()));
        }
        let bytes = match src {
            PresetSource::Factory(i) => presetdoc::FACTORY
                .get(i)
                .map(|(_, b)| b.to_vec())
                .ok_or_else(|| SessionError::Other("bad factory preset".into()))?,
            PresetSource::User(i) if i < 8 => {
                self.dev.read_preset(8 + i as u32).map_err(device_error)?
            }
            PresetSource::User(_) => return Err(SessionError::Other("bad user preset".into())),
        };
        ubjson::decode(&bytes).map_err(|e| {
            SessionError::Other(format!("that preset holds no preset document ({e})"))
        })?;
        if let Ok(old) = self.dev.read_button_slot(slot as u32) {
            if !old.is_empty() {
                self.backup(&format!("slot{}", slot + 1), &old)?;
            }
        }
        self.dev
            .write_button_slot(slot as u32, &bytes)
            .map_err(device_error)
    }

    fn delete_user(&mut self, n: usize) -> Result<(), SessionError> {
        if n >= 8 {
            return Err(SessionError::Other("bad user preset".into()));
        }
        let old = self.dev.read_preset(8 + n as u32).unwrap_or_default();
        if !old.is_empty() {
            self.backup(&format!("user{}", n + 1), &old)?;
        }
        let empty =
            ubjson::encode(&ubjson::Value::Object(Vec::new())).map_err(SessionError::Other)?;
        self.dev
            .write_user_preset(n as u32, &empty)
            .map_err(device_error)
    }

    fn save_user(&mut self, n: usize, name: &str) -> Result<(), SessionError> {
        if n >= 8 {
            return Err(SessionError::Other("bad user preset".into()));
        }
        let area = 8 + n as u32;
        let old = self.dev.read_preset(area).unwrap_or_default();
        // An empty area has no document to build on: start from a factory one.
        let base = match ubjson::decode(&old) {
            Ok(d) if presetdoc::name_of(&d).is_some() => d,
            _ => ubjson::decode(presetdoc::FACTORY[0].1).map_err(SessionError::Other)?,
        };
        if !old.is_empty() {
            self.backup(&format!("user{}", n + 1), &old)?;
        }
        let doc = presetdoc::to_doc(&self.fx, name, &base).map_err(SessionError::Other)?;
        let bytes = ubjson::encode(&doc).map_err(SessionError::Other)?;
        self.dev
            .write_user_preset(n as u32, &bytes)
            .map_err(device_error)
    }

    fn load_slot(&mut self, slot: usize) -> Result<(), SessionError> {
        if slot >= 4 {
            return Err(SessionError::Other("bad slot".into()));
        }
        let raw = self
            .dev
            .read_button_slot(slot as u32)
            .map_err(device_error)?;
        let doc = ubjson::decode(&raw).map_err(|e| {
            SessionError::Other(format!("slot {} holds no preset document ({e})", slot + 1))
        })?;
        let (engines, _) = presetdoc::apply(&doc, &mut self.fx);
        for e in engines {
            self.mark(e);
        }
        // Echo (Delay) crashes the firmware at 88.2/96 kHz and is refused when the rate is
        // unknown: do not load it blindly.
        if self.fx.get("voicefx.model") == dsp::ECHO_MODEL && !self.rate.is_some_and(|r| r < 88_200)
        {
            let _ = self.fx.set("voicefx.on", 0.0);
            let _ = self.fx.set("voicefx.model", 0.0);
            self.mark(Engine::VoiceFx);
            return Err(SessionError::Other(
                "the slot uses Echo, which needs a known sample rate below 88.2 kHz: Voice FX was left off".into(),
            ));
        }
        Ok(())
    }
}

enum Cmd {
    Connect,
    Set(String, f32),
    Recall(usize),
    LoadSlot(usize),
    RefreshPresets,
    AssignSlot(usize, PresetSource),
    SaveUser(usize, String),
    DeleteUser(usize),
    Quit,
}

/// State shared with the device thread.
struct Shared {
    state: UiState,
    /// Set by the thread when `state` changed; cleared by `snapshot`.
    dirty: bool,
    wake: Option<Wake>,
    hook: Option<StatusHook>,
}

/// Apply `f` (returns whether it changed anything); on change mark dirty, run the status
/// hook and wake the UI.
fn publish(shared: &Mutex<Shared>, f: impl FnOnce(&mut UiState) -> bool) {
    let wake = {
        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
        if !f(&mut g.state) {
            return;
        }
        g.dirty = true;
        if let Some(h) = &g.hook {
            h(&g.state);
        }
        g.wake.clone()
    };
    if let Some(w) = wake {
        w();
    }
}

fn set_if_ne<T: PartialEq>(slot: &mut T, v: T) -> bool {
    if *slot == v {
        false
    } else {
        *slot = v;
        true
    }
}

pub struct RealBackend {
    tx: Sender<Cmd>,
    shared: Arc<Mutex<Shared>>,
    handle: Option<JoinHandle<()>>,
}

pub struct Timing {
    pub retry: Duration,
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            // Each retry enumerates the USB bus, so do it sparingly while unplugged.
            retry: Duration::from_secs(2),
            // Meters ride on the same state poll: 10 Hz keeps them lively and the USB load low.
            poll: Duration::from_millis(100),
        }
    }
}

impl RealBackend {
    pub fn spawn(opener: Box<dyn Opener>, timing: Timing) -> Self {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared {
            state: UiState {
                connection: Connection::Connecting,
                connection_detail: "Looking for a Revelator Dynamic...".into(),
                ..UiState::default()
            },
            dirty: true,
            wake: None,
            hook: None,
        }));
        let sh = shared.clone();
        let handle = std::thread::Builder::new()
            .name("revelator-device".into())
            .spawn(move || device_thread(opener, timing, rx, sh))
            .expect("spawn device thread");
        RealBackend {
            tx,
            shared,
            handle: Some(handle),
        }
    }
}

impl Drop for RealBackend {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Backend for RealBackend {
    fn name(&self) -> &str {
        "Device"
    }
    fn connect(&mut self) {
        let _ = self.tx.send(Cmd::Connect);
    }
    fn snapshot(&mut self) -> Option<UiState> {
        let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if !g.dirty {
            return None;
        }
        g.dirty = false;
        let out = g.state.clone();
        g.state.last_error = None; // errors are delivered once
        g.state.notice = None;
        Some(out)
    }
    fn set_wake(&mut self, wake: Wake) {
        let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        g.wake = Some(wake);
        g.dirty = true; // deliver the current state to the new listener
    }
    fn remote(&self) -> Option<RemoteFn> {
        let tx = self.tx.clone();
        let shared = self.shared.clone();
        Some(Arc::new(move |a| {
            let _ = match a {
                Action::Slot(n) => tx.send(Cmd::Recall(n)),
                Action::ToggleMute => {
                    let muted = shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .state
                        .values
                        .get("mic.mute")
                        .is_some_and(|v| *v >= 0.5);
                    tx.send(Cmd::Set("mic.mute".into(), if muted { 0.0 } else { 1.0 }))
                }
            };
        }))
    }
    fn set_status_hook(&mut self, hook: StatusHook) {
        let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        hook(&g.state);
        g.hook = Some(hook);
    }
    fn set(&mut self, id: &str, v: f32) -> Result<(), String> {
        self.tx
            .send(Cmd::Set(id.to_string(), v))
            .map_err(|_| "device thread stopped".to_string())
    }
    fn recall_slot(&mut self, slot: usize) -> Result<(), String> {
        self.tx
            .send(Cmd::Recall(slot))
            .map_err(|_| "device thread stopped".to_string())
    }
    fn load_slot(&mut self, slot: usize) -> Result<(), String> {
        self.tx
            .send(Cmd::LoadSlot(slot))
            .map_err(|_| "device thread stopped".to_string())
    }
    fn refresh_mic_presets(&mut self) -> Result<(), String> {
        self.tx
            .send(Cmd::RefreshPresets)
            .map_err(|_| "device thread stopped".to_string())
    }
    fn assign_slot(&mut self, slot: usize, src: PresetSource) -> Result<(), String> {
        self.tx
            .send(Cmd::AssignSlot(slot, src))
            .map_err(|_| "device thread stopped".to_string())
    }
    fn delete_user_preset(&mut self, n: usize) -> Result<(), String> {
        self.tx
            .send(Cmd::DeleteUser(n))
            .map_err(|_| "device thread stopped".to_string())
    }
    fn save_user_preset(&mut self, n: usize, name: &str) -> Result<(), String> {
        self.tx
            .send(Cmd::SaveUser(n, name.to_string()))
            .map_err(|_| "device thread stopped".to_string())
    }
}

/// Run one UI command against the session.
fn run_cmd(s: &mut dyn Session, cmd: Cmd) -> Result<(), SessionError> {
    match cmd {
        Cmd::Set(id, v) => s.set(&id, v).map_err(|e| match e {
            SessionError::Other(m) => SessionError::Other(format!("{id}: {m}")),
            d => d,
        }),
        Cmd::Recall(n) => s.recall_slot(n),
        Cmd::LoadSlot(n) => s.load_slot(n),
        Cmd::AssignSlot(n, src) => s.assign_slot(n, src),
        Cmd::SaveUser(n, name) => s.save_user(n, &name),
        Cmd::DeleteUser(n) => s.delete_user(n),
        Cmd::RefreshPresets => Ok(()),
        Cmd::Connect | Cmd::Quit => Ok(()),
    }
}

/// Minimum time between two sends of queued edits. The first edit after a quiet period goes
/// out at once (responsive); edits arriving faster than this (a slider drag) are merged, and the
/// latest value always goes out when the interval has passed (smooth, little USB traffic).
const FLUSH_INTERVAL: Duration = Duration::from_millis(40);

/// Failed state reads in a row (without a disconnect report) before the session is reopened.
const MAX_READ_FAILURES: u32 = 5;

/// Queue a `Set`, replacing an earlier queued value for the same id (the latest wins).
fn queue_set(pending: &mut Vec<(String, f32)>, id: String, v: f32) {
    match pending.iter_mut().find(|(k, _)| *k == id) {
        Some(slot) => slot.1 = v,
        None => pending.push((id, v)),
    }
}

/// Send every queued edit, then the DSP / mixer blocks they changed (once each).
fn apply_pending(
    session: &mut Option<Box<dyn Session>>,
    pending: &mut Vec<(String, f32)>,
    errors: &mut Vec<SessionError>,
) {
    let Some(s) = session.as_mut() else {
        pending.clear();
        return;
    };
    for (id, v) in pending.drain(..) {
        if let Err(e) = run_cmd(s.as_mut(), Cmd::Set(id, v)) {
            errors.push(e);
        }
    }
    if let Err(e) = s.flush() {
        errors.push(e);
    }
}

fn device_thread(
    mut opener: Box<dyn Opener>,
    timing: Timing,
    rx: mpsc::Receiver<Cmd>,
    shared: Arc<Mutex<Shared>>,
) {
    let mut session: Option<Box<dyn Session>> = None;
    let mut last_try: Option<Instant> = None;
    let mut last_poll = Instant::now();
    // Active mic slot seen by the last poll of this connection.
    let mut last_slot: Option<usize> = None;
    // Edits waiting for the next send, and when the last send happened.
    let mut pending: Vec<(String, f32)> = Vec::new();
    let mut last_send: Option<Instant> = None;
    // Consecutive failed state reads that were not reported as a disconnect (timeouts, USB
    // errors after the mic reset): past `MAX_READ_FAILURES` the session is dropped and reopened.
    let mut read_failures = 0u32;
    loop {
        // Sleep until the next thing to do; commands wake us early.
        let mut wait = if session.is_some() {
            timing.poll.saturating_sub(last_poll.elapsed())
        } else {
            last_try.map_or(Duration::ZERO, |t| timing.retry.saturating_sub(t.elapsed()))
        };
        if !pending.is_empty() {
            wait = wait.min(last_send.map_or(Duration::ZERO, |t| {
                FLUSH_INTERVAL.saturating_sub(t.elapsed())
            }));
        }
        let mut force_open = false;
        let mut errors = Vec::new();
        let mut quit = false;
        match rx.recv_timeout(wait) {
            Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Cmd::Connect) => force_open = true,
            Ok(first) => {
                // Handle the whole burst of queued commands.
                let mut next = Some(first);
                while let Some(cmd) = next.take() {
                    match (&mut session, cmd) {
                        (_, Cmd::Quit) => quit = true,
                        (_, Cmd::Connect) => force_open = true,
                        (None, _) => errors.push(SessionError::Other("not connected".into())),
                        (Some(_), Cmd::Set(id, v)) => queue_set(&mut pending, id, v),
                        (Some(_), cmd) => {
                            // Keep slot/push commands ordered after the edits before them.
                            apply_pending(&mut session, &mut pending, &mut errors);
                            last_send = Some(Instant::now());
                            let done = match &cmd {
                                Cmd::LoadSlot(n) => {
                                    Some(format!("Loaded the settings of slot {}", n + 1))
                                }
                                Cmd::AssignSlot(n, _) => {
                                    Some(format!("Slot {} now holds the chosen preset", n + 1))
                                }
                                Cmd::DeleteUser(n) => {
                                    Some(format!("Emptied user preset {}", n + 1))
                                }
                                Cmd::SaveUser(n, _) => Some(format!(
                                    "Stored the current settings in user preset {}",
                                    n + 1
                                )),
                                _ => None,
                            };
                            let refresh = matches!(
                                cmd,
                                Cmd::RefreshPresets
                                    | Cmd::AssignSlot(..)
                                    | Cmd::SaveUser(..)
                                    | Cmd::DeleteUser(..)
                            );
                            if let Some(s) = session.as_mut() {
                                match run_cmd(s.as_mut(), cmd).and_then(|_| s.flush()) {
                                    Ok(()) => {
                                        if refresh {
                                            if let Ok(p) = s.mic_presets() {
                                                publish(&shared, |st| {
                                                    st.mic_presets = Some(p);
                                                    true
                                                });
                                            }
                                        }
                                        if let Some(m) = done {
                                            publish(&shared, |st| {
                                                st.notice = Some(m);
                                                true
                                            });
                                        }
                                    }
                                    Err(e) => errors.push(e),
                                }
                            }
                        }
                    }
                    next = match rx.try_recv() {
                        Ok(c) => Some(c),
                        Err(TryRecvError::Empty) => None,
                        Err(TryRecvError::Disconnected) => {
                            quit = true;
                            None
                        }
                    };
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if !pending.is_empty() && last_send.is_none_or(|t| t.elapsed() >= FLUSH_INTERVAL) {
            apply_pending(&mut session, &mut pending, &mut errors);
            last_send = Some(Instant::now());
        }
        for e in errors {
            match e {
                SessionError::Disconnected => session = None,
                SessionError::Other(m) => publish(&shared, |s| {
                    s.last_error = Some(m);
                    true
                }),
            }
        }
        if session.is_none() {
            pending.clear();
        }
        if quit {
            return;
        }

        if session.is_none() {
            if force_open || last_try.is_none_or(|t| t.elapsed() >= timing.retry) {
                last_try = Some(Instant::now());
                match opener.open() {
                    Ok(s) => {
                        let info = s.info();
                        publish(&shared, |st| {
                            st.connection = Connection::Connected;
                            st.connection_detail.clear();
                            st.info = info;
                            true
                        });
                        session = Some(s);
                        read_failures = 0;
                        // Send the saved settings: the mic restarts with its own defaults.
                        if let Some(s) = session.as_mut() {
                            match s.on_connect() {
                                Ok(()) => {}
                                Err(SessionError::Disconnected) => session = None,
                                Err(SessionError::Other(m)) => publish(&shared, |st| {
                                    st.last_error = Some(format!("restoring saved settings: {m}"));
                                    true
                                }),
                            }
                        }
                        if session.is_none() {
                            continue;
                        }
                        // The slot names, for the taskbar title even with the window hidden.
                        if let Some(Ok(p)) = session.as_mut().map(|s| s.mic_presets()) {
                            publish(&shared, |st| set_if_ne(&mut st.mic_presets, Some(p)));
                        }
                        last_slot = None;
                        last_poll = Instant::now() - timing.poll;
                    }
                    Err(reason) => publish(&shared, |st| {
                        set_if_ne(&mut st.connection, Connection::Disconnected)
                            | set_if_ne(&mut st.connection_detail, reason)
                    }),
                }
            }
            continue;
        }

        if last_poll.elapsed() >= timing.poll {
            last_poll = Instant::now();
            let mut lost_contact = false;
            if let Some(s) = session.as_mut() {
                match s.read() {
                    Ok(r) => {
                        read_failures = 0;
                        // The mic only stores the active slot; the sound is sent from the PC.
                        // So when the slot changes (button or app), load that slot's settings.
                        // Done here, not in the UI, so it works with the window hidden.
                        let slot = r
                            .values
                            .get("mic.slot")
                            .map(|v| (v.round().max(0.0) as usize).min(3));
                        publish(&shared, |st| {
                            set_if_ne(&mut st.values, r.values)
                                | set_if_ne(&mut st.meters, r.meters)
                        });
                        if let Some(n) = slot {
                            if last_slot.is_some_and(|p| p != n) {
                                match run_cmd(s.as_mut(), Cmd::LoadSlot(n)).and_then(|_| s.flush())
                                {
                                    Ok(()) => publish(&shared, |st| {
                                        st.notice =
                                            Some(format!("Loaded the settings of slot {}", n + 1));
                                        true
                                    }),
                                    Err(SessionError::Disconnected) => session = None,
                                    Err(SessionError::Other(m)) => publish(&shared, |st| {
                                        st.last_error = Some(format!("load slot {}: {m}", n + 1));
                                        true
                                    }),
                                }
                            }
                            last_slot = Some(n);
                        }
                    }
                    Err(SessionError::Disconnected) => session = None,
                    Err(SessionError::Other(m)) => {
                        read_failures += 1;
                        if read_failures >= MAX_READ_FAILURES {
                            // The mic stopped answering without a clean unplug: start over.
                            session = None;
                            lost_contact = true;
                        }
                        publish(&shared, |st| {
                            st.last_error = Some(m);
                            true
                        })
                    }
                }
            }
            if session.is_none() {
                let detail = if lost_contact {
                    "Lost contact with the mic. Reconnecting..."
                } else {
                    "Device disconnected. Plug in your Revelator Dynamic."
                };
                publish(&shared, |st| {
                    st.connection = Connection::Disconnected;
                    st.connection_detail = detail.into();
                    true
                });
                // Not reused by the next failure.
                read_failures = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakeSession {
        vals: HashMap<ParamId, f32>,
        unplugged: Arc<AtomicBool>,
    }
    impl Session for FakeSession {
        fn info(&self) -> DeviceInfo {
            DeviceInfo {
                name: "fake".into(),
                firmware: "1".into(),
                protocol: "2".into(),
            }
        }
        fn read(&mut self) -> Result<Reading, SessionError> {
            if self.unplugged.load(Ordering::SeqCst) {
                return Err(SessionError::Disconnected);
            }
            Ok(Reading {
                values: self.vals.clone(),
                meters: HashMap::new(),
            })
        }
        fn set(&mut self, id: &str, v: f32) -> Result<(), SessionError> {
            if id == "bad" {
                return Err(SessionError::Other("rejected".into()));
            }
            self.vals.insert(id.into(), v);
            Ok(())
        }
        fn recall_slot(&mut self, _: usize) -> Result<(), SessionError> {
            Ok(())
        }
    }

    struct FakeOpener {
        fail_first: usize,
        tries: Arc<AtomicUsize>,
        unplugged: Arc<AtomicBool>,
    }
    impl Opener for FakeOpener {
        fn open(&mut self) -> Result<Box<dyn Session>, String> {
            let n = self.tries.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_first || self.unplugged.load(Ordering::SeqCst) {
                return Err("plug in your Revelator Dynamic".into());
            }
            Ok(Box::new(FakeSession {
                vals: HashMap::new(),
                unplugged: self.unplugged.clone(),
            }))
        }
    }

    fn wait_for(b: &mut RealBackend, f: impl Fn(&UiState) -> bool) -> UiState {
        let end = Instant::now() + Duration::from_secs(5);
        let mut last = UiState::default();
        loop {
            if let Some(s) = b.snapshot() {
                last = s;
                if f(&last) {
                    return last;
                }
            }
            assert!(Instant::now() < end, "timed out; last state {last:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn fast() -> Timing {
        Timing {
            retry: Duration::from_millis(20),
            poll: Duration::from_millis(10),
        }
    }

    #[test]
    fn connects_after_retries_set_and_reconnects() {
        let tries = Arc::new(AtomicUsize::new(0));
        let unplugged = Arc::new(AtomicBool::new(false));
        let mut b = RealBackend::spawn(
            Box::new(FakeOpener {
                fail_first: 2,
                tries: tries.clone(),
                unplugged: unplugged.clone(),
            }),
            fast(),
        );
        let s = wait_for(&mut b, |s| {
            s.connection == Connection::Disconnected && !s.connection_detail.is_empty()
        });
        assert!(s.connection_detail.contains("plug in"));
        wait_for(&mut b, |s| s.connection == Connection::Connected);
        assert!(tries.load(Ordering::SeqCst) >= 3);

        b.set("mic.gain", 12.0).unwrap();
        wait_for(&mut b, |s| s.values.get("mic.gain") == Some(&12.0));

        b.set("bad", 1.0).unwrap();
        let s = wait_for(&mut b, |s| s.last_error.is_some());
        assert!(s.last_error.unwrap().contains("rejected"));

        // Unplug: UI shows disconnected, then replug reconnects.
        unplugged.store(true, Ordering::SeqCst);
        wait_for(&mut b, |s| s.connection == Connection::Disconnected);
        unplugged.store(false, Ordering::SeqCst);
        wait_for(&mut b, |s| s.connection == Connection::Connected);
    }

    #[test]
    fn snapshot_is_none_until_something_changes_and_wakes_ui() {
        let woken = Arc::new(AtomicUsize::new(0));
        let w = woken.clone();
        let mut b = RealBackend::spawn(
            Box::new(FakeOpener {
                fail_first: 0,
                tries: Arc::new(AtomicUsize::new(0)),
                unplugged: Arc::new(AtomicBool::new(false)),
            }),
            fast(),
        );
        b.set_wake(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        wait_for(&mut b, |s| s.connection == Connection::Connected);
        std::thread::sleep(Duration::from_millis(100)); // many polls, no change
        let _ = b.snapshot();
        let before = woken.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert!(b.snapshot().is_none());
        assert_eq!(woken.load(Ordering::SeqCst), before);
        b.set("mic.gain", 7.0).unwrap();
        wait_for(&mut b, |s| s.values.get("mic.gain") == Some(&7.0));
        assert!(woken.load(Ordering::SeqCst) > before);
    }

    #[test]
    fn queue_set_keeps_the_latest_value_per_id() {
        let mut q = Vec::new();
        queue_set(&mut q, "a".into(), 1.0);
        queue_set(&mut q, "b".into(), 2.0);
        queue_set(&mut q, "a".into(), 3.0);
        assert_eq!(q, [("a".to_string(), 3.0), ("b".to_string(), 2.0)]);
    }

    #[test]
    fn fast_edits_are_merged_but_the_last_value_always_arrives() {
        let mut b = RealBackend::spawn(
            Box::new(FakeOpener {
                fail_first: 0,
                tries: Arc::new(AtomicUsize::new(0)),
                unplugged: Arc::new(AtomicBool::new(false)),
            }),
            fast(),
        );
        wait_for(&mut b, |s| s.connection == Connection::Connected);
        let t0 = Instant::now();
        for i in 0..100 {
            b.set("mic.gain", i as f32).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        wait_for(&mut b, |s| s.values.get("mic.gain") == Some(&99.0));
        // 100 edits over ~100+ ms: far fewer than 100 sends reached the device
        assert!(t0.elapsed() >= Duration::from_millis(100));
    }

    #[test]
    fn hw_session_over_mock_roundtrips() {
        use revelator_core::mock::MockTransport;
        let mut s = HwSession::open_with_state(MockTransport::new(), "mock".into(), None).unwrap();
        assert_eq!(s.info().protocol, "v1");
        let r = s.read().unwrap();
        assert_eq!(r.values["mic.gain"], 22.0);
        assert_eq!(r.values["mic.mute"], 0.0);
        assert!((r.values["mic.hp_volume"] - 70.0).abs() < 1e-3);
        assert_eq!(r.values["mixer.mic.main"], 0.0);
        assert_eq!(r.values["mixer.playback.mixa"], -96.0);
        // button modes go out as Appl x=17 / x=8
        s.set("mic.preset_button_mode", 1.0).unwrap();
        s.set("mic.mute_button_mode", 0.0).unwrap();
        s.set("mic.gain", 40.0).unwrap();
        s.set("dsp.gate.attack", 5.0).unwrap();
        s.set("mic.hp_volume", 50.0).unwrap();
        let r = s.read().unwrap();
        assert_eq!(r.values["mic.gain"], 40.0);
        assert!((r.values["dsp.gate.attack"] - 5.0).abs() < 1e-3);
        assert!((r.values["mic.hp_volume"] - 50.0).abs() < 1e-3);
        assert!(s.set("rev.type", 1.0).is_err());
        s.flush().unwrap();
        s.recall_slot(2).unwrap();
        assert_eq!(s.read().unwrap().values["mic.slot"], 2.0);
    }

    #[test]
    fn delay_blocked_until_rate_known_and_at_high_rates() {
        use revelator_core::mock::MockTransport;
        let mut s = HwSession::open_with_state(MockTransport::new(), "mock".into(), None).unwrap();
        s.rate_probe = Box::new(|| None);
        assert!(s.set(ECHO_ID, 5.0).is_err()); // rate unknown
        s.set("sys.samplerate", 96000.0).unwrap();
        assert!(s.set(ECHO_ID, 5.0).is_err());
        s.set("sys.samplerate", 48000.0).unwrap();
        s.set(ECHO_ID, 5.0).unwrap();
        s.flush().unwrap();
        assert_eq!(s.read().unwrap().values["sys.samplerate"], 48000.0);
    }

    #[test]
    fn windows_rate_wins_over_manual_choice() {
        use revelator_core::mock::MockTransport;
        let mut s = HwSession::open_with_state(MockTransport::new(), "mock".into(), None).unwrap();
        let rate = Arc::new(Mutex::new(Some(96_000u32)));
        let r2 = rate.clone();
        s.rate_probe = Box::new(move || *r2.lock().unwrap());
        let v = s.read().unwrap().values;
        assert_eq!(v["sys.samplerate"], 96_000.0);
        assert_eq!(v[RATE_FROM_WINDOWS_ID], 1.0);
        assert!(s.set("sys.samplerate", 48_000.0).is_err());
        assert!(s.set(ECHO_ID, 5.0).is_err()); // 96 kHz: echo blocked
        *rate.lock().unwrap() = None;
        let v = s.read().unwrap().values;
        assert!(!v.contains_key("sys.samplerate") && !v.contains_key(RATE_FROM_WINDOWS_ID));
        *rate.lock().unwrap() = Some(48_000);
        s.read().unwrap();
        s.set(ECHO_ID, 5.0).unwrap();
        s.flush().unwrap();
        // Windows switches to 96 kHz while Echo runs: Echo is moved off and sent at once.
        *rate.lock().unwrap() = Some(96_000);
        let before = s.dev.transport_mut().sent.len();
        assert!(s.read().is_err()); // reported once
        assert!(s.dev.transport_mut().sent.len() > before);
        assert_eq!(s.fx.get("voicefx.model"), 0.0);
        assert_eq!(s.fx.get("voicefx.on"), 0.0);
        let v = s.read().unwrap().values;
        assert_eq!(v[ECHO_ID], 0.0);
    }

    #[test]
    fn vofx_only_on_model_change() {
        use revelator_core::frame::{decode_frame, tag};
        use revelator_core::mock::MockTransport;
        use revelator_core::safety::Command;
        let mut s = HwSession::open_with_state(MockTransport::new(), "mock".into(), None).unwrap();
        let vofx = |s: &mut HwSession<MockTransport>, from: usize| -> usize {
            s.dev.transport_mut().sent[from..]
                .iter()
                .map(|f| Command::parse(decode_frame(f).unwrap().payload).unwrap())
                .filter(|c| c.blob_tag == Some(tag::VOFX))
                .count()
        };
        let id = |k: &str| format!("{}{k}", dsp::PREFIX);
        let n0 = s.dev.transport_mut().sent.len();
        s.set(&id("voicefx.model"), 4.0).unwrap();
        s.set(&id("voicefx.on"), 1.0).unwrap();
        s.flush().unwrap();
        assert_eq!(vofx(&mut s, n0), 1);
        // a setting change of the same model: the blob only, no engine rebuild
        let n1 = s.dev.transport_mut().sent.len();
        s.set(&id("voicefx.robot.pitch"), 0.8).unwrap();
        s.flush().unwrap();
        assert_eq!(vofx(&mut s, n1), 0);
        assert!(s.dev.transport_mut().sent.len() > n1);
        // a model change selects again
        let n2 = s.dev.transport_mut().sent.len();
        s.set(&id("voicefx.model"), 1.0).unwrap();
        s.flush().unwrap();
        assert_eq!(vofx(&mut s, n2), 1);
    }

    #[test]
    fn mixer_sends_crosspoints() {
        use revelator_core::frame::{decode_frame, tag};
        use revelator_core::mock::MockTransport;
        use revelator_core::safety::Command;
        let mut s = HwSession::open_with_state(MockTransport::new(), "mock".into(), None).unwrap();
        let cmds = |s: &mut HwSession<MockTransport>, from: usize| -> Vec<Command> {
            s.dev.transport_mut().sent[from..]
                .iter()
                .map(|f| Command::parse(decode_frame(f).unwrap().payload).unwrap())
                .collect()
        };
        // first mixer change: the whole mixer (15 crosspoints + 3 mono + the reverb send) plus the
        // mic channel parameters UC pushes with it (processing channel, DSP amount, pan, DAW post-DSP)
        let n0 = s.dev.transport_mut().sent.len();
        s.set("mixer.playback.main", -6.0).unwrap();
        s.flush().unwrap();
        let c = cmds(&mut s, n0);
        assert_eq!(c.len(), 24);
        assert!(c.iter().all(|c| c.is_allowed()));
        // never the output delay (block 0 x = 8 / 9), which would tear Voice FX down
        assert!(!c
            .iter()
            .any(|c| c.block == mixer::FX_BLOCK && matches!(c.param_id, Some(4 | 8 | 9))));
        assert_eq!(c.iter().filter(|c| c.block == mixer::BLOCK).count(), 18);
        let pb = c
            .iter()
            .find(|c| c.index == 0 && c.param_id == Some(0) && c.blob_tag == Some(tag::PARA))
            .unwrap();
        assert!((f32::from_bits(pb.value_bits.unwrap()) + 6.0).abs() < 1e-3);
        // later changes: only what changed
        let n1 = s.dev.transport_mut().sent.len();
        s.set("mixer.out.mixa.mute", 1.0).unwrap();
        s.flush().unwrap();
        let c = cmds(&mut s, n1);
        assert_eq!(c.len(), 5); // the five sources into Mix A
        assert!(c
            .iter()
            .all(|c| c.index == 1 && f32::from_bits(c.value_bits.unwrap()) == mixer::SILENT_DB));
        // monitor blend scales the main bus: mic only -> returns silent on Main
        let n2 = s.dev.transport_mut().sent.len();
        s.set("mic.monitor_blend", 0.0).unwrap(); // 0 % = -1 = mic only
        s.flush().unwrap();
        let c: Vec<_> = cmds(&mut s, n2)
            .into_iter()
            .filter(|c| c.block == mixer::BLOCK)
            .collect();
        assert!(c
            .iter()
            .any(|c| c.param_id == Some(0)
                && f32::from_bits(c.value_bits.unwrap()) == mixer::SILENT_DB));
        assert!(c.iter().all(|c| c.index == 0));
        assert_eq!(s.read().unwrap().values["mixer.out.mixa.mute"], 1.0);
        // the mic's reverb send: one SetP 0 0 write
        let n3 = s.dev.transport_mut().sent.len();
        s.set("mixer.mic.reverb", -12.0).unwrap();
        s.flush().unwrap();
        let c = cmds(&mut s, n3);
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].block, c[0].index, c[0].param_id),
            (mixer::FX_BLOCK, 0, Some(1))
        );
        assert!(c[0].is_allowed());
        assert!((f32::from_bits(c[0].value_bits.unwrap()) + 12.0).abs() < 1e-3);
        // the reverb return strip
        let n4 = s.dev.transport_mut().sent.len();
        s.set("mixer.rev.main", 0.0).unwrap();
        s.flush().unwrap();
        let c = cmds(&mut s, n4);
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].block, c[0].index, c[0].param_id),
            (mixer::BLOCK, 0, Some(5))
        );
        let r = s.read().unwrap();
        assert_eq!(r.values["mixer.mic.reverb"], -12.0);
        assert_eq!(r.values["mixer.rev.main"], 0.0);
    }

    #[test]
    fn dsp_edits_are_coalesced_into_one_block_per_engine() {
        use revelator_core::mock::MockTransport;
        let mut s = HwSession::open_with_state(MockTransport::new(), "mock".into(), None).unwrap();
        let before = s.dev.transport_mut().sent.len();
        for v in [10.0, 20.0, 30.0] {
            s.set("dsp.comp.threshold", -v).unwrap();
        }
        s.set("dsp.limiter.on", 1.0).unwrap();
        assert_eq!(s.dev.transport_mut().sent.len(), before); // nothing sent before flush
        s.flush().unwrap();
        let comp = s.fx.writes(Engine::Comp, 48_000.0).len();
        let lim = s.fx.writes(Engine::Limiter, 48_000.0).len();
        assert_eq!(s.dev.transport_mut().sent.len(), before + comp + lim);
        assert_eq!(s.read().unwrap().values["dsp.comp.threshold"], -30.0);
        let after_read = s.dev.transport_mut().sent.len();
        s.flush().unwrap(); // nothing left
        assert_eq!(s.dev.transport_mut().sent.len(), after_read);
    }

    #[test]
    fn disconnect_is_reported() {
        use revelator_core::mock::MockTransport;
        let mut s = HwSession::open_with_state(MockTransport::new(), "mock".into(), None).unwrap();
        s.dev.transport_mut().disconnected = true;
        assert!(matches!(s.read(), Err(SessionError::Disconnected)));
    }

    /// First session answers every read with a non-disconnect error (mic hung / reset); later
    /// ones are healthy. Counts `on_connect` calls.
    struct HungSession {
        hung: bool,
        connects: Arc<AtomicUsize>,
    }
    impl Session for HungSession {
        fn info(&self) -> DeviceInfo {
            DeviceInfo {
                name: "hung".into(),
                firmware: "1".into(),
                protocol: "2".into(),
            }
        }
        fn read(&mut self) -> Result<Reading, SessionError> {
            if self.hung {
                return Err(SessionError::Other("timeout".into()));
            }
            Ok(Reading {
                values: HashMap::new(),
                meters: HashMap::new(),
            })
        }
        fn set(&mut self, _: &str, _: f32) -> Result<(), SessionError> {
            Ok(())
        }
        fn recall_slot(&mut self, _: usize) -> Result<(), SessionError> {
            Ok(())
        }
        fn on_connect(&mut self) -> Result<(), SessionError> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    struct HungOpener {
        opens: usize,
        connects: Arc<AtomicUsize>,
    }
    impl Opener for HungOpener {
        fn open(&mut self) -> Result<Box<dyn Session>, String> {
            self.opens += 1;
            Ok(Box::new(HungSession {
                hung: self.opens == 1,
                connects: self.connects.clone(),
            }))
        }
    }

    #[test]
    fn silent_failures_reopen_the_session_and_resend_saved_settings() {
        let connects = Arc::new(AtomicUsize::new(0));
        let mut b = RealBackend::spawn(
            Box::new(HungOpener {
                opens: 0,
                connects: connects.clone(),
            }),
            fast(),
        );
        let end = Instant::now() + Duration::from_secs(5);
        while connects.load(Ordering::SeqCst) < 2 {
            let _ = b.snapshot();
            assert!(Instant::now() < end, "never reconnected");
            std::thread::sleep(Duration::from_millis(10));
        }
        wait_for(&mut b, |s| s.connection == Connection::Connected);
    }

    #[test]
    fn saved_state_is_pushed_on_connect() {
        use revelator_core::mock::MockTransport;
        let dir = std::env::temp_dir().join(format!("revelator-push-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fx.json");
        // Nothing saved yet: the mic keeps its own settings.
        let mut s =
            HwSession::open_with_state(MockTransport::new(), "mock".into(), Some(path.clone()))
                .unwrap();
        s.rate_probe = Box::new(|| None);
        let n0 = s.dev.transport_mut().sent.len();
        s.on_connect().unwrap();
        assert_eq!(s.dev.transport_mut().sent.len(), n0);
        // A saved profile: every engine goes out once.
        fx::save_state(&path, &s.fx).unwrap();
        s.on_connect().unwrap();
        let expected: usize = dsp::ENGINES
            .into_iter()
            .map(|e| s.fx.writes(e, DEFAULT_FS).len())
            .sum();
        assert!(s.dev.transport_mut().sent.len() >= n0 + expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_errors_are_friendly() {
        assert_eq!(
            open_error_message(&OpenError::NoDevice),
            "Plug in your Revelator Dynamic"
        );
        assert!(open_error_message(&OpenError::NoVendorInterface)
            .contains("No usable control interface"));
        let m = open_error_message(&OpenError::InterfaceNotRegistered);
        assert!(
            m.contains("revelator-cli setup --yes")
                && m.contains("elevated")
                && !m.contains("Zadig")
        );
        let m = open_error_message(&OpenError::InterfaceBusy("x".into()));
        assert!(m.contains("Universal Control") && m.contains("PreSonus Hardware Access Service"));
        assert!(open_error_message(&OpenError::Usb("open failed: x".into())).contains("USB error"));
        assert!(open_error_message(&OpenError::UnsupportedMode("X".into()))
            .contains("unsupported mode"));
        assert!(open_error_message(&OpenError::UnsupportedModel("X".into()))
            .contains("Revelator Dynamic"));
    }

    #[test]
    fn hw_opener_reports_reason() {
        let mut b = RealBackend::spawn(Box::new(HwOpener), fast());
        let s = wait_for(&mut b, |s| s.connection == Connection::Disconnected);
        assert!(!s.connection_detail.is_empty());
    }
}

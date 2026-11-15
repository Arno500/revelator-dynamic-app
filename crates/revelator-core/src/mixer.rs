//! The Revelator Dynamic's stream mixer, as Universal Control drives it (from
//! `hwaccess/dspusbdevice.dll`; see `docs/uc-dll-notes.md`, "Mixer").
//!
//! The device has no per-channel fader/mute/solo/send parameters. It has a gain matrix: one
//! crosspoint per (source, output bus), each a dB value. UC keeps the channel settings itself,
//! combines them into each crosspoint gain (0x180057840) and sends
//! `SetP 100 <bus> [Para, 0x14, y = 0, x = <source>, gain dB]` (float sender 0x180054920).
//!
//! - Buses (`main` / `aux` channel objects, 0x180044630): block 100, index 0 = Main,
//!   1 = Stream Mix A, 2 = Stream Mix B. Bus `mono` (param 300) is `Pari` at x = 8.
//! - Sources (channel number at object +0x128, 0x18003e510): `return` ch1..3 (Playback,
//!   Virtual A, Virtual B) = 0, 1, 2; `line` ch1 (the mic) = 3; `fxreturn` (Reverb) = 5.
//! - Main bus (0x180031ea0): source mute, solo, fader and the monitor-blend factor (mic and
//!   returns only), plus the bus fader and mute. Mix A/B: the source's `assign_auxN` and `auxN`
//!   send level, plus the bus fader and mute (source mute/solo do not apply there).
//! - Monitor blend b (-1..1, 0x18003784d): b < 0 scales the returns by 1 + b; b >= 0 scales
//!   the mic by 1 - b. So -1 = mic only, +1 = playback only.
//!
//! - Mic -> reverb send (`FXA`, param 145, 0x180032540): one more crosspoint, address
//!   `{block 0, index 0, x = 1, y = line 0}` (a literal; the write queue 0x180054d00 /
//!   0x180055280 only coalesces by address), so `SetP 0 0 [Para, 0x14, 0, 1, gain dB]`. Gain
//!   inputs: the mic's `mute` (silences), `volume` (added in dB), `FXA` (the fader) and the blend.
//!
//! The device cannot report these values; like the DSP state they live in a small state file.

use crate::frame::{self, tag, ParamValue, PARAM_BLOB_SIZE};

/// SetP block of the mixer buses.
pub const BLOCK: u32 = 100;
/// SetP block / index / `x` of the mic -> reverb send crosspoint.
pub const FX_BLOCK: u32 = 0;
pub const FX_INDEX: u32 = 0;
pub const FX_X: u32 = 1;
/// `x` of the mic channel's pan (`pan`, param 143) and DAW post-DSP switch (`dawpostdsp`, 146) on
/// the same `SetP 0 0` address (UC bindings at 0x180041839 / 0x180041899: block 0, y = channel).
pub const PAN_X: u32 = 2;
pub const DAW_POST_X: u32 = 3;
// Not written by us (UC never writes them on `SetP 0 0` either; the firmware drives them from
// its own state): `Para` x = 4 is the per-channel DSP amount (from host `Appl` x = 4), and
// `Pari` x = 8 / `Para` x = 9 are the OUTPUT DELAY bus / time (firmware states 23/24, from host
// `Appl` Pari x = 13 / Para x = 14). Setting x = 8 to a bus (>= 0) switches the delay on, which
// shares memory with Voice FX and tears it down. `fx_write_ok` refuses all three.
/// `x` of the bus mono switch.
pub const MONO_X: u32 = 8;
/// Gain UC sends for a silenced crosspoint.
pub const SILENT_DB: f32 = -145.0;
/// Lowest gain UC sends for an open crosspoint.
pub const FLOOR_DB: f32 = -144.0;
/// Fader range (UC `volume` / `aux1` / `aux2`).
pub const FADER_MIN: f32 = -96.0;
pub const FADER_MAX: f32 = 10.0;
/// Largest crosspoint gain: source fader + bus fader at their maximum.
pub const GAIN_MAX: f32 = 2.0 * FADER_MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Mic,
    Playback,
    VirtualA,
    VirtualB,
    Reverb,
}

impl Source {
    pub const ALL: [Source; 5] = [
        Source::Mic,
        Source::Playback,
        Source::VirtualA,
        Source::VirtualB,
        Source::Reverb,
    ];
    /// Key prefix in the state file and CLI.
    pub fn key(self) -> &'static str {
        match self {
            Source::Mic => "mic",
            Source::Playback => "playback",
            Source::VirtualA => "virtuala",
            Source::VirtualB => "virtualb",
            Source::Reverb => "reverb",
        }
    }
    /// Crosspoint `x`: the source's channel number in UC.
    pub fn x(self) -> u32 {
        match self {
            Source::Playback => 0,
            Source::VirtualA => 1,
            Source::VirtualB => 2,
            Source::Mic => 3,
            Source::Reverb => 5,
        }
    }
    /// The monitor blend scales this source on the main bus (UC channel kinds 0 and 6).
    fn blended(self) -> bool {
        self != Source::Reverb
    }
}

/// Every crosspoint `x` the mixer sends.
pub const SOURCE_XS: [u32; 5] = [0, 1, 2, 3, 5];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    Main,
    MixA,
    MixB,
}

impl Bus {
    pub const ALL: [Bus; 3] = [Bus::Main, Bus::MixA, Bus::MixB];
    pub fn key(self) -> &'static str {
        match self {
            Bus::Main => "main",
            Bus::MixA => "mixa",
            Bus::MixB => "mixb",
        }
    }
    /// SetP block index.
    pub fn index(self) -> u32 {
        self as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceState {
    pub volume: f32,
    pub mute: bool,
    pub solo: bool,
    /// `assign_aux1` / `assign_aux2`.
    pub assign: [bool; 2],
    /// `aux1` / `aux2` send levels (dB).
    pub aux: [f32; 2],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BusState {
    pub volume: f32,
    pub mute: bool,
    pub mono: bool,
}

/// Mixer settings; `Default` is UC's "Default Mix" scene for the Revelator Dynamic.
#[derive(Debug, Clone, PartialEq)]
pub struct MixerState {
    pub sources: [SourceState; 5],
    pub buses: [BusState; 3],
    /// Monitor blend -1 (mic only) .. 1 (playback only); mirrors the device's monitorBlend.
    pub blend: f32,
    /// Mic -> reverb send level (UC `FXA`, "Reverb Level"), dB.
    pub reverb_send: f32,
    /// Mic pan 0..1 (UC `pan`, default 0.5), `SetP 0 0` x = 2.
    pub pan: f32,
    /// "DAW Send Post DSP" (UC `dawpostdsp`, default on), `SetP 0 0` x = 3.
    pub daw_post_dsp: bool,
    /// "DSP Amount" 0..1 (UC `dspAmount`, default 1) and "Bypass" (UC `bypassDSP`); UC sends
    /// `Appl` x = 4 = amount, or 0 while bypassed.
    pub dsp_amount: f32,
    pub bypass: bool,
    /// "Assigned Processing Channel" (UC `processingChannel`, -1..1), `Appl` `Pari` x = 12.
    pub fx_channel: f32,
    /// Output delay bus (UC `outputDelayBus` - 1), `Appl` `Pari` x = 13; -1 = off (default). Any
    /// bus switches the output delay on, which disables Voice FX.
    pub output_delay_bus: f32,
}

impl Default for MixerState {
    fn default() -> Self {
        let src = |volume, aux| SourceState {
            volume,
            mute: false,
            solo: false,
            assign: [true, true],
            aux: [aux, aux],
        };
        let bus = BusState {
            volume: 0.0,
            mute: false,
            mono: false,
        };
        MixerState {
            sources: [
                src(0.0, 0.0),             // Microphone: aux1/aux2 0 dB
                src(0.0, FADER_MIN),       // Playback
                src(0.0, FADER_MIN),       // Virtual A
                src(0.0, FADER_MIN),       // Virtual B
                src(FADER_MIN, FADER_MIN), // Reverb return
            ],
            buses: [bus; 3],
            blend: 0.0,
            reverb_send: FADER_MIN,
            pan: 0.5,
            daw_post_dsp: true,
            dsp_amount: 1.0,
            bypass: false,
            fx_channel: 0.0,
            output_delay_bus: -1.0,
        }
    }
}

/// One write to the device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixWrite {
    pub block: u32,
    pub index: u32,
    pub x: u32,
    pub value: ParamValue,
}

impl MixWrite {
    /// The frame for this write (`y` is always 0).
    pub fn frame(&self, uid: u8) -> Vec<u8> {
        frame::set_scalar(uid, self.block, self.index, self.x, 0, self.value)
    }
    /// Short name of the target: the bus key, or `reverb-send`.
    pub fn label(&self) -> &'static str {
        if self.block == FX_BLOCK {
            match self.x {
                PAN_X => "mic-pan",
                DAW_POST_X => "daw-post-dsp",
                _ => "reverb-send",
            }
        } else if self.block == tag::APPL {
            "channel"
        } else {
            Bus::ALL
                .get(self.index as usize)
                .map(|b| b.key())
                .unwrap_or("?")
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum MixError {
    Unknown(String),
    Range { key: String, min: f32, max: f32 },
}

impl std::fmt::Display for MixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MixError::Unknown(k) => write!(f, "unknown mixer key '{k}' (see `mixer list`)"),
            MixError::Range { key, min, max } => write!(f, "{key} must be in [{min}, {max}]"),
        }
    }
}

fn flag(v: f32) -> Result<bool, ()> {
    match v {
        0.0 => Ok(false),
        1.0 => Ok(true),
        _ => Err(()),
    }
}

impl MixerState {
    fn src(&self, s: Source) -> &SourceState {
        &self.sources[Source::ALL.iter().position(|&x| x == s).unwrap()]
    }

    fn solo_any(&self) -> bool {
        self.sources.iter().any(|s| s.solo)
    }

    /// Blend factor UC applies to `s` on the main bus.
    fn blend_factor(&self, s: Source) -> f32 {
        if !s.blended() {
            return 1.0;
        }
        let b = self.blend.clamp(-1.0, 1.0);
        match (s, b < 0.0) {
            (Source::Mic, true) => 1.0,
            (Source::Mic, false) => 1.0 - b,
            (_, true) => 1.0 + b,
            (_, false) => 1.0,
        }
    }

    /// Crosspoint gain (dB) from `s` into `bus`, computed like UC (0x1800576a0 / 0x180057840).
    pub fn gain(&self, s: Source, bus: Bus) -> f32 {
        let src = self.src(s);
        let out = &self.buses[bus.index() as usize];
        let factor = if bus == Bus::Main {
            self.blend_factor(s)
        } else {
            1.0
        };
        let silent = out.mute
            || factor == 0.0
            || match bus {
                Bus::Main => src.mute || (self.solo_any() && !src.solo),
                Bus::MixA => !src.assign[0],
                Bus::MixB => !src.assign[1],
            };
        if silent {
            return SILENT_DB;
        }
        let fader = match bus {
            Bus::Main => src.volume,
            Bus::MixA => src.aux[0],
            Bus::MixB => src.aux[1],
        };
        combine(fader, factor, out.volume)
    }

    /// Mic -> reverb send gain (dB), like UC (0x180032540 inputs into 0x180057840): silent when
    /// the mic is muted or blended out, else `FXA` fader x blend factor, plus the mic volume.
    pub fn reverb_send_gain(&self) -> f32 {
        let mic = self.src(Source::Mic);
        let factor = self.blend_factor(Source::Mic);
        if mic.mute || factor == 0.0 {
            return SILENT_DB;
        }
        combine(self.reverb_send, factor, mic.volume)
    }

    /// Every write that defines the mixer: 15 crosspoints, 3 mono switches and the mic ->
    /// reverb send.
    pub fn writes(&self) -> Vec<MixWrite> {
        let mut v = Vec::new();
        // Output delay off first (UC's default): a delay left on by an earlier session keeps
        // Voice FX torn down and puts a 0.25 s echo on the outputs.
        v.push(MixWrite {
            block: tag::APPL,
            index: 0,
            x: 13,
            value: ParamValue::I32(self.output_delay_bus.round() as i32),
        });
        // The mic channel's own parameters, which UC pushes along with the mixer: processing
        // channel and DSP amount (`Appl`), then pan and DAW post-DSP (`SetP 0 0`).
        v.push(MixWrite {
            block: tag::APPL,
            index: 0,
            x: 12,
            // UC binds processingChannel as an int (`Pari`), firmware states 19/20.
            value: ParamValue::I32(self.fx_channel.round() as i32),
        });
        v.push(MixWrite {
            block: tag::APPL,
            index: 0,
            x: 4,
            value: ParamValue::F32(if self.bypass { 0.0 } else { self.dsp_amount }),
        });
        v.push(MixWrite {
            block: FX_BLOCK,
            index: FX_INDEX,
            x: PAN_X,
            value: ParamValue::F32(self.pan),
        });
        v.push(MixWrite {
            block: FX_BLOCK,
            index: FX_INDEX,
            x: DAW_POST_X,
            value: ParamValue::I32(self.daw_post_dsp as i32),
        });
        for bus in Bus::ALL {
            for s in Source::ALL {
                v.push(MixWrite {
                    block: BLOCK,
                    index: bus.index(),
                    x: s.x(),
                    value: ParamValue::F32(self.gain(s, bus)),
                });
            }
            v.push(MixWrite {
                block: BLOCK,
                index: bus.index(),
                x: MONO_X,
                value: ParamValue::I32(self.buses[bus.index() as usize].mono as i32),
            });
        }
        v.push(MixWrite {
            block: FX_BLOCK,
            index: FX_INDEX,
            x: FX_X,
            value: ParamValue::F32(self.reverb_send_gain()),
        });
        v
    }

    /// Every settable key with its value, range and unit.
    pub fn entries(&self) -> Vec<(String, f32, f32, f32, &'static str)> {
        let mut v = Vec::new();
        for (i, s) in Source::ALL.iter().enumerate() {
            let st = &self.sources[i];
            let k = s.key();
            v.push((format!("{k}.volume"), st.volume, FADER_MIN, FADER_MAX, "dB"));
            v.push((format!("{k}.mute"), st.mute as u8 as f32, 0.0, 1.0, ""));
            v.push((format!("{k}.solo"), st.solo as u8 as f32, 0.0, 1.0, ""));
            if *s == Source::Mic {
                v.push((
                    "mic.reverb".into(),
                    self.reverb_send,
                    FADER_MIN,
                    FADER_MAX,
                    "dB",
                ));
                v.push(("mic.pan".into(), self.pan, 0.0, 1.0, ""));
                v.push((
                    "mic.dawpostdsp".into(),
                    self.daw_post_dsp as u8 as f32,
                    0.0,
                    1.0,
                    "",
                ));
                v.push(("mic.dspamount".into(), self.dsp_amount, 0.0, 1.0, ""));
                v.push(("mic.bypass".into(), self.bypass as u8 as f32, 0.0, 1.0, ""));
                v.push(("mic.fxchannel".into(), self.fx_channel, -1.0, 1.0, ""));
            }
            for n in 0..2 {
                v.push((
                    format!("{k}.aux{}", n + 1),
                    st.aux[n],
                    FADER_MIN,
                    FADER_MAX,
                    "dB",
                ));
                v.push((
                    format!("{k}.assign{}", n + 1),
                    st.assign[n] as u8 as f32,
                    0.0,
                    1.0,
                    "",
                ));
            }
        }
        for (i, b) in Bus::ALL.iter().enumerate() {
            let st = &self.buses[i];
            let k = b.key();
            v.push((format!("{k}.volume"), st.volume, FADER_MIN, FADER_MAX, "dB"));
            v.push((format!("{k}.mute"), st.mute as u8 as f32, 0.0, 1.0, ""));
            v.push((format!("{k}.mono"), st.mono as u8 as f32, 0.0, 1.0, ""));
        }
        v.push(("blend".into(), self.blend, -1.0, 1.0, ""));
        v.push((
            "outputdelay.bus".into(),
            self.output_delay_bus,
            -1.0,
            3.0,
            "",
        ));
        v
    }

    pub fn get(&self, key: &str) -> Option<f32> {
        self.entries().into_iter().find(|e| e.0 == key).map(|e| e.1)
    }

    /// Set one value by key (`mic.volume`, `playback.aux1`, `mixa.mute`, `blend`, ...).
    pub fn set(&mut self, key: &str, v: f32) -> Result<(), MixError> {
        let (_, _, min, max, _) = self
            .entries()
            .into_iter()
            .find(|e| e.0 == key)
            .ok_or_else(|| MixError::Unknown(key.into()))?;
        let range = || MixError::Range {
            key: key.into(),
            min,
            max,
        };
        if !(v >= min && v <= max) {
            return Err(range());
        }
        if key == "blend" {
            self.blend = v;
            return Ok(());
        }
        if key == "outputdelay.bus" {
            if v.fract() != 0.0 {
                return Err(range());
            }
            self.output_delay_bus = v;
            return Ok(());
        }
        match key {
            "mic.reverb" => {
                self.reverb_send = v;
                return Ok(());
            }
            "mic.pan" => {
                self.pan = v;
                return Ok(());
            }
            "mic.dawpostdsp" => {
                self.daw_post_dsp = flag(v).map_err(|_| range())?;
                return Ok(());
            }
            "mic.dspamount" => {
                self.dsp_amount = v;
                return Ok(());
            }
            "mic.bypass" => {
                self.bypass = flag(v).map_err(|_| range())?;
                return Ok(());
            }
            "mic.fxchannel" => {
                if v.fract() != 0.0 {
                    return Err(range());
                }
                self.fx_channel = v;
                return Ok(());
            }
            _ => {}
        }
        let (head, field) = key
            .split_once('.')
            .ok_or_else(|| MixError::Unknown(key.into()))?;
        if let Some(i) = Source::ALL.iter().position(|s| s.key() == head) {
            let s = &mut self.sources[i];
            match field {
                "volume" => s.volume = v,
                "mute" => s.mute = flag(v).map_err(|_| range())?,
                "solo" => s.solo = flag(v).map_err(|_| range())?,
                "aux1" => s.aux[0] = v,
                "aux2" => s.aux[1] = v,
                "assign1" => s.assign[0] = flag(v).map_err(|_| range())?,
                "assign2" => s.assign[1] = flag(v).map_err(|_| range())?,
                _ => return Err(MixError::Unknown(key.into())),
            }
        } else if let Some(i) = Bus::ALL.iter().position(|b| b.key() == head) {
            let b = &mut self.buses[i];
            match field {
                "volume" => b.volume = v,
                "mute" => b.mute = flag(v).map_err(|_| range())?,
                "mono" => b.mono = flag(v).map_err(|_| range())?,
                _ => return Err(MixError::Unknown(key.into())),
            }
        } else {
            return Err(MixError::Unknown(key.into()));
        }
        Ok(())
    }

    pub fn to_text(&self) -> String {
        self.entries()
            .into_iter()
            .map(|(k, v, ..)| format!("{k}={v}\n"))
            .collect()
    }

    /// Parse `key=value` lines; unknown keys and bad values are skipped.
    pub fn from_text(s: &str) -> Self {
        let mut st = Self::default();
        for line in s.lines() {
            if let Some((k, v)) = line.split_once('=') {
                if let Ok(v) = v.trim().parse::<f32>() {
                    let _ = st.set(k.trim(), v);
                }
            }
        }
        st
    }
}

/// UC's gain law (0x1800576a0 / 0x180057840) for an open crosspoint: fader (dB) x linear
/// factor, plus an output volume (dB), floored at -144 dB.
fn combine(fader: f32, factor: f32, volume: f32) -> f32 {
    const TINY: f32 = 6.309_573e-8; // -144 dB
    let lin = if fader < FLOOR_DB {
        TINY
    } else {
        10f32.powf(fader * 0.05)
    } * factor;
    let db = if lin < TINY {
        FLOOR_DB
    } else {
        20.0 * lin.log10()
    } + volume;
    db.max(FLOOR_DB)
}

/// Default mixer state file, next to the DSP state: `%APPDATA%\revelator-control\mixer-state.txt`.
pub fn default_state_path() -> std::path::PathBuf {
    crate::fx::default_state_path().with_file_name("mixer-state.txt")
}

pub fn load_state(path: &std::path::Path) -> MixerState {
    std::fs::read_to_string(path)
        .map(|s| MixerState::from_text(&s))
        .unwrap_or_default()
}

pub fn save_state(path: &std::path::Path, st: &MixerState) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
    }
    std::fs::write(path, st.to_text()).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Safety rule for a `SetP 100 <bus>` scalar (see `safety.rs`): bus 0..=2, y = 0, and either a
/// crosspoint gain (`Para`, known source, finite, in [`SILENT_DB`, `GAIN_MAX`]) or the mono
/// switch (`Pari` x = 8, 0 or 1).
pub fn write_ok(index: u32, t: u32, y: u32, x: u32, bits: u32) -> bool {
    if index > 2 || y != 0 {
        return false;
    }
    match t {
        tag::PARA if SOURCE_XS.contains(&x) => {
            let v = f32::from_bits(bits);
            v.is_finite() && (SILENT_DB..=GAIN_MAX).contains(&v)
        }
        tag::PARI => x == MONO_X && bits <= 1,
        _ => false,
    }
}

/// Safety rule for the mic channel's `SetP 0 0` writes, exactly UC's addresses with y = 0:
/// the reverb send (`Para` x = 1, finite gain in [`SILENT_DB`, `GAIN_MAX`]), pan (`Para` x = 2,
/// 0..1) and the DAW post-DSP switch (`Pari` x = 3, 0 or 1).
pub fn fx_write_ok(index: u32, t: u32, y: u32, x: u32, bits: u32) -> bool {
    let v = f32::from_bits(bits);
    if index != FX_INDEX || y != 0 {
        return false;
    }
    match (x, t) {
        (FX_X, tag::PARA) => v.is_finite() && (SILENT_DB..=GAIN_MAX).contains(&v),
        (PAN_X, tag::PARA) => v.is_finite() && (0.0..=1.0).contains(&v),
        (DAW_POST_X, tag::PARI) => bits <= 1,
        _ => false,
    }
}

/// Blob size of every mixer write.
pub const BLOB_SIZE: u32 = PARAM_BLOB_SIZE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scene_gains() {
        let m = MixerState::default();
        // 0 dB faders, blend centred: everything at unity on Main
        for s in [
            Source::Mic,
            Source::Playback,
            Source::VirtualA,
            Source::VirtualB,
        ] {
            assert!(m.gain(s, Bus::Main).abs() < 1e-4, "{s:?}");
        }
        // reverb return at -96 dB fader
        assert!((m.gain(Source::Reverb, Bus::Main) + 96.0).abs() < 1e-3);
        // Mix A: mic sent at 0 dB, the returns at -96
        assert!(m.gain(Source::Mic, Bus::MixA).abs() < 1e-4);
        assert!((m.gain(Source::Playback, Bus::MixA) + 96.0).abs() < 1e-3);
    }

    #[test]
    fn blend_mute_solo_assign() {
        let mut m = MixerState::default();
        m.set("blend", -1.0).unwrap(); // mic only
        assert_eq!(m.gain(Source::Playback, Bus::Main), SILENT_DB);
        assert!(m.gain(Source::Mic, Bus::Main).abs() < 1e-4);
        m.set("blend", 0.5).unwrap(); // mic at half (-6 dB)
        assert!((m.gain(Source::Mic, Bus::Main) + 6.0206).abs() < 1e-3);
        // blend does not touch the stream mixes
        assert!(m.gain(Source::Mic, Bus::MixA).abs() < 1e-4);
        m.set("blend", 0.0).unwrap();

        m.set("mic.mute", 1.0).unwrap();
        assert_eq!(m.gain(Source::Mic, Bus::Main), SILENT_DB);
        assert!(m.gain(Source::Mic, Bus::MixA).abs() < 1e-4); // like UC: mute is main-only
        m.set("mic.mute", 0.0).unwrap();

        m.set("virtuala.solo", 1.0).unwrap();
        assert_eq!(m.gain(Source::Mic, Bus::Main), SILENT_DB);
        assert!(m.gain(Source::VirtualA, Bus::Main).abs() < 1e-4);
        m.set("virtuala.solo", 0.0).unwrap();

        m.set("mic.assign2", 0.0).unwrap();
        assert_eq!(m.gain(Source::Mic, Bus::MixB), SILENT_DB);
        m.set("mixb.mute", 1.0).unwrap();
        m.set("main.volume", -10.0).unwrap();
        assert!((m.gain(Source::Playback, Bus::Main) + 10.0).abs() < 1e-3);
        assert_eq!(m.gain(Source::Playback, Bus::MixB), SILENT_DB);
        // the loudest possible setting stays inside the safety range
        m.set("main.volume", 10.0).unwrap();
        m.set("playback.volume", 10.0).unwrap();
        assert!((m.gain(Source::Playback, Bus::Main) - GAIN_MAX).abs() < 1e-3);
    }

    #[test]
    fn writes_are_safe_and_complete() {
        let m = MixerState::default();
        let w = m.writes();
        assert_eq!(w.len(), 24);
        // output delay off goes first
        assert_eq!(
            (w[0].block, w[0].x, w[0].value),
            (tag::APPL, 13, ParamValue::I32(-1))
        );
        // never the output delay (x = 8 / 9) or the firmware-driven DSP amount (x = 4)
        assert!(w
            .iter()
            .all(|x| x.block != FX_BLOCK || ![4, 8, 9].contains(&x.x)));
        assert!(!fx_write_ok(0, tag::PARI, 0, 8, 0));
        assert!(!fx_write_ok(0, tag::PARI, 0, 8, -1i32 as u32));
        assert!(!fx_write_ok(0, tag::PARA, 0, 9, 1f32.to_bits()));
        assert!(!fx_write_ok(0, tag::PARA, 0, 4, 1f32.to_bits()));
        for x in &w {
            if x.block == tag::APPL {
                // checked by the Appl allow-list (`appl::find`)
                let (key, v) = match (x.x, x.value) {
                    (4, ParamValue::F32(v)) => ("fx.amount", v),
                    (12, ParamValue::I32(v)) => ("fx.channel", v as f32),
                    (13, ParamValue::I32(v)) => ("output.delay.bus", v as f32),
                    _ => panic!("{x:?}"),
                };
                let p = crate::appl::find(key).unwrap();
                assert!((p.min..=p.max).contains(&v), "{x:?}");
                continue;
            }
            let (t, bits) = match x.value {
                ParamValue::F32(f) => (tag::PARA, f.to_bits()),
                ParamValue::I32(i) => (tag::PARI, i as u32),
            };
            let ok = if x.block == FX_BLOCK {
                fx_write_ok(x.index, t, 0, x.x, bits)
            } else {
                write_ok(x.index, t, 0, x.x, bits)
            };
            assert!(ok, "{x:?}");
        }
        assert!(!fx_write_ok(0, tag::PARA, 0, 3, 0f32.to_bits()));
        assert!(!fx_write_ok(1, tag::PARA, 0, 1, 0f32.to_bits()));
        assert!(!fx_write_ok(0, tag::PARA, 1, 1, 0f32.to_bits()));
        assert!(!fx_write_ok(0, tag::PARI, 0, 1, 0));
        assert!(!fx_write_ok(0, tag::PARA, 0, 1, f32::INFINITY.to_bits()));
        assert!(!write_ok(3, tag::PARA, 0, 3, 0f32.to_bits()));
        assert!(!write_ok(0, tag::PARA, 1, 3, 0f32.to_bits()));
        assert!(!write_ok(0, tag::PARA, 0, 4, 0f32.to_bits()));
        assert!(!write_ok(0, tag::PARA, 0, 3, 25f32.to_bits()));
        assert!(!write_ok(0, tag::PARA, 0, 3, f32::NAN.to_bits()));
        assert!(!write_ok(0, tag::PARI, 0, 8, 2));
        assert!(!write_ok(0, tag::PARI, 0, 3, 1));
    }

    #[test]
    fn reverb_send() {
        let mut m = MixerState::default();
        // default scene: FXA at -96 dB
        assert!((m.reverb_send_gain() + 96.0).abs() < 1e-3);
        m.set("mic.reverb", -6.0).unwrap();
        m.set("mic.volume", -4.0).unwrap();
        assert!((m.reverb_send_gain() + 10.0).abs() < 1e-3);
        // blend +1 (playback only) blends the mic out, and the send with it
        m.set("blend", 1.0).unwrap();
        assert_eq!(m.reverb_send_gain(), SILENT_DB);
        m.set("blend", 0.0).unwrap();
        m.set("mic.mute", 1.0).unwrap();
        assert_eq!(m.reverb_send_gain(), SILENT_DB);
        let w = *m.writes().last().unwrap();
        assert_eq!((w.block, w.index, w.x, w.label()), (0, 0, 1, "reverb-send"));
        let f = w.frame(1);
        // payload after the 8-byte header: SetP, block 0, index 0, Para, 0x14, y 0, x 1, value
        assert_eq!(&f[8 + 4..8 + 12], &[0u8; 8]);
        assert_eq!(&f[8 + 20..8 + 28], &[0, 0, 0, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn text_roundtrip_and_ranges() {
        let mut m = MixerState::default();
        m.set("playback.aux1", -12.5).unwrap();
        m.set("mixa.mono", 1.0).unwrap();
        m.set("blend", 0.25).unwrap();
        m.set("mic.reverb", -20.0).unwrap();
        assert_eq!(MixerState::from_text(&m.to_text()), m);
        assert!(m.set("mic.volume", 11.0).is_err());
        assert!(m.set("mic.mute", 0.5).is_err());
        assert!(m.set("mic.volume", f32::NAN).is_err());
        assert!(m.set("nope.volume", 0.0).is_err());
    }
}

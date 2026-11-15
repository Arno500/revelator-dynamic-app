//! `dsp` command: DSP block settings (HPF, gate, compressor, EQ, limiter).
//!
//! The mic cannot be asked for these values, so the CLI keeps them in a small state file and
//! sends a whole block whenever one of its parameters changes (that is what Universal Control
//! does too: every blob carries the full block state).

use std::path::PathBuf;

use revelator_core::fx::{self, Engine, FxState};
use revelator_core::{Device, Transport};

pub const HELP: &str = "\
    dsp list              Show every DSP parameter with the value last sent (from the state file)
    dsp set <key> <value> [<key> <value>...]
                          Change DSP parameters and send the affected blocks
    dsp push              Send every block (makes the mic match the state file)
    dsp reset             Reset the state file to Universal Control's defaults (sends nothing)
      dsp options: --rate <Hz> sample rate the mic runs at (default: read from the
                               Revelator's Windows audio endpoints, else 48000 and
                               unknown; echo needs a known 44100/48000)
                   --ch <n>    processing channel (default 0)
                   --dry       print the frames instead of sending them (no device needed)
                   --state <file> state file (default %APPDATA%\\revelator-control\\dsp-state.txt)";

pub struct Opts {
    pub rate: f32,
    /// The rate is known (`--rate`, or read from Windows): tell the safety layer.
    pub rate_explicit: bool,
    pub ch: u32,
    pub dry: bool,
    pub state: PathBuf,
}

fn take_value(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    match args.iter().position(|a| a == flag) {
        Some(i) if i + 1 < args.len() => {
            let v = args.remove(i + 1);
            args.remove(i);
            Ok(Some(v))
        }
        Some(_) => Err(format!("{flag} needs a value")),
        None => Ok(None),
    }
}

pub fn parse_opts(args: &mut Vec<String>) -> Result<Opts, String> {
    let rate_arg = take_value(args, "--rate")?;
    let detected = revelator_usb::audio_rate::revelator_sample_rate();
    let rate_explicit = rate_arg.is_some() || detected.is_some();
    let rate = match (rate_arg, detected) {
        (Some(s), d) => {
            let r = s.parse::<f32>().map_err(|_| format!("bad --rate '{s}'"))?;
            if let Some(d) = d.filter(|&d| d as f32 != r) {
                return Err(format!(
                    "--rate {r} does not match the {d} Hz Windows uses for the Revelator"
                ));
            }
            r
        }
        (None, Some(d)) => d as f32,
        (None, None) => 48_000.0,
    };
    if !fx::MULTI_RATES.contains(&rate) {
        return Err(format!(
            "--rate must be one of 44100, 48000, 88200, 96000 (got {rate})"
        ));
    }
    let ch = match take_value(args, "--ch")? {
        Some(s) => s.parse::<u32>().map_err(|_| format!("bad --ch '{s}'"))?,
        None => 0,
    };
    if ch > 1 {
        return Err("--ch must be 0 or 1".into());
    }
    let dry = {
        let n = args.len();
        args.retain(|a| a != "--dry");
        args.len() != n
    };
    let state = match take_value(args, "--state")? {
        Some(s) => PathBuf::from(s),
        None => default_state_path(),
    };
    Ok(Opts {
        rate,
        rate_explicit,
        ch,
        dry,
        state,
    })
}

fn default_state_path() -> PathBuf {
    fx::default_state_path()
}

fn load(path: &std::path::Path) -> FxState {
    fx::load_state(path)
}

fn save(path: &std::path::Path, st: &FxState) -> Result<(), String> {
    fx::save_state(path, st)
}

pub fn needs_device(sub: &str, o: &Opts) -> bool {
    matches!(sub, "set" | "push") && !o.dry
}

pub fn list(o: &Opts) {
    let st = load(&o.state);
    println!("DSP parameters (state file {}):", o.state.display());
    let mut last = None;
    for (p, v) in st.iter() {
        if last != Some(p.engine) {
            println!();
            last = Some(p.engine);
        }
        println!(
            "  {:<28} {:>8} {:<6} {}  [{} .. {}]",
            p.key, v, p.unit, p.label, p.min, p.max
        );
    }
}

pub fn reset(o: &Opts) -> Result<(), String> {
    save(&o.state, &FxState::default())?;
    println!("state file reset to defaults: {}", o.state.display());
    Ok(())
}

/// Apply `key value` pairs to the state; returns the new state and the engines to resend.
fn apply(o: &Opts, pairs: &[String]) -> Result<(FxState, Vec<Engine>), String> {
    if pairs.is_empty() || !pairs.len().is_multiple_of(2) {
        return Err("usage: dsp set <key> <value> [<key> <value>...]  (see `dsp list`)".into());
    }
    let mut st = load(&o.state);
    let mut engines = Vec::new();
    for kv in pairs.chunks(2) {
        let v: f32 = kv[1]
            .parse()
            .map_err(|_| format!("'{}' is not a number", kv[1]))?;
        let e = st.set(&kv[0], v).map_err(|e| e.to_string())?;
        if !engines.contains(&e) {
            engines.push(e);
        }
    }
    Ok((st, engines))
}

fn print_frames(o: &Opts, writes: &[fx::DspWrite]) {
    for (i, w) in writes.iter().enumerate() {
        let f = revelator_core::frame::set_blob(i as u8 + 1, w.block, o.ch, &w.blob);
        println!("{}:", fx::describe(w));
        for chunk in f.chunks(32) {
            println!(
                "  {}",
                chunk
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }
}

fn send<T: Transport + 'static>(t: T, o: &Opts, writes: &[fx::DspWrite]) -> Result<(), String> {
    let mut dev = Device::open(t).map_err(|e| e.to_string())?;
    if o.rate_explicit {
        dev.set_sample_rate(Some(o.rate as u32));
        println!("sample rate: {} Hz", o.rate);
    } else {
        println!("sample rate: unknown (filters designed for 48000 Hz; echo blocked)");
    }
    let acks = dev.send_dsp_acks(o.ch, writes).map_err(|e| e.to_string())?;
    for (w, a) in writes.iter().zip(&acks) {
        let hex: Vec<String> = a.iter().map(|b| format!("{b:02x}")).collect();
        println!("sent {}  ack: {}", fx::describe(w), hex.join(" "));
    }
    Ok(())
}

pub fn set_dry(o: &Opts, pairs: &[String]) -> Result<(), String> {
    let (st, engines) = apply(o, pairs)?;
    let writes: Vec<_> = engines.iter().flat_map(|e| st.writes(*e, o.rate)).collect();
    print_frames(o, &writes);
    println!("(dry run: nothing sent, state file unchanged)");
    Ok(())
}

pub fn set<T: Transport + 'static>(t: T, o: &Opts, pairs: &[String]) -> Result<(), String> {
    let (st, engines) = apply(o, pairs)?;
    let writes: Vec<_> = engines.iter().flat_map(|e| st.writes(*e, o.rate)).collect();
    send(t, o, &writes)?;
    save(&o.state, &st)?;
    println!("saved to {}", o.state.display());
    Ok(())
}

pub fn push_dry(o: &Opts) {
    print_frames(o, &load(&o.state).all_writes(o.rate));
    println!("(dry run: nothing sent)");
}

pub fn push<T: Transport + 'static>(t: T, o: &Opts) -> Result<(), String> {
    let st = load(&o.state);
    send(t, o, &st.all_writes(o.rate))
}

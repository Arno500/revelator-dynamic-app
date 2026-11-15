//! `mixer` command: the stream mixer (Main, Stream Mix A/B), via the crosspoint gains Universal
//! Control sends (see `revelator_core::mixer`). The mic cannot report these values, so like
//! `dsp` the CLI keeps them in a state file (shared with the GUI).

use std::path::PathBuf;

use revelator_core::frame::ParamValue;
use revelator_core::mixer::{self, MixerState};
use revelator_core::{Device, Transport};

pub const HELP: &str = "\
    mixer list            Show every mixer setting (from the state file) and the crosspoint gains
    mixer set <key> <value> [<key> <value>...]
                          Change mixer settings and send the whole mixer, e.g.
                          mixer set playback.volume -6 mixa.mute 1 blend -0.5
    mixer push            Send the whole mixer (makes the mic match the state file)
      mixer options: --dry          print the frames instead of sending them
                     --state <file> state file (default %APPDATA%\\revelator-control\\mixer-state.txt)";

pub struct Opts {
    pub dry: bool,
    pub state: PathBuf,
}

pub fn parse_opts(args: &mut Vec<String>) -> Result<Opts, String> {
    let dry = {
        let n = args.len();
        args.retain(|a| a != "--dry");
        args.len() != n
    };
    let state = match args.iter().position(|a| a == "--state") {
        Some(i) if i + 1 < args.len() => {
            let v = args.remove(i + 1);
            args.remove(i);
            PathBuf::from(v)
        }
        Some(_) => return Err("--state needs a value".into()),
        None => mixer::default_state_path(),
    };
    Ok(Opts { dry, state })
}

pub fn needs_device(sub: &str, o: &Opts) -> bool {
    matches!(sub, "set" | "push") && !o.dry
}

pub fn list(o: &Opts) {
    let st = mixer::load_state(&o.state);
    println!("mixer settings (state file {}):", o.state.display());
    for (k, v, min, max, unit) in st.entries() {
        println!("  {k:<20} {v:>8} {unit:<3} [{min} .. {max}]");
    }
    println!(
        "\ncrosspoint gains sent (dB; {} = silent):",
        mixer::SILENT_DB
    );
    print!("  {:<10}", "");
    for b in mixer::Bus::ALL {
        print!("{:>9}", b.key());
    }
    println!();
    for s in mixer::Source::ALL {
        print!("  {:<10}", s.key());
        for b in mixer::Bus::ALL {
            print!("{:>9.1}", st.gain(s, b));
        }
        println!();
    }
    println!("  mic -> reverb send: {:.1}", st.reverb_send_gain());
}

fn apply(o: &Opts, pairs: &[String]) -> Result<MixerState, String> {
    if pairs.is_empty() || !pairs.len().is_multiple_of(2) {
        return Err("usage: mixer set <key> <value> [<key> <value>...]  (see `mixer list`)".into());
    }
    let mut st = mixer::load_state(&o.state);
    for kv in pairs.chunks(2) {
        let v: f32 = kv[1]
            .parse()
            .map_err(|_| format!("'{}' is not a number", kv[1]))?;
        st.set(&kv[0], v).map_err(|e| e.to_string())?;
    }
    Ok(st)
}

fn print_frames(st: &MixerState) {
    for (i, w) in st.writes().iter().enumerate() {
        let v = match w.value {
            ParamValue::F32(f) => format!("{f:.2} dB"),
            ParamValue::I32(i) => format!("mono {i}"),
        };
        let f = w.frame(i as u8 + 1);
        let hex: Vec<_> = f.iter().map(|b| format!("{b:02x}")).collect();
        println!("{} x={} {v}: {}", w.label(), w.x, hex.join(" "));
    }
}

fn send<T: Transport + 'static>(t: T, st: &MixerState) -> Result<(), String> {
    let mut dev = Device::open(t).map_err(|e| e.to_string())?;
    let w = st.writes();
    dev.send_mixer(&w).map_err(|e| e.to_string())?;
    println!("sent {} mixer writes", w.len());
    Ok(())
}

pub fn set_dry(o: &Opts, pairs: &[String]) -> Result<(), String> {
    print_frames(&apply(o, pairs)?);
    println!("(dry run: nothing sent, state file unchanged)");
    Ok(())
}

pub fn set<T: Transport + 'static>(t: T, o: &Opts, pairs: &[String]) -> Result<(), String> {
    let st = apply(o, pairs)?;
    send(t, &st)?;
    mixer::save_state(&o.state, &st)?;
    println!("saved to {}", o.state.display());
    Ok(())
}

pub fn push_dry(o: &Opts) {
    print_frames(&mixer::load_state(&o.state));
    println!("(dry run: nothing sent)");
}

pub fn push<T: Transport + 'static>(t: T, o: &Opts) -> Result<(), String> {
    send(t, &mixer::load_state(&o.state))
}

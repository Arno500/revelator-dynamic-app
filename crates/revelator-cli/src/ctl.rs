//! Glue to revelator-core's `Device` (state commands).

use std::{thread, time::Duration};

use revelator_core::appl::{self, Readback};
use revelator_core::{mock::MockTransport, params, slots, Device, State, Transport};

pub fn mock_transport() -> impl Transport + 'static {
    MockTransport::new()
}

fn open<T: Transport + 'static>(t: T) -> Result<Device<T>, String> {
    Device::open(t).map_err(|e| e.to_string())
}

pub fn dump_state<T: Transport + 'static>(t: T) -> Result<(), String> {
    let mut dev = open(t)?;
    let st = dev.read_state().map_err(|e| e.to_string())?;
    match st.controls() {
        Some(c) => {
            println!("hardware controls (verified on a Revelator Dynamic):");
            println!("  preset   {} of 4 (raw value {})", c.preset + 1, c.preset);
            println!("  mute     {}", if c.muted { "on" } else { "off" });
            println!("  knob     {:.2} ({:.0} %)", c.knob, c.knob * 100.0);
            println!(
                "  flags    0x{:08X} (bit 3 = mute; other bits unknown)",
                c.flags
            );
        }
        None => println!(
            "state too short to decode the hardware controls ({} slots)",
            st.len()
        ),
    }
    println!("\ndevice parameters (writable with `set`; verified on a Revelator Dynamic):");
    for p in appl::ALL {
        let v = match p.readback {
            Readback::F32(i) => st.slot(i).map(|v| format!("{v:.3}")),
            Readback::I32(i) => st.slot_bits(i).map(|b| (b as i32).to_string()),
            Readback::Bit(i, b) => st.slot_bits(i).map(|w| ((w >> b) & 1).to_string()),
            Readback::None => Some("?".into()),
        };
        println!(
            "  {:<20} {:>10} {:<6} {}",
            p.key,
            v.unwrap_or_else(|| "-".into()),
            p.unit,
            p.label
        );
    }
    // The io24-based DSP table (params::ALL) is not printed: its state slots were guesses and
    // the Dynamic's state reply does not carry DSP values (see docs/uc-dll-notes.md).
    Ok(())
}

/// Parse a blob size: decimal or 0x-hex. Default 0x7EC.
pub fn parse_size(s: Option<&String>) -> Result<u32, String> {
    let Some(s) = s else {
        return Ok(revelator_core::frame::JAST_BLOB_SIZE);
    };
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|_| format!("'{s}' is not a blob size (decimal or 0x-hex)"))
}

fn slot_line(i: usize, bits: u32) -> String {
    let f = f32::from_bits(bits);
    format!(
        "slot {i:>3}  @0x{:03X}  0x{bits:08X}  f32 {f:>14.6}  i32 {:>11}",
        revelator_core::frame::DATA_START + 4 * i,
        bits as i32
    )
}

/// Every non-zero slot of the raw state, as hex, f32 and i32.
pub fn raw_state<T: Transport + 'static>(t: T, size: u32) -> Result<(), String> {
    let mut dev = open(t)?;
    let st = dev.read_state_sized(size).map_err(|e| e.to_string())?;
    println!("blob size 0x{size:X}: {} slots", st.len());
    let mut nonzero = 0;
    for (i, f) in st.floats().iter().enumerate() {
        if f.to_bits() != 0 {
            nonzero += 1;
            println!("{}", slot_line(i, f.to_bits()));
        }
    }
    println!("{nonzero} non-zero slot(s)");
    Ok(())
}

/// Poll the raw state and print every slot that changes. Slots that change during the first
/// `baseline_ms` without anyone touching the mic (level meters) are muted afterwards.
pub fn watch_raw<T: Transport + 'static>(
    t: T,
    size: u32,
    ms: u64,
    baseline_ms: u64,
) -> Result<(), String> {
    let mut dev = open(t)?;
    let read = |dev: &mut Device<T>| -> Result<Vec<u32>, String> {
        Ok(dev
            .read_state_sized(size)
            .map_err(|e| e.to_string())?
            .floats()
            .iter()
            .map(|f| f.to_bits())
            .collect())
    };
    let mut prev = read(&mut dev)?;
    println!("blob size 0x{size:X}: {} slots. Learning which slots change on their own for {baseline_ms} ms; don't touch the mic...", prev.len());
    let mut noisy = vec![false; prev.len()];
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(baseline_ms) {
        thread::sleep(Duration::from_millis(ms));
        let cur = read(&mut dev)?;
        for (i, (a, b)) in prev.iter().zip(&cur).enumerate() {
            if a != b {
                noisy[i] = true;
            }
        }
        prev = cur;
    }
    let muted: Vec<usize> = noisy
        .iter()
        .enumerate()
        .filter(|(_, n)| **n)
        .map(|(i, _)| i)
        .collect();
    println!("muted {} self-changing slot(s): {muted:?}", muted.len());
    println!("now change one control at a time (Ctrl+C to stop):");
    loop {
        thread::sleep(Duration::from_millis(ms));
        let cur = read(&mut dev)?;
        if cur.len() != prev.len() {
            println!(
                "state length changed: {} -> {} slots",
                prev.len(),
                cur.len()
            );
        }
        for (i, (a, b)) in prev.iter().zip(&cur).enumerate() {
            if a != b && !noisy.get(i).copied().unwrap_or(false) {
                println!(
                    "{}   (was 0x{a:08X} = {})",
                    slot_line(i, *b),
                    f32::from_bits(*a)
                );
            }
        }
        prev = cur;
    }
}

pub fn get<T: Transport + 'static>(t: T, key: &str) -> Result<(), String> {
    let p = appl::find(key)
        .ok_or_else(|| format!("unknown parameter '{key}'. Known: {}", appl_keys()))?;
    match open(t)?.get_appl(p).map_err(|e| e.to_string())? {
        Some(v) => println!("{} = {} {}", p.key, v, p.unit),
        None => println!("{}: no known state location (write-only)", p.key),
    }
    Ok(())
}

fn appl_keys() -> String {
    appl::ALL
        .iter()
        .map(|p| p.key)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Write a device-level parameter. Only the `appl` table (addresses from Universal Control's
/// DLL) is writable; the io24-based table addresses are not used for writes.
pub fn set<T: Transport + 'static>(t: T, key: &str, value: &str) -> Result<(), String> {
    let v: f32 = value
        .parse()
        .map_err(|_| format!("'{value}' is not a number"))?;
    let Some(p) = appl::find(key) else {
        return Err(if params::find(key).is_some() {
            format!("'{key}' has no confirmed write address yet, so writing it is disabled. Writable: {}", appl_keys())
        } else {
            format!("unknown parameter '{key}'. Writable: {}", appl_keys())
        });
    };
    let mut dev = open(t)?;
    let back = dev.set_appl(p, v).map_err(|e| {
        format!("{e}. The write itself was sent; check whether the mic changed (`dump-state`).")
    })?;
    if dev.full_state_fallback {
        println!("(the mic only answered the full-size state request after this write)");
    }
    match back {
        Some(b) if (b - v).abs() < 1e-3 => println!("{} = {v} {} (state confirms)", p.key, p.unit),
        Some(b) => println!("{} sent {v} {}, but the state reads {b}", p.key, p.unit),
        None => println!(
            "{} sent {v} {} (no state location known to confirm it)",
            p.key, p.unit
        ),
    }
    Ok(())
}

pub fn watch<T: Transport + 'static>(t: T, ms: u64) -> Result<(), String> {
    let mut dev = open(t)?;
    let mut prev: Option<Vec<f32>> = None;
    loop {
        let st = dev.read_state().map_err(|e| e.to_string())?;
        let cur = st.floats().to_vec();
        if let Some(old) = &prev {
            if let (Some(a), Some(b)) = (State::from_floats(old.clone()).controls(), st.controls())
            {
                if a.preset != b.preset {
                    println!("preset: {} -> {}", a.preset + 1, b.preset + 1);
                }
                if a.muted != b.muted {
                    println!("mute: {}", if b.muted { "on" } else { "off" });
                }
                if a.knob != b.knob {
                    println!("knob: {:.2} -> {:.2}", a.knob, b.knob);
                }
                if a.flags & !slots::FLAG_MUTE != b.flags & !slots::FLAG_MUTE {
                    println!(
                        "flags (unknown bits): 0x{:08X} -> 0x{:08X}",
                        a.flags, b.flags
                    );
                }
            }
            for p in appl::ALL {
                if let (Some(a), Some(b)) = (
                    p.readback.read(&State::from_floats(old.clone())),
                    p.readback.read(&st),
                ) {
                    if a != b {
                        println!("{}: {} -> {} {}", p.key, a, b, p.unit);
                    }
                }
            }
        }
        prev = Some(cur);
        thread::sleep(Duration::from_millis(ms));
    }
}

/// `meters`: print the slots that move by themselves (the probable level meters) as f32, so
/// speaking into the mic shows which one is the mic level. Read-only. Ctrl+C to stop.
pub fn meters<T: Transport + 'static>(t: T, ms: u64, all: bool) -> Result<(), String> {
    let mut dev = open(t)?;
    let slots: Vec<usize> = if all {
        (0..=28).collect()
    } else {
        vec![6, 7, 8, 9, 10, 11, 14, 15, 16, 17, 18, 19, 26]
    };
    print!("slots:");
    for i in &slots {
        print!("{i:>7}");
    }
    println!();
    loop {
        let st = dev.read_state().map_err(|e| e.to_string())?;
        print!("      ");
        for &i in &slots {
            match st.slot(i) {
                Some(v) => print!("{v:>7.3}"),
                None => print!("{:>7}", "-"),
            }
        }
        println!();
        thread::sleep(Duration::from_millis(ms));
    }
}

/// `readback-test`: find where the mic reports the write-only settings. Writes each of
/// headphones.mute, mute.button and preset.button through its values (UC's own addresses and
/// ranges) and prints which state slots / flag bits change, ignoring slots that change on their
/// own (meters). Ends on UC's defaults: headphones unmuted, mute button = mic, preset button =
/// multi.
pub fn readback_test<T: Transport + 'static>(t: T) -> Result<(), String> {
    let mut dev = open(t)?;
    let read = |dev: &mut Device<T>| -> Result<Vec<u32>, String> {
        Ok(dev
            .read_state()
            .map_err(|e| e.to_string())?
            .floats()
            .iter()
            .map(|f| f.to_bits())
            .collect())
    };
    println!("learning which slots change on their own (2 s); don't touch the mic...");
    let mut prev = read(&mut dev)?;
    let mut noisy = vec![false; prev.len()];
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(100));
        let cur = read(&mut dev)?;
        for (i, (a, b)) in prev.iter().zip(&cur).enumerate() {
            if a != b {
                noisy[i] = true;
            }
        }
        prev = cur;
    }
    let flags0 = prev.get(slots::FLAGS).copied().unwrap_or(0);
    println!("flag word now 0x{flags0:08X}\n");
    let steps: [(&str, &[f32]); 3] = [
        ("headphones.mute", &[1.0, 0.0]),
        ("mute.button", &[0.0, 1.0]),
        ("preset.button", &[0.0, 1.0, 2.0]),
    ];
    // Read the state after a write, waiting and retrying: the mic may be slow to answer.
    let read_retry = |dev: &mut Device<T>| -> Result<Vec<u32>, String> {
        let mut last = String::new();
        for _ in 0..4 {
            thread::sleep(Duration::from_millis(300));
            match read(dev) {
                Ok(v) => return Ok(v),
                Err(e) => last = e,
            }
        }
        Err(last)
    };
    let defaults: [(&str, f32); 3] = [
        ("headphones.mute", 0.0),
        ("mute.button", 1.0),
        ("preset.button", 2.0),
    ];
    for (key, values) in steps {
        let p = appl::find(key).ok_or("internal: missing appl key")?;
        for &v in values {
            let r = dev
                .send_appl(p, v)
                .map_err(|e| e.to_string())
                .and_then(|_| read_retry(&mut dev));
            let cur = match r {
                Ok(c) => c,
                Err(e) => {
                    // put the settings back to UC's defaults before giving up
                    for (k, dv) in defaults {
                        if let Some(dp) = appl::find(k) {
                            let _ = dev.send_appl(dp, dv);
                        }
                    }
                    return Err(format!(
                        "after sending {key} = {v}: {e}. Sent UC's defaults back (headphones unmuted, mute button = mic, preset button = multi)."
                    ));
                }
            };
            let mut changes = Vec::new();
            for (i, (a, b)) in prev.iter().zip(&cur).enumerate() {
                if a == b || noisy.get(i).copied().unwrap_or(true) {
                    continue;
                }
                if i == slots::FLAGS {
                    let d = a ^ b;
                    let bits: Vec<String> = (0..32)
                        .filter(|k| d >> k & 1 == 1)
                        .map(|k| format!("bit {k} -> {}", b >> k & 1))
                        .collect();
                    changes.push(format!("flags {}", bits.join(", ")));
                } else {
                    changes.push(format!(
                        "slot {i}: 0x{a:08X} -> 0x{b:08X} (f32 {} / i32 {})",
                        f32::from_bits(*b),
                        *b as i32
                    ));
                }
            }
            println!(
                "{key} = {v}: {}",
                if changes.is_empty() {
                    "no change in the state".to_string()
                } else {
                    changes.join("; ")
                }
            );
            prev = cur;
        }
    }
    println!(
        "\nleft at UC's defaults: headphones.mute 0, mute.button 1 (mic), preset.button 2 (multi)."
    );
    println!("Please send this output so the readback can be wired up.");
    Ok(())
}

/// `presets dump [area...]`: read preset memory areas through UC's MemP transfer (read-only) and
/// print what comes back, as text when it is UTF-8, else as hex. Saves each area to
/// `preset-<n>.bin` (`PrsM n`) or `slot-<n>.bin` (`Stat n`) in `save_dir` when given.
pub fn presets_dump<T: Transport + 'static>(
    t: T,
    slots: &[(u32, u32)],
    save_dir: Option<&std::path::Path>,
) -> Result<(), String> {
    let mut dev = open(t)?;
    let mut failed = 0;
    for &addr in slots {
        let n = addr.1;
        let (label, file) = if addr.0 == revelator_core::frame::tag::STAT {
            (
                format!("button slot {} (Stat {n})", n + 1),
                format!("slot-{n}.bin"),
            )
        } else if n < 8 {
            (format!("factory preset {n}"), format!("preset-{n}.bin"))
        } else {
            (
                format!("user preset {} (PrsM {n})", n - 7),
                format!("preset-{n}.bin"),
            )
        };
        println!("=== {label} ===");
        match dev.read_mem(addr) {
            Ok(bytes) => {
                println!("{} bytes", bytes.len());
                let trimmed = {
                    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
                    &bytes[..end]
                };
                match std::str::from_utf8(trimmed) {
                    Ok(s) if !s.is_empty() => println!("{s}"),
                    _ if revelator_core::ubjson::decode(trimmed).is_ok() => {
                        // Universal Control's binary (UBJSON) preset document
                        if let Ok(v) = revelator_core::ubjson::decode(trimmed) {
                            println!(
                                "(UBJSON document)
{v}"
                            );
                        }
                    }
                    _ => {
                        for (i, c) in bytes.chunks(32).enumerate() {
                            let hex: Vec<_> = c.iter().map(|b| format!("{b:02x}")).collect();
                            println!("  {:04x}: {}", i * 32, hex.join(" "));
                        }
                    }
                }
                if let Some(dir) = save_dir {
                    let p = dir.join(&file);
                    std::fs::write(&p, &bytes).map_err(|e| format!("{}: {e}", p.display()))?;
                    println!("(saved to {})", p.display());
                }
            }
            Err(e) => {
                failed += 1;
                println!("error: {e}");
            }
        }
        println!();
    }
    if failed == slots.len() {
        Err("no preset slot could be read".into())
    } else {
        Ok(())
    }
}

/// Diagnostic: read back the mic's stored Voice FX / reverb blob (`dsp-get voicefx|reverb`).
pub fn dsp_get<T: Transport + 'static>(t: T, what: &str) -> Result<(), String> {
    use revelator_core::fx::{blk, tag as ftag};
    let (block, tg, size) = match what {
        "voicefx" => (blk::VOICE_FX, ftag::VOFX, ftag::VOFX_SIZE),
        "reverb" => (blk::REVERB, ftag::VRVB, ftag::VRVB_SIZE),
        _ => return Err("usage: dsp-get voicefx|reverb".into()),
    };
    let mut dev = open(t)?;
    let d = dev.get_dsp(block, tg, size).map_err(|e| e.to_string())?;
    let hex: Vec<String> = d.iter().map(|b| format!("{b:02x}")).collect();
    println!("{what}: {} bytes: {}", d.len(), hex.join(" "));
    Ok(())
}

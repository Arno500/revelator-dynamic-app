//! `trace`: read-only wire-level diagnostic. Listens for unsolicited data, then sends the
//! state request (GetP Appl JaSt) with one or more blob sizes and hex-dumps everything that
//! comes back. Every frame still goes through the core safety allow-list.

use revelator_core::frame::{self, fourcc_str};
use revelator_core::safety::{Command, SafetyContext};
use revelator_core::{Transport, TransportError};
use revelator_usb::NusbTransport;

/// 0x7EC: io24 notes. 0xDC: seen next to the JaSt request in Universal Control's DSP USB module.
const DEFAULT_SIZES: [u32; 2] = [0x7EC, 0xDC];

fn hex(b: &[u8], max: usize) -> String {
    let s: Vec<String> = b.iter().take(max).map(|x| format!("{x:02X}")).collect();
    let more = if b.len() > max {
        format!(" ... (+{} bytes)", b.len() - max)
    } else {
        String::new()
    };
    format!("{}{more}", s.join(" "))
}

fn parse_size(s: &str) -> Result<u32, String> {
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|_| format!("'{s}' is not a blob size (decimal or 0x-hex)"))
}

/// Read raw transfers until one read times out after `quiet_ms`; print each.
fn drain(t: &mut NusbTransport, quiet_ms: u32) -> Result<usize, String> {
    let mut total = 0;
    loop {
        match t.read_raw(quiet_ms) {
            Ok(b) => {
                total += b.len();
                println!("  < {} bytes: {}", b.len(), hex(&b, 64));
            }
            Err(TransportError::Timeout) => return Ok(total),
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
}

fn describe_reply(buf: &[u8]) {
    match frame::parse_reply(buf) {
        Ok(r) => println!(
            "  reply: uid {} block {} index {} blob {} data {} bytes",
            r.uid,
            fourcc_str(r.block),
            r.index,
            r.blob
                .map(|(t, n)| format!("{} size 0x{n:X}", fourcc_str(t)))
                .unwrap_or("-".into()),
            r.data.len()
        ),
        Err(e) => println!("  (received bytes do not parse as one reply frame: {e})"),
    }
}

pub fn run(t: &mut NusbTransport, args: &[String]) -> Result<(), String> {
    let sizes: Vec<u32> = if args.is_empty() {
        DEFAULT_SIZES.to_vec()
    } else {
        args.iter()
            .map(|s| parse_size(s))
            .collect::<Result<_, _>>()?
    };
    let ep = t.endpoints();
    println!(
        "bulk IN max packet {} bytes, OUT max packet {} bytes",
        ep.in_max_packet, ep.out_max_packet
    );

    println!("listening 1500 ms for unsolicited data...");
    let n = drain(t, 1500)?;
    if n == 0 {
        println!("  (nothing)");
    }

    for (i, size) in sizes.iter().enumerate() {
        let uid = (i + 1) as u8;
        let f = frame::get_state_sized(uid, *size);
        let payload = frame::decode_frame(&f).map_err(|e| e.to_string())?.payload;
        Command::check_payload(payload, &SafetyContext::default()).map_err(|e| e.to_string())?;
        println!("\nGetP Appl JaSt, blob size 0x{size:X}, uid {uid}");
        println!("  > {} bytes: {}", f.len(), hex(&f, 64));
        match t.write_frame(&f, 1000) {
            Ok(()) => println!("  write ok"),
            Err(e) => {
                println!("  write FAILED: {e}");
                continue;
            }
        }
        // Collect everything for 2 s of silence, then try to parse it as one frame.
        let mut got = Vec::new();
        loop {
            match t.read_raw(2000) {
                Ok(b) => {
                    println!("  < {} bytes: {}", b.len(), hex(&b, 64));
                    got.extend_from_slice(&b);
                }
                Err(TransportError::Timeout) => break,
                Err(e) => return Err(format!("read failed: {e}")),
            }
        }
        if got.is_empty() {
            println!("  no reply within 2000 ms");
        } else {
            println!("  total {} bytes", got.len());
            describe_reply(&got);
        }
    }
    Ok(())
}

/// `trace-write <key> <value>`: one safety-checked device write (same frame as `set`), then a
/// timestamped record of what the mic does: anything it sends on its own, and whether it still
/// accepts and answers a state request, polled for about 8 s. Finds where a stall starts.
pub fn write_trace(t: &mut NusbTransport, args: &[String]) -> Result<(), String> {
    use std::time::Instant;
    let [key, value] = args else {
        return Err("usage: trace-write <param> <value>   (e.g. trace-write mic.mute 1)".into());
    };
    let p = revelator_core::appl::find(key).ok_or_else(|| format!("unknown parameter '{key}'"))?;
    let v: f32 = value
        .parse()
        .map_err(|_| format!("'{value}' is not a number"))?;
    if !v.is_finite() || v < p.min || v > p.max {
        return Err(format!("{key} must be in [{}, {}]", p.min, p.max));
    }
    let pv = match p.kind {
        revelator_core::params::Kind::F32 => frame::ParamValue::F32(v),
        revelator_core::params::Kind::I32 => frame::ParamValue::I32(v.round() as i32),
    };
    let check = |f: &[u8]| -> Result<(), String> {
        let payload = frame::decode_frame(f).map_err(|e| e.to_string())?.payload;
        Command::check_payload(payload, &SafetyContext::default())
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    let t0 = Instant::now();
    let ms = || t0.elapsed().as_millis();
    let w = frame::set_appl(1, p.x, p.y, pv);
    check(&w)?;
    println!("[{:>5} ms] SetP {key} = {v}: > {}", ms(), hex(&w, 64));
    match t.write_frame(&w, 1000) {
        Ok(()) => println!("[{:>5} ms] write ok", ms()),
        Err(e) => return Err(format!("write failed: {e}")),
    }
    // anything the mic volunteers right after the write
    loop {
        match t.read_raw(400) {
            Ok(b) => println!("[{:>5} ms] < {} bytes: {}", ms(), b.len(), hex(&b, 64)),
            Err(TransportError::Timeout) => break,
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
    println!("[{:>5} ms] (no more unsolicited data)", ms());
    for n in 0..6u8 {
        let uid = 2 + n;
        let f = frame::get_state_sized(uid, frame::STATE_POLL_SIZE);
        check(&f)?;
        println!("[{:>5} ms] GetP JaSt 0xDC uid {uid}", ms());
        match t.write_frame(&f, 1000) {
            Ok(()) => println!("[{:>5} ms]   write ok", ms()),
            Err(e) => {
                println!("[{:>5} ms]   write FAILED: {e}", ms());
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
        }
        match t.read_frame(1000) {
            Ok(r) => {
                match frame::parse_reply(&r) {
                    Ok(rep) => {
                        let flags = rep.floats().get(44).map(|x| x.to_bits());
                        println!(
                            "[{:>5} ms]   reply uid {} ({} bytes), flag word {}",
                            ms(),
                            rep.uid,
                            r.len(),
                            flags.map_or("-".into(), |b| format!("0x{b:08X}"))
                        );
                    }
                    Err(e) => println!("[{:>5} ms]   unparsable reply: {e}", ms()),
                }
                return Ok(());
            }
            Err(e) => println!("[{:>5} ms]   no reply: {e}", ms()),
        }
    }
    println!("the mic never answered in this run");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("0xDC").unwrap(), 220);
        assert_eq!(parse_size("2028").unwrap(), 0x7EC);
        assert!(parse_size("zz").is_err());
    }

    #[test]
    fn default_requests_pass_safety() {
        for s in DEFAULT_SIZES {
            let f = frame::get_state_sized(1, s);
            let p = frame::decode_frame(&f).unwrap().payload;
            assert!(Command::check_payload(p, &SafetyContext::default()).is_ok());
        }
    }
}

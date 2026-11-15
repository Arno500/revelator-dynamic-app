//! revelator-cli: probe / dump-state / get / set / watch for the Revelator Dynamic.

mod ctl;
mod dsp;
mod mixer;
mod trace;

use std::process::ExitCode;

use revelator_core::Transport;
use revelator_usb::{describe_devices, NusbTransport, OpenError, Support};

const USAGE_TEMPLATE: &str = "\
revelator-cli - control the PreSonus/Fender Revelator Dynamic

USAGE:
    revelator-cli [--mock] <command> [args]

COMMANDS:
    diagnose              Windows only: show which driver each interface of the mic is bound to
    audio                 Windows only: list the active audio endpoints and their sample rates,
                          marking the ones recognised as the mic (by USB VID/PID, else by name)
    probe                 Dump USB descriptors, protocol version and max lengths (read-only)
    trace [sizes...]      Read-only wire trace: send the state request (default blob sizes
                          0x7EC and 0xDC) and hex-dump every byte the mic sends back
    dump-state            Read and print the full device state
    get <param>           Read one parameter
    set <param> <value>   Write one device parameter, then read the state back. Writable:
                          preamp.gain (0-60 dB), mic.mute (0/1), preset (0-3),
                          headphones.volume (0-1), headphones.mute (0/1),
                          main.volume (0-1), monitor.blend (-1 mic only .. 1 playback only),
                          mute.button (0 mutes headphones, 1 mutes mic),
                          preset.button (0 single, 1 double, 2 multi)
    watch [interval_ms]   Poll the state and print changes (default 250 ms)
    raw-state [size]      Print every non-zero raw state slot (hex/f32/i32); size default 0x7EC
    watch-raw [size] [interval_ms]
                          Poll the raw state and print every slot that changes; slots that
                          change on their own during the first 3 s (meters) are muted
    trace-write <param> <value>
                          Do one `set` write, then print a timestamped record of what the mic
                          does (diagnoses mic stalls after a write)
    meters [interval_ms] [all]
                          Print the level-meter slots live (`all` = slots 0-28), read-only;
                          these are what the GUI meters show
    readback-test         Write headphones.mute, mute.button and preset.button through their
                          values and print which state slots/bits change (to locate their
                          readback); ends on UC's defaults (unmuted, mic, multi)
{DSP_HELP}
{MIXER_HELP}
    presets dump [area...] [--save <dir>]   areas: 0-7 factory presets, 8-15 user presets, s0-s3 button slots
                          Read the preset slots (0-3, default all) from the mic and print
                          them (read-only; same transfer Universal Control uses)
    setup [--yes]         Windows only: one-time registration of a WinUSB interface GUID on the
                          vendor interface node (MI_05) only. Without --yes: prints the plan (dry run)
    setup --undo [--yes]  Remove that registration again

OPTIONS:
    --mock                Use the in-memory mock device instead of USB hardware
    -h, --help            Show this help

EXIT CODES:
    0 ok, 1 error, 2 no (supported) Revelator found";

/// List the Windows audio endpoints the sample-rate detection sees.
fn audio() -> Result<(), String> {
    use revelator_usb::audio_rate::{self, Match};
    let all = audio_rate::endpoints();
    if all.is_empty() {
        return Err("no active audio endpoints found (or Core Audio unavailable)".into());
    }
    for e in &all {
        let mark = match audio_rate::revelator_match(e) {
            Some(Match::UsbId) => "MIC (USB id)",
            Some(Match::Name) => "MIC (name)",
            None => "",
        };
        println!(
            "{:12} {:7} {:>6} Hz  {}
{:21}{}",
            mark,
            if e.capture { "input" } else { "output" },
            e.rate,
            e.name,
            "",
            e.instance.as_deref().unwrap_or("(no device node id)")
        );
    }
    match audio_rate::revelator_sample_rate() {
        Some(r) => println!(
            "
mic sample rate: {r} Hz"
        ),
        None => println!(
            "
mic sample rate: unknown (no endpoint recognised as the Revelator)"
        ),
    }
    Ok(())
}

fn usage() -> String {
    USAGE_TEMPLATE
        .replace("{DSP_HELP}", dsp::HELP)
        .replace("{MIXER_HELP}", mixer::HELP)
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mock = take_flag(&mut args, "--mock");
    if take_flag(&mut args, "-h") || take_flag(&mut args, "--help") || args.is_empty() {
        println!("{}", usage());
        return ExitCode::from(if args.is_empty() { 1 } else { 0 });
    }
    let cmd = args.remove(0);
    if !matches!(
        cmd.as_str(),
        "probe"
            | "audio"
            | "diagnose"
            | "setup"
            | "trace"
            | "dump-state"
            | "raw-state"
            | "watch-raw"
            | "readback-test"
            | "trace-write"
            | "meters"
            | "get"
            | "set"
            | "watch"
            | "dsp"
            | "dsp-get"
            | "presets"
            | "mixer"
    ) {
        eprintln!("unknown command '{cmd}'\n\n{}", usage());
        return ExitCode::from(1);
    }

    if cmd == "diagnose" {
        return finish(diagnose());
    }
    if cmd == "audio" {
        return finish(audio());
    }
    if cmd == "setup" {
        let yes = take_flag(&mut args, "--yes");
        let undo = take_flag(&mut args, "--undo");
        return setup(yes, undo);
    }

    let mut dsp_opts = None;
    if cmd == "dsp" {
        let o = match dsp::parse_opts(&mut args) {
            Ok(o) => o,
            Err(e) => return finish(Err(e)),
        };
        let sub = args.first().cloned().unwrap_or_default();
        let rest = args.get(1..).unwrap_or_default();
        match sub.as_str() {
            "list" => {
                dsp::list(&o);
                return ExitCode::SUCCESS;
            }
            "reset" => return finish(dsp::reset(&o)),
            "set" if o.dry => return finish(dsp::set_dry(&o, rest)),
            "push" if o.dry => {
                dsp::push_dry(&o);
                return ExitCode::SUCCESS;
            }
            "set" | "push" => {}
            _ => {
                return finish(Err(
                    "usage: dsp list | dsp set <key> <value>... | dsp push | dsp reset".into(),
                ))
            }
        }
        debug_assert!(dsp::needs_device(&sub, &o));
        dsp_opts = Some(o);
    }

    let mut mixer_opts = None;
    if cmd == "mixer" {
        let o = match mixer::parse_opts(&mut args) {
            Ok(o) => o,
            Err(e) => return finish(Err(e)),
        };
        let sub = args.first().cloned().unwrap_or_default();
        let rest = args.get(1..).unwrap_or_default();
        match sub.as_str() {
            "list" => {
                mixer::list(&o);
                return ExitCode::SUCCESS;
            }
            "set" if o.dry => return finish(mixer::set_dry(&o, rest)),
            "push" if o.dry => {
                mixer::push_dry(&o);
                return ExitCode::SUCCESS;
            }
            "set" | "push" => {}
            _ => {
                return finish(Err(
                    "usage: mixer list | mixer set <key> <value>... | mixer push".into(),
                ))
            }
        }
        debug_assert!(mixer::needs_device(&sub, &o));
        mixer_opts = Some(o);
    }

    if mock {
        let t = ctl::mock_transport();
        if let Some(o) = &dsp_opts {
            return finish(run_dsp(&args, t, o));
        }
        if let Some(o) = &mixer_opts {
            return finish(run_mixer(&args, t, o));
        }
        return finish(run(&cmd, &args, t, true));
    }

    // Real hardware.
    if cmd == "probe" {
        match describe_devices() {
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
            Ok(reports) if reports.is_empty() => {
                println!("no Revelator found (no USB device with VID 194F is attached)");
                return ExitCode::from(2);
            }
            Ok(reports) => {
                for r in &reports {
                    println!("{r}");
                }
                if !reports
                    .iter()
                    .any(|r| r.support == Some(Support::Supported))
                {
                    println!("no supported Revelator Dynamic found among the devices above");
                    return ExitCode::from(2);
                }
            }
        }
    }
    let t = match NusbTransport::open_first() {
        Ok(t) => t,
        Err(e @ OpenError::NoDevice) => {
            println!("no Revelator found");
            let _ = e;
            return ExitCode::from(2);
        }
        Err(e @ (OpenError::UnsupportedMode(_) | OpenError::UnsupportedModel(_))) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    println!("opened {}", t.description());
    if cmd == "trace" {
        let mut t = t;
        return finish(trace::run(&mut t, &args));
    }
    if cmd == "trace-write" {
        let mut t = t;
        return finish(trace::write_trace(&mut t, &args));
    }
    if let Some(o) = &dsp_opts {
        return finish(run_dsp(&args, t, o));
    }
    if let Some(o) = &mixer_opts {
        return finish(run_mixer(&args, t, o));
    }
    finish(run(&cmd, &args, t, false))
}

fn run_mixer<T: Transport + 'static>(args: &[String], t: T, o: &mixer::Opts) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("set") => mixer::set(t, o, &args[1..]),
        Some("push") => mixer::push(t, o),
        _ => unreachable!(),
    }
}

fn run_dsp<T: Transport + 'static>(args: &[String], t: T, o: &dsp::Opts) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("set") => dsp::set(t, o, &args[1..]),
        Some("push") => dsp::push(t, o),
        _ => unreachable!(),
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let n = args.len();
    args.retain(|a| a != flag);
    args.len() != n
}

fn finish(r: Result<(), String>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run<T: Transport + 'static>(
    cmd: &str,
    args: &[String],
    mut t: T,
    mock: bool,
) -> Result<(), String> {
    match cmd {
        "probe" => probe_link(&mut t, mock),
        "dump-state" => ctl::dump_state(t),
        "dsp-get" => ctl::dsp_get(t, args.first().map(String::as_str).unwrap_or("")),
        "readback-test" => ctl::readback_test(t),
        "meters" => ctl::meters(
            t,
            args.iter().find_map(|s| s.parse().ok()).unwrap_or(200),
            args.iter().any(|a| a == "all"),
        ),
        "get" => ctl::get(t, args.first().ok_or("usage: get <param>")?),
        "set" => match args {
            [p, v] => ctl::set(t, p, v),
            _ => Err("usage: set <param> <value>".into()),
        },
        "watch" => {
            let ms = match args.first() {
                Some(s) => s
                    .parse::<u64>()
                    .map_err(|_| "interval must be milliseconds")?,
                None => 250,
            };
            ctl::watch(t, ms)
        }
        "raw-state" => ctl::raw_state(t, ctl::parse_size(args.first())?),
        "watch-raw" => {
            let ms = match args.get(1) {
                Some(s) => s
                    .parse::<u64>()
                    .map_err(|_| "interval must be milliseconds")?,
                None => 100,
            };
            ctl::watch_raw(t, ctl::parse_size(args.first())?, ms, 3000)
        }
        "presets" => {
            let mut a = args.to_vec();
            let save = match a.iter().position(|x| x == "--save") {
                Some(i) if i + 1 < a.len() => {
                    let d = a.remove(i + 1);
                    a.remove(i);
                    Some(std::path::PathBuf::from(d))
                }
                Some(_) => return Err("--save needs a directory".into()),
                None => None,
            };
            if a.first().map(String::as_str) != Some("dump") {
                return Err("usage: presets dump [area...] [--save <dir>]".into());
            }
            let slots = if a.len() > 1 {
                a[1..]
                    .iter()
                    .map(|x| {
                        use revelator_core::frame::tag;
                        match (x.strip_prefix('s'), x.parse::<u32>()) {
                            (Some(n), _) => match n.parse::<u32>() {
                                Ok(n) if n <= 3 => Ok((tag::STAT, n)),
                                _ => Err(format!("bad button slot '{x}' (s0-s3)")),
                            },
                            (None, Ok(n)) if n <= 15 => Ok((tag::PRSM, n)),
                            _ => Err(format!("bad area '{x}' (0-15 or s0-s3)")),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                use revelator_core::frame::tag;
                let mut v: Vec<(u32, u32)> = (0..4).map(|n| (tag::STAT, n)).collect();
                v.extend((8..16).map(|n| (tag::PRSM, n)));
                v
            };
            ctl::presets_dump(t, &slots, save.as_deref())
        }
        "trace" | "trace-write" => {
            Err("trace needs real hardware; it does not run with --mock".into())
        }
        _ => unreachable!(),
    }
}

/// Protocol version and max command/response length via vendor IN control requests.
fn probe_link<T: Transport>(t: &mut T, mock: bool) -> Result<(), String> {
    if mock {
        println!("(mock transport)");
    }
    let ver = t
        .vendor_in(0, 0, 2)
        .map_err(|e| format!("protocol version query failed: {e}"))?;
    match ver.as_slice() {
        [a, b, ..] => println!("protocol version: {}", u16::from_le_bytes([*a, *b])),
        _ => println!("protocol version: short reply {ver:02X?}"),
    }
    for (value, label) in [(0u16, "max command length"), (1, "max response length")] {
        let r = t
            .vendor_in(1, value, 4)
            .map_err(|e| format!("{label} query failed: {e}"))?;
        match r.as_slice() {
            [a, b, c, d, ..] => println!("{label}: {}", u32::from_le_bytes([*a, *b, *c, *d])),
            _ => println!("{label}: short reply {r:02X?}"),
        }
    }
    Ok(())
}

/// Read-only Windows driver-binding report (runs PowerShell; changes nothing).
fn diagnose() -> Result<(), String> {
    if !cfg!(windows) {
        return Err("diagnose is only available on Windows".into());
    }
    const SCRIPT: &str = r#"
$ErrorActionPreference='SilentlyContinue'
"== PnP nodes for VID_194F (parent device and one node per USB interface) =="
Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like 'USB\VID_194F*' } | ForEach-Object {
  $id=$_.InstanceId
  $svc=(Get-PnpDeviceProperty -InstanceId $id -KeyName 'DEVPKEY_Device_Service').Data
  $inf=(Get-PnpDeviceProperty -InstanceId $id -KeyName 'DEVPKEY_Device_DriverInfPath').Data
  $prov=(Get-PnpDeviceProperty -InstanceId $id -KeyName 'DEVPKEY_Device_DriverProvider').Data
  $prob=(Get-PnpDeviceProperty -InstanceId $id -KeyName 'DEVPKEY_Device_ProblemCode').Data
  $reg="HKLM:\SYSTEM\CurrentControlSet\Enum\$id\Device Parameters"
  $g1=(Get-ItemProperty $reg).DeviceInterfaceGUIDs; $g2=(Get-ItemProperty $reg).DeviceInterfaceGUID
  "{0}`n    name='{1}' status={2} problem={3}`n    service='{4}' inf='{5}' provider='{6}'`n    DeviceInterfaceGUID(s)='{7} {8}'" -f $id,$_.FriendlyName,$_.Status,$prob,$svc,$inf,$prov,($g1 -join ','),$g2
}
"== Device interfaces registered for the mic's nodes (class GUID -> node) =="
Get-ChildItem 'HKLM:\SYSTEM\CurrentControlSet\Control\DeviceClasses' | ForEach-Object {
  $cls=$_.PSChildName
  Get-ChildItem $_.PSPath | Where-Object { $_.PSChildName -like '*VID_194F&PID_0D0A*' } | ForEach-Object { "{0}  ->  {1}" -f $cls,$_.PSChildName }
}
"== Raw registry values for the vendor interface node (MI_05) =="
Get-ChildItem 'HKLM:\SYSTEM\CurrentControlSet\Enum\USB' | Where-Object { $_.PSChildName -like 'VID_194F&PID_0D0A&MI_05' } | ForEach-Object {
  Get-ChildItem $_.PSPath | ForEach-Object {
    "node: " + $_.Name
    (Get-ItemProperty $_.PSPath).PSObject.Properties | Where-Object { $_.Name -notlike 'PS*' } | ForEach-Object { "  {0} = {1}" -f $_.Name,($_.Value -join ' ; ') }
    $dp=Join-Path $_.PSPath 'Device Parameters'
    if (Test-Path $dp) { "  [Device Parameters]"; (Get-ItemProperty $dp).PSObject.Properties | Where-Object { $_.Name -notlike 'PS*' } | ForEach-Object { "    {0} = {1}" -f $_.Name,($_.Value -join ' ; ') } }
  }
}
"== Installed PreSonus/Fender USB drivers =="
Get-CimInstance Win32_PnPSignedDriver | Where-Object { $_.DeviceID -like 'USB\VID_194F*' -or $_.DriverProviderName -match 'PreSonus|Fender Musical' } | ForEach-Object { "{0} | {1} | {2} | {3}" -f $_.DeviceID,$_.DeviceName,$_.DriverVersion,$_.InfName }
"== Universal Control / PreSonus services and processes =="
Get-Process | Where-Object { $_.ProcessName -match 'Universal Control|PreSonus|^Fender' } | ForEach-Object { "process: " + $_.ProcessName }
Get-Service | Where-Object { $_.Name -match 'PreSonus|Fender Musical|PaeDSPUSB' -or $_.DisplayName -match 'PreSonus|Fender Musical' } | ForEach-Object { "service: {0} ({1})" -f $_.Name,$_.Status }
"#;
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .output()
        .map_err(|e| format!("could not run powershell: {e}"))?;
    print!("{}", String::from_utf8_lossy(&out.stdout));
    Ok(())
}

/// `setup` / `setup --undo`: see `revelator_usb::winusb_setup`.
fn setup(yes: bool, undo: bool) -> ExitCode {
    use revelator_usb::winusb_setup as ws;
    let node = match ws::find_vendor_node() {
        Ok(n) => n,
        Err(ws::SetupError::NodeNotFound) => {
            println!("{}", ws::SetupError::NodeNotFound);
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    println!("{}", ws::plan_text(&node, undo));
    let needed = match if undo {
        ws::check_unregister(&node)
    } else {
        ws::check_register(&node)
    } {
        Ok(n) => n,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    if !needed {
        return ExitCode::SUCCESS;
    }
    if !yes {
        println!("Dry run only: nothing was changed. Re-run with --yes (from an elevated terminal) to apply.");
        return ExitCode::SUCCESS;
    }
    if !ws::is_elevated() {
        eprintln!("error: {}", ws::SetupError::NotElevated);
        return ExitCode::from(1);
    }
    let r = if undo {
        ws::unregister_guid(&node)
    } else {
        ws::register_guid(&node)
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    println!(
        "{} applied; waiting for the device node to come back...",
        if undo { "undo" } else { "registration" }
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut back = false;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Ok(n) = ws::find_vendor_node() {
            if n.is_ours() != undo && n.service.is_some() {
                back = true;
                break;
            }
        }
    }
    if !back {
        eprintln!("warning: the node did not report back within 20 s; replug the mic or reboot, then run `revelator-cli probe`");
        return ExitCode::from(1);
    }
    if undo {
        println!("done: registration removed.");
        return ExitCode::SUCCESS;
    }
    println!("node is back; trying to open the control interface (same as `probe`)...");
    let mut last = None;
    for _ in 0..20 {
        match NusbTransport::open_first() {
            Ok(mut t) => {
                println!("opened {}", t.description());
                return match probe_link(&mut t, false) {
                    Ok(()) => {
                        println!("success: the app can now talk to the mic.");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("error: opened, but the link check failed: {e}");
                        ExitCode::from(1)
                    }
                };
            }
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    eprintln!(
        "error: registration done but opening still fails: {}",
        last.map(|e| e.to_string()).unwrap_or_default()
    );
    ExitCode::from(1)
}

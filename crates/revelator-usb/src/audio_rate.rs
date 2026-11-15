//! Sample rate of the Revelator's Windows audio endpoints, from the Core Audio API.
//!
//! The mic does not report its sample rate over the control interface. Windows knows it: each
//! active endpoint (`IMMDeviceEnumerator`) has a shared-mode mix format (`IAudioClient::GetMixFormat`)
//! that follows the "Default Format" in Sound settings. [`RateWatcher`] also registers an
//! `IMMNotificationClient`, so a format change, unplug or replug is picked up as it happens.
//!
//! Endpoints are matched to the mic by the USB VID/PID of the device node they belong to (the
//! endpoint property `{b3f8fa53-0004-438e-9003-51a46e139bfc},2`), with the same IDs the safety
//! layer accepts: `{1}.USB\VID_194F&PID_0D0A&MI_00\...` with the Windows class driver, or
//! `{1}.TUSBAUDIO_ENUM\VID_194F&PID_0D0A&MI_00&KS\...` with the PreSonus driver. Only if that
//! node has no USB VID/PID (a driver-created bus) does the name ("Revelator") decide.

use revelator_core::safety::{SUPPORTED_PIDS, VENDOR_ID};

/// One active audio endpoint, as Windows reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// "Microphone (Revelator Dynamic)"
    pub name: String,
    /// Device node instance ID behind the endpoint (`{1}.USB\VID_...`), if reported.
    pub instance: Option<String>,
    /// Shared-mode mix rate.
    pub rate: u32,
    pub capture: bool,
}

/// `(vid, pid)` from a device instance ID such as `{1}.USB\VID_194F&PID_0D0A&MI_00\7&...`.
pub fn usb_ids(instance: &str) -> Option<(u16, u16)> {
    let up = instance.to_ascii_uppercase();
    let hex = |tag: &str| {
        let i = up.find(tag)? + tag.len();
        u16::from_str_radix(up.get(i..i + 4)?, 16).ok()
    };
    Some((hex("VID_")?, hex("PID_")?))
}

/// How an endpoint was recognised as the mic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    UsbId,
    Name,
}

/// Is this endpoint the Revelator? By VID/PID when its device node has one, else by name.
pub fn revelator_match(e: &Endpoint) -> Option<Match> {
    match e.instance.as_deref().and_then(usb_ids) {
        Some((vid, pid)) => {
            (vid == VENDOR_ID && SUPPORTED_PIDS.contains(&pid)).then_some(Match::UsbId)
        }
        None => e
            .name
            .to_lowercase()
            .contains("revelator")
            .then_some(Match::Name),
    }
}

/// The Windows default capture endpoints (default and default-communications device).
pub fn default_capture() -> Vec<Endpoint> {
    imp::default_capture().unwrap_or_default()
}

/// If a default input is the Revelator's plain mic (Mic L/R) rather than a Stream Mix, its name.
///
/// The mic's Voice FX and reverb are produced inside its mixer, which feeds the Main and Stream
/// Mix A/B outputs. The plain mic capture device does not pass through the mixer, so an app
/// recording from it (Discord, a game chat) never hears the effects.
pub fn plain_mic_default(defaults: &[Endpoint]) -> Option<String> {
    defaults.iter().find_map(|e| {
        revelator_match(e)?;
        let n = e.name.to_lowercase();
        let mixed = ["stream", "mix", "virtual"].iter().any(|k| n.contains(k));
        (e.capture && !mixed).then(|| e.name.clone())
    })
}

/// One rate for all of the Revelator's active endpoints. Endpoints should agree; if they do not,
/// the highest wins, so the Delay hazard check stays on the safe side.
pub fn pick_rate(rates: &[u32]) -> Option<u32> {
    rates.iter().copied().max()
}

fn revelator_rate(all: &[Endpoint]) -> Option<u32> {
    let rates: Vec<u32> = all
        .iter()
        .filter(|e| revelator_match(e).is_some())
        .map(|e| e.rate)
        .collect();
    pick_rate(&rates)
}

/// Every active audio endpoint (empty if Core Audio is unavailable or not Windows).
pub fn endpoints() -> Vec<Endpoint> {
    imp::endpoints().unwrap_or_default()
}

/// Current rate of the active Revelator endpoints, or `None` if none is found.
pub fn revelator_sample_rate() -> Option<u32> {
    revelator_rate(&endpoints())
}

/// Keeps the Revelator's sample rate up to date in the background (Core Audio notifications).
pub struct RateWatcher {
    rx: std::sync::mpsc::Receiver<Option<u32>>,
    current: Option<u32>,
    stop: Option<std::sync::mpsc::Sender<imp::Event>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RateWatcher {
    pub fn start() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let (stop, thread) = imp::spawn(tx, revelator_rate);
        RateWatcher {
            rx,
            current: None,
            stop,
            thread,
        }
    }

    /// Latest known rate (non-blocking).
    pub fn current(&mut self) -> Option<u32> {
        while let Ok(r) = self.rx.try_recv() {
            self.current = r;
        }
        self.current
    }
}

impl Drop for RateWatcher {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(imp::Event::Stop);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::Endpoint;
    use std::sync::mpsc::{channel, Sender};
    use windows::core::{implement, Interface, Result, GUID, PCWSTR};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        eAll, eCapture, eCommunications, eConsole, EDataFlow, ERole, IAudioClient, IMMDevice,
        IMMDeviceEnumerator, IMMEndpoint, IMMNotificationClient, IMMNotificationClient_Impl,
        MMDeviceEnumerator, DEVICE_STATE, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;

    /// Device node instance ID of the endpoint's device (`{1}.USB\VID_...`).
    const PKEY_ENDPOINT_DEVNODE: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xb3f8fa53_0004_438e_9003_51a46e139bfc),
        pid: 2,
    };

    pub enum Event {
        Changed,
        Stop,
    }

    /// COM for the current thread (multithreaded apartment); undone on drop.
    struct Com(bool);
    impl Com {
        fn init() -> Self {
            // S_FALSE = already initialised (still needs CoUninitialize); RPC_E_CHANGED_MODE =
            // the thread is STA: COM works, but this call must not be balanced.
            Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn enumerator() -> Result<IMMDeviceEnumerator> {
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
    }

    fn prop(dev: &IMMDevice, key: &PROPERTYKEY) -> Option<String> {
        let store = unsafe { dev.OpenPropertyStore(STGM_READ) }.ok()?;
        let v = unsafe { store.GetValue(key) }.ok()?;
        let s = v.to_string();
        (!s.is_empty()).then_some(s)
    }

    fn mix_rate(dev: &IMMDevice) -> Option<u32> {
        let client: IAudioClient = unsafe { dev.Activate(CLSCTX_ALL, None) }.ok()?;
        let fmt = unsafe { client.GetMixFormat() }.ok()?;
        if fmt.is_null() {
            return None;
        }
        let hz = unsafe { (*fmt).nSamplesPerSec };
        unsafe { CoTaskMemFree(Some(fmt as *const _)) };
        Some(hz)
    }

    fn list(en: &IMMDeviceEnumerator) -> Result<Vec<Endpoint>> {
        let col = unsafe { en.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE) }?;
        let mut out = Vec::new();
        for i in 0..unsafe { col.GetCount() }? {
            let Ok(dev) = (unsafe { col.Item(i) }) else {
                continue;
            };
            let Some(rate) = mix_rate(&dev) else { continue };
            let capture = dev
                .cast::<IMMEndpoint>()
                .and_then(|e| unsafe { e.GetDataFlow() })
                .is_ok_and(|f| f == eCapture);
            out.push(Endpoint {
                name: prop(&dev, &PKEY_Device_FriendlyName).unwrap_or_default(),
                instance: prop(&dev, &PKEY_ENDPOINT_DEVNODE),
                rate,
                capture,
            });
        }
        Ok(out)
    }

    pub fn default_capture() -> Result<Vec<Endpoint>> {
        let _com = Com::init();
        let en = enumerator()?;
        let mut out: Vec<Endpoint> = Vec::new();
        for role in [eConsole, eCommunications] {
            let Ok(dev) = (unsafe { en.GetDefaultAudioEndpoint(eCapture, role) }) else {
                continue;
            };
            let e = Endpoint {
                name: prop(&dev, &PKEY_Device_FriendlyName).unwrap_or_default(),
                instance: prop(&dev, &PKEY_ENDPOINT_DEVNODE),
                rate: mix_rate(&dev).unwrap_or(0),
                capture: true,
            };
            if !out.contains(&e) {
                out.push(e);
            }
        }
        Ok(out)
    }

    pub fn endpoints() -> Result<Vec<Endpoint>> {
        let _com = Com::init();
        list(&enumerator()?)
    }

    /// Forwards every endpoint event; the watcher thread re-queries (cheap, a few endpoints).
    #[implement(IMMNotificationClient)]
    struct Notifier(Sender<Event>);

    impl IMMNotificationClient_Impl for Notifier_Impl {
        fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
            let _ = self.0.send(Event::Changed);
            Ok(())
        }
        fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
            let _ = self.0.send(Event::Changed);
            Ok(())
        }
        fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
            let _ = self.0.send(Event::Changed);
            Ok(())
        }
        fn OnDefaultDeviceChanged(&self, _: EDataFlow, _: ERole, _: &PCWSTR) -> Result<()> {
            Ok(())
        }
        fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
            // Fires for PKEY_AudioEngine_DeviceFormat when the default format is changed.
            let _ = self.0.send(Event::Changed);
            Ok(())
        }
    }

    pub fn spawn(
        out: Sender<Option<u32>>,
        pick: fn(&[Endpoint]) -> Option<u32>,
    ) -> (Option<Sender<Event>>, Option<std::thread::JoinHandle<()>>) {
        let (tx, rx) = channel();
        let ev = tx.clone();
        let t = std::thread::Builder::new()
            .name("audio-rate-watch".into())
            .spawn(move || {
                let _com = Com::init();
                let Ok(en) = enumerator() else {
                    let _ = out.send(None);
                    return;
                };
                let client: IMMNotificationClient = Notifier(ev).into();
                let registered =
                    unsafe { en.RegisterEndpointNotificationCallback(&client) }.is_ok();
                let rate = || pick(&list(&en).unwrap_or_default());
                let mut last = rate();
                let _ = out.send(last);
                while let Ok(e) = rx.recv() {
                    if let Event::Stop = e {
                        break;
                    }
                    // Events come in bursts (one per property); collapse what is queued.
                    let mut stop = false;
                    while let Ok(e) = rx.try_recv() {
                        stop |= matches!(e, Event::Stop);
                    }
                    if stop {
                        break;
                    }
                    let now = rate();
                    if now != last {
                        last = now;
                        if out.send(now).is_err() {
                            break;
                        }
                    }
                }
                if registered {
                    let _ = unsafe { en.UnregisterEndpointNotificationCallback(&client) };
                }
            })
            .ok();
        (Some(tx), t)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Endpoint;
    use std::sync::mpsc::Sender;

    pub enum Event {
        Stop,
    }

    pub fn endpoints() -> Result<Vec<Endpoint>, ()> {
        Ok(Vec::new())
    }

    pub fn default_capture() -> Result<Vec<Endpoint>, ()> {
        Ok(Vec::new())
    }

    pub fn spawn(
        out: Sender<Option<u32>>,
        _: fn(&[Endpoint]) -> Option<u32>,
    ) -> (Option<Sender<Event>>, Option<std::thread::JoinHandle<()>>) {
        let _ = out.send(None);
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(name: &str, instance: Option<&str>, rate: u32) -> Endpoint {
        Endpoint {
            name: name.into(),
            instance: instance.map(Into::into),
            rate,
            capture: true,
        }
    }

    #[test]
    fn plain_mic_default_is_flagged_but_stream_mix_is_not() {
        let id = Some(r"{1}.USB\VID_194F&PID_0D0A&MI_00\7&1");
        let mic = ep("Mic L/R (Revelator Dynamic)", id, 48000);
        let stream = ep("Stream Mix A (Revelator Dynamic)", id, 48000);
        let other = ep(
            "Microphone (Realtek)",
            Some(r"{1}.HDAUDIO\FUNC_01&VEN_10EC"),
            48000,
        );
        assert_eq!(
            plain_mic_default(std::slice::from_ref(&mic)).as_deref(),
            Some("Mic L/R (Revelator Dynamic)")
        );
        assert_eq!(plain_mic_default(std::slice::from_ref(&stream)), None);
        assert_eq!(plain_mic_default(&[other]), None);
        assert!(plain_mic_default(&[stream, mic]).is_some());
    }

    #[test]
    fn parses_usb_ids() {
        assert_eq!(
            usb_ids(r"{1}.USB\VID_041E&PID_3271&MI_03\7&1230BB1A&0&0003"),
            Some((0x041E, 0x3271))
        );
        assert_eq!(
            usb_ids(r"{1}.usb\vid_194f&pid_0d0a&mi_00\x"),
            Some((0x194F, 0x0D0A))
        );
        assert_eq!(usb_ids(r"{1}.SWD\MMDEVAPI\x"), None);
        assert_eq!(usb_ids(r"USB\VID_19"), None);
    }

    #[test]
    fn matches_by_usb_id_then_name() {
        // PreSonus (Thesycon) driver: endpoints hang off its KS child node (paedspusbks.inf).
        let presonus = ep(
            "Microphone (Revelator Dynamic)",
            Some(r"{1}.TUSBAUDIO_ENUM\VID_194F&PID_0D0A&MI_00&KS\8&2"),
            48_000,
        );
        assert_eq!(revelator_match(&presonus), Some(Match::UsbId));
        let rev = ep(
            "Microphone (Revelator Dynamic)",
            Some(r"{1}.USB\VID_194F&PID_0D0A&MI_00\7&1"),
            48_000,
        );
        assert_eq!(revelator_match(&rev), Some(Match::UsbId));
        let other_pid = ep(
            "Revelator io24",
            Some(r"{1}.USB\VID_194F&PID_0D01&MI_00\7&1"),
            96_000,
        );
        assert_eq!(revelator_match(&other_pid), None); // another PreSonus device: IDs win over name
        let renamed = ep(
            "My mic",
            Some(r"{1}.USB\VID_194F&PID_0D04&MI_00\7&1"),
            44_100,
        );
        assert_eq!(revelator_match(&renamed), Some(Match::UsbId));
        let vendor_bus = ep(
            "Microphone (Revelator Dynamic)",
            Some(r"{1}.PSUSB\AUDIO\1"),
            48_000,
        );
        assert_eq!(revelator_match(&vendor_bus), Some(Match::Name));
        let webcam = ep(
            "Microphone (C922)",
            Some(r"{1}.USB\VID_046D&PID_085C&MI_02\7&2"),
            32_000,
        );
        assert_eq!(revelator_match(&webcam), None);
        assert_eq!(
            revelator_rate(&[rev, other_pid, renamed, webcam]),
            Some(48_000)
        );
    }

    #[test]
    fn highest_rate_wins() {
        assert_eq!(pick_rate(&[]), None);
        assert_eq!(pick_rate(&[48_000, 96_000]), Some(96_000));
    }

    #[test]
    fn query_and_watcher_do_not_fail() {
        // No Revelator needed: whatever is found must be plausible, and the watcher must start
        // and stop cleanly.
        let all = endpoints();
        for e in &all {
            assert!((8_000..=768_000).contains(&e.rate), "{e:?}");
        }
        let mut w = RateWatcher::start();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(w.current(), revelator_sample_rate());
        drop(w);
    }
}

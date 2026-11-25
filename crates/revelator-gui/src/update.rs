//! "Update available" check: asks GitHub for the latest release now and then (on start, then
//! daily), and when it is newer than this build shows a Windows notification (toast) plus a
//! banner in the window. Nothing is downloaded or installed: the notification opens the
//! release page. The check can be turned off, and a version can be skipped.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

const LATEST_URL: &str =
    "https://api.github.com/repos/Arno500/revelator-dynamic-app/releases/latest";
/// Release pages only ever live here: anything else from the API is ignored.
const URL_PREFIX: &str = "https://github.com/Arno500/revelator-dynamic-app/";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Wait before the first check, so startup (and the device connection) is not competing.
const START_DELAY: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
/// After a failed check (offline, rate limited).
const RETRY_AFTER: Duration = Duration::from_secs(3600);

const AUMID: &str = "RevelatorControl.App";

/// A published release newer than this build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

/// Saved choices (`%APPDATA%\revelator-control\update.json`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Config {
    enabled: bool,
    /// Version the user chose to ignore.
    skipped: Option<String>,
    /// Last version a notification was shown for (one notification per release).
    notified: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            skipped: None,
            notified: None,
        }
    }
}

impl Config {
    fn parse(text: &str) -> Config {
        let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Config {
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            skipped: s("skipped"),
            notified: s("notified"),
        }
    }

    fn to_json(&self) -> String {
        serde_json::json!({
            "enabled": self.enabled,
            "skipped": self.skipped,
            "notified": self.notified,
        })
        .to_string()
    }
}

fn config_path() -> PathBuf {
    crate::presets::base_dir().join("update.json")
}

struct State {
    cfg: Config,
    available: Option<Release>,
}

pub struct Updater {
    state: Mutex<State>,
    /// Wakes the checker thread for an immediate check.
    kick: Mutex<Option<mpsc::Sender<()>>>,
}

impl Updater {
    pub fn new() -> Arc<Updater> {
        let cfg = std::fs::read_to_string(config_path())
            .map(|t| Config::parse(&t))
            .unwrap_or_default();
        Arc::new(Updater {
            state: Mutex::new(State {
                cfg,
                available: None,
            }),
            kick: Mutex::new(None),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(cfg: &Config) {
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, cfg.to_json());
    }

    pub fn enabled(&self) -> bool {
        self.lock().cfg.enabled
    }

    /// Turn the periodic check on (checking right away) or off.
    pub fn set_enabled(&self, on: bool) {
        {
            let mut s = self.lock();
            s.cfg.enabled = on;
            if !on {
                s.available = None;
            }
            Self::save(&s.cfg);
        }
        if on {
            if let Some(tx) = self.kick.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                let _ = tx.send(());
            }
        }
    }

    /// The newer release to offer, if any.
    pub fn available(&self) -> Option<Release> {
        self.lock().available.clone()
    }

    /// Stop offering the release currently shown (a later one is offered again).
    pub fn skip(&self) {
        let mut s = self.lock();
        if let Some(r) = s.available.take() {
            s.cfg.skipped = Some(r.version);
            Self::save(&s.cfg);
        }
    }

    /// Start the checker thread. `wake` repaints the window when a release was found.
    pub fn spawn(self: &Arc<Self>, wake: impl Fn() + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        *self.kick.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                toast_init();
                let mut wait = START_DELAY;
                loop {
                    match rx.recv_timeout(wait) {
                        Err(RecvTimeoutError::Disconnected) => return,
                        Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                    }
                    if !me.enabled() {
                        wait = CHECK_EVERY;
                        continue;
                    }
                    wait = match fetch_latest() {
                        Ok(latest) => {
                            me.found(latest, &wake);
                            CHECK_EVERY
                        }
                        Err(_) => RETRY_AFTER,
                    };
                }
            });
    }

    fn found(&self, latest: Release, wake: &impl Fn()) {
        let notify = {
            let mut s = self.lock();
            if !s.cfg.enabled
                || !is_newer(CURRENT, &latest.version)
                || s.cfg.skipped.as_deref() == Some(&latest.version)
            {
                s.available = None;
                return;
            }
            s.available = Some(latest.clone());
            let first = s.cfg.notified.as_deref() != Some(&latest.version);
            if first {
                s.cfg.notified = Some(latest.version.clone());
                Self::save(&s.cfg);
            }
            first
        };
        if notify {
            let _ = toast(
                "Revelator Control update available",
                &format!(
                    "Version {} is out (you have {CURRENT}). Click to open the download page.",
                    latest.version
                ),
                &latest.url,
            );
        }
        wake();
    }
}

/// The latest published (non-draft, non-prerelease) release, whatever its version.
fn fetch_latest() -> Result<Release, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    let agent = ureq::AgentBuilder::new()
        .tls_connector(Arc::new(tls))
        .timeout(Duration::from_secs(15))
        .build();
    let body = agent
        .get(LATEST_URL)
        .set("User-Agent", &format!("revelator-control/{CURRENT}"))
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    parse_release(&body)
}

fn parse_release(body: &str) -> Result<Release, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    if flag("draft") || flag("prerelease") {
        return Err("not a stable release".into());
    }
    let version = s("tag_name")
        .map(|t| t.trim_start_matches('v').to_string())
        .ok_or("no tag_name")?;
    let url = s("html_url").ok_or("no html_url")?;
    if parse_version(&version).is_none() || !url.starts_with(URL_PREFIX) {
        return Err("unexpected release data".into());
    }
    Ok(Release {
        version,
        url: url.to_string(),
    })
}

/// `1.2.3` or `1.2.3-beta.1` as a sortable key: a pre-release sorts before its release.
fn parse_version(v: &str) -> Option<(u64, u64, u64, bool)> {
    let v = v.trim().trim_start_matches('v');
    let (core, pre) = match v.split_once('-') {
        Some((c, _)) => (c, true),
        None => (v, false),
    };
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let key = (it.next()??, it.next()??, it.next()??, !pre);
    it.next().is_none().then_some(key)
}

fn is_newer(current: &str, latest: &str) -> bool {
    matches!((parse_version(current), parse_version(latest)), (Some(c), Some(l)) if l > c)
}

// ---- Windows toast ----------------------------------------------------------------------

fn toast_init() {
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    // SAFETY: plain COM initialisation of this thread; a failure (already initialised in
    // another mode) only means the toast may not show.
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Show a notification whose click opens `url` (protocol activation: handled by Windows, so
/// it also works from the notification center after the app's popup is gone).
fn toast(title: &str, body: &str, url: &str) -> windows::core::Result<()> {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    // An unpackaged app needs a registered app id for its name to show on the toast.
    if let Ok(key) =
        crate::autostart::Key::create(&format!(r"Software\Classes\AppUserModelId\{AUMID}"))
    {
        let _ = key.set(
            "DisplayName",
            crate::autostart::REG_SZ,
            &crate::autostart::reg_sz("Revelator Control"),
        );
    }
    let xml = format!(
        r#"<toast activationType="protocol" launch="{}"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual></toast>"#,
        xml_escape(url),
        xml_escape(title),
        xml_escape(body)
    );
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert!(is_newer("0.2.1", "0.2.2"));
        assert!(is_newer("0.2.1", "0.10.0"));
        assert!(is_newer("0.2.1", "1.0.0"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("0.2.1-beta.1", "0.2.1"));
        assert!(!is_newer("0.2.1", "0.2.1-beta.1"));
        assert!(is_newer("0.2.1", "v0.3.0"));
        assert!(!is_newer("0.2.1", "garbage"));
        assert!(!is_newer("0.2.1", "1.2"));
        assert!(!is_newer("0.2.1", "1.2.3.4"));
    }

    #[test]
    fn release_json() {
        let ok = r#"{"tag_name":"v0.3.0","html_url":"https://github.com/Arno500/revelator-dynamic-app/releases/tag/v0.3.0","draft":false,"prerelease":false}"#;
        assert_eq!(
            parse_release(ok).unwrap(),
            Release {
                version: "0.3.0".into(),
                url: "https://github.com/Arno500/revelator-dynamic-app/releases/tag/v0.3.0".into()
            }
        );
        assert!(parse_release(&ok.replace("\"prerelease\":false", "\"prerelease\":true")).is_err());
        assert!(parse_release(&ok.replace("github.com/Arno500", "evil.example/x")).is_err());
        assert!(parse_release(&ok.replace("v0.3.0\",", "nightly\",")).is_err());
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn config_round_trip() {
        assert_eq!(Config::parse(""), Config::default());
        let c = Config {
            enabled: false,
            skipped: Some("0.3.0".into()),
            notified: Some("0.2.5".into()),
        };
        assert_eq!(Config::parse(&c.to_json()), c);
    }

    /// Manual check against the real network and desktop:
    /// `cargo test -p revelator-gui manual_toast -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn manual_toast_and_fetch() {
        println!("latest release: {:?}", fetch_latest());
        toast_init();
        toast(
            "Revelator Control update available",
            "Test notification: click to open the download page.",
            "https://github.com/Arno500/revelator-dynamic-app/releases",
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(3));
    }

    #[test]
    fn toast_text_is_escaped() {
        assert_eq!(xml_escape(r#"a<b>&"c""#), "a&lt;b&gt;&amp;&quot;c&quot;");
    }
}

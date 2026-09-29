//! Find Anki, install the add-ons the Anki page needs, and start or stop Anki.
//!
//! Two add-ons go into Anki's addons21 folder:
//!   2055492159             AnkiConnect, from AnkiWeb: the local API the app talks to. A fresh install
//!                          gets a random API key, which the app reads back from the add-on's settings.
//!   canvas_mcp_companion   ours (assets/anki_companion): opens Anki in the tray when the app starts it.
//! An AnkiConnect that's already installed is left as it is. Anki loads add-ons only when it starts,
//! so installing while Anki runs means restarting it.
//!
//! AnkiConnect listens on 127.0.0.1:8765 unless its webBindPort setting says otherwise. When another
//! program already has that port, the app moves AnkiConnect to a free one (set_port) and then finds it
//! there by reading the same setting (url).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::signin::{flatpak_has, which};
use crate::util::token_urlsafe;
use crate::{Error, Result, config};

pub const ANKICONNECT_ID: &str = "2055492159";
pub const COMPANION_DIR: &str = "canvas_mcp_companion";
pub const COMPANION_INIT: &str = include_str!("../../../assets/anki_companion/__init__.py");
pub const COMPANION_MANIFEST: &str = include_str!("../../../assets/anki_companion/manifest.json");
const FLATPAK_ID: &str = "net.ankiweb.Anki";
const DEFAULT_POINT: i64 = 250200; // asks AnkiWeb for a build for Anki 25.02+ when the version can't be read
pub const DEFAULT_PORT: u16 = 8765;
const SPARE_PORTS: std::ops::Range<u16> = 8766..8800;

// --- where Anki is -----------------------------------------------------------------------------------
fn windows_anki() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let exe = PathBuf::from(local).join("Programs").join("Anki").join("anki.exe");
    exe.exists().then_some(exe)
}

pub fn launcher() -> Option<Vec<String>> {
    if let Some(exe) = which("anki").or_else(windows_anki) {
        return Some(vec![exe.to_string_lossy().into_owned()]);
    }
    if flatpak_has(FLATPAK_ID) {
        return Some(vec!["flatpak".into(), "run".into(), FLATPAK_ID.into()]);
    }
    None
}

/// Anki's data folder (profiles, addons21). The first that exists, else the platform default.
pub fn base_dir() -> PathBuf {
    if let Ok(env) = std::env::var("ANKI_BASE") {
        if !env.is_empty() {
            return config::expand_user(&env);
        }
    }
    let home = config::home();
    let candidates = if cfg!(target_os = "macos") {
        vec![home.join("Library/Application Support/Anki2")]
    } else if cfg!(windows) {
        vec![std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| home.join("AppData/Roaming")).join("Anki2")]
    } else {
        let data = std::env::var("XDG_DATA_HOME").ok().filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
        let mut c = vec![data.join("Anki2"), home.join(".var/app").join(FLATPAK_ID).join("data/Anki2")];
        if launcher().map(|l| l[0] == "flatpak").unwrap_or(false) {
            c.reverse();
        }
        c
    };
    let first = candidates[0].clone();
    candidates.into_iter().find(|p| p.exists()).unwrap_or(first)
}

pub fn addons_dir() -> PathBuf {
    base_dir().join("addons21")
}

/// The installed Anki's version, from the package metadata next to the launcher (if it's there).
pub fn anki_version() -> Option<Vec<i64>> {
    let exe = which("anki")?;
    let root = std::fs::canonicalize(exe).ok()?.parent()?.to_path_buf();
    for d in [root.clone(), root.parent()?.to_path_buf(), root.parent()?.join("share/anki")] {
        let mut infos: Vec<PathBuf> = Vec::new();
        for pat in [d.join("app_packages"), d.clone()] {
            if let Ok(rd) = std::fs::read_dir(&pat) {
                let mut found: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with("anki-") && n.ends_with(".dist-info")).unwrap_or(false))
                    .collect();
                found.sort();
                infos.extend(found);
            }
        }
        for info in infos {
            let name = info.file_name()?.to_str()?;
            let v = &name["anki-".len()..name.len() - ".dist-info".len()];
            if let Ok(parts) = v.split('.').map(|x| x.parse::<i64>()).collect::<std::result::Result<Vec<_>, _>>() {
                return Some(parts);
            }
        }
    }
    None
}

/// AnkiWeb's "p" parameter: 66 for 2.1.66, 250200 for 25.02, 260903 for 26.09.3.
pub fn point_version(v: Option<&[i64]>) -> i64 {
    let Some(v) = v.filter(|v| !v.is_empty()) else { return DEFAULT_POINT };
    let at = |i: usize| v.get(i).copied().unwrap_or(0);
    if v[0] == 2 { at(2) } else { v[0] * 10000 + at(1) * 100 + at(2) }
}

// --- what's installed --------------------------------------------------------------------------------
fn companion_version(folder: &Path) -> Option<i64> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(folder.join("manifest.json")).ok()?).ok()?;
    v["version"].as_i64()
}

fn our_companion_version() -> Option<i64> {
    serde_json::from_str::<Value>(COMPANION_MANIFEST).ok()?["version"].as_i64()
}

pub fn status() -> Value {
    let addons = addons_dir();
    json!({
        "launcher": launcher().is_some(),
        "addons_dir": addons.to_string_lossy(),
        "ankiconnect": addons.join(ANKICONNECT_ID).is_dir(),
        "companion": companion_version(&addons.join(COMPANION_DIR)).is_some() && companion_version(&addons.join(COMPANION_DIR)) == our_companion_version(),
        "running": !anki_pids().is_empty(),
    })
}

/// One of AnkiConnect's settings: as saved in its meta.json, else its defaults (config.json).
fn setting(key: &str) -> Option<Value> {
    let folder = addons_dir().join(ANKICONNECT_ID);
    for (name, is_meta) in [("meta.json", true), ("config.json", false)] {
        let Ok(text) = std::fs::read_to_string(folder.join(name)) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let cfg = if is_meta { v.get("config").cloned().filter(|c| crate::util::truthy(c)).unwrap_or(json!({})) } else { v };
        if let Some(x) = cfg.get(key) {
            return Some(x.clone());
        }
    }
    None
}

/// The apiKey AnkiConnect is using.
pub fn api_key() -> Option<String> {
    setting("apiKey").and_then(|v| v.as_str().map(String::from)).filter(|s| !s.is_empty())
}

/// The port AnkiConnect listens on.
pub fn port() -> u16 {
    match setting("webBindPort") {
        Some(Value::Number(n)) => n.as_u64().map(|p| p as u16).filter(|p| *p != 0).unwrap_or(DEFAULT_PORT),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(DEFAULT_PORT),
        _ => DEFAULT_PORT,
    }
}

pub fn url() -> String {
    format!("http://127.0.0.1:{}", port())
}

// --- its port ----------------------------------------------------------------------------------------
pub fn port_free(p: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", p)).is_ok()
}

pub fn spare_port() -> Result<u16> {
    SPARE_PORTS.into_iter().find(|p| port_free(*p)).ok_or_else(|| Error::Other("Couldn't find a free port for AnkiConnect".into()))
}

/// Save webBindPort in AnkiConnect's settings. Anki reads it when it starts, and rewrites
/// meta.json from memory when it quits, so Anki shouldn't be running.
pub fn set_port(p: u16, folder: Option<&Path>) -> Result<()> {
    let folder = folder.map(PathBuf::from).unwrap_or_else(|| addons_dir().join(ANKICONNECT_ID));
    let meta_path = folder.join("meta.json");
    let mut meta: Value = std::fs::read_to_string(&meta_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({"name": "AnkiConnect"}));
    if !meta.get("config").map(crate::util::truthy).unwrap_or(false) {
        let defaults = folder.join("config.json");
        meta["config"] = std::fs::read_to_string(&defaults).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    }
    meta["config"]["webBindPort"] = json!(p);
    meta["mod"] = json!(crate::store::now() as i64);
    std::fs::write(&meta_path, serde_json::to_string(&meta)?)?;
    Ok(())
}

// --- installing --------------------------------------------------------------------------------------
pub async fn install_ankiconnect() -> Result<()> {
    let version = anki_version();
    let url = format!("https://ankiweb.net/shared/download/{ANKICONNECT_ID}?v=2.1&p={}", point_version(version.as_deref()));
    let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().map_err(|e| Error::Fetch(e.to_string()))?;
    let resp = http.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(crate::client::status_error(resp.status(), &url));
    }
    let bytes = resp.bytes().await?.to_vec();
    let folder = addons_dir().join(ANKICONNECT_ID);
    let tmp = folder.with_file_name(format!("{ANKICONNECT_ID}.installing"));
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut zf = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| Error::Other("AnkiWeb didn't return the AnkiConnect add-on".into()))?;
        if zf.by_name("__init__.py").is_err() {
            return Err(Error::Other("AnkiWeb's AnkiConnect download doesn't look like an Anki add-on".into()));
        }
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        for i in 0..zf.len() {
            let mut f = zf.by_index(i).map_err(|e| Error::Other(e.to_string()))?;
            let Some(rel) = f.enclosed_name() else {
                return Err(Error::Other("unexpected file path in the AnkiConnect download".into()));
            };
            let target = tmp.join(rel);
            if f.is_dir() {
                std::fs::create_dir_all(&target)?;
                continue;
            }
            if let Some(dir) = target.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut data = Vec::new();
            f.read_to_end(&mut data)?;
            std::fs::write(&target, data)?;
        }
        // Settings go in meta.json, where Anki keeps an add-on's saved config: its defaults, plus an
        // API key so other programs and web pages can't drive Anki through it.
        let mut cfg: Value = std::fs::read_to_string(tmp.join("config.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        cfg["apiKey"] = json!(token_urlsafe(24));
        if !port_free(DEFAULT_PORT) {
            cfg["webBindPort"] = json!(spare_port()?); // another program has AnkiConnect's usual port
        }
        std::fs::write(tmp.join("meta.json"), serde_json::to_string(&json!({"name": "AnkiConnect", "mod": crate::store::now() as i64, "config": cfg}))?)?;
        std::fs::rename(&tmp, &folder)?;
        Ok(())
    })
    .await
    .map_err(|e| Error::Other(e.to_string()))?
}

pub fn install_companion() -> Result<()> {
    let folder = addons_dir().join(COMPANION_DIR);
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join("__init__.py"), COMPANION_INIT)?;
    std::fs::write(folder.join("manifest.json"), COMPANION_MANIFEST)?;
    let meta = folder.join("meta.json");
    if !meta.exists() {
        // Anki also keeps its own state here (e.g. disabled), so only create it
        std::fs::write(meta, serde_json::to_string(&json!({"name": "canvas-mcp companion (open in tray)", "mod": crate::store::now() as i64}))?)?;
    }
    Ok(())
}

/// Install whatever's missing. Returns what was installed.
pub async fn install() -> Result<Vec<String>> {
    let mut done = Vec::new();
    if !addons_dir().join(ANKICONNECT_ID).is_dir() {
        install_ankiconnect().await?;
        done.push("AnkiConnect".to_string());
    }
    if !status()["companion"].as_bool().unwrap_or(false) {
        install_companion()?;
        done.push("companion".to_string());
    }
    Ok(done)
}

// --- starting and stopping Anki ---------------------------------------------------------------------
static LAUNCHED: Mutex<Vec<Child>> = Mutex::new(Vec::new());

/// Running Anki processes.
pub fn anki_pids() -> Vec<u32> {
    // reap the ones we started, so they don't linger as zombies
    LAUNCHED.lock().unwrap().retain_mut(|c| matches!(c.try_wait(), Ok(None)));
    let me = std::process::id();
    #[cfg(target_os = "linux")]
    {
        let mut pids = Vec::new();
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
                let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
                // "pid (comm) state ...": skip exited processes whose parent hasn't reaped them yet
                let Some((head, tail)) = stat.rsplit_once(')') else { continue };
                let comm = head.split_once('(').map(|x| x.1).unwrap_or("");
                if comm == "anki" && tail.split_whitespace().next() != Some("Z") && pid != me {
                    pids.push(pid);
                }
            }
        }
        pids
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = Command::new("tasklist").args(["/FI", "IMAGENAME eq anki.exe", "/FO", "CSV", "/NH"]).creation_flags(0x0800_0000).output();
        let Ok(out) = out else { return vec![] };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split("\",\"").nth(1).and_then(|p| p.trim_matches('"').parse().ok()))
            .filter(|p| *p != me)
            .collect()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = me;
        vec![]
    }
}

pub fn launch(tray: bool) -> bool {
    let Some(mut cmd) = launcher() else { return false };
    if tray && cmd[0] == "flatpak" {
        cmd.insert(2, "--env=CANVAS_ANKI_TRAY=1".into());
    }
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).current_dir(config::home());
    if tray {
        c.env("CANVAS_ANKI_TRAY", "1");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    match c.spawn() {
        Ok(child) => {
            LAUNCHED.lock().unwrap().push(child);
            true
        }
        Err(_) => false,
    }
}

/// Ask Anki to quit (SIGTERM, which Anki handles by closing normally) and wait for it.
pub fn stop(timeout: Duration) -> bool {
    for pid in anki_pids() {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill").args(["/PID", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
    }
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if anki_pids().is_empty() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ANKI_BASE is process-wide, so these run as one test.
    #[test]
    fn port_settings() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ANKI_BASE", dir.path()) };
        let folder = dir.path().join("addons21").join(ANKICONNECT_ID);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("config.json"), r#"{"apiKey": null, "webBindAddress": "127.0.0.1", "webBindPort": 8765}"#).unwrap();
        // defaults to AnkiConnect's own
        assert_eq!(port(), 8765);
        assert_eq!(url(), "http://127.0.0.1:8765");
        // set_port keeps other settings
        std::fs::write(folder.join("meta.json"), r#"{"name": "AnkiConnect", "config": {"apiKey": "k", "webBindPort": 8765}}"#).unwrap();
        set_port(8771, None).unwrap();
        let meta: Value = serde_json::from_str(&std::fs::read_to_string(folder.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["config"], json!({"apiKey": "k", "webBindPort": 8771}));
        assert_eq!(url(), "http://127.0.0.1:8771");
        assert_eq!(api_key().as_deref(), Some("k"));
        // set_port without meta.json starts from the defaults
        std::fs::remove_file(folder.join("meta.json")).unwrap();
        set_port(8772, None).unwrap();
        let meta: Value = serde_json::from_str(&std::fs::read_to_string(folder.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["config"]["webBindAddress"], "127.0.0.1");
        assert_eq!(port(), 8772);
        // a spare port is free
        let p = spare_port().unwrap();
        assert!(SPARE_PORTS.contains(&p));
        assert_eq!(point_version(Some(&[2, 1, 66])), 66);
        assert_eq!(point_version(Some(&[25, 2])), 250200);
        assert_eq!(point_version(Some(&[26, 9, 3])), 260903);
        assert_eq!(point_version(None), DEFAULT_POINT);
        unsafe { std::env::remove_var("ANKI_BASE") };
    }
}

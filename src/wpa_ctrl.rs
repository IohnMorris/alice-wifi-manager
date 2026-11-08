use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const WPA_SOCKET_PRIMARY: &str = "/var/run/wpa_supplicant/wlan0";
pub const WPA_SOCKET_FALLBACK: &str = "/run/wpa_supplicant/wlan0";
pub const AP_DEBUG_LOG_PATH: &str = "/bkupgrade/data/ap_debug.log";
pub const AP_RUNTIME_LOG_PATH: &str = "/bkupgrade/data/ap_wpa_supplicant.log";
pub const AP_CONFIG_PATH: &str = "/bkupgrade/data/wpa_supplicant.ap.conf";
pub const WPA_PID_PATH: &str = "/var/run/wpa_supplicant.pid";
const WPA_SUPPLICANT_BIN: &str = "/usr/sbin/wpa_supplicant";
const STA_CONFIG_PATH: &str = "/bkupgrade/data/wpa_supplicant.conf";

pub const AP_DEFAULT_SSID: &str = "Alice-Mini2-Setup";
pub const AP_DEFAULT_PSK: &str = "alice1234";
pub const AP_FREQUENCY_MHZ: u32 = 2437; // Канал 6 (2.4 ГГц)

static CLIENT_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WpaStatus {
    pub wpa_state: String,
    pub mode: String,
    pub ssid: Option<String>,
    pub id: Option<i32>,
    pub freq: Option<u32>,
    pub ip_address: Option<String>,
}

impl WpaStatus {
    pub fn is_completed_station(&self) -> bool {
        self.wpa_state == "COMPLETED" && self.mode == "station"
    }

    pub fn is_completed_ap(&self) -> bool {
        self.wpa_state == "COMPLETED"
            && (self.mode.eq_ignore_ascii_case("AP") || self.mode == "mode=AP")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedNetwork {
    pub ssid: String,
    pub bssid: String,
    pub frequency: u32,
    pub band: String,
    pub signal_dbm: i32,
    pub signal_bars: u8,
    pub security: String,
    pub supported: bool,
}

/// Записывает диагностическую строку с таймстампом в `/bkupgrade/data/ap_debug.log`.
pub fn append_debug_log(line: &str) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let formatted = format!("[{}] {}\n", ts, line);
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(AP_DEBUG_LOG_PATH)
    {
        let _ = f.write_all(formatted.as_bytes());
        let _ = f.sync_data();
    }
}

pub struct WpaCtrl {
    sock: UnixDatagram,
    local_path: PathBuf,
    attached: bool,
}

impl WpaCtrl {
    pub fn open() -> Result<Self, String> {
        let server_path = if Path::new(WPA_SOCKET_PRIMARY).exists() {
            WPA_SOCKET_PRIMARY
        } else if Path::new(WPA_SOCKET_FALLBACK).exists() {
            WPA_SOCKET_FALLBACK
        } else {
            return Err("wpa_supplicant control socket not found".to_string());
        };

        let pid = std::process::id();
        let seq = CLIENT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let local_path = PathBuf::from(format!("/tmp/wpa_ctrl_{}_{}", pid, seq));
        let _ = fs::remove_file(&local_path);

        let sock = UnixDatagram::bind(&local_path)
            .map_err(|e| format!("Failed to bind local wpa_ctrl socket: {}", e))?;

        sock.set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(|e| format!("Failed to set socket read timeout: {}", e))?;
        sock.set_write_timeout(Some(Duration::from_secs(3)))
            .map_err(|e| format!("Failed to set socket write timeout: {}", e))?;

        sock.connect(server_path)
            .map_err(|e| format!("Failed to connect to {}: {}", server_path, e))?;

        Ok(Self {
            sock,
            local_path,
            attached: false,
        })
    }

    /// Открывает отдельное соединение в режиме монитора (`ATTACH`), чтобы перехватывать все события `<3>...`.
    pub fn open_event_monitor() -> Result<Self, String> {
        let mut ctrl = Self::open()?;
        let reply = ctrl.request("ATTACH")?;
        if reply.trim() != "OK" {
            return Err(format!("ATTACH failed: {}", reply.trim()));
        }
        let _ = ctrl.sock.set_read_timeout(Some(Duration::from_millis(80)));
        ctrl.attached = true;
        Ok(ctrl)
    }

    /// Вычитывает все накопившиеся асинхронные события `<...>` из сокета монитора.
    pub fn drain_events(&self) -> Vec<String> {
        let mut events = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = self.sock.recv(&mut buf) {
            if n == 0 {
                break;
            }
            let msg = String::from_utf8_lossy(&buf[..n]).trim().to_string();
            events.push(msg);
        }
        events
    }

    pub fn request(&self, cmd: &str) -> Result<String, String> {
        // SET_NETWORK may contain a user's passphrase. Never include it in errors,
        // which can be propagated to diagnostic logs or the HTTP API.
        let safe_cmd = if cmd.contains(" psk ") {
            "SET_NETWORK <redacted> psk <redacted>"
        } else {
            cmd
        };
        self.sock
            .send(cmd.as_bytes())
            .map_err(|e| format!("wpa_ctrl send '{}' failed: {}", safe_cmd, e))?;

        let mut buf = [0u8; 8192];
        loop {
            let n = self
                .sock
                .recv(&mut buf)
                .map_err(|e| format!("wpa_ctrl recv for '{}' failed: {}", safe_cmd, e))?;
            let reply = String::from_utf8_lossy(&buf[..n]).to_string();
            if !reply.starts_with('<') {
                return Ok(reply);
            } else {
                append_debug_log(&format!("wpa_event (inline): {}", reply.trim()));
            }
        }
    }

    pub fn request_ok(&self, cmd: &str) -> Result<(), String> {
        let reply = self.request(cmd)?;
        if reply.trim() == "OK" {
            Ok(())
        } else {
            let safe_cmd = if cmd.contains(" psk ") {
                "SET_NETWORK <redacted> psk <redacted>"
            } else {
                cmd
            };
            Err(format!(
                "Command '{}' returned '{}'",
                safe_cmd,
                reply.trim()
            ))
        }
    }
}

impl Drop for WpaCtrl {
    fn drop(&mut self) {
        if self.attached {
            let _ = self.sock.send(b"DETACH");
        }
        let _ = fs::remove_file(&self.local_path);
    }
}

pub fn wait_for_wpa_socket(timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(ctrl) = WpaCtrl::open() {
            if let Ok(resp) = ctrl.request("PING") {
                if resp.trim() == "PONG" {
                    return true;
                }
            }
        }
        thread::sleep(Duration::from_millis(300));
    }
    false
}

pub fn get_status() -> Result<WpaStatus, String> {
    let ctrl = WpaCtrl::open()?;
    let raw = ctrl.request("STATUS")?;
    let mut status = WpaStatus::default();

    for line in raw.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "wpa_state" => status.wpa_state = v.trim().to_string(),
                "mode" => status.mode = v.trim().to_string(),
                "ssid" => status.ssid = Some(unescape_wpa_ssid(v.trim())),
                "id" => status.id = v.trim().parse::<i32>().ok(),
                "freq" => status.freq = v.trim().parse::<u32>().ok(),
                "ip_address" => status.ip_address = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    Ok(status)
}

pub fn scan_networks() -> Result<Vec<ScannedNetwork>, String> {
    let status = get_status()?;
    if status.mode.eq_ignore_ascii_case("AP") {
        return Err("Live Wi-Fi scan is not supported while wlan0 is in AP mode".to_string());
    }

    let ctrl = WpaCtrl::open()?;
    let scan_reply = ctrl.request("SCAN")?;
    if scan_reply.trim() != "OK" && !scan_reply.trim().is_empty() {
        return Err(format!("SCAN failed: {}", scan_reply.trim()));
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut raw = String::new();
    while Instant::now() < deadline {
        raw = ctrl.request("SCAN_RESULTS")?;
        let result_count = raw
            .lines()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .count();
        if result_count > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(400));
    }

    if raw.is_empty() {
        return Err("SCAN_RESULTS returned no data".to_string());
    }

    let mut by_ssid: HashMap<String, ScannedNetwork> = HashMap::new();

    for (idx, line) in raw.lines().enumerate() {
        if idx == 0 || line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(5, '\t').collect();
        if parts.len() < 5 {
            continue;
        }

        let bssid = parts[0].trim().to_string();
        let frequency = parts[1].trim().parse::<u32>().unwrap_or(2412);
        let signal_dbm = parts[2].trim().parse::<i32>().unwrap_or(-90);
        let flags = parts[3].trim();
        let ssid = unescape_wpa_ssid(parts[4].trim());

        if ssid.is_empty() || ssid == AP_DEFAULT_SSID {
            continue;
        }

        let band = if frequency >= 4900 {
            "5 ГГц".to_string()
        } else {
            "2.4 ГГц".to_string()
        };

        let signal_bars = dbm_to_bars(signal_dbm);
        let (security, supported) = parse_security_flags(flags);

        let candidate = ScannedNetwork {
            ssid: ssid.clone(),
            bssid,
            frequency,
            band,
            signal_dbm,
            signal_bars,
            security,
            supported,
        };

        match by_ssid.get(&ssid) {
            Some(existing) if existing.signal_dbm >= candidate.signal_dbm => {}
            _ => {
                by_ssid.insert(ssid, candidate);
            }
        }
    }

    let mut list: Vec<ScannedNetwork> = by_ssid.into_values().collect();
    if list.is_empty() {
        return Err("SCAN completed but returned 0 visible BSSes".to_string());
    }
    list.sort_by(|a, b| b.signal_dbm.cmp(&a.signal_dbm));
    Ok(list)
}

/// Перезапускает wpa_supplicant с отдельным AP-конфигом и подтверждает AP-ENABLED.
pub fn start_ap_mode() -> Result<(), String> {
    append_debug_log("Step AP-1: validating committed STA configuration");
    // load() restores the last committed version when the running
    // wpa_supplicant has rewritten the STA file with stale runtime data.
    crate::config_store::WpaConfigFile::load()?;
    write_ap_config()?;
    restart_wpa_supplicant(AP_CONFIG_PATH, AP_RUNTIME_LOG_PATH)?;

    let monitor = WpaCtrl::open_event_monitor()?;
    let start = Instant::now();
    let mut ap_enabled = false;
    let mut last_status = String::new();

    while start.elapsed() < Duration::from_secs(10) {
        for event in monitor.drain_events() {
            append_debug_log(&format!("wpa_event: {}", event));
            if event.contains("AP-ENABLED") {
                ap_enabled = true;
            }
            if event.contains("AP-DISABLED") || event.contains("INTERFACE-DISABLED") {
                append_debug_log(&format!("AP startup rejected: {}", event));
                return Err(format!("wpa_supplicant rejected AP mode: {}", event));
            }
        }

        if let Ok(status) = get_status() {
            let current = format!(
                "state={}, mode={}, ssid={:?}, freq={:?}",
                status.wpa_state, status.mode, status.ssid, status.freq
            );
            if current != last_status {
                append_debug_log(&format!("AP STATUS: {}", current));
                last_status = current;
            }
            if !ap_enabled && runtime_log_contains("AP-ENABLED") {
                ap_enabled = true;
                append_debug_log("AP-ENABLED confirmed from runtime supplicant log");
            }
            if ap_enabled && status.is_completed_ap() {
                append_debug_log("AP-ENABLED confirmed with mode=AP and wpa_state=COMPLETED");
                return Ok(());
            }
        }

        fn runtime_log_contains(marker: &str) -> bool {
            fs::read_to_string(AP_RUNTIME_LOG_PATH)
                .map(|content| content.contains(marker))
                .unwrap_or(false)
        }
        thread::sleep(Duration::from_millis(200));
    }

    Err(format!(
        "Timeout waiting for AP-ENABLED and AP status; last status: {}",
        last_status
    ))
}

fn write_ap_config() -> Result<(), String> {
    let content = format!(
        "ctrl_interface=/var/run/wpa_supplicant\n\
         country=RU\n\
         ap_scan=2\n\
         p2p_disabled=1\n\
         \n\
         network={{\n\
             ssid=\"{}\"\n\
             mode=2\n\
             frequency={}\n\
             key_mgmt=WPA-PSK\n\
             proto=RSN\n\
             pairwise=CCMP\n\
             group=CCMP\n\
             psk=\"{}\"\n\
         }}\n",
        AP_DEFAULT_SSID, AP_FREQUENCY_MHZ, AP_DEFAULT_PSK
    );
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(AP_CONFIG_PATH)
        .map_err(|e| format!("Failed to create AP config: {}", e))?;
    file.write_all(content.as_bytes())
        .map_err(|e| format!("Failed to write AP config: {}", e))?;
    file.sync_all()
        .map_err(|e| format!("Failed to sync AP config: {}", e))?;
    append_debug_log("AP config written to persistent data directory (PSK redacted)");
    Ok(())
}

fn restart_wpa_supplicant(config_path: &str, runtime_log: &str) -> Result<(), String> {
    append_debug_log("Step WPA-1: stopping existing wpa_supplicant");
    stop_wpa_supplicant()?;
    remove_stale_wpa_runtime_files();

    append_debug_log(&format!(
        "Step WPA-2: starting {} with config {}",
        WPA_SUPPLICANT_BIN, config_path
    ));
    let log_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(runtime_log)
        .map_err(|e| format!("Failed to open wpa_supplicant runtime log: {}", e))?;
    let err_file = log_file
        .try_clone()
        .map_err(|e| format!("Failed to clone wpa_supplicant runtime log: {}", e))?;

    Command::new(WPA_SUPPLICANT_BIN)
        .args([
            "-B",
            "-Dnl80211",
            "-i",
            WIFI_INTERFACE,
            "-c",
            config_path,
            "-dd",
            "-f",
            runtime_log,
            "-P",
            WPA_PID_PATH,
        ])
        .stdout(log_file)
        .stderr(err_file)
        .spawn()
        .map_err(|e| format!("Failed to start wpa_supplicant: {}", e))?;

    if !wait_for_wpa_socket(Duration::from_secs(8)) {
        return Err("wpa_supplicant control socket did not appear after restart".to_string());
    }
    append_debug_log("Step WPA-3: wpa_supplicant control socket is ready");
    Ok(())
}

const WIFI_INTERFACE: &str = "wlan0";

fn find_wpa_supplicant_pids() -> Vec<(i32, String)> {
    let mut result = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return result;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(raw) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args = raw
            .split(|&b| b == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>();
        let cmdline = args.join(" ");
        if is_wpa_supplicant_for_wlan0(&args) {
            result.push((pid, cmdline));
        }
    }

    fn is_wpa_supplicant_for_wlan0(args: &[String]) -> bool {
        let executable = args.first().map(String::as_str).unwrap_or_default();
        let is_wpa = executable.ends_with("/wpa_supplicant") || executable == "wpa_supplicant";
        if !is_wpa {
            return false;
        }

        args.windows(2)
            .any(|pair| pair[0] == "-i" && pair[1] == WIFI_INTERFACE)
    }
    result
}

fn stop_wpa_supplicant() -> Result<(), String> {
    let pids = find_wpa_supplicant_pids();
    for (pid, cmdline) in &pids {
        append_debug_log(&format!(
            "Stopping wpa_supplicant PID {} ({})",
            pid, cmdline
        ));
        unsafe {
            libc::kill(*pid, libc::SIGTERM);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if find_wpa_supplicant_pids().is_empty() {
            append_debug_log("Old wpa_supplicant stopped cleanly");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(150));
    }

    for (pid, _) in find_wpa_supplicant_pids() {
        append_debug_log(&format!(
            "wpa_supplicant PID {} did not stop; sending SIGKILL",
            pid
        ));
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    thread::sleep(Duration::from_millis(250));
    if !find_wpa_supplicant_pids().is_empty() {
        return Err("Unable to stop existing wpa_supplicant instance".to_string());
    }
    Ok(())
}

fn remove_stale_wpa_runtime_files() {
    let _ = fs::remove_file(WPA_SOCKET_PRIMARY);
    let _ = fs::remove_file(WPA_SOCKET_FALLBACK);
    let _ = fs::remove_file(WPA_PID_PATH);
    append_debug_log("Removed stale wpa_supplicant socket and PID files");
}

pub fn stop_ap_mode(ap_net_id: Option<i32>) -> Result<(), String> {
    if let Ok(ctrl) = WpaCtrl::open() {
        if let Some(id) = ap_net_id {
            let _ = ctrl.request(&format!("DISABLE_NETWORK {}", id));
            let _ = ctrl.request(&format!("REMOVE_NETWORK {}", id));
        }
    }
    Ok(())
}

pub fn reload_config_and_reconnect() -> Result<(), String> {
    let ctrl = WpaCtrl::open()?;
    ctrl.request_ok("RECONFIGURE")?;
    let _ = ctrl.request("RECONNECT");
    Ok(())
}

/// Перечитывает сохранённые профили без принудительного разрыва текущего STA.
pub fn reconfigure_saved_config() -> Result<(), String> {
    let status = get_status()?;
    if !status.mode.eq_ignore_ascii_case("station") {
        return Ok(());
    }
    let ctrl = WpaCtrl::open()?;
    ctrl.request_ok("RECONFIGURE")
}

/// Возвращает штатный STA-экземпляр после AP-рестарта и подтверждает его запуск.
/// Перед запуском восстанавливает последнюю проверенную копию конфигурации.
pub fn restart_to_station() -> Result<(), String> {
    append_debug_log("STA restart: restoring verified configuration");
    crate::config_store::WpaConfigFile::restore_from_backup()?;
    restart_wpa_supplicant(STA_CONFIG_PATH, "/bkupgrade/data/sta_wpa_supplicant.log")?;

    let ctrl = WpaCtrl::open()?;
    ctrl.request_ok("RECONFIGURE")?;
    let _ = ctrl.request("RECONNECT");

    let start = Instant::now();
    let mut last_status = String::new();
    while start.elapsed() < Duration::from_secs(20) {
        if let Ok(status) = get_status() {
            let current = format!(
                "state={}, mode={}, ssid={:?}, freq={:?}",
                status.wpa_state, status.mode, status.ssid, status.freq
            );
            if current != last_status {
                append_debug_log(&format!("STA STATUS: {}", current));
                last_status = current;
            }
            if status.is_completed_station() {
                append_debug_log("STA wpa_state=COMPLETED confirmed");
                return Ok(());
            }
        }
        thread::sleep(Duration::from_millis(300));
    }

    Err(format!(
        "Timeout waiting for STA reconnection; last status: {}",
        last_status
    ))
}

pub fn activate_candidate_network_in_ram(
    ssid: &str,
    psk: Option<&str>,
    is_hidden: bool,
) -> Result<i32, String> {
    let ctrl = WpaCtrl::open()?;
    let _ = ctrl.request("DISABLE_NETWORK all");

    let id_reply = ctrl.request("ADD_NETWORK")?;
    let net_id: i32 = id_reply
        .trim()
        .parse()
        .map_err(|_| format!("Invalid network id: {}", id_reply.trim()))?;

    let escaped_ssid = escape_wpa_quoted(ssid);
    ctrl.request_ok(&format!("SET_NETWORK {} ssid \"{}\"", net_id, escaped_ssid))?;
    ctrl.request_ok(&format!("SET_NETWORK {} auth_alg OPEN", net_id))?;

    if is_hidden {
        let _ = ctrl.request_ok(&format!("SET_NETWORK {} scan_ssid 1", net_id));
    }

    match psk {
        Some(password) if !password.is_empty() => {
            let escaped_psk = escape_wpa_quoted(password);
            ctrl.request_ok(&format!("SET_NETWORK {} key_mgmt WPA-PSK", net_id))?;
            ctrl.request_ok(&format!("SET_NETWORK {} psk \"{}\"", net_id, escaped_psk))?;
        }
        _ => {
            ctrl.request_ok(&format!("SET_NETWORK {} key_mgmt NONE", net_id))?;
        }
    }

    ctrl.request_ok(&format!("SELECT_NETWORK {}", net_id))?;
    Ok(net_id)
}

fn dbm_to_bars(dbm: i32) -> u8 {
    if dbm >= -55 {
        4
    } else if dbm >= -67 {
        3
    } else if dbm >= -78 {
        2
    } else {
        1
    }
}

fn parse_security_flags(flags: &str) -> (String, bool) {
    if flags.contains("EAP") {
        return ("WPA-Enterprise".to_string(), false);
    }
    let has_sae = flags.contains("SAE");
    let has_psk = flags.contains("PSK");
    if has_sae && has_psk {
        ("WPA2/WPA3".to_string(), true)
    } else if has_sae {
        ("WPA3".to_string(), true)
    } else if has_psk {
        ("WPA2".to_string(), true)
    } else {
        ("Открытая".to_string(), true)
    }
}

fn escape_wpa_quoted(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn unescape_wpa_ssid(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if i + 3 < bytes.len() && bytes[i] == b'\\' && bytes[i + 1] == b'x' {
            let hex_str = std::str::from_utf8(&bytes[i + 2..i + 4]).unwrap_or("");
            if let Ok(val) = u8::from_str_radix(hex_str, 16) {
                out.push(val);
                i += 4;
                continue;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'\\' {
            out.push(bytes[i + 1]);
            i += 2;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

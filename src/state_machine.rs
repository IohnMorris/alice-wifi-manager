use crate::ap::ApController;
use crate::config_store::WpaConfigFile;
use crate::http::HttpServerHandle;
use crate::net_iface;
use crate::process_guard;
use crate::voice;
use crate::wpa_ctrl::{self, ScannedNetwork};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const CLI_CONTROL_SOCKET: &str = "/var/run/alice-wifi-manager.sock";
const PRIMARY_WAIT_TIMEOUT: Duration = Duration::from_secs(20);
const AP_RETRY_DELAY: Duration = Duration::from_secs(5);
const TRANSACTION_TEST_TIMEOUT: Duration = Duration::from_secs(20);
const AP_SAVED_NETWORK_PROBE_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingMode {
    Station,
    AccessPoint,
    TestingNetwork,
}

#[derive(Debug, Clone)]
pub enum TransitionCommand {
    ForceStartAp,
    StopApAndReturnToPrimary,
    TestAndConnect {
        ssid: String,
        psk: Option<String>,
        is_hidden: bool,
    },
}

pub struct SharedManagerState {
    pub mode: OperatingMode,
    pub scan_cache: Vec<ScannedNetwork>,
    pub connected_ssid: Option<String>,
    pub current_ip: Option<String>,
    pub last_error: Option<String>,
    pub last_result: String,
    pub busy_transaction: bool,
}

impl SharedManagerState {
    pub fn new() -> Self {
        Self {
            mode: OperatingMode::Station,
            scan_cache: Vec::new(),
            connected_ssid: None,
            current_ip: None,
            last_error: None,
            last_result: "idle".to_string(),
            busy_transaction: false,
        }
    }

    pub fn mode_str(&self) -> &'static str {
        match self.mode {
            OperatingMode::Station => "STA",
            OperatingMode::AccessPoint => "AP",
            OperatingMode::TestingNetwork => "TESTING",
        }
    }
}

pub fn run_daemon(term_flag: Arc<AtomicBool>) -> Result<(), String> {
    // 1. Гарантируем пробуждение системных служб, если они остались в SIGSTOP после сбоя питания
    process_guard::ensure_all_resumed();

    // 2. Ожидаем готовности интерфейса wlan0 и сокета wpa_supplicant
    while !net_iface::wait_for_wlan0(Duration::from_secs(25)) {
        if term_flag.load(Ordering::Relaxed) {
            return Ok(());
        }
        crate::wpa_ctrl::append_debug_log("Startup: wlan0 is not ready after 25 seconds; retrying");
        thread::sleep(Duration::from_secs(5));
    }
    while !wpa_ctrl::wait_for_wpa_socket(Duration::from_secs(25)) {
        if term_flag.load(Ordering::Relaxed) {
            return Ok(());
        }
        crate::wpa_ctrl::append_debug_log(
            "Startup: wpa_supplicant socket is not ready after 25 seconds; retrying",
        );
        thread::sleep(Duration::from_secs(5));
    }

    // 3. Проверяем целостность /bkupgrade/data/wpa_supplicant.conf (восстанавливаем из .bak при сбое)
    WpaConfigFile::ensure_valid_or_restore()?;
    if wpa_ctrl::get_status()
        .map(|status| status.mode.eq_ignore_ascii_case("AP"))
        .unwrap_or(false)
    {
        wpa_ctrl::append_debug_log(
            "Startup detected AP-mode wpa_supplicant; performing controlled STA restore",
        );
        let _ = wpa_ctrl::restart_to_station();
    } else {
        let _ = wpa_ctrl::reload_config_and_reconnect();
    }

    let state = Arc::new(Mutex::new(SharedManagerState::new()));
    let (cmd_tx, cmd_rx) = mpsc::channel::<TransitionCommand>();

    // Запускаем поток приёма CLI-команд (start-ap, stop-ap, status)
    start_cli_ipc_listener(Arc::clone(&state), cmd_tx.clone(), Arc::clone(&term_flag));

    let mut active_ap: Option<ApController> = None;
    let mut active_http: Option<HttpServerHandle> = None;
    let mut disconnected_since: Option<Instant> = Some(Instant::now());
    let mut ap_retry_after: Option<Instant> = None;
    let mut next_ap_saved_network_probe = Instant::now() + AP_SAVED_NETWORK_PROBE_INTERVAL;

    while !term_flag.load(Ordering::Relaxed) {
        // Обрабатываем входящие команды от Web UI или CLI
        if let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                TransitionCommand::ForceStartAp => {
                    if active_ap.is_none() {
                        ap_retry_after = None;
                        enter_ap_state(&state, &cmd_tx, &mut active_ap, &mut active_http);
                        next_ap_saved_network_probe =
                            Instant::now() + AP_SAVED_NETWORK_PROBE_INTERVAL;
                    }
                }
                TransitionCommand::StopApAndReturnToPrimary => {
                    if active_ap.is_some() {
                        exit_ap_state(&state, &mut active_ap, &mut active_http, true);
                        disconnected_since = Some(Instant::now());
                        ap_retry_after = None;
                    }
                }
                TransitionCommand::TestAndConnect {
                    ssid,
                    psk,
                    is_hidden,
                } => {
                    execute_connection_transaction(
                        &state,
                        &cmd_tx,
                        &mut active_ap,
                        &mut active_http,
                        &ssid,
                        psk.as_deref(),
                        is_hidden,
                    );
                    disconnected_since = Some(Instant::now());
                    ap_retry_after = None;
                }
            }
        }

        // В AP-режиме периодически пробуем сохранённые профили через реальный STA-переход.
        // sprdwl_ng не даёт надёжного live scan в AP, поэтому один только SCAN здесь
        // не показывает, появилась ли домашняя сеть.
        if active_ap.is_some() {
            let now = Instant::now();
            let busy = state.lock().map(|g| g.busy_transaction).unwrap_or(true);
            if !busy && now >= next_ap_saved_network_probe {
                probe_saved_networks_from_ap(&state, &cmd_tx, &mut active_ap, &mut active_http);
                next_ap_saved_network_probe = Instant::now() + AP_SAVED_NETWORK_PROBE_INTERVAL;
                disconnected_since = Some(Instant::now());
                ap_retry_after = None;
            }
        } else {
            // Если мы в режиме обычной работы (STA), мониторим связь с основной сетью
            let cfg = WpaConfigFile::load().ok();
            let has_primary = cfg.as_ref().and_then(|c| c.get_primary()).is_some();

            if !has_primary {
                // Если в конфиге вообще нет основной сети — сразу поднимаем точку доступа
                if ap_retry_after
                    .map(|deadline| deadline <= Instant::now())
                    .unwrap_or(true)
                {
                    crate::wpa_ctrl::append_debug_log(
                        "No saved primary network exists; starting automatic AP",
                    );
                    voice::speak(
                        "Основная сеть Wi-Fi не найдена. Запускаю точку доступа для настройки.",
                    );
                    enter_ap_state(&state, &cmd_tx, &mut active_ap, &mut active_http);
                    if active_ap.is_none() {
                        ap_retry_after = Some(Instant::now() + AP_RETRY_DELAY);
                    }
                    next_ap_saved_network_probe = Instant::now() + AP_SAVED_NETWORK_PROBE_INTERVAL;
                }
            } else if let Some((ssid, ip)) = check_station_connected() {
                if disconnected_since.is_some() {
                    crate::wpa_ctrl::append_debug_log(&format!(
                        "STA connection restored to '{}'; cancelling automatic AP timer",
                        ssid
                    ));
                }
                disconnected_since = None;
                ap_retry_after = None;
                if let Ok(mut g) = state.lock() {
                    g.mode = OperatingMode::Station;
                    g.connected_ssid = Some(ssid);
                    g.current_ip = Some(ip);
                }
            } else {
                if let Ok(mut g) = state.lock() {
                    g.connected_ssid = None;
                    g.current_ip = None;
                }
                let since = *disconnected_since.get_or_insert_with(Instant::now);
                let now = Instant::now();
                if should_start_automatic_ap(Some(since), now, ap_retry_after) {
                    crate::wpa_ctrl::append_debug_log(
                        "No saved Wi-Fi connection for 20 seconds; starting automatic AP",
                    );
                    voice::speak(
                        "Основная сеть Wi-Fi недоступна. Запускаю точку доступа для настройки.",
                    );
                    enter_ap_state(&state, &cmd_tx, &mut active_ap, &mut active_http);
                    if active_ap.is_none() {
                        ap_retry_after = Some(Instant::now() + AP_RETRY_DELAY);
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(500));
    }

    // При штатной остановке демона гарантированно выключаем AP и будим platformd и dhcpcd
    if active_ap.is_some() {
        exit_ap_state(&state, &mut active_ap, &mut active_http, true);
    } else {
        process_guard::ensure_all_resumed();
    }
    let _ = fs::remove_file(CLI_CONTROL_SOCKET);

    Ok(())
}

fn probe_saved_networks_from_ap(
    state: &Arc<Mutex<SharedManagerState>>,
    cmd_tx: &Sender<TransitionCommand>,
    active_ap: &mut Option<ApController>,
    active_http: &mut Option<HttpServerHandle>,
) {
    let cfg = match WpaConfigFile::load() {
        Ok(cfg) => cfg,
        Err(error) => {
            crate::wpa_ctrl::append_debug_log(&format!(
                "AP periodic STA probe: failed to load saved networks: {}",
                error
            ));
            return;
        }
    };

    let mut profiles = cfg
        .networks
        .iter()
        .map(|network| {
            (
                network.is_primary,
                network.ssid.clone(),
                crate::config_store::extract_clean_psk(network),
                network.scan_ssid,
            )
        })
        .collect::<Vec<_>>();
    profiles.sort_by_key(|profile| !profile.0);

    if profiles.is_empty() {
        crate::wpa_ctrl::append_debug_log(
            "AP periodic STA probe: no saved networks; keeping AP active",
        );
        return;
    }

    crate::wpa_ctrl::append_debug_log(&format!(
        "AP periodic STA probe: testing {} saved network profile(s)",
        profiles.len()
    ));

    for (_is_primary, ssid, psk, is_hidden) in profiles {
        if active_ap.is_none() {
            break;
        }
        crate::wpa_ctrl::append_debug_log(&format!(
            "AP periodic STA probe: testing saved network '{}'",
            ssid
        ));
        execute_connection_transaction(
            state,
            cmd_tx,
            active_ap,
            active_http,
            &ssid,
            psk.as_deref(),
            is_hidden,
        );

        if active_ap.is_none() {
            crate::wpa_ctrl::append_debug_log(&format!(
                "AP periodic STA probe: network '{}' restored STA successfully",
                ssid
            ));
            voice::speak("Подключение к сохранённой сети восстановлено.");
            return;
        }
        if let Ok(mut g) = state.lock() {
            g.scan_cache.retain(|network| network.ssid != ssid);
        }
    }

    crate::wpa_ctrl::append_debug_log(
        "AP periodic STA probe: no saved network connected; AP remains active",
    );
    voice::speak("Сохранённые сети пока недоступны. Точка доступа остаётся включённой.");
}

fn should_start_automatic_ap(
    disconnected_since: Option<Instant>,
    now: Instant,
    retry_after: Option<Instant>,
) -> bool {
    let Some(since) = disconnected_since else {
        return false;
    };
    now.saturating_duration_since(since) >= PRIMARY_WAIT_TIMEOUT
        && retry_after.map(|deadline| deadline <= now).unwrap_or(true)
}

/// Проверяет выполнение двух условий подключения в режиме клиента:
/// `wpa_state=COMPLETED` и наличие полученного от роутера IPv4-адреса на `wlan0`.
fn check_station_connected() -> Option<(String, String)> {
    let status = wpa_ctrl::get_status().ok()?;
    if !status.is_completed_station() {
        return None;
    }
    let ip = net_iface::get_station_ipv4()?;
    let ssid = status.ssid.unwrap_or_default();
    Some((ssid, ip.to_string()))
}

fn enter_ap_state(
    state: &Arc<Mutex<SharedManagerState>>,
    cmd_tx: &Sender<TransitionCommand>,
    active_ap: &mut Option<ApController>,
    active_http: &mut Option<HttpServerHandle>,
) {
    match ApController::enter_ap_mode() {
        Ok((ap_ctrl, scanned)) => {
            voice::speak("Точка доступа для настройки Wi-Fi запущена.");
            if let Ok(mut g) = state.lock() {
                g.mode = OperatingMode::AccessPoint;
                if !scanned.is_empty() {
                    g.scan_cache = scanned;
                }
                g.connected_ssid = None;
                g.current_ip = Some("192.168.4.1".to_string());
                g.busy_transaction = false;
            }
            *active_ap = Some(ap_ctrl);
            if let Ok(http) = HttpServerHandle::start(Arc::clone(state), cmd_tx.clone()) {
                *active_http = Some(http);
            }
        }
        Err(e) => {
            crate::wpa_ctrl::append_debug_log(&format!(
                "AP CONTROLLER ERROR; manager remains alive and will retry: {}",
                e
            ));
            process_guard::ensure_all_resumed();
        }
    }
}

fn exit_ap_state(
    state: &Arc<Mutex<SharedManagerState>>,
    active_ap: &mut Option<ApController>,
    active_http: &mut Option<HttpServerHandle>,
    reload_wpa: bool,
) {
    if let Some(mut http) = active_http.take() {
        http.stop();
    }
    if let Some(ap_ctrl) = active_ap.take() {
        let _ = ap_ctrl.shutdown_and_resume_station(reload_wpa);
    } else {
        process_guard::ensure_all_resumed();
    }
    if let Ok(mut g) = state.lock() {
        g.mode = OperatingMode::Station;
        g.busy_transaction = false;
    }
}

/// Выполняет 3-этапный транзакционный сценарий подключения к новой сети (Раздел 5 ТЗ).
fn execute_connection_transaction(
    state: &Arc<Mutex<SharedManagerState>>,
    cmd_tx: &Sender<TransitionCommand>,
    active_ap: &mut Option<ApController>,
    active_http: &mut Option<HttpServerHandle>,
    ssid: &str,
    psk: Option<&str>,
    is_hidden: bool,
) {
    if let Ok(mut g) = state.lock() {
        g.mode = OperatingMode::TestingNetwork;
        g.busy_transaction = true;
    }
    voice::speak(&format!("Подключаюсь к сети Wi-Fi {}.", ssid));

    // --- ЭТАП 1. Подготовка ---
    // Останавливаем HTTP-сервер на время сброса 192.168.4.1
    if let Some(mut http) = active_http.take() {
        http.stop();
    }

    // 1.4. Останавливаем DHCP/DNS и очищаем адрес 192.168.4.1, удаляем временный профиль AP
    if let Some(ref mut ap) = active_ap {
        let _ = ap.suspend_ap_for_network_test();
    } else {
        let _ = net_iface::flush_wlan0_ip();
    }

    // --- ЭТАП 2. Подключение и получение IP ---
    // 2.2. Активируем выбранный профиль сети в оперативной памяти wpa_supplicant
    let temp_sta_id = wpa_ctrl::activate_candidate_network_in_ram(ssid, psk, is_hidden).ok();

    // 2.3. Размораживаем штатный DHCP-клиент dhcpcd и инициируем запрос IP
    process_guard::resume_dhcpcd_only();
    net_iface::request_dhcp_rebind();

    // 2.4. Ожидаем до 20 секунд выполнения двух условий: wpa_state=COMPLETED + IPv4-адрес
    let start = Instant::now();
    let mut connected_ip: Option<String> = None;

    while start.elapsed() < TRANSACTION_TEST_TIMEOUT {
        if let Some((_conn_ssid, ip_str)) = check_station_connected() {
            connected_ip = Some(ip_str);
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }

    if let Some(ip_str) = connected_ip {
        // --- ЭТАП 3А. Подтверждение (Успех) ---
        // 1-2. Фиксируем новую сеть как единственную основную в /bkupgrade/data/wpa_supplicant.conf атомарно
        if let Ok(mut cfg) = WpaConfigFile::load() {
            cfg.upsert_network(ssid, psk, is_hidden, true);
            if let Err(error) = cfg.save_atomic() {
                crate::wpa_ctrl::append_debug_log(&format!(
                    "Failed to persist successfully tested network {}: {}",
                    ssid, error
                ));
                if let Ok(mut g) = state.lock() {
                    g.last_error = Some(format!("Не удалось сохранить сеть: {}", error));
                    g.last_result = "failed".to_string();
                }
            }
        }

        // Удаляем временный тестовый профиль из RAM и перезагружаем сохранённый конфиг
        if let Some(id) = temp_sta_id {
            let _ = wpa_ctrl::stop_ap_mode(Some(id));
        }
        let _ = wpa_ctrl::reload_config_and_reconnect();

        // 3-4. Полностью закрываем контроллер AP и размораживаем штатный демон platformd (SIGCONT)
        if let Some(ap_ctrl) = active_ap.take() {
            let _ = ap_ctrl.shutdown_and_resume_station(false);
        } else {
            process_guard::ensure_all_resumed();
        }

        if let Ok(mut g) = state.lock() {
            g.mode = OperatingMode::Station;
            g.connected_ssid = Some(ssid.to_string());
            g.current_ip = Some(ip_str);
            g.last_error = None;
            g.last_result = "success".to_string();
            g.busy_transaction = false;
        }
        voice::speak("Подключение к новой сети успешно.");
    } else {
        // --- ЭТАП 3Б. Откат (Ошибка пароля или недоступность сети) ---
        let _ = WpaConfigFile::restore_from_backup();

        let rollback_result = if let Some(ref mut ap) = active_ap {
            ap.rollback_to_ap(temp_sta_id)
        } else {
            match ApController::enter_ap_mode() {
                Ok((ap_ctrl, _)) => {
                    *active_ap = Some(ap_ctrl);
                    Ok(())
                }
                Err(error) => Err(error),
            }
        };

        if let Err(rollback_error) = rollback_result {
            crate::wpa_ctrl::append_debug_log(&format!(
                "AP rollback failed: {}. Falling back to STA.",
                rollback_error
            ));
            if let Some(ap_ctrl) = active_ap.take() {
                let _ = ap_ctrl.shutdown_and_resume_station(true);
            } else {
                process_guard::ensure_all_resumed();
                let _ = wpa_ctrl::restart_to_station();
                net_iface::request_dhcp_rebind();
            }
            if let Ok(mut g) = state.lock() {
                g.mode = OperatingMode::Station;
                g.connected_ssid = None;
                g.current_ip = net_iface::get_station_ipv4().map(|ip| ip.to_string());
                g.last_result = "failed".to_string();
                g.last_error = Some(format!("Не удалось восстановить AP: {}", rollback_error));
                g.busy_transaction = false;
            }
            return;
        }

        if let Ok(mut g) = state.lock() {
            g.mode = OperatingMode::AccessPoint;
            g.connected_ssid = None;
            g.current_ip = Some("192.168.4.1".to_string());
            g.last_result = "failed".to_string();
            g.last_error = Some(
                "Не удалось подключиться к сети. Проверьте правильность пароля и повторите попытку"
                    .to_string(),
            );
            g.busy_transaction = false;
        }

        // Снова поднимаем HTTP-сервер на 192.168.4.1:80
        if let Ok(http) = HttpServerHandle::start(Arc::clone(state), cmd_tx.clone()) {
            *active_http = Some(http);
        }
        voice::speak("Не удалось подключиться. Возвращаю точку доступа для настройки.");
    }
}

/// Запускает фоновый слушатель `/var/run/alice-wifi-manager.sock` для CLI-команд (`start-ap`, `stop-ap`, `status`).
fn start_cli_ipc_listener(
    state: Arc<Mutex<SharedManagerState>>,
    cmd_tx: Sender<TransitionCommand>,
    term_flag: Arc<AtomicBool>,
) {
    let _ = fs::remove_file(CLI_CONTROL_SOCKET);
    let listener = match UnixListener::bind(CLI_CONTROL_SOCKET) {
        Ok(listener) => listener,
        Err(error) => {
            crate::wpa_ctrl::append_debug_log(&format!(
                "Failed to bind manager control socket {}: {}",
                CLI_CONTROL_SOCKET, error
            ));
            return;
        }
    };
    let _ = listener.set_nonblocking(true);

    thread::spawn(move || {
        while !term_flag.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut cmd_buf = [0u8; 128];
                    if let Ok(n) = stream.read(&mut cmd_buf) {
                        let cmd_str = String::from_utf8_lossy(&cmd_buf[..n]).trim().to_string();
                        let reply = match cmd_str.as_str() {
                            "start-ap" => {
                                let _ = cmd_tx.send(TransitionCommand::ForceStartAp);
                                "OK: Switching to Access Point mode (Alice-Mini2-Setup)\n"
                                    .to_string()
                            }
                            "stop-ap" => {
                                let _ = cmd_tx.send(TransitionCommand::StopApAndReturnToPrimary);
                                "OK: Stopping Access Point mode and returning to primary Wi-Fi\n"
                                    .to_string()
                            }
                            "status" => format_cli_status(&state),
                            _ => "ERR: Unknown command\n".to_string(),
                        };
                        let _ = stream.write_all(reply.as_bytes());
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(_) => {
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }
    });
}

pub fn format_cli_status(state: &Arc<Mutex<SharedManagerState>>) -> String {
    let mode_str = state.lock().ok().map(|g| g.mode_str()).unwrap_or("UNKNOWN");

    let wpa = wpa_ctrl::get_status().unwrap_or_default();
    let cfg = WpaConfigFile::load().ok();
    let primary_ssid = cfg
        .as_ref()
        .and_then(|c| c.get_primary().map(|n| n.ssid.clone()))
        .unwrap_or_else(|| "none".to_string());

    let mut out = String::new();
    out.push_str("=== Alice WiFi Manager Status ===\n");
    out.push_str(&format!("Manager Mode : {}\n", mode_str));
    out.push_str(&format!(
        "WPA State    : {} (mode={})\n",
        wpa.wpa_state, wpa.mode
    ));
    out.push_str(&format!(
        "Active SSID  : {}\n",
        wpa.ssid.unwrap_or_else(|| "-".to_string())
    ));
    out.push_str(&format!(
        "IPv4 Address : {}\n",
        net_iface::get_station_ipv4()
            .map(|ip| ip.to_string())
            .or(wpa.ip_address)
            .unwrap_or_else(|| "-".to_string())
    ));
    out.push_str(&format!("Primary Net  : {}\n", primary_ssid));
    out.push_str(&format!(
        "platformd    : {}\n",
        if process_guard::is_platformd_stopped() {
            "PAUSED (SIGSTOP)"
        } else {
            "RUNNING"
        }
    ));

    if let Some(c) = cfg {
        out.push_str("\nSaved Networks in /bkupgrade/data/wpa_supplicant.conf:\n");
        if c.networks.is_empty() {
            out.push_str("  (empty)\n");
        } else {
            for net in c.networks {
                let badge = if net.is_primary {
                    "[★ PRIMARY]"
                } else {
                    "[disabled=1]"
                };
                out.push_str(&format!(
                    "  #{} {} ({}) {}\n",
                    net.id, net.ssid, net.key_mgmt, badge
                ));
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{should_start_automatic_ap, PRIMARY_WAIT_TIMEOUT};
    use std::time::{Duration, Instant};

    #[test]
    fn sta_loss_under_twenty_seconds_does_not_start_ap() {
        let started = Instant::now();
        assert!(!should_start_automatic_ap(
            Some(started),
            started + Duration::from_secs(10),
            None
        ));
    }

    #[test]
    fn sta_recovery_clears_timer_input() {
        let started = Instant::now();
        assert!(!should_start_automatic_ap(
            None,
            started + PRIMARY_WAIT_TIMEOUT,
            None
        ));
    }

    #[test]
    fn sta_loss_after_twenty_seconds_starts_ap() {
        let started = Instant::now();
        assert!(should_start_automatic_ap(
            Some(started),
            started + PRIMARY_WAIT_TIMEOUT,
            None
        ));
    }

    #[test]
    fn failed_ap_attempt_is_rate_limited() {
        let started = Instant::now();
        assert!(!should_start_automatic_ap(
            Some(started),
            started + PRIMARY_WAIT_TIMEOUT,
            Some(started + Duration::from_secs(5))
        ));
    }
}

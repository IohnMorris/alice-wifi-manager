pub mod dhcp;
pub mod dns;

use crate::net_iface;
use crate::process_guard::{self, ProcessGuard};
use crate::wpa_ctrl::{self, ScannedNetwork};
use dhcp::DhcpServerHandle;
use dns::DnsServerHandle;
use std::thread;
use std::time::Duration;

pub struct ApController {
    process_guard: Option<ProcessGuard>,
    ap_net_id: Option<i32>,
    dhcp_handle: Option<DhcpServerHandle>,
    dns_handle: Option<DnsServerHandle>,
}

impl ApController {
    /// Полный запуск режима точки доступа по алгоритму раздела 3.2 ТЗ:
    /// 1. Предсканирование эфира в кэш.
    /// 2. Заморозка `maind --platformd` и `dhcpcd` (`SIGSTOP`).
    /// 3. Очистка старых IP-адресов (`ip addr flush dev wlan0`).
    /// 4. Активация временного профиля `Alice-Mini2-Setup` (`mode=2`, `2437 МГц`).
    /// 5. Назначение `192.168.4.1/24` и запуск встроенных серверов DHCP и DNS.
    pub fn enter_ap_mode() -> Result<(Self, Vec<ScannedNetwork>), String> {
        crate::wpa_ctrl::append_debug_log("Step 1: Pre-scanning Wi-Fi networks into cache...");
        let cached_scan = match wpa_ctrl::scan_networks() {
            Ok(networks) => networks,
            Err(error) => {
                crate::wpa_ctrl::append_debug_log(&format!(
                    "Pre-scan failed; AP will use an empty scan cache: {}",
                    error
                ));
                Vec::new()
            }
        };
        crate::wpa_ctrl::append_debug_log(&format!(
            "Pre-scan found {} networks",
            cached_scan.len()
        ));

        crate::wpa_ctrl::append_debug_log(
            "Step 2: Freezing maind --platformd and dhcpcd (SIGSTOP)...",
        );
        let frozen_report = match process_guard::freeze_all_conflicting_verified() {
            Ok(rep) => rep,
            Err(e) => {
                crate::wpa_ctrl::append_debug_log(&format!("ERROR in process_guard: {}", e));
                process_guard::ensure_all_resumed();
                return Err(e);
            }
        };
        for item in &frozen_report {
            crate::wpa_ctrl::append_debug_log(&format!(
                "Frozen PID {} ({}) -> state '{}'",
                item.pid, item.cmdline, item.state_char
            ));
        }
        let mut guard = Some(ProcessGuard::from_frozen_report(frozen_report));

        crate::wpa_ctrl::append_debug_log("Step 3: Flushing old IPv4 from wlan0...");
        if let Err(e) = net_iface::flush_wlan0_ip() {
            return Err(rollback_ap_start(&mut guard, e));
        }

        crate::wpa_ctrl::append_debug_log("Step 4: Restarting wpa_supplicant with AP config...");
        if let Err(e) = wpa_ctrl::start_ap_mode() {
            return Err(rollback_ap_start(&mut guard, e));
        }

        crate::wpa_ctrl::append_debug_log(
            "Step 5: Assigning 192.168.4.1/24 and verifying wlan0 UP...",
        );
        if let Err(e) = net_iface::configure_ap_ip() {
            return Err(rollback_ap_start(&mut guard, e));
        }

        crate::wpa_ctrl::append_debug_log(&format!(
            "wlan0 state after AP IP assign:\n{}",
            net_iface::dump_wlan0_state()
        ));

        crate::wpa_ctrl::append_debug_log("Step 6: Starting embedded DHCP and DNS servers...");
        let mut dhcp_handle = match DhcpServerHandle::start() {
            Ok(handle) => handle,
            Err(e) => {
                return Err(rollback_ap_start(&mut guard, e));
            }
        };

        let dns_handle = match DnsServerHandle::start() {
            Ok(handle) => handle,
            Err(e) => {
                crate::wpa_ctrl::append_debug_log(&format!("DNS startup failed: {}", e));
                dhcp_handle.stop();
                return Err(rollback_ap_start(&mut guard, e));
            }
        };
        crate::wpa_ctrl::append_debug_log("=== AP mode fully operational ===");

        Ok((
            Self {
                process_guard: guard,
                ap_net_id: None,
                dhcp_handle: Some(dhcp_handle),
                dns_handle: Some(dns_handle),
            },
            cached_scan,
        ))
    }

    /// Временно останавливает службы DHCP/DNS и удаляет профиль AP из памяти `wpa_supplicant`
    /// для проведения 20-секундной транзакционной проверки новой домашней сети (Этапы 1–2).
    /// При этом `maind --platformd` остаётся замороженным!
    pub fn suspend_ap_for_network_test(&mut self) -> Result<(), String> {
        if let Some(mut dns) = self.dns_handle.take() {
            dns.stop();
        }
        if let Some(mut dhcp) = self.dhcp_handle.take() {
            dhcp.stop();
        }

        let _ = net_iface::flush_wlan0_ip();
        self.ap_net_id.take();
        crate::wpa_ctrl::append_debug_log(
            "Suspending AP for network test: restarting wpa_supplicant in STA mode",
        );
        wpa_ctrl::restart_to_station()?;
        Ok(())
    }

    /// Откат (Этап 3Б): снова замораживает `dhcpcd`, очищает `wlan0` и немедленно поднимает
    /// обратно точку доступа `Alice-Mini2-Setup` и серверы DHCP/DNS.
    pub fn rollback_to_ap(&mut self, temp_sta_net_id: Option<i32>) -> Result<(), String> {
        crate::wpa_ctrl::append_debug_log(
            "AP rollback: reusing the existing process guard; no second freeze cycle",
        );
        let _ = net_iface::flush_wlan0_ip();

        if let Some(sta_id) = temp_sta_net_id {
            let _ = wpa_ctrl::stop_ap_mode(Some(sta_id));
        }

        wpa_ctrl::start_ap_mode()?;
        self.ap_net_id = None;

        thread::sleep(Duration::from_millis(600));
        net_iface::configure_ap_ip()?;

        let dhcp = DhcpServerHandle::start()?;
        let dns = match DnsServerHandle::start() {
            Ok(handle) => handle,
            Err(error) => {
                crate::wpa_ctrl::append_debug_log(&format!(
                    "AP rollback DNS startup failed: {}",
                    error
                ));
                let mut dhcp = dhcp;
                dhcp.stop();
                return Err(error);
            }
        };
        self.dhcp_handle = Some(dhcp);
        self.dns_handle = Some(dns);
        Ok(())
    }

    /// Полный выход из режима точки доступа (при успешном подключении на Этапе 3А или по команде `stop-ap`):
    /// останавливает DHCP/DNS, перезагружает рабочий конфиг и размораживает `platformd` и `dhcpcd` (`SIGCONT`).
    pub fn shutdown_and_resume_station(mut self, reload_wpa: bool) -> Result<(), String> {
        if let Some(mut dns) = self.dns_handle.take() {
            dns.stop();
        }
        if let Some(mut dhcp) = self.dhcp_handle.take() {
            dhcp.stop();
        }

        if let Some(ap_id) = self.ap_net_id.take() {
            let _ = net_iface::flush_wlan0_ip();
            let _ = wpa_ctrl::stop_ap_mode(Some(ap_id));
        }

        let mut station_result = Ok(());
        if reload_wpa {
            crate::wpa_ctrl::append_debug_log("STA transition: controlled wpa_supplicant restart");
            station_result = wpa_ctrl::restart_to_station();
        }

        if let Some(mut guard) = self.process_guard.take() {
            guard.release();
        } else {
            process_guard::ensure_all_resumed();
        }

        net_iface::request_dhcp_rebind();
        if station_result.is_ok()
            && reload_wpa
            && !net_iface::wait_for_station_ipv4(Duration::from_secs(15))
        {
            station_result =
                Err("STA wpa_supplicant started but DHCP did not assign IPv4".to_string());
        }
        station_result
    }
}

fn rollback_ap_start(guard: &mut Option<ProcessGuard>, reason: String) -> String {
    crate::wpa_ctrl::append_debug_log(&format!("AP rollback started: {}", reason));
    if let Err(error) = wpa_ctrl::restart_to_station() {
        crate::wpa_ctrl::append_debug_log(&format!("AP rollback STA restart failed: {}", error));
    }
    let _ = net_iface::flush_wlan0_ip();
    if let Some(mut process_guard) = guard.take() {
        process_guard.release();
    }
    crate::wpa_ctrl::append_debug_log("AP rollback completed");
    net_iface::request_dhcp_rebind();
    reason
}

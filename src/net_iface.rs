use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

pub const WIFI_IFACE: &str = "wlan0";
pub const AP_IP_CIDR: &str = "192.168.4.1/24";
pub const AP_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);

const BUSYBOX_BIN: &str = "/bin/busybox";
const DHCPCD_BIN: &str = "/sbin/dhcpcd";

pub fn wait_for_wlan0(timeout: Duration) -> bool {
    let start = Instant::now();
    let sys_path = format!("/sys/class/net/{}", WIFI_IFACE);
    while start.elapsed() < timeout {
        if Path::new(&sys_path).exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(250));
    }
    false
}

/// Проверяет по `/sys/class/net/wlan0/flags`, поднят ли интерфейс (бит IFF_UP = 0x1).
pub fn is_wlan0_up() -> bool {
    let path = format!("/sys/class/net/{}/flags", WIFI_IFACE);
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let clean = raw.trim().trim_start_matches("0x");
    if let Ok(flags) = u32::from_str_radix(clean, 16) {
        (flags & 0x1) != 0
    } else {
        false
    }
}

/// Явно поднимает интерфейс `ip link set wlan0 up` и проверяет флаг `IFF_UP`.
pub fn set_wlan0_up() -> Result<(), String> {
    let output = Command::new(BUSYBOX_BIN)
        .args(["ip", "link", "set", WIFI_IFACE, "up"])
        .output()
        .map_err(|e| format!("Failed to execute ip link set up: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ip link set wlan0 up failed: {}", stderr.trim()));
    }

    thread::sleep(Duration::from_millis(100));
    if !is_wlan0_up() {
        return Err("wlan0 interface did not set IFF_UP flag after ip link set up".to_string());
    }
    Ok(())
}

pub fn flush_wlan0_ip() -> Result<(), String> {
    let output = Command::new(BUSYBOX_BIN)
        .args(["ip", "addr", "flush", "dev", WIFI_IFACE])
        .output()
        .map_err(|e| format!("Failed to run busybox ip flush: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ip addr flush failed: {}", stderr.trim()));
    }
    Ok(())
}

/// Назначает `192.168.4.1/24` и проверяет, что адрес действительно присутствует на `wlan0`.
pub fn configure_ap_ip() -> Result<(), String> {
    flush_wlan0_ip()?;
    set_wlan0_up()?;

    let output = Command::new(BUSYBOX_BIN)
        .args(["ip", "addr", "add", AP_IP_CIDR, "dev", WIFI_IFACE])
        .output()
        .map_err(|e| format!("Failed to assign AP IP: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("File exists") {
            return Err(format!("ip addr add failed: {}", stderr.trim()));
        }
    }

    // Верифицируем, что 192.168.4.1 реально назначен на интерфейс
    let dump = dump_wlan0_state();
    if !dump.contains("192.168.4.1") {
        return Err(format!(
            "Verification failed: 192.168.4.1 not found on wlan0. Current state: {}",
            dump
        ));
    }

    Ok(())
}

/// Возвращает полный вывод `ip addr show dev wlan0` для записи в диагностический лог.
pub fn dump_wlan0_state() -> String {
    Command::new(BUSYBOX_BIN)
        .args(["ip", "addr", "show", "dev", WIFI_IFACE])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("ip addr show error: {}", e))
}

pub fn get_station_ipv4() -> Option<Ipv4Addr> {
    let output = Command::new(BUSYBOX_BIN)
        .args(["ip", "-4", "addr", "show", "dev", WIFI_IFACE])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("inet ") {
            let addr_part = rest.split_whitespace().next()?;
            let ip_str = addr_part.split('/').next()?;
            if let Ok(ip) = ip_str.parse::<Ipv4Addr>() {
                if ip != AP_IP && ip.octets()[0] != 169 && !ip.is_loopback() {
                    return Some(ip);
                }
            }
        }
    }
    None
}

pub fn request_dhcp_rebind() {
    let _ = Command::new(DHCPCD_BIN).args(["-n", WIFI_IFACE]).output();
}

pub fn wait_for_station_ipv4(timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if get_station_ipv4().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(250));
    }
    false
}

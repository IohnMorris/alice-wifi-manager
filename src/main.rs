mod ap;
mod config_store;
mod http;
mod net_iface;
mod process_guard;
mod state_machine;
mod voice;
mod wpa_ctrl;

use state_machine::{SharedManagerState, CLI_CONTROL_SOCKET};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static TERM_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_termination_signal(_sig: libc::c_int) {
    TERM_REQUESTED.store(true, Ordering::SeqCst);
}

fn register_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, handle_termination_signal as usize);
        libc::signal(libc::SIGINT, handle_termination_signal as usize);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

fn send_cli_command(cmd: &str) -> Result<String, String> {
    if !Path::new(CLI_CONTROL_SOCKET).exists() {
        return Err(
            "Daemon control socket not found (/var/run/alice-wifi-manager.sock)".to_string(),
        );
    }
    let mut stream = UnixStream::connect(CLI_CONTROL_SOCKET)
        .map_err(|e| format!("Failed to connect to daemon socket: {}", e))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    stream
        .write_all(cmd.as_bytes())
        .map_err(|e| format!("Failed to send command: {}", e))?;

    let mut resp = String::new();
    stream
        .read_to_string(&mut resp)
        .map_err(|e| format!("Failed to read response: {}", e))?;
    Ok(resp)
}

fn force_emergency_sta_restore() {
    process_guard::ensure_all_resumed();
    if let Err(e) = wpa_ctrl::restart_to_station() {
        let msg = format!("Emergency controlled STA restore failed: {}", e);
        eprintln!("[alice-wifi-manager] {}", msg);
        wpa_ctrl::append_debug_log(&msg);
    }
    net_iface::request_dhcp_rebind();
}

struct EmergencyStaGuard {
    active: bool,
}

impl EmergencyStaGuard {
    fn new() -> Self {
        Self { active: true }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for EmergencyStaGuard {
    fn drop(&mut self) {
        if self.active {
            eprintln!("[diag-ap] Emergency guard triggered: forcing fallback to STA mode...");
            wpa_ctrl::append_debug_log(
                "EmergencyStaGuard triggered: running low-level STA restore...",
            );
            force_emergency_sta_restore();
        }
    }
}

fn run_diag_ap() -> Result<(), String> {
    register_signal_handlers();
    let mut rollback_guard = EmergencyStaGuard::new();

    println!("Running safe 20-second AP diagnostic (auto-revert to STA enabled)...");
    println!("Log file: {}", wpa_ctrl::AP_DEBUG_LOG_PATH);
    wpa_ctrl::append_debug_log("=== Starting diag-ap (20s safe diagnostic) ===");

    let (ap_ctrl, _) = ap::ApController::enter_ap_mode().map_err(|e| {
        let err_msg = format!("enter_ap_mode failed: {}", e);
        wpa_ctrl::append_debug_log(&err_msg);
        err_msg
    })?;

    wpa_ctrl::append_debug_log(
        "Holding AP mode for 20 seconds for phone Wi-Fi visibility check...",
    );

    // Ожидаем 20 секунд (100 шагов по 200 мс) с контролем сигналов завершения
    for _ in 0..100 {
        if TERM_REQUESTED.load(Ordering::SeqCst) {
            println!(
                "\nTermination signal received during diag-ap! Reverting to STA immediately..."
            );
            wpa_ctrl::append_debug_log(
                "Signal received during 20s hold. Triggering immediate rollback to STA...",
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    wpa_ctrl::append_debug_log("Reverting back to primary STA network...");

    match ap_ctrl.shutdown_and_resume_station(true) {
        Ok(()) => {
            process_guard::ensure_all_resumed();
            rollback_guard.disarm();
            wpa_ctrl::append_debug_log("diag-ap finished: cleanly reverted to STA.");
            println!("Diagnostic finished: AP succeeded and reverted to STA.");
            Ok(())
        }
        Err(e) => {
            let err_msg = format!(
                "shutdown_and_resume_station returned error: {}. Executing emergency fallback...",
                e
            );
            eprintln!("[diag-ap] {}", err_msg);
            wpa_ctrl::append_debug_log(&err_msg);
            // При выходе из функции сработает Drop у rollback_guard и вызовет force_emergency_sta_restore()
            Err(err_msg)
        }
    }
}

fn run_uninstall(args: &[String]) -> Result<(), String> {
    if !args.iter().any(|arg| arg == "--yes") {
        return Err("Refusing to uninstall without confirmation. Re-run: \
             alice-wifi-manager uninstall --yes"
            .to_string());
    }

    if unsafe { libc::geteuid() } != 0 {
        return Err("uninstall must be run as root".to_string());
    }

    const RECOVERY_ROOT: &str = "/bkupgrade/data";
    let stamp = format!(
        "recovery-uninstall-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("Failed to read system time: {}", e))?
            .as_secs()
    );
    let recovery_dir = Path::new(RECOVERY_ROOT).join(stamp);
    fs::create_dir_all(&recovery_dir)
        .map_err(|e| format!("Failed to create recovery directory: {}", e))?;

    for source in [
        "/usr/bin/alice-wifi-manager",
        "/etc/init.d/S99z-wifi-manager",
        "/usr/sbin/wpa_supplicant",
        "/bkupgrade/data/wpa_supplicant.conf",
        "/bkupgrade/data/wpa_supplicant.conf.bak",
        "/bkupgrade/data/wpa_supplicant.ap.conf",
    ] {
        if Path::new(source).exists() {
            let destination = recovery_dir.join(
                Path::new(source)
                    .file_name()
                    .ok_or_else(|| format!("Invalid recovery path: {}", source))?,
            );
            fs::copy(source, destination)
                .map_err(|e| format!("Failed to preserve {}: {}", source, e))?;
        }
    }

    let stop_status = Command::new("/etc/init.d/S99z-wifi-manager")
        .arg("stop")
        .status()
        .map_err(|e| format!("Failed to stop manager: {}", e))?;
    if !stop_status.success() {
        return Err(format!(
            "Manager stop failed; nothing was removed. Recovery: {}",
            recovery_dir.display()
        ));
    }

    process_guard::ensure_all_resumed();
    for path in [
        "/usr/bin/alice-wifi-manager",
        "/etc/init.d/S99z-wifi-manager",
        "/var/run/alice-wifi-manager.pid",
        "/var/run/alice-wifi-manager.sock",
        "/bkupgrade/data/wpa_supplicant.ap.conf",
    ] {
        if Path::new(path).exists() {
            fs::remove_file(path).map_err(|e| format!("Failed to remove {}: {}", path, e))?;
        }
    }

    println!(
        "Manager removed. Wi-Fi profiles and wpa_supplicant were preserved.\nRecovery: {}",
        recovery_dir.display()
    );
    Ok(())
}

fn print_usage() {
    println!("Alice WiFi Manager v2.1 (Yandex Station Mini 2)");
    println!("Usage:");
    println!("  alice-wifi-manager [daemon]   Run background provisioning service");
    println!("  alice-wifi-manager start-ap   Force start Access Point (Alice-Mini2-Setup)");
    println!("  alice-wifi-manager stop-ap    Stop Access Point and return to primary Wi-Fi");
    println!("  alice-wifi-manager status     Show connection status and saved networks");
    println!("  alice-wifi-manager diag-ap    Run 20s safe AP diagnostic with auto-rollback");
    println!("  alice-wifi-manager uninstall --yes  Remove manager and preserve Wi-Fi data");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str()).unwrap_or("daemon");

    match subcommand {
        "daemon" => {
            register_signal_handlers();
            let term_flag = Arc::new(AtomicBool::new(false));

            // Синхронизируем глобальный атомик сигнала с флагом остановки демона
            let flag_clone = Arc::clone(&term_flag);
            std::thread::spawn(move || {
                while !TERM_REQUESTED.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                flag_clone.store(true, Ordering::SeqCst);
            });

            if let Err(e) = state_machine::run_daemon(term_flag) {
                eprintln!("[alice-wifi-manager] Fatal error: {}", e);
                process_guard::ensure_all_resumed();
                std::process::exit(1);
            }
        }

        "start-ap" => match send_cli_command("start-ap") {
            Ok(reply) => print!("{}", reply),
            Err(e) => {
                eprintln!("Error: {}. Is alice-wifi-manager daemon running?", e);
                std::process::exit(1);
            }
        },

        "stop-ap" => match send_cli_command("stop-ap") {
            Ok(reply) => print!("{}", reply),
            Err(_) => {
                // Даже если демон не запущен, принудительно размораживаем системные службы
                force_emergency_sta_restore();
                println!("OK: System services resumed and wpa_supplicant reconfigured");
            }
        },

        "status" => match send_cli_command("status") {
            Ok(reply) => print!("{}", reply),
            Err(_) => {
                let fallback_state = Arc::new(Mutex::new(SharedManagerState::new()));
                print!("{}", state_machine::format_cli_status(&fallback_state));
            }
        },

        "diag-ap" => {
            if let Err(e) = run_diag_ap() {
                eprintln!("Diagnostic caught AP failure (rolled back to STA): {}", e);
                std::process::exit(1);
            }
        }

        "uninstall" => {
            if let Err(e) = run_uninstall(&args[2..]) {
                eprintln!("Uninstall failed: {}", e);
                std::process::exit(1);
            }
        }

        "-h" | "--help" | "help" => print_usage(),

        other => {
            eprintln!("Unknown command: {}", other);
            print_usage();
            std::process::exit(1);
        }
    }
}

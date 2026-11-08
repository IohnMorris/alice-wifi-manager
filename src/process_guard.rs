use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const FROZEN_MARKER: &str = "/var/run/alice-wifi-manager.frozen";

#[derive(Debug, Clone)]
pub struct FrozenProcessReport {
    pub pid: i32,
    pub cmdline: String,
    pub state_char: char,
    pub confirmed_stopped: bool,
    pub stopped_by_manager: bool,
}

/// Ищет в /proc PID всех процессов `maind --platformd` и `dhcpcd`.
pub fn find_conflicting_pids() -> Vec<(i32, String)> {
    let mut result = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return result;
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = name_str.parse::<i32>() else {
            continue;
        };

        let cmdline_path = entry.path().join("cmdline");
        let Ok(raw_cmd) = fs::read(&cmdline_path) else {
            continue;
        };
        if raw_cmd.is_empty() {
            continue;
        }

        let args = command_args(&raw_cmd);
        let cmdline = args.join(" ");
        let is_platformd = args
            .iter()
            .any(|arg| arg.ends_with("/maind") || arg == "maind")
            && args.iter().any(|arg| arg == "--platformd");
        let is_dhcpcd = args
            .first()
            .map(|arg| arg.ends_with("/dhcpcd") || arg == "dhcpcd")
            .unwrap_or(false);

        if is_platformd || is_dhcpcd {
            result.push((pid, cmdline));
        }
    }

    result
}

/// Читает символ состояния процесса (R, S, D, T, t, Z) из `/proc/<pid>/stat`.
pub fn read_proc_state(pid: i32) -> Option<char> {
    let stat_path = format!("/proc/{}/stat", pid);
    let content = fs::read_to_string(Path::new(&stat_path)).ok()?;
    // В /proc/<pid>/stat имя процесса в скобках: "1234 (maind) T ..."
    let rparen = content.rfind(')')?;
    let after = content.get(rparen + 2..)?;
    after.chars().next()
}

/// Замораживает `maind --platformd` и `dhcpcd` и проверяет, что каждый PID перешёл в состояние `T`.
pub fn freeze_all_conflicting_verified() -> Result<Vec<FrozenProcessReport>, String> {
    let targets = find_conflicting_pids();
    let initial_states = targets
        .iter()
        .map(|(pid, _)| (*pid, read_proc_state(*pid).unwrap_or('?')))
        .collect::<std::collections::HashMap<_, _>>();

    for (pid, _) in &targets {
        if initial_states.get(pid) == Some(&'T') {
            continue;
        }
        unsafe {
            libc::kill(*pid, libc::SIGSTOP);
        }
    }

    let deadline = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < deadline {
        let all_stopped = targets
            .iter()
            .all(|(pid, _)| matches!(read_proc_state(*pid), Some('T') | None));
        if all_stopped {
            break;
        }
        for (pid, _) in &targets {
            if initial_states.get(pid) == Some(&'T') {
                continue;
            }
            unsafe {
                libc::kill(*pid, libc::SIGSTOP);
            }
        }
        thread::sleep(Duration::from_millis(80));
    }

    let mut reports = Vec::new();
    let mut unstopped = Vec::new();

    for (pid, cmdline) in targets {
        let st = read_proc_state(pid).unwrap_or('?');
        let ok = st == 'T' || st == '?'; // '?' если процесс уже завершился сам
        if !ok {
            unstopped.push(format!(
                "PID {} ({}) remained in state '{}'",
                pid, cmdline, st
            ));
        }
        reports.push(FrozenProcessReport {
            pid,
            cmdline,
            state_char: st,
            confirmed_stopped: ok,
            stopped_by_manager: initial_states.get(&pid) != Some(&'T') && st == 'T',
        });
    }

    if !unstopped.is_empty() {
        resume_reported_processes(&reports);
        return Err(format!(
            "Failed to freeze conflicting processes: {}",
            unstopped.join("; ")
        ));
    }

    write_frozen_marker(&reports);
    Ok(reports)
}

pub fn freeze_all_conflicting() -> Vec<i32> {
    match freeze_all_conflicting_verified() {
        Ok(list) => list.into_iter().map(|r| r.pid).collect(),
        Err(_) => Vec::new(),
    }
}

pub fn freeze_dhcpcd_only() {
    for (pid, cmdline) in find_conflicting_pids() {
        if is_dhcpcd_cmdline(&cmdline) {
            unsafe {
                libc::kill(pid, libc::SIGSTOP);
            }
        }
    }
}

pub fn resume_dhcpcd_only() {
    for (pid, cmdline) in find_conflicting_pids() {
        if is_dhcpcd_cmdline(&cmdline) {
            unsafe {
                libc::kill(pid, libc::SIGCONT);
            }
        }
    }
}

pub fn ensure_all_resumed() {
    let Ok(content) = fs::read_to_string(FROZEN_MARKER) else {
        return;
    };

    for line in content.lines() {
        let Some((pid_text, expected_cmdline)) = line.split_once('\t') else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<i32>() else {
            continue;
        };
        if process_matches(pid, expected_cmdline) && read_proc_state(pid) == Some('T') {
            unsafe {
                libc::kill(pid, libc::SIGCONT);
            }
        }
    }
    let _ = fs::remove_file(FROZEN_MARKER);
}

pub struct ProcessGuard {
    active: bool,
    frozen: Vec<FrozenProcessReport>,
}

impl ProcessGuard {
    pub fn from_frozen_report(frozen: Vec<FrozenProcessReport>) -> Self {
        Self {
            active: true,
            frozen,
        }
    }

    pub fn release(&mut self) {
        if self.active {
            resume_reported_processes(&self.frozen);
            clear_frozen_marker();
            self.active = false;
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if self.active {
            resume_reported_processes(&self.frozen);
            clear_frozen_marker();
        }
    }
}

fn resume_reported_processes(processes: &[FrozenProcessReport]) {
    for process in processes {
        if !process.stopped_by_manager || !process_matches(process.pid, &process.cmdline) {
            continue;
        }
        if read_proc_state(process.pid) == Some('T') {
            unsafe {
                libc::kill(process.pid, libc::SIGCONT);
            }
        }
    }
}

fn process_matches(pid: i32, expected_cmdline: &str) -> bool {
    let path = format!("/proc/{}/cmdline", pid);
    let Ok(raw) = fs::read(path) else {
        return false;
    };
    let current = command_args(&raw).join(" ");
    current == expected_cmdline
}

fn command_args(raw: &[u8]) -> Vec<String> {
    raw.split(|&b| b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

fn is_dhcpcd_cmdline(cmdline: &str) -> bool {
    cmdline
        .split_whitespace()
        .next()
        .map(|arg| arg.ends_with("/dhcpcd") || arg == "dhcpcd")
        .unwrap_or(false)
}

fn write_frozen_marker(processes: &[FrozenProcessReport]) {
    let content = processes
        .iter()
        .filter(|process| process.stopped_by_manager)
        .map(|process| format!("{}\t{}", process.pid, process.cmdline))
        .collect::<Vec<_>>()
        .join("\n");
    if content.is_empty() {
        let _ = fs::remove_file(FROZEN_MARKER);
        return;
    }

    let temporary = format!("{}.tmp", FROZEN_MARKER);
    if fs::write(&temporary, format!("{}\n", content)).is_ok() {
        let _ = fs::rename(temporary, FROZEN_MARKER);
    }
}

fn clear_frozen_marker() {
    let _ = fs::remove_file(FROZEN_MARKER);
}

pub fn is_platformd_stopped() -> bool {
    for (pid, cmdline) in find_conflicting_pids() {
        if cmdline.contains("--platformd") && read_proc_state(pid) == Some('T') {
            return true;
        }
    }
    false
}

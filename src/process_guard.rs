use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

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

        let cmdline = raw_cmd
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect::<Vec<String>>()
            .join(" ");

        let is_platformd = cmdline.contains("maind") && cmdline.contains("--platformd");
        let is_dhcpcd = cmdline.contains("dhcpcd");

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
        if cmdline.contains("dhcpcd") {
            unsafe {
                libc::kill(pid, libc::SIGSTOP);
            }
        }
    }
}

pub fn resume_dhcpcd_only() {
    for (pid, cmdline) in find_conflicting_pids() {
        if cmdline.contains("dhcpcd") {
            unsafe {
                libc::kill(pid, libc::SIGCONT);
            }
        }
    }
}

pub fn ensure_all_resumed() {
    for (pid, _cmdline) in find_conflicting_pids() {
        unsafe {
            libc::kill(pid, libc::SIGCONT);
        }
    }

    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let stat_path = entry.path().join("stat");
            if let Ok(stat_str) = fs::read_to_string(stat_path) {
                if (stat_str.contains("(maind)") || stat_str.contains("(dhcpcd)"))
                    && stat_str.contains(") T ")
                {
                    unsafe {
                        libc::kill(pid, libc::SIGCONT);
                    }
                }
            }
        }
    }
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
            self.active = false;
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if self.active {
            resume_reported_processes(&self.frozen);
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
    let current = raw
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect::<Vec<String>>()
        .join(" ");
    current == expected_cmdline
}

pub fn is_platformd_stopped() -> bool {
    for (pid, cmdline) in find_conflicting_pids() {
        if cmdline.contains("--platformd") && read_proc_state(pid) == Some('T') {
            return true;
        }
    }
    false
}

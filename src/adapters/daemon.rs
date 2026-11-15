use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::adapters::state_store::JsonStateStore;
use crate::core::domain::{BotConfig, BotState, DomainError};
use crate::core::ports::StateStorePort;

/// Writes the given process ID into the PID file.
pub fn write_pid_file(pid_path: &Path, pid: u32) -> Result<(), DomainError> {
    if let Some(parent) = pid_path.parent() {
        fs::create_dir_all(parent).map_err(|e| DomainError::DaemonError {
            message: format!("failed to create PID directory: {}", e),
        })?;
    }
    fs::write(pid_path, format!("{}\n", pid)).map_err(|e| DomainError::DaemonError {
        message: format!("failed to write PID file '{}': {}", pid_path.display(), e),
    })
}

/// Reads the process ID from the PID file if it exists.
pub fn read_pid_file(pid_path: &Path) -> Result<Option<u32>, DomainError> {
    if !pid_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(pid_path).map_err(|e| DomainError::DaemonError {
        message: format!("failed to read PID file '{}': {}", pid_path.display(), e),
    })?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let pid = trimmed.parse::<u32>().map_err(|e| DomainError::DaemonError {
        message: format!("invalid PID '{}' in '{}': {}", trimmed, pid_path.display(), e),
    })?;
    Ok(Some(pid))
}

/// Removes the PID file if present.
pub fn remove_pid_file(pid_path: &Path) {
    let _ = fs::remove_file(pid_path);
}

/// Checks whether a Unix process ID is currently alive using `kill -0 <pid>`.
pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Appends a timestamped log line to `bot.log`.
pub fn append_log_line(log_path: &Path, message: &str) -> Result<(), DomainError> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|e| DomainError::DaemonError {
            message: format!("failed to create log directory: {}", e),
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| DomainError::DaemonError {
            message: format!("failed to open log file '{}': {}", log_path.display(), e),
        })?;
    let ts = chrono::Utc::now().to_rfc3339();
    writeln!(file, "[{}] {}", ts, message).map_err(|e| DomainError::DaemonError {
        message: format!("failed to append to log file: {}", e),
    })
}

/// Reads the last `lines` lines from `bot.log`.
pub fn read_recent_logs(log_path: &Path, lines: usize) -> Result<Vec<String>, DomainError> {
    if !log_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(log_path).map_err(|e| DomainError::DaemonError {
        message: format!("failed to read log file '{}': {}", log_path.display(), e),
    })?;
    let all_lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let start = all_lines.len().saturating_sub(lines);
    Ok(all_lines[start..].to_vec())
}

/// Spawns the trading bot worker loop as a detached background daemon process.
pub fn spawn_background_daemon(
    config_path: &Path,
    cfg: &BotConfig,
    store: &JsonStateStore,
) -> Result<u32, DomainError> {
    store.ensure_dir()?;
    let pid_path = store.pid_file_path();

    if let Some(existing_pid) = read_pid_file(&pid_path)? {
        if is_pid_alive(existing_pid) {
            return Err(DomainError::DaemonError {
                message: format!(
                    "bot daemon is already running in background with PID {}",
                    existing_pid
                ),
            });
        }
        remove_pid_file(&pid_path);
    }

    let exe_path = std::env::current_exe().map_err(|e| DomainError::DaemonError {
        message: format!("failed to determine current executable path: {}", e),
    })?;

    let log_path = store.log_file_path();
    let stdout_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| DomainError::DaemonError {
            message: format!("failed to open log file '{}': {}", log_path.display(), e),
        })?;
    let stderr_file = stdout_file.try_clone().map_err(|e| DomainError::DaemonError {
        message: format!("failed to clone log file descriptor: {}", e),
    })?;

    let child = Command::new(exe_path)
        .arg("--config")
        .arg(config_path)
        .arg("run-worker")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .map_err(|e| DomainError::DaemonError {
            message: format!("failed to spawn background daemon process: {}", e),
        })?;

    let pid = child.id();
    write_pid_file(&pid_path, pid)?;

    let mut state = store.load_state()?.unwrap_or_else(|| BotState::new(cfg));
    state.running = true;
    state.pid = Some(pid);
    state.started_at_ms = Some(chrono::Utc::now().timestamp_millis());
    store.save_state(&state)?;

    append_log_line(
        &log_path,
        &format!(
            "Background daemon started (PID={}, mode={}, symbol={}, interval={})",
            pid, cfg.mode, cfg.symbol, cfg.interval
        ),
    )?;

    Ok(pid)
}

/// Stops a running background daemon process via `SIGTERM` and updates state.
pub fn stop_background_daemon(store: &JsonStateStore) -> Result<Option<u32>, DomainError> {
    let pid_path = store.pid_file_path();
    let pid_opt = read_pid_file(&pid_path)?;

    let Some(pid) = pid_opt else {
        if let Some(mut state) = store.load_state()? {
            state.running = false;
            state.pid = None;
            store.save_state(&state)?;
        }
        return Ok(None);
    };

    if is_pid_alive(pid) {
        let status = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| DomainError::DaemonError {
                message: format!("failed to send SIGTERM to PID {}: {}", pid, e),
            })?;

        if !status.success() {
            return Err(DomainError::DaemonError {
                message: format!("kill -TERM failed for PID {}", pid),
            });
        }
    }

    remove_pid_file(&pid_path);

    if let Some(mut state) = store.load_state()? {
        state.running = false;
        state.pid = None;
        store.save_state(&state)?;
    }

    let _ = append_log_line(
        &store.log_file_path(),
        &format!("Background daemon stopped (PID={})", pid),
    );

    Ok(Some(pid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn should_manage_pid_and_log_files_correctly() {
        let dir = env::temp_dir().join(format!(
            "trade_bot_test_daemon_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(1)
        ));
        let store = JsonStateStore::new(&dir);
        store.ensure_dir().unwrap();

        let pid_path = store.pid_file_path();
        assert_eq!(read_pid_file(&pid_path).unwrap(), None);

        let current_pid = std::process::id();
        write_pid_file(&pid_path, current_pid).unwrap();
        assert_eq!(read_pid_file(&pid_path).unwrap(), Some(current_pid));
        assert!(is_pid_alive(current_pid));
        assert!(!is_pid_alive(0));

        remove_pid_file(&pid_path);
        assert_eq!(read_pid_file(&pid_path).unwrap(), None);

        let log_path = store.log_file_path();
        assert!(read_recent_logs(&log_path, 5).unwrap().is_empty());
        for i in 1..=5 {
            append_log_line(&log_path, &format!("line-{}", i)).unwrap();
        }
        let recent = read_recent_logs(&log_path, 3).unwrap();
        assert_eq!(recent.len(), 3);
        assert!(recent[0].contains("line-3"));
        assert!(recent[2].contains("line-5"));

        // Stopping when no PID file exists returns Ok(None)
        let cfg = BotConfig::default();
        let mut st = BotState::new(&cfg);
        st.running = true;
        store.save_state(&st).unwrap();
        assert_eq!(stop_background_daemon(&store).unwrap(), None);
        assert!(!store.load_state().unwrap().unwrap().running);

        // Stopping with a stale/non-existent PID cleans up PID file and updates state
        write_pid_file(&pid_path, 999_999).unwrap();
        assert_eq!(stop_background_daemon(&store).unwrap(), Some(999_999));
        assert_eq!(read_pid_file(&pid_path).unwrap(), None);

        let _ = fs::remove_dir_all(&dir);
    }
}

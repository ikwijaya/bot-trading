use std::fs;
use std::path::{Path, PathBuf};

use crate::core::domain::{BotConfig, BotState, DomainError};
use crate::core::ports::{ClockPort, StateStorePort};

/// Wall-clock implementation of [`ClockPort`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl ClockPort for SystemClock {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// Loads [`BotConfig`] from a JSON config file, or returns [`BotConfig::default`] if the path does not exist.
pub fn load_config_file(path: &Path) -> Result<BotConfig, DomainError> {
    if !path.exists() {
        return Ok(BotConfig::default());
    }
    let raw = fs::read_to_string(path).map_err(|e| DomainError::StateError {
        message: format!("failed to read config '{}': {}", path.display(), e),
    })?;
    serde_json::from_str::<BotConfig>(&raw).map_err(|e| DomainError::StateError {
        message: format!("invalid config JSON in '{}': {}", path.display(), e),
    })
}

/// Writes [`BotConfig`] as formatted JSON to the given file path.
pub fn save_config_file(path: &Path, cfg: &BotConfig) -> Result<(), DomainError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| DomainError::StateError {
                message: format!("failed to create parent directory: {}", e),
            })?;
        }
    }
    let content = serde_json::to_string_pretty(cfg).map_err(|e| DomainError::StateError {
        message: format!("failed to serialize config to JSON: {}", e),
    })?;
    fs::write(path, content).map_err(|e| DomainError::StateError {
        message: format!("failed to write config '{}': {}", path.display(), e),
    })
}

/// Local filesystem JSON implementation of [`StateStorePort`].
#[derive(Debug, Clone)]
pub struct JsonStateStore {
    data_dir: PathBuf,
}

impl JsonStateStore {
    /// Creates a new [`JsonStateStore`] rooted at `data_dir` (e.g. `.trade-bot`).
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// Returns path to `bot_state.json`.
    pub fn state_file_path(&self) -> PathBuf {
        self.data_dir.join("bot_state.json")
    }

    /// Returns path to `bot.pid`.
    pub fn pid_file_path(&self) -> PathBuf {
        self.data_dir.join("bot.pid")
    }

    /// Returns path to `bot.log`.
    pub fn log_file_path(&self) -> PathBuf {
        self.data_dir.join("bot.log")
    }

    /// Ensures the data directory exists on disk.
    pub fn ensure_dir(&self) -> Result<(), DomainError> {
        fs::create_dir_all(&self.data_dir).map_err(|e| DomainError::StateError {
            message: format!(
                "failed to create data directory '{}': {}",
                self.data_dir.display(),
                e
            ),
        })
    }
}

impl StateStorePort for JsonStateStore {
    fn load_state(&self) -> Result<Option<BotState>, DomainError> {
        let path = self.state_file_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path).map_err(|e| DomainError::StateError {
            message: format!("failed to read state file '{}': {}", path.display(), e),
        })?;
        let state =
            serde_json::from_str::<BotState>(&content).map_err(|e| DomainError::StateError {
                message: format!("corrupted state file '{}': {}", path.display(), e),
            })?;
        Ok(Some(state))
    }

    fn save_state(&self, state: &BotState) -> Result<(), DomainError> {
        self.ensure_dir()?;
        let path = self.state_file_path();
        let tmp_path = self.data_dir.join("bot_state.json.tmp");
        let content =
            serde_json::to_string_pretty(state).map_err(|e| DomainError::StateError {
                message: format!("failed to serialize state JSON: {}", e),
            })?;
        fs::write(&tmp_path, content).map_err(|e| DomainError::StateError {
            message: format!("failed to write temporary state file: {}", e),
        })?;
        fs::rename(&tmp_path, &path).map_err(|e| DomainError::StateError {
            message: format!("failed to atomically replace state file: {}", e),
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn should_save_and_load_bot_state_and_config_roundtrip() {
        let unique_dir = env::temp_dir().join(format!(
            "trade_bot_test_store_{}",
            SystemClock.now_ms()
        ));
        let store = JsonStateStore::new(&unique_dir);
        assert!(store.load_state().unwrap().is_none());

        let cfg = BotConfig::default();
        let cfg_path = unique_dir.join("config.toml");
        save_config_file(&cfg_path, &cfg).unwrap();
        let loaded_cfg = load_config_file(&cfg_path).unwrap();
        assert_eq!(loaded_cfg, cfg);

        let mut state = BotState::new(&cfg);
        state.running = true;
        state.total_trades = 3;
        store.save_state(&state).unwrap();

        let loaded_state = store.load_state().unwrap().unwrap();
        assert_eq!(loaded_state.total_trades, 3);
        assert!(loaded_state.running);

        let _ = fs::remove_dir_all(&unique_dir);
    }
}

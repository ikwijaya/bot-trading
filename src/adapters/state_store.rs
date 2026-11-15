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

/// Parses `.env` formatted text into a list of `(key, value)` pairs without performing any I/O.
///
/// Supports:
/// - Blank lines and `#` comments
/// - Optional `export ` prefix (e.g. `export KEY=VALUE`)
/// - Single-quoted (`'...'`) or double-quoted (`"..."`) values
/// - Inline values containing `=`
pub fn parse_dotenv_content(content: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let line = trimmed.strip_prefix("export ").map(str::trim).unwrap_or(trimmed);
        let Some((raw_key, raw_val)) = line.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        if key.is_empty() {
            continue;
        }
        let mut val = raw_val.trim();
        if (val.starts_with('"') && val.ends_with('"') && val.len() >= 2)
            || (val.starts_with('\'') && val.ends_with('\'') && val.len() >= 2)
        {
            val = &val[1..val.len() - 1];
        } else if let Some((before_comment, _)) = val.split_once(" #") {
            val = before_comment.trim();
        }
        pairs.push((key.to_string(), val.to_string()));
    }

    pairs
}

/// Loads environment variables from a `.env` file if it exists on disk.
/// Existing environment variables in the process are preserved (not overwritten).
///
/// Returns the number of newly set environment variables.
pub fn load_dotenv_file(path: &Path) -> Result<usize, DomainError> {
    if !path.exists() {
        return Ok(0);
    }
    let content = fs::read_to_string(path).map_err(|e| DomainError::StateError {
        message: format!("failed to read env file '{}': {}", path.display(), e),
    })?;
    let mut loaded_count = 0usize;
    for (key, value) in parse_dotenv_content(&content) {
        if std::env::var_os(&key).is_none() {
            std::env::set_var(&key, &value);
            loaded_count += 1;
        }
    }
    Ok(loaded_count)
}

/// Returns `true` if the given file path has a SQLite extension (`.db`, `.sqlite`, `.sqlite3`).
pub fn is_sqlite_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "db" | "sqlite" | "sqlite3"))
        .unwrap_or(false)
}

/// Saves [`BotConfig`] into a SQLite database table (`bot_configs`) under the given `profile_id`.
pub fn save_config_sqlite(
    path: &Path,
    profile_id: &str,
    cfg: &BotConfig,
) -> Result<(), DomainError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| DomainError::StateError {
                message: format!("failed to create SQLite parent directory: {}", e),
            })?;
        }
    }
    let conn = rusqlite::Connection::open(path).map_err(|e| DomainError::StateError {
        message: format!("failed to open SQLite database '{}': {}", path.display(), e),
    })?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS bot_configs (
            profile_id TEXT PRIMARY KEY,
            config_json TEXT NOT NULL,
            updated_at_ms INTEGER NOT NULL
        )",
        [],
    )
    .map_err(|e| DomainError::StateError {
        message: format!("failed to initialize bot_configs table: {}", e),
    })?;

    let json_str = serde_json::to_string_pretty(cfg).map_err(|e| DomainError::StateError {
        message: format!("failed to serialize config for SQLite: {}", e),
    })?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO bot_configs (profile_id, config_json, updated_at_ms)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(profile_id) DO UPDATE SET
            config_json = excluded.config_json,
            updated_at_ms = excluded.updated_at_ms",
        rusqlite::params![profile_id, json_str, now_ms],
    )
    .map_err(|e| DomainError::StateError {
        message: format!("failed to upsert config in SQLite: {}", e),
    })?;

    Ok(())
}

/// Loads [`BotConfig`] from a SQLite database table (`bot_configs`) for the given `profile_id`.
pub fn load_config_sqlite(path: &Path, profile_id: &str) -> Result<BotConfig, DomainError> {
    if !path.exists() {
        return Ok(BotConfig::default());
    }
    let conn = rusqlite::Connection::open(path).map_err(|e| DomainError::StateError {
        message: format!("failed to open SQLite database '{}': {}", path.display(), e),
    })?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS bot_configs (
            profile_id TEXT PRIMARY KEY,
            config_json TEXT NOT NULL,
            updated_at_ms INTEGER NOT NULL
        )",
        [],
    )
    .map_err(|e| DomainError::StateError {
        message: format!("failed to ensure bot_configs table: {}", e),
    })?;

    let mut stmt = conn
        .prepare("SELECT config_json FROM bot_configs WHERE profile_id = ?1 LIMIT 1")
        .map_err(|e| DomainError::StateError {
            message: format!("failed to prepare SQLite config query: {}", e),
        })?;

    let mut rows = stmt
        .query(rusqlite::params![profile_id])
        .map_err(|e| DomainError::StateError {
            message: format!("failed to query SQLite config: {}", e),
        })?;

    if let Some(row) = rows.next().map_err(|e| DomainError::StateError {
        message: format!("failed to read SQLite config row: {}", e),
    })? {
        let raw_json: String = row.get(0).map_err(|e| DomainError::StateError {
            message: format!("invalid config_json column in SQLite: {}", e),
        })?;
        serde_json::from_str::<BotConfig>(&raw_json).map_err(|e| DomainError::StateError {
            message: format!("invalid config JSON stored in SQLite '{}': {}", path.display(), e),
        })
    } else {
        Ok(BotConfig::default())
    }
}

/// Loads [`BotConfig`] from a JSON config file or SQLite database (`.db`/`.sqlite`), or returns [`BotConfig::default`] if the path does not exist.
pub fn load_config_file(path: &Path) -> Result<BotConfig, DomainError> {
    if is_sqlite_path(path) {
        return load_config_sqlite(path, "default");
    }
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

/// Writes [`BotConfig`] to a JSON file or SQLite database (`.db`/`.sqlite`) based on file extension.
pub fn save_config_file(path: &Path, cfg: &BotConfig) -> Result<(), DomainError> {
    if is_sqlite_path(path) {
        return save_config_sqlite(path, "default", cfg);
    }
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

/// Local filesystem JSON & Markdown implementation of [`StateStorePort`] with multi-worker isolation support.
#[derive(Debug, Clone)]
pub struct JsonStateStore {
    data_dir: PathBuf,
    worker_id: String,
}

impl JsonStateStore {
    /// Creates a new [`JsonStateStore`] rooted at `data_dir` (default worker `"default"`).
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self::with_worker(data_dir, "default")
    }

    /// Creates a worker-isolated [`JsonStateStore`].
    /// When `worker_id` is `"default"` or empty, files live directly in `data_dir`.
    /// When `worker_id` is custom (e.g. `"eth-worker"`), files are isolated under `<data_dir>/workers/<worker_id>/`.
    pub fn with_worker(data_dir: impl Into<PathBuf>, worker_id: &str) -> Self {
        let base: PathBuf = data_dir.into();
        let trimmed = worker_id.trim();
        let normalized_id = if trimmed.is_empty() {
            "default".to_string()
        } else {
            trimmed.to_string()
        };
        let resolved_dir = if normalized_id == "default" {
            base
        } else {
            base.join("workers").join(&normalized_id)
        };
        Self {
            data_dir: resolved_dir,
            worker_id: normalized_id,
        }
    }

    /// Returns the active worker identifier (`"default"` for primary worker).
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// Returns `true` if this is the primary worker (`"default"`), which is allowed to poll Telegram `getUpdates`.
    pub fn is_primary_worker(&self) -> bool {
        self.worker_id == "default"
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

    /// Returns path to `skills.md` (auto-generated AI continuous learning skills file).
    pub fn skills_file_path(&self) -> PathBuf {
        self.data_dir.join("skills.md")
    }

    /// Saves the auto-generated `skills.md` markdown file.
    pub fn save_skills_markdown(&self, markdown: &str) -> Result<(), DomainError> {
        self.ensure_dir()?;
        let path = self.skills_file_path();
        fs::write(&path, markdown).map_err(|e| DomainError::StateError {
            message: format!("failed to write skills file '{}': {}", path.display(), e),
        })
    }

    /// Reads the current `skills.md` markdown file, or returns a default message if not generated yet.
    pub fn load_skills_markdown(&self) -> Result<String, DomainError> {
        let path = self.skills_file_path();
        if !path.exists() {
            return Ok(
                "# Learned Trading Skills (skills.md)\n\nBelum ada data pembelajaran siklus yang terekam. Jalankan `trade-bot start` atau `trade-bot tick-once` untuk memulai pembelajaran otomatis.\n"
                    .to_string(),
            );
        }
        fs::read_to_string(&path).map_err(|e| DomainError::StateError {
            message: format!("failed to read skills file '{}': {}", path.display(), e),
        })
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

    #[test]
    fn should_parse_dotenv_content_with_comments_quotes_and_export_prefix() {
        let raw = r#"
            # Sample comment
            BINANCE_API_KEY=key_123
            export BINANCE_API_SECRET="secret=with=equals"
            AI_API_KEY='single_quoted_key'
            RUST_LOG=debug # inline comment
            INVALID_LINE_WITHOUT_EQUALS
            =empty_key
        "#;

        let parsed = parse_dotenv_content(raw);
        assert_eq!(
            parsed,
            vec![
                ("BINANCE_API_KEY".to_string(), "key_123".to_string()),
                (
                    "BINANCE_API_SECRET".to_string(),
                    "secret=with=equals".to_string()
                ),
                ("AI_API_KEY".to_string(), "single_quoted_key".to_string()),
                ("RUST_LOG".to_string(), "debug".to_string()),
            ]
        );
    }

    #[test]
    fn should_load_dotenv_file_when_present_or_ignore_missing() {
        let unique_dir = env::temp_dir().join(format!(
            "trade_bot_test_dotenv_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(1)
        ));
        fs::create_dir_all(&unique_dir).unwrap();
        let env_path = unique_dir.join(".env");

        // Missing file returns Ok(0)
        assert_eq!(load_dotenv_file(&env_path).unwrap(), 0);

        fs::write(
            &env_path,
            "TRADE_BOT_TEST_VAR_A=val_a\nTRADE_BOT_TEST_VAR_B=\"val_b\"\n",
        )
        .unwrap();

        let count = load_dotenv_file(&env_path).unwrap();
        assert_eq!(count, 2);
        assert_eq!(env::var("TRADE_BOT_TEST_VAR_A").unwrap(), "val_a");
        assert_eq!(env::var("TRADE_BOT_TEST_VAR_B").unwrap(), "val_b");

        // Second call does not overwrite existing env vars -> returns 0 newly loaded
        assert_eq!(load_dotenv_file(&env_path).unwrap(), 0);

        env::remove_var("TRADE_BOT_TEST_VAR_A");
        env::remove_var("TRADE_BOT_TEST_VAR_B");
        let _ = fs::remove_dir_all(&unique_dir);
    }

    #[test]
    fn should_save_and_load_config_in_sqlite_database_and_isolate_workers() {
        let unique_dir = env::temp_dir().join(format!(
            "trade_bot_test_sqlite_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(1)
        ));
        let db_path = unique_dir.join("config.db");
        assert!(is_sqlite_path(&db_path));

        let mut cfg = BotConfig::default();
        cfg.symbol = "ETHUSDT".to_string();
        cfg.risk.leverage = 5;

        // Save and load via automatic SQLite detection in save_config_file / load_config_file
        save_config_file(&db_path, &cfg).unwrap();
        let loaded = load_config_file(&db_path).unwrap();
        assert_eq!(loaded.symbol, "ETHUSDT");
        assert_eq!(loaded.risk.leverage, 5);

        // Worker isolation & skills.md persistence
        let primary_store = JsonStateStore::with_worker(&unique_dir, "default");
        assert!(primary_store.is_primary_worker());
        let secondary_store = JsonStateStore::with_worker(&unique_dir, "eth-worker");
        assert!(!secondary_store.is_primary_worker());
        assert_eq!(secondary_store.worker_id(), "eth-worker");
        assert!(secondary_store
            .state_file_path()
            .to_string_lossy()
            .contains("workers/eth-worker"));

        secondary_store
            .save_skills_markdown("# Skills Test\n- Rule 1")
            .unwrap();
        let skills_txt = secondary_store.load_skills_markdown().unwrap();
        assert!(skills_txt.contains("# Skills Test"));

        let _ = fs::remove_dir_all(&unique_dir);
    }
}

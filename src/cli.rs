use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::adapters::ai_advisor::HttpAiAdvisor;
use crate::adapters::binance::{resolve_binance_base_url, BinanceFuturesClient};
use crate::adapters::daemon::{
    append_log_line, is_pid_alive, read_pid_file, read_recent_logs, spawn_background_daemon,
    stop_background_daemon,
};
use crate::adapters::paper::PaperExchangeAdapter;
use crate::adapters::state_store::{
    load_config_file, load_dotenv_file, save_config_file, JsonStateStore, SystemClock,
};
use crate::adapters::telegram::{
    format_telegram_cycle_notification, format_telegram_help, is_authorized_chat,
    parse_telegram_command, TelegramBotAdapter, TelegramCommand,
};
use crate::core::agent::{format_skills_markdown, AgentEngine};
use crate::core::domain::{
    BotConfig, BotState, CycleReport, DecisionPolicy, DomainError, ExchangeProvider, ExecutionMode,
    OrderFill, SignalAction, SizingMode,
};
use crate::core::ports::{ExchangePort, NotifierPort, StateStorePort};
use crate::core::risk::validate_config_requirements;

/// Multi-Exchange Futures Hybrid Trading Bot Agent CLI (Binance, Bybit, OKX).
#[derive(Debug, Parser)]
#[command(
    name = "trade-bot",
    version,
    about = "Multi-Exchange Futures Hybrid Trading Bot Agent (CLI & Background Daemon)"
)]
pub struct Cli {
    /// Path to configuration file (.json or SQLite .db / .sqlite / .sqlite3).
    #[arg(short, long, default_value = "config.json", global = true)]
    pub config: PathBuf,

    /// Worker instance identifier for safe multi-worker isolation (default: "default").
    #[arg(short = 'w', long, default_value = "default", global = true)]
    pub worker_id: String,

    #[command(subcommand)]
    pub command: Commands,
}

/// Supported CLI subcommands.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Commands {
    /// Generate a default config.toml file.
    InitConfig {
        /// Overwrite existing config file if present.
        #[arg(short, long, default_value_t = false)]
        force: bool,
    },

    /// Start the trading bot agent (defaults to background daemon mode).
    Start {
        /// Run in background daemon mode (default: true unless --foreground is set).
        #[arg(short, long, default_value_t = true)]
        daemon: bool,

        /// Run in foreground terminal instead of spawning background daemon.
        #[arg(long, default_value_t = false)]
        foreground: bool,
    },

    /// Stop the running background trading bot daemon.
    Stop,

    /// Show current daemon process status, balance, and latest agent decision.
    Status,

    /// List open futures positions and recent order fills.
    Positions,

    /// Show recent lines from the background daemon log file.
    Logs {
        /// Number of recent log lines to display.
        #[arg(short = 'n', long, default_value_t = 30)]
        lines: usize,

        /// Follow log output in real time (Press Ctrl+C to exit).
        #[arg(short = 'f', long, default_value_t = false)]
        follow: bool,
    },

    /// Interactive live terminal dashboard showing spinner, heartbeat, positions, and logs.
    Monitor {
        /// Refresh interval in milliseconds.
        #[arg(short, long, default_value_t = 1000)]
        refresh_ms: u64,

        /// Optional maximum frames to render before exiting (useful for non-interactive/test runs).
        #[arg(long)]
        max_frames: Option<usize>,
    },

    /// Approve and execute the currently held pending order proposal (when decision_policy = 'confirm_by_chat').
    Approve,

    /// Reject and clear the currently held pending order proposal.
    Reject,

    /// Emergency action: stop daemon and immediately market-close all open positions.
    PanicClose,

    /// Execute a single evaluation cycle immediately and print the report.
    TickOnce,

    /// Display detailed reference and current values for all configuration keys.
    ConfigInfo,

    /// Display the continuously learned AI & market skills document (`skills.md` / `/actual-skill`).
    ActualSkill,

    /// Validate whether the active config file (.json or .db) and environment keys meet all requirements.
    CheckConfig,

    /// Interactive setup wizard to configure config path (.json or SQLite .db), exchange, mode, and limits.
    Wizard {
        /// Run non-interactively using default/preset values (useful for automation & tests).
        #[arg(long, default_value_t = false)]
        non_interactive: bool,
    },

    /// Send a test message to the configured Telegram Bot to verify connectivity.
    TelegramTest,

    /// Internal worker loop invoked by the background daemon spawner.
    #[command(hide = true)]
    RunWorker,
}

/// Formats a complete reference of every configuration key in `config.json` alongside its active value.
pub fn format_config_keys_reference(cfg: &BotConfig) -> String {
    format!(
        "=== Referensi Parameter Konfigurasi (config.json) ===\n\n\
         [1. Pengaturan Umum (Root)]\n\
         - mode                          : '{}' (Mode eksekusi: 'paper' simulasi lokal, 'testnet' Binance Testnet, 'live' Binance Mainnet)\n\
         - symbol                        : '{}' (Pasangan kontrak USDⓈ-M Futures, misal 'BTCUSDT', 'ETHUSDT')\n\
         - interval                      : '{}' (Timeframe candlestick klines, misal '1m', '5m', '15m', '1h')\n\
         - kline_limit                   : {} (Jumlah bar candlestick historis yang diambil setiap siklus untuk perhitungan indikator)\n\
         - poll_interval_secs            : {} (Jeda waktu dalam detik antar siklus evaluasi di background daemon)\n\
         - data_dir                      : '{}' (Direktori penyimpanan bot_state.json, bot.pid, dan bot.log)\n\n\
         [2. Pengaturan Strategi & Kebijakan Keputusan (strategy.*)]\n\
         - strategy.decision_policy      : '{}' (Kebijakan eksekusi: 'auto' otomatis beli, 'confirm_by_chat' tahan & minta konfirmasi via chat/CLI, 'skip' lewati eksekusi)\n\
         - strategy.fast_ema_period      : {} (Periode Exponential Moving Average cepat untuk deteksi arah tren jangka pendek)\n\
         - strategy.slow_ema_period      : {} (Periode Exponential Moving Average lambat sebagai baseline tren utama)\n\
         - strategy.rsi_period           : {} (Periode Relative Strength Index untuk mengukur kejenuhan momentum harga)\n\
         - strategy.rsi_overbought       : {:.1} (Batas atas RSI; sinyal LONG diblokir jika RSI >= nilai ini karena rawan koreksi)\n\
         - strategy.rsi_oversold         : {:.1} (Batas bawah RSI; sinyal SHORT diblokir jika RSI <= nilai ini karena rawan rebound)\n\
         - strategy.atr_period           : {} (Periode Average True Range untuk mengukur volatilitas rata-rata per candle)\n\
         - strategy.min_confidence       : {:.2} (Ambang batas skor keyakinan minimum 0.0..1.0 agar sinyal dieksekusi menjadi order)\n\n\
         [3. Manajemen Risiko, Batas Pembelian & Probabilitas Anti-Rugi (risk.*)]\n\
         - risk.sizing_mode              : '{}' (Mode ukuran order: 'probability_ev' probabilitas & Half-Kelly, 'risk_pct' % risiko, 'max_balance' sesuai saldo maks, 'fixed_min' nominal minimum)\n\
         - risk.min_order_usdt           : {:.2} (Batas minimum nilai pembelian notional per order dalam USDT)\n\
         - risk.max_order_usdt           : {:.2} (Batas maksimum nilai pembelian notional per order dalam USDT; 0.0 = otomatis ikut saldo & leverage)\n\
         - risk.require_positive_ev      : {} (Wajibkan Expected Value > 0 dan win probability > break-even win rate setelah fee agar tidak rugi secara matematis)\n\
         - risk.leverage                 : {}x (Target pengungkit/leverage yang digunakan saat membuka posisi Futures)\n\
         - risk.max_leverage             : {}x (Batas pengaman maksimum leverage yang diizinkan oleh Risk Engine)\n\
         - risk.risk_per_trade_pct       : {:.4} (Persentase saldo tersedia yang dipertaruhkan per transaksi, misal 0.01 = 1%)\n\
         - risk.atr_sl_multiplier        : {:.2} (Kelipatan ATR untuk jarak harga Stop-Loss otomatis dari harga entry)\n\
         - risk.atr_tp_multiplier        : {:.2} (Kelipatan ATR untuk jarak harga Take-Profit otomatis dari harga entry)\n\
         - risk.max_daily_drawdown_pct   : {:.4} (Circuit breaker penurunan ekuitas harian maksimum, misal 0.05 = 5%)\n\
         - risk.max_open_positions       : {} (Jumlah maksimum posisi terbuka secara bersamaan)\n\
         - risk.initial_paper_balance_usdt: {:.2} (Saldo awal virtual USDT khusus untuk mode 'paper')\n\n\
         [4. AI / LLM Market Advisor Opsional (ai.*)]\n\
         - ai.enabled                    : {} (Aktifkan/nonaktifkan modul konfirmasi sinyal menggunakan LLM)\n\
         - ai.endpoint_url               : '{}' (URL endpoint HTTP Chat Completions kompatibel OpenAI)\n\
         - ai.model                      : '{}' (Nama model LLM yang digunakan untuk analisis konteks indikator)\n\
         - ai.api_key_env                : '{}' (Nama environment variable di .env yang menyimpan API key LLM)\n\
         - ai.weight                     : {:.2} (Bobot kontribusi skor AI 0.0..1.0 saat digabungkan dengan sinyal teknikal)\n\n\
         [5. Koneksi Multi-Exchange Futures (exchange.*)]\n\
         - exchange.provider             : '{}' (Provider bursa Futures: 'binance', 'bybit', atau 'okx')\n\
         - exchange.api_key_env          : '{}' (Nama environment variable di .env untuk Exchange API Key)\n\
         - exchange.api_secret_env       : '{}' (Nama environment variable di .env untuk Exchange API Secret)\n\
         - exchange.testnet_base_url     : '{}' (Base URL REST API Futures Testnet)\n\
         - exchange.live_base_url        : '{}' (Base URL REST API Futures Mainnet)\n\
         - exchange.recv_window_ms       : {} (Toleransi waktu request bertanda tangan HMAC-SHA256 dalam milidetik)\n\n\
         [6. Integrasi Monitoring Bot Telegram (telegram.*)]\n\
         - telegram.enabled              : {} (Aktifkan/nonaktifkan notifikasi & kontrol via Bot Telegram)\n\
         - telegram.bot_token_env        : '{}' (Nama environment variable di .env untuk token Bot Telegram dari @BotFather)\n\
         - telegram.chat_id_env          : '{}' (Nama environment variable di .env untuk Chat ID pengguna yang diotorisasi)\n\
         - telegram.api_base_url         : '{}' (Base URL API Telegram Bot)\n\
         - telegram.notify_on_trade      : {} (Kirim notifikasi otomatis ke Telegram setiap ada eksekusi order / penutupan posisi)\n\
         - telegram.notify_on_cycle      : {} (Kirim laporan ringkas ke Telegram pada setiap siklus evaluasi)\n\
         - telegram.listen_commands      : {} (Aktifkan polling perintah interaktif /status, /positions, /actual-skill, /approve, /reject, /logs, /config, /panic dari Telegram)\n",
        cfg.mode,
        cfg.symbol,
        cfg.interval,
        cfg.kline_limit,
        cfg.poll_interval_secs,
        cfg.data_dir,
        cfg.strategy.decision_policy,
        cfg.strategy.fast_ema_period,
        cfg.strategy.slow_ema_period,
        cfg.strategy.rsi_period,
        cfg.strategy.rsi_overbought,
        cfg.strategy.rsi_oversold,
        cfg.strategy.atr_period,
        cfg.strategy.min_confidence,
        cfg.risk.sizing_mode,
        cfg.risk.min_order_usdt,
        cfg.risk.max_order_usdt,
        cfg.risk.require_positive_ev,
        cfg.risk.leverage,
        cfg.risk.max_leverage,
        cfg.risk.risk_per_trade_pct,
        cfg.risk.atr_sl_multiplier,
        cfg.risk.atr_tp_multiplier,
        cfg.risk.max_daily_drawdown_pct,
        cfg.risk.max_open_positions,
        cfg.risk.initial_paper_balance_usdt,
        cfg.ai.enabled,
        cfg.ai.endpoint_url,
        cfg.ai.model,
        cfg.ai.api_key_env,
        cfg.ai.weight,
        cfg.exchange.provider,
        cfg.exchange.api_key_env,
        cfg.exchange.api_secret_env,
        cfg.exchange.testnet_base_url,
        cfg.exchange.live_base_url,
        cfg.exchange.recv_window_ms,
        cfg.telegram.enabled,
        cfg.telegram.bot_token_env,
        cfg.telegram.chat_id_env,
        cfg.telegram.api_base_url,
        cfg.telegram.notify_on_trade,
        cfg.telegram.notify_on_cycle,
        cfg.telegram.listen_commands,
    )
}

/// Returns a deterministic Braille spinner character for a given animation step.
pub fn spinner_glyph(step: usize) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[step % FRAMES.len()]
}

/// Formats an elapsed time duration between `from_ms` and `now_ms` into a concise human-readable string.
pub fn format_elapsed_duration(from_ms: Option<i64>, now_ms: i64) -> String {
    let Some(start) = from_ms else {
        return "never".to_string();
    };
    let diff_secs = ((now_ms - start).max(0)) / 1_000;
    let hours = diff_secs / 3_600;
    let mins = (diff_secs % 3_600) / 60;
    let secs = diff_secs % 60;

    if hours > 0 {
        format!("{}h {}m {}s", hours, mins, secs)
    } else if mins > 0 {
        format!("{}m {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

/// Renders a single interactive terminal dashboard frame showing live process activity, spinner, state, and recent logs.
pub fn format_live_dashboard_frame(
    cfg: &BotConfig,
    state: &BotState,
    live_pid: Option<u32>,
    recent_logs: &[String],
    step: usize,
    now_ms: i64,
) -> String {
    let glyph = spinner_glyph(step);
    let (status_badge, activity_hint) = match live_pid {
        Some(pid) => (
            format!("{} [ACTIVE / RUNNING] (PID: {})", glyph, pid),
            format!(
                "Bot sedang memproses di background setiap {}s (Last tick: {} ago | Uptime: {})",
                cfg.poll_interval_secs,
                format_elapsed_duration(state.last_tick_ms, now_ms),
                format_elapsed_duration(state.started_at_ms, now_ms)
            ),
        ),
        None => (
            "■ [STOPPED]".to_string(),
            "Daemon belum berjalan. Jalankan 'trade-bot start --daemon' untuk memulai."
                .to_string(),
        ),
    };

    let decision_str = match &state.last_decision {
        Some(d) => format!(
            "{:?} (conf {:.2}) — {}",
            d.action, d.confidence, d.technical_reason
        ),
        None => "Waiting for first evaluation cycle...".to_string(),
    };

    let pending_str = match &state.pending_order {
        Some(p) => format!(
            "MENUNGGU KONFIRMASI: {:?} {} qty={:.3} (~{:.2} USDT) @ {:.2} | WinProb={:.1}% (EV={:+.2}R) -> Ketik /approve atau 'trade-bot approve'",
            p.request.position_side,
            p.request.symbol,
            p.request.quantity,
            p.notional_usdt,
            p.request.entry_price,
            p.probability.win_probability * 100.0,
            p.probability.expected_value_r
        ),
        None => "Tidak ada order tertahan".to_string(),
    };

    let mut out = format!(
        "========================================================================\n\
         {}  TRADE-BOT LIVE ACTIVITY MONITOR  (Tekan Ctrl+C untuk keluar monitor)\n\
         ========================================================================\n\
         Status Proses   : {}\n\
         Info Aktivitas  : {}\n\
         Mode & Market   : {} | {} ({}) | Leverage {}x | Policy: {} | Sizing: {}\n\
         Wallet Balance  : {:.2} USDT (Start: {:.2} USDT | Realized PnL: {:+.2} USDT)\n\
         Total Trades    : {} | Open Positions: {}\n\
         Sinyal Terakhir : {}\n\
         Pending Order   : {}\n\
         ------------------------------------------------------------------------\n\
         [Posisi Futures Aktif]\n",
        glyph,
        status_badge,
        activity_hint,
        cfg.mode,
        cfg.symbol,
        cfg.interval,
        cfg.risk.leverage,
        cfg.strategy.decision_policy,
        cfg.risk.sizing_mode,
        state.current_balance_usdt,
        state.day_start_balance_usdt,
        state.realized_pnl_usdt,
        state.total_trades,
        state.open_positions.len(),
        decision_str,
        pending_str
    );

    if state.open_positions.is_empty() {
        out.push_str("  (Tidak ada posisi terbuka saat ini)\n");
    } else {
        for pos in &state.open_positions {
            out.push_str(&format!(
                "  • {} {:?} qty={:.3} | Entry={:.2} | Mark={:.2} | uPnL={:+.2} USDT | SL={:?} | TP={:?}\n",
                pos.symbol,
                pos.side,
                pos.quantity,
                pos.entry_price,
                pos.mark_price,
                pos.unrealized_pnl,
                pos.stop_loss,
                pos.take_profit
            ));
        }
    }

    out.push_str("------------------------------------------------------------------------\n");
    out.push_str("[Live Stream Log Aktivitas Terakhir]\n");
    if recent_logs.is_empty() {
        out.push_str("  (Belum ada log aktivitas)\n");
    } else {
        for line in recent_logs {
            out.push_str(&format!("  {}\n", line));
        }
    }
    out.push_str("========================================================================\n");
    out
}

/// Translates a [`SignalAction`] into an easy-to-read Bahasa Indonesia badge.
pub fn format_action_badge(action: SignalAction) -> &'static str {
    match action {
        SignalAction::Long => "🟢 BELI / LONG (Pasar Diprediksi Naik)",
        SignalAction::Short => "🔴 JUAL / SHORT (Pasar Diprediksi Turun)",
        SignalAction::Hold => "⏸️ TAHAN / HOLD (Menunggu Momentum Valid)",
        SignalAction::Close => "⏹️ TUTUP POSISI / CLOSE",
    }
}

/// Validates config requirements and renders a user-friendly Bahasa Indonesia checklist report.
pub fn format_check_config_report(config_path: &std::path::Path, cfg: &BotConfig) -> (bool, String) {
    let has_ex = std::env::var(&cfg.exchange.api_key_env)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
        && std::env::var(&cfg.exchange.api_secret_env)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
    let has_ai = std::env::var(&cfg.ai.api_key_env)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_tg = std::env::var(&cfg.telegram.bot_token_env)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
        && std::env::var(&cfg.telegram.chat_id_env)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);

    let checks = validate_config_requirements(cfg, has_ex, has_ai, has_tg);
    let all_passed = checks.iter().all(|c| c.passed);

    let mut out = format!(
        "========================================================================\n\
         🔍 HASIL PENGECEKAN REQUIREMENT KONFIGURASI ({})\n\
         ========================================================================\n",
        config_path.display()
    );

    for item in &checks {
        let icon = if item.passed {
            "✅ [LOLOS]"
        } else {
            "❌ [GAGAL]"
        };
        out.push_str(&format!(" {} {:<24} : {}\n", icon, item.field, item.message));
    }

    out.push_str("------------------------------------------------------------------------\n");
    if all_passed {
        out.push_str(" 🎉 STATUS AKHIR: SEMUA REQUIREMENT TERPENUHI (SIAP DIJALANKAN)\n");
    } else {
        out.push_str(" ⚠️ STATUS AKHIR: ADA REQUIREMENT YANG BELUM TERPENUHI (PERIKSA ITEM ❌)\n");
    }
    out.push_str("========================================================================\n");

    (all_passed, out)
}

/// Formats a human-readable status summary from [`BotConfig`], [`BotState`], and live PID check.
pub fn format_status_report(cfg: &BotConfig, state: &BotState, live_pid: Option<u32>) -> String {
    let status_label = match live_pid {
        Some(pid) => format!("🟢 RUNNING (PID {}) — Bot sedang aktif bekerja di background", pid),
        None => "⚪ STOPPED — Bot sedang berhenti (jalankan 'trade-bot start --daemon')".to_string(),
    };
    let decision_summary = match &state.last_decision {
        Some(d) => format!(
            "{} | Keyakinan: {:.0}% ({:?}, confidence {:.2})\n   ↳ Alasan Teknikal: {}",
            format_action_badge(d.action),
            d.confidence * 100.0,
            d.action,
            d.confidence,
            d.technical_reason
        ),
        None => "Belum ada siklus evaluasi (jalankan 'trade-bot tick-once' atau 'start')".to_string(),
    };
    let pending_summary = match &state.pending_order {
        Some(p) => format!(
            "⏳ MENUNGGU KONFIRMASI ANDA: {:?} {} qty={:.3} (~${:.2} USDT) @ {:.2}\n   \
             ↳ Peluang Menang: {:.1}% | Expected Value: {:+.2}R\n   \
             ↳ Tindakan: Ketik 'trade-bot approve' (atau /approve di Telegram) untuk BELI, atau 'trade-bot reject' untuk BATAL",
            p.request.position_side,
            p.request.symbol,
            p.request.quantity,
            p.notional_usdt,
            p.request.entry_price,
            p.probability.win_probability * 100.0,
            p.probability.expected_value_r
        ),
        None => "Tidak ada order yang tertahan (None)".to_string(),
    };

    let max_order_label = if cfg.risk.max_order_usdt <= 0.0 {
        "Otomatis Sesuai Saldo".to_string()
    } else {
        format!("${:.2} USDT", cfg.risk.max_order_usdt)
    };

    format!(
        "========================================================================\n\
         🤖 RINGKASAN STATUS TRADING BOT (USER-FRIENDLY VIEW)\n\
         ========================================================================\n\
         [1. Status & Identitas Bot]\n\
         • Status Proses   : {status_label}\n\
         • Bursa & Mode    : {} | Mode: {}\n\
         • Pasangan Market : {} (Timeframe: {})\n\n\
         [2. Saldo & Performa Akun]\n\
         • Saldo Dompet    : {:.2} USDT (Saldo Awal Hari Ini: {:.2} USDT)\n\
         • Total Untung/Rugi: {:+.2} USDT (Realized PnL)\n\
         • Total Transaksi : {} kali eksekusi | Posisi Terbuka: {} posisi\n\n\
         [3. Aturan Keputusan & Batas Pembelian]\n\
         • Mode Keputusan  : {} | Mode Ukuran Order: {}\n\
         • Batas Pembelian : Min ${:.2} USDT | Max {} | Anti-Rugi (+EV): {}\n\
         • Leverage        : {}x (Batas Maksimum: {}x)\n\n\
         [4. Sinyal & Order Terakhir]\n\
         • Keputusan Bot   : {}\n\
         • Order Tertahan  : {}\n\n\
         [5. Memori Pembelajaran AI (Continuous Skills)]\n\
         • Siklus Dipelajari: {} siklus (Bullish: {}, Bearish: {}) — Lihat detail: 'trade-bot actual-skill' atau /actual-skill\n\
         ========================================================================\n",
        cfg.exchange.provider,
        cfg.mode,
        cfg.symbol,
        cfg.interval,
        state.current_balance_usdt,
        state.day_start_balance_usdt,
        state.realized_pnl_usdt,
        state.total_trades,
        state.open_positions.len(),
        cfg.strategy.decision_policy,
        cfg.risk.sizing_mode,
        cfg.risk.min_order_usdt,
        max_order_label,
        cfg.risk.require_positive_ev,
        cfg.risk.leverage,
        cfg.risk.max_leverage,
        decision_summary,
        pending_summary,
        state.skill_knowledge.total_cycles,
        state.skill_knowledge.bullish_cycles,
        state.skill_knowledge.bearish_cycles,
    )
}

/// Formats a human-readable report of open positions and recent fills.
pub fn format_positions_report(state: &BotState) -> String {
    let mut out = String::from("=== Active Futures Positions (Posisi Terbuka Saat Ini) ===\n");
    if state.open_positions.is_empty() {
        out.push_str("Tidak ada posisi terbuka saat ini (No open positions).\n");
    } else {
        for (idx, pos) in state.open_positions.iter().enumerate() {
            out.push_str(&format!(
                "{}. {} {:?} | Qty: {:.3} | Harga Beli (Entry): {:.2} | Harga Pasar (Mark): {:.2} | Untung/Rugi (uPnL): {:+.2} USDT | Lev: {}x | SL: {:?} | TP: {:?}\n",
                idx + 1,
                pos.symbol,
                pos.side,
                pos.quantity,
                pos.entry_price,
                pos.mark_price,
                pos.unrealized_pnl,
                pos.leverage,
                pos.stop_loss,
                pos.take_profit
            ));
        }
    }

    out.push_str("\n=== Recent Order Fills (5 Riwayat Transaksi Terakhir) ===\n");
    if state.recent_fills.is_empty() {
        out.push_str("Belum ada riwayat transaksi (No recent fills).\n");
    } else {
        let start = state.recent_fills.len().saturating_sub(5);
        for fill in &state.recent_fills[start..] {
            out.push_str(&format!(
                "- [{}] {} {:?} {:?} qty={:.3} @ {:.2} (Realized PnL={:+.2} USDT)\n",
                fill.order_id,
                fill.symbol,
                fill.position_side,
                fill.side,
                fill.executed_qty,
                fill.avg_price,
                fill.realized_pnl
            ));
        }
    }
    out
}

/// Formats a single [`CycleReport`] for console or log output in both structured log format and easy-to-read Bahasa Indonesia.
pub fn format_cycle_report(report: &CycleReport) -> String {
    let (order_info, friendly_order) = if let Some(fill) = &report.executed_order {
        (
            format!(
                "EXECUTED {} {:?} qty={:.3} @ {:.2} (SL={:?}, TP={:?})",
                fill.order_id,
                fill.position_side,
                fill.executed_qty,
                fill.avg_price,
                fill.stop_loss,
                fill.take_profit
            ),
            format!(
                "✅ Order Otomatis Dieksekusi: {:?} {} sebesar {:.3} kontrak di harga {:.2} (Stop-Loss: {:?}, Take-Profit: {:?})",
                fill.position_side, fill.symbol, fill.executed_qty, fill.avg_price, fill.stop_loss, fill.take_profit
            ),
        )
    } else if let Some(prop) = &report.pending_order {
        (
            format!(
                "HELD_FOR_CHAT_APPROVAL {:?} qty={:.3} (~{:.2} USDT) @ {:.2} (WinProb={:.1}%, EV={:+.2}R)",
                prop.request.position_side,
                prop.request.quantity,
                prop.notional_usdt,
                prop.request.entry_price,
                prop.probability.win_probability * 100.0,
                prop.probability.expected_value_r
            ),
            format!(
                "⏳ Order Ditahan Menunggu Persetujuan Anda: {:?} {:.3} (~${:.2} USDT) @ {:.2} | Peluang Menang {:.1}% (EV {:+.2}R) -> Jalankan 'trade-bot approve' atau /approve",
                prop.request.position_side,
                prop.request.quantity,
                prop.notional_usdt,
                prop.request.entry_price,
                prop.probability.win_probability * 100.0,
                prop.probability.expected_value_r
            ),
        )
    } else {
        (
            "NO_NEW_ORDER".to_string(),
            "ℹ️ Tidak ada pembukaan order baru pada siklus ini.".to_string(),
        )
    };
    format!(
        "Symbol={} | Decision={:?} (conf={:.2}) | Tech={} | Closed={} | Order={}\n\
         ↳ 📊 Ringkasan Mudah: [{}] Keyakinan {:.0}% | Harga {:.2} (RSI {:.1}) | {}",
        report.symbol,
        report.decision.action,
        report.decision.confidence,
        report.technical.reason,
        report.closed_positions,
        order_info,
        format_action_badge(report.decision.action),
        report.decision.confidence * 100.0,
        report.technical.last_price,
        report.technical.rsi,
        friendly_order
    )
}

/// Executes a single cycle using the configured exchange mode (`paper`, `testnet`, or `live`),
/// injecting learned `skills.md` into the AI advisor and auto-updating `skills.md` afterward.
pub async fn run_single_cycle(
    cfg: &BotConfig,
    store: Arc<JsonStateStore>,
) -> Result<CycleReport, DomainError> {
    let ai_key = std::env::var(&cfg.ai.api_key_env).ok();
    let existing_skills = store.load_skills_markdown().ok();
    let ai_advisor = HttpAiAdvisor::new(cfg.ai.clone(), ai_key).with_learned_skills(existing_skills);

    let report = match cfg.mode {
        ExecutionMode::Paper => {
            let exchange = PaperExchangeAdapter::new(cfg, Arc::clone(&store), SystemClock);
            let engine = AgentEngine::new(exchange, ai_advisor, (*store).clone(), SystemClock);
            engine.execute_cycle(cfg).await?
        }
        ExecutionMode::Testnet | ExecutionMode::Live => {
            let api_key = std::env::var(&cfg.exchange.api_key_env).unwrap_or_default();
            let api_secret = std::env::var(&cfg.exchange.api_secret_env).unwrap_or_default();
            let base_url = resolve_binance_base_url(cfg.mode, &cfg.exchange);
            let exchange = BinanceFuturesClient::new(
                base_url,
                api_key,
                api_secret,
                cfg.exchange.recv_window_ms,
                SystemClock,
            )
            .with_provider(cfg.exchange.provider);
            let engine = AgentEngine::new(exchange, ai_advisor, (*store).clone(), SystemClock);
            engine.execute_cycle(cfg).await?
        }
    };

    if let Ok(Some(state)) = store.load_state() {
        let md = format_skills_markdown(&cfg.symbol, &state.skill_knowledge, &state);
        let _ = store.save_skills_markdown(&md);
    }

    Ok(report)
}

/// Helper for reading a single line of user input with a fallback default during the interactive wizard.
fn prompt_wizard_line(prompt: &str, default_val: &str) -> String {
    use std::io::{self, Write};
    print!("{} [default: {}]: ", prompt, default_val);
    let _ = io::stdout().flush();
    let mut buf = String::new();
    if io::stdin().read_line(&mut buf).is_ok() {
        let trimmed = buf.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    default_val.to_string()
}

/// Runs the interactive or non-interactive configuration setup wizard, saves the config (.json or SQLite .db),
/// and verifies that all config requirements are met.
pub fn run_setup_wizard(
    default_config_path: &std::path::Path,
    non_interactive: bool,
) -> Result<String, DomainError> {
    let mut cfg = load_config_file(default_config_path).unwrap_or_default();
    let target_path = if non_interactive {
        default_config_path.to_path_buf()
    } else {
        println!("========================================================================");
        println!("🧙 TRADE-BOT INTERACTIVE SETUP WIZARD (JSON / SQLite DB)");
        println!("========================================================================");
        let path_str = prompt_wizard_line(
            "1. Lokasi penyimpanan file konfigurasi (.json atau SQLite .db)",
            &default_config_path.to_string_lossy(),
        );
        let provider_str = prompt_wizard_line(
            "2. Pilih Exchange Provider (binance / bybit / okx)",
            &cfg.exchange.provider.to_string(),
        );
        cfg.exchange.provider = match provider_str.to_lowercase().as_str() {
            "bybit" => ExchangeProvider::Bybit,
            "okx" => ExchangeProvider::Okx,
            _ => ExchangeProvider::Binance,
        };
        let mode_str = prompt_wizard_line(
            "3. Pilih Mode Eksekusi (paper / testnet / live)",
            &cfg.mode.to_string(),
        );
        cfg.mode = match mode_str.to_lowercase().as_str() {
            "testnet" => ExecutionMode::Testnet,
            "live" => ExecutionMode::Live,
            _ => ExecutionMode::Paper,
        };
        cfg.symbol = prompt_wizard_line("4. Simbol Kontrak Futures (contoh: BTCUSDT)", &cfg.symbol)
            .to_uppercase();
        let policy_str = prompt_wizard_line(
            "5. Kebijakan Pengambilan Keputusan (auto / confirm_by_chat / skip)",
            &cfg.strategy.decision_policy.to_string(),
        );
        cfg.strategy.decision_policy = match policy_str.to_lowercase().as_str() {
            "confirm_by_chat" | "confirm" | "hold" => DecisionPolicy::ConfirmByChat,
            "skip" => DecisionPolicy::Skip,
            _ => DecisionPolicy::Auto,
        };
        let sizing_str = prompt_wizard_line(
            "6. Mode Ukuran Pembelian (probability_ev / risk_pct / max_balance / fixed_min)",
            &cfg.risk.sizing_mode.to_string(),
        );
        cfg.risk.sizing_mode = match sizing_str.to_lowercase().as_str() {
            "risk_pct" => SizingMode::RiskPct,
            "max_balance" => SizingMode::MaxBalance,
            "fixed_min" => SizingMode::FixedMin,
            _ => SizingMode::ProbabilityEv,
        };
        let min_str = prompt_wizard_line(
            "7. Batas Pembelian Minimum dalam USDT (min_order_usdt)",
            &format!("{:.1}", cfg.risk.min_order_usdt),
        );
        if let Ok(v) = min_str.parse::<f64>() {
            cfg.risk.min_order_usdt = v;
        }
        let max_str = prompt_wizard_line(
            "8. Batas Pembelian Maksimum dalam USDT (0.0 = sesuai saldo)",
            &format!("{:.1}", cfg.risk.max_order_usdt),
        );
        if let Ok(v) = max_str.parse::<f64>() {
            cfg.risk.max_order_usdt = v;
        }
        PathBuf::from(path_str)
    };

    save_config_file(&target_path, &cfg)?;
    let (_, check_report) = format_check_config_report(&target_path, &cfg);

    Ok(format!(
        "✅ Wizard Setup Selesai! Konfigurasi berhasil disimpan ke '{}'\n\n{}",
        target_path.display(),
        check_report
    ))
}

/// Dispatches CLI commands and returns user-facing output text.
pub async fn execute_cli(cli: Cli) -> Result<String, DomainError> {
    let _ = load_dotenv_file(std::path::Path::new(".env"));
    match cli.command {
        Commands::InitConfig { force } => {
            if cli.config.exists() && !force {
                return Err(DomainError::InvalidConfig {
                    field: "config",
                    reason: format!(
                        "config file '{}' already exists (use --force to overwrite)",
                        cli.config.display()
                    ),
                });
            }
            let default_cfg = BotConfig::default();
            save_config_file(&cli.config, &default_cfg)?;
            Ok(format!(
                "Initialized default configuration at '{}'",
                cli.config.display()
            ))
        }

        Commands::Start { daemon, foreground } => {
            let cfg = load_config_file(&cli.config)?;
            if !cli.config.exists() {
                save_config_file(&cli.config, &cfg)?;
            }
            let store = JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id);
            if daemon && !foreground {
                let pid = spawn_background_daemon(&cli.config, &cfg, &store)?;
                Ok(format!(
                    "✓ Trading bot daemon started in background (Worker: {}, PID: {}, Exchange: {}, Mode: {}, Symbol: {}, Log: {})\n\
                     💡 Tip: Jalankan 'trade-bot monitor' atau 'trade-bot logs -f' untuk melihat aktivitas bot secara live!",
                    store.worker_id(),
                    pid,
                    cfg.exchange.provider,
                    cfg.mode,
                    cfg.symbol,
                    store.log_file_path().display()
                ))
            } else {
                run_worker_loop(&cfg, Arc::new(store), true).await?;
                Ok("Foreground worker stopped.".to_string())
            }
        }

        Commands::Stop => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id);
            match stop_background_daemon(&store)? {
                Some(pid) => Ok(format!("Stopped background trading bot daemon (PID: {}).", pid)),
                None => Ok("Trading bot daemon is not currently running.".to_string()),
            }
        }

        Commands::Status => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id);
            let state = store.load_state()?.unwrap_or_else(|| BotState::new(&cfg));
            let live_pid = read_pid_file(&store.pid_file_path())?
                .filter(|&pid| is_pid_alive(pid));
            Ok(format_status_report(&cfg, &state, live_pid))
        }

        Commands::Positions => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id);
            let state = store.load_state()?.unwrap_or_else(|| BotState::new(&cfg));
            Ok(format_positions_report(&state))
        }

        Commands::Logs { lines, follow } => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id);
            let entries = read_recent_logs(&store.log_file_path(), lines)?;
            if !follow {
                if entries.is_empty() {
                    Ok("No daemon logs found yet.".to_string())
                } else {
                    Ok(entries.join("\n"))
                }
            } else {
                println!(
                    "=== Mengikuti Live Log ({}) — Tekan Ctrl+C untuk keluar ===",
                    store.log_file_path().display()
                );
                for line in &entries {
                    println!("{}", line);
                }
                let mut prev_last = entries.last().cloned();
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(800)) => {
                            let latest = read_recent_logs(&store.log_file_path(), 20)?;
                            if let Some(last) = latest.last() {
                                if prev_last.as_ref() != Some(last) {
                                    println!("{}", last);
                                    prev_last = Some(last.clone());
                                }
                            }
                        }
                        _ = tokio::signal::ctrl_c() => {
                            break;
                        }
                    }
                }
                Ok("Exited live log follow.".to_string())
            }
        }

        Commands::Monitor {
            refresh_ms,
            max_frames,
        } => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id);
            let interval = refresh_ms.max(100);
            let mut step = 0usize;

            let final_frame = loop {
                let state = store.load_state()?.unwrap_or_else(|| BotState::new(&cfg));
                let live_pid = read_pid_file(&store.pid_file_path())?
                    .filter(|&pid| is_pid_alive(pid));
                let recent_logs = read_recent_logs(&store.log_file_path(), 6)?;
                let now_ms = chrono::Utc::now().timestamp_millis();

                let frame = format_live_dashboard_frame(
                    &cfg,
                    &state,
                    live_pid,
                    &recent_logs,
                    step,
                    now_ms,
                );

                // Clear terminal screen and position cursor at top-left when running interactively
                if max_frames.is_none() {
                    print!("\x1B[2J\x1B[H{}", frame);
                }
                step += 1;

                if let Some(limit) = max_frames {
                    if step >= limit {
                        break frame;
                    }
                }

                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(interval)) => {}
                    _ = tokio::signal::ctrl_c() => {
                        break frame;
                    }
                }
            };
            Ok(final_frame)
        }

        Commands::Approve => {
            let cfg = load_config_file(&cli.config)?;
            let store = Arc::new(JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id));
            let fill = approve_pending_order(&cfg, Arc::clone(&store)).await?;
            let msg = format!(
                "✓ Approved & executed pending order: {} {:?} {} qty={:.3} @ {:.2} (SL={:?}, TP={:?})",
                fill.order_id,
                fill.position_side,
                fill.symbol,
                fill.executed_qty,
                fill.avg_price,
                fill.stop_loss,
                fill.take_profit
            );
            let _ = append_log_line(&store.log_file_path(), &msg);
            let tg = TelegramBotAdapter::from_env(&cfg.telegram);
            if tg.is_ready() {
                let _ = tg.send_notification(&format!("[Trade-Bot] {}", msg)).await;
            }
            Ok(msg)
        }

        Commands::Reject => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id);
            let had_pending = reject_pending_order(&store)?;
            let msg = if had_pending {
                "✓ Pending order proposal rejected and cleared.".to_string()
            } else {
                "No pending order proposal to reject.".to_string()
            };
            let _ = append_log_line(&store.log_file_path(), &msg);
            Ok(msg)
        }

        Commands::PanicClose => {
            let cfg = load_config_file(&cli.config)?;
            let store = Arc::new(JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id));
            let stopped_pid = stop_background_daemon(&store)?;
            let closed_count = close_all_exchange_positions(&cfg, Arc::clone(&store)).await?;

            let msg = format!(
                "Panic close complete: stopped daemon PID {:?}, closed {} open position(s).",
                stopped_pid, closed_count
            );
            let _ = append_log_line(&store.log_file_path(), &msg);
            let tg = TelegramBotAdapter::from_env(&cfg.telegram);
            if tg.is_ready() {
                let _ = tg.send_notification(&format!("[Trade-Bot] {}", msg)).await;
            }
            Ok(msg)
        }

        Commands::TickOnce => {
            let cfg = load_config_file(&cli.config)?;
            let store = Arc::new(JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id));
            let report = run_single_cycle(&cfg, Arc::clone(&store)).await?;
            let line = format_cycle_report(&report);
            let _ = append_log_line(&store.log_file_path(), &line);
            Ok(line)
        }

        Commands::ConfigInfo => {
            let cfg = load_config_file(&cli.config)?;
            Ok(format_config_keys_reference(&cfg))
        }

        Commands::ActualSkill => {
            let cfg = load_config_file(&cli.config)?;
            let store = JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id);
            if let Ok(md) = store.load_skills_markdown() {
                Ok(md)
            } else {
                let state = store.load_state()?.unwrap_or_else(|| BotState::new(&cfg));
                let md = format_skills_markdown(&cfg.symbol, &state.skill_knowledge, &state);
                let _ = store.save_skills_markdown(&md);
                Ok(md)
            }
        }

        Commands::CheckConfig => {
            let cfg = load_config_file(&cli.config)?;
            let (_, report) = format_check_config_report(&cli.config, &cfg);
            Ok(report)
        }

        Commands::Wizard { non_interactive } => run_setup_wizard(&cli.config, non_interactive),

        Commands::TelegramTest => {
            let mut cfg = load_config_file(&cli.config)?;
            cfg.telegram.enabled = true;
            let tg = TelegramBotAdapter::from_env(&cfg.telegram);
            tg.send_notification(&format!(
                "[Trade-Bot Test] Koneksi Bot Telegram berhasil! (Mode: {}, Symbol: {})",
                cfg.mode, cfg.symbol
            ))
            .await?;
            Ok("Telegram test message sent successfully.".to_string())
        }

        Commands::RunWorker => {
            let cfg = load_config_file(&cli.config)?;
            let store = Arc::new(JsonStateStore::with_worker(&cfg.data_dir, &cli.worker_id));
            run_worker_loop(&cfg, store, false).await?;
            Ok("Worker finished.".to_string())
        }
    }
}

async fn close_all_exchange_positions(
    cfg: &BotConfig,
    store: Arc<JsonStateStore>,
) -> Result<usize, DomainError> {
    let closed = match cfg.mode {
        ExecutionMode::Paper => {
            let exchange = PaperExchangeAdapter::new(cfg, Arc::clone(&store), SystemClock);
            exchange.close_all_positions(&cfg.symbol).await?
        }
        ExecutionMode::Testnet | ExecutionMode::Live => {
            let api_key = std::env::var(&cfg.exchange.api_key_env).unwrap_or_default();
            let api_secret = std::env::var(&cfg.exchange.api_secret_env).unwrap_or_default();
            let base_url = resolve_binance_base_url(cfg.mode, &cfg.exchange);
            let exchange = BinanceFuturesClient::new(
                base_url,
                api_key,
                api_secret,
                cfg.exchange.recv_window_ms,
                SystemClock,
            )
            .with_provider(cfg.exchange.provider);
            exchange.close_all_positions(&cfg.symbol).await?
        }
    };
    Ok(closed.len())
}

/// Approves and executes the currently held [`PendingOrderProposal`] in [`BotState`].
pub async fn approve_pending_order(
    cfg: &BotConfig,
    store: Arc<JsonStateStore>,
) -> Result<OrderFill, DomainError> {
    let state = store.load_state()?.unwrap_or_else(|| BotState::new(cfg));
    let Some(proposal) = state.pending_order.clone() else {
        return Err(DomainError::StateError {
            message: "Tidak ada pending order yang menunggu konfirmasi saat ini.".to_string(),
        });
    };

    let (fill, account) = match cfg.mode {
        ExecutionMode::Paper => {
            let exchange = PaperExchangeAdapter::new(cfg, Arc::clone(&store), SystemClock);
            exchange
                .set_leverage(&proposal.request.symbol, proposal.request.leverage)
                .await?;
            let fill = exchange.place_order(&proposal.request).await?;
            let acct = exchange.fetch_account(&proposal.request.symbol).await?;
            (fill, acct)
        }
        ExecutionMode::Testnet | ExecutionMode::Live => {
            let api_key = std::env::var(&cfg.exchange.api_key_env).unwrap_or_default();
            let api_secret = std::env::var(&cfg.exchange.api_secret_env).unwrap_or_default();
            let base_url = resolve_binance_base_url(cfg.mode, &cfg.exchange);
            let exchange = BinanceFuturesClient::new(
                base_url,
                api_key,
                api_secret,
                cfg.exchange.recv_window_ms,
                SystemClock,
            )
            .with_provider(cfg.exchange.provider);
            exchange
                .set_leverage(&proposal.request.symbol, proposal.request.leverage)
                .await?;
            let fill = exchange.place_order(&proposal.request).await?;
            let acct = exchange.fetch_account(&proposal.request.symbol).await?;
            (fill, acct)
        }
    };

    let mut updated_state = store.load_state()?.unwrap_or_else(|| BotState::new(cfg));
    updated_state.pending_order = None;
    updated_state.total_trades += 1;
    updated_state.recent_fills.push(fill.clone());
    updated_state.current_balance_usdt = account.total_wallet_balance;
    updated_state.open_positions = account.positions;
    store.save_state(&updated_state)?;

    Ok(fill)
}

/// Rejects and clears any held [`PendingOrderProposal`] in [`BotState`].
pub fn reject_pending_order(store: &JsonStateStore) -> Result<bool, DomainError> {
    let Some(mut state) = store.load_state()? else {
        return Ok(false);
    };
    let had_pending = state.pending_order.is_some();
    state.pending_order = None;
    store.save_state(&state)?;
    Ok(had_pending)
}

async fn run_worker_loop(
    cfg: &BotConfig,
    store: Arc<JsonStateStore>,
    print_stdout: bool,
) -> Result<(), DomainError> {
    let poll_secs = cfg.poll_interval_secs.max(1);
    let tg = TelegramBotAdapter::from_env(&cfg.telegram);
    let mut tg_offset: i64 = 0;
    let mut cycle_idx: usize = 0;

    let _ = append_log_line(
        &store.log_file_path(),
        &format!(
            "Worker loop active (mode={}, symbol={}, poll={}s, telegram_ready={})",
            cfg.mode,
            cfg.symbol,
            poll_secs,
            tg.is_ready()
        ),
    );

    if print_stdout {
        println!(
            "=== Trade-Bot Foreground Worker Active (Mode: {}, Symbol: {}, Interval: {}, Poll: {}s) ===",
            cfg.mode, cfg.symbol, cfg.interval, poll_secs
        );
        println!("Tekan Ctrl+C kapan saja untuk menghentikan bot.\n");
    }

    if tg.is_ready() {
        let _ = tg
            .send_notification(&format!(
                "[Trade-Bot] Daemon aktif di background (Mode: {}, Symbol: {}, Interval: {})\nKetik /help untuk daftar perintah monitoring.",
                cfg.mode, cfg.symbol, cfg.interval
            ))
            .await;
    }

    loop {
        cycle_idx += 1;
        if print_stdout {
            println!(
                "{} [Siklus #{}] Mengevaluasi market {} ({})...",
                spinner_glyph(cycle_idx),
                cycle_idx,
                cfg.symbol,
                cfg.interval
            );
        }

        match run_single_cycle(cfg, Arc::clone(&store)).await {
            Ok(report) => {
                let summary = format_cycle_report(&report);
                let _ = append_log_line(&store.log_file_path(), &summary);
                if print_stdout {
                    println!(
                        "  ✓ Selesai: {}\n  ⏳ Menunggu {}s untuk siklus berikutnya...",
                        summary, poll_secs
                    );
                }

                if tg.is_ready() {
                    if let Ok(Some(state)) = store.load_state() {
                        if let Some(alert) = format_telegram_cycle_notification(
                            &report,
                            &state,
                            cfg.telegram.notify_on_trade,
                            cfg.telegram.notify_on_cycle,
                        ) {
                            let _ = tg.send_notification(&alert).await;
                        }
                    }
                }
            }
            Err(err) => {
                let _ = append_log_line(&store.log_file_path(), &format!("Cycle error: {}", err));
                if print_stdout {
                    println!("  ✗ Error siklus: {}", err);
                }
            }
        }

        // Only the primary ("default") worker polls Telegram getUpdates to avoid HTTP 409 conflicts across workers
        if tg.is_ready() && cfg.telegram.listen_commands && store.is_primary_worker() {
            if let Ok(updates) = tg.fetch_updates(tg_offset).await {
                for upd in updates {
                    tg_offset = upd.update_id + 1;
                    if !is_authorized_chat(&upd.chat_id, tg.authorized_chat_id()) {
                        continue;
                    }
                    if let Some(cmd) = parse_telegram_command(&upd.text) {
                        let should_exit =
                            handle_telegram_command(cmd, cfg, Arc::clone(&store), &tg).await;
                        if should_exit {
                            return Ok(());
                        }
                    }
                }
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(poll_secs)) => {}
            _ = tokio::signal::ctrl_c() => {
                let _ = append_log_line(&store.log_file_path(), "Received shutdown signal; exiting worker loop.");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_telegram_command(
    cmd: TelegramCommand,
    cfg: &BotConfig,
    store: Arc<JsonStateStore>,
    tg: &TelegramBotAdapter,
) -> bool {
    match cmd {
        TelegramCommand::Status => {
            let state = store
                .load_state()
                .ok()
                .flatten()
                .unwrap_or_else(|| BotState::new(cfg));
            let live_pid = read_pid_file(&store.pid_file_path())
                .ok()
                .flatten()
                .filter(|&pid| is_pid_alive(pid));
            let _ = tg
                .send_notification(&format_status_report(cfg, &state, live_pid))
                .await;
            false
        }
        TelegramCommand::Positions => {
            let state = store
                .load_state()
                .ok()
                .flatten()
                .unwrap_or_else(|| BotState::new(cfg));
            let _ = tg.send_notification(&format_positions_report(&state)).await;
            false
        }
        TelegramCommand::Logs => {
            let lines = read_recent_logs(&store.log_file_path(), 10).unwrap_or_default();
            let text = if lines.is_empty() {
                "No logs yet.".to_string()
            } else {
                lines.join("\n")
            };
            let _ = tg.send_notification(&text).await;
            false
        }
        TelegramCommand::Config => {
            let _ = tg
                .send_notification(&format!(
                    "=== Active Config ===\nExchange: {}\nMode: {}\nSymbol: {} ({})\nDecision Policy: {}\nSizing Mode: {} (Min: ${:.2}, Max: ${:.2}, +EV: {})\nLeverage: {}x\nRisk/Trade: {:.2}%\nSL/TP ATR: {:.1}x / {:.1}x\nMax Daily Drawdown: {:.1}%\nAI Enabled: {}",
                    cfg.exchange.provider,
                    cfg.mode,
                    cfg.symbol,
                    cfg.interval,
                    cfg.strategy.decision_policy,
                    cfg.risk.sizing_mode,
                    cfg.risk.min_order_usdt,
                    cfg.risk.max_order_usdt,
                    cfg.risk.require_positive_ev,
                    cfg.risk.leverage,
                    cfg.risk.risk_per_trade_pct * 100.0,
                    cfg.risk.atr_sl_multiplier,
                    cfg.risk.atr_tp_multiplier,
                    cfg.risk.max_daily_drawdown_pct * 100.0,
                    cfg.ai.enabled
                ))
                .await;
            false
        }
        TelegramCommand::ActualSkill => {
            let md = store.load_skills_markdown().unwrap_or_else(|_| {
                let state = store
                    .load_state()
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| BotState::new(cfg));
                format_skills_markdown(&cfg.symbol, &state.skill_knowledge, &state)
            });
            let _ = tg.send_notification(&md).await;
            false
        }
        TelegramCommand::Approve => {
            match approve_pending_order(cfg, Arc::clone(&store)).await {
                Ok(fill) => {
                    let msg = format!(
                        "✅ [APPROVED] Order berhasil dieksekusi: {} {:?} {} qty={:.3} @ {:.2} (SL={:?}, TP={:?})",
                        fill.order_id,
                        fill.position_side,
                        fill.symbol,
                        fill.executed_qty,
                        fill.avg_price,
                        fill.stop_loss,
                        fill.take_profit
                    );
                    let _ = append_log_line(&store.log_file_path(), &msg);
                    let _ = tg.send_notification(&msg).await;
                }
                Err(err) => {
                    let _ = tg
                        .send_notification(&format!("⚠️ Gagal eksekusi /approve: {}", err))
                        .await;
                }
            }
            false
        }
        TelegramCommand::Reject => {
            let had_pending = reject_pending_order(&store).unwrap_or(false);
            let msg = if had_pending {
                "🚫 [REJECTED] Proposal order yang tertahan telah dibatalkan."
            } else {
                "ℹ️ Tidak ada proposal order yang tertahan saat ini."
            };
            let _ = append_log_line(&store.log_file_path(), msg);
            let _ = tg.send_notification(msg).await;
            false
        }
        TelegramCommand::Panic => {
            let closed = close_all_exchange_positions(cfg, Arc::clone(&store))
                .await
                .unwrap_or(0);
            let _ = stop_background_daemon(&store);
            let _ = tg
                .send_notification(&format!(
                    "[PANIC CLOSE] Daemon dihentikan & {} posisi terbuka telah ditutup.",
                    closed
                ))
                .await;
            true
        }
        TelegramCommand::Help => {
            let _ = tg.send_notification(&format_telegram_help()).await;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::{
        AgentDecision, DecisionPolicy, OrderFill, OrderSide, Position, PositionSide, SignalAction,
        TechnicalSignal,
    };

    #[test]
    fn should_format_status_positions_and_cycle_reports() {
        let cfg = BotConfig::default();
        let mut state = BotState::new(&cfg);
        state.last_decision = Some(AgentDecision {
            action: SignalAction::Long,
            confidence: 0.78,
            technical_reason: "EMA9 > EMA21".to_string(),
            ai_reason: None,
        });
        state.open_positions.push(Position {
            symbol: "BTCUSDT".to_string(),
            side: PositionSide::Long,
            quantity: 0.02,
            entry_price: 65_000.0,
            mark_price: 65_500.0,
            unrealized_pnl: 10.0,
            leverage: 3,
            stop_loss: Some(64_200.0),
            take_profit: Some(66_600.0),
        });
        state.recent_fills.push(OrderFill {
            order_id: "paper-1".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            position_side: PositionSide::Long,
            executed_qty: 0.02,
            avg_price: 65_000.0,
            stop_loss: Some(64_200.0),
            take_profit: Some(66_600.0),
            realized_pnl: 0.0,
            timestamp_ms: 1_700_000_000_000,
        });

        let status_txt = format_status_report(&cfg, &state, Some(4321));
        assert!(status_txt.contains("RUNNING (PID 4321)"));
        assert!(status_txt.contains("BTCUSDT"));
        assert!(format_action_badge(SignalAction::Short).contains("SHORT"));
        assert!(format_action_badge(SignalAction::Hold).contains("HOLD"));
        assert!(format_action_badge(SignalAction::Close).contains("CLOSE"));

        let pos_txt = format_positions_report(&state);
        assert!(pos_txt.contains("Qty: 0.020"));
        assert!(pos_txt.contains("paper-1"));

        let cycle = CycleReport {
            timestamp_ms: 1_700_000_000_000,
            symbol: "BTCUSDT".to_string(),
            technical: TechnicalSignal {
                action: SignalAction::Long,
                fast_ema: 65_100.0,
                slow_ema: 64_800.0,
                rsi: 56.0,
                atr: 150.0,
                last_price: 65_200.0,
                confidence: 0.75,
                reason: "Bullish".to_string(),
            },
            ai: None,
            decision: state.last_decision.unwrap(),
            executed_order: state.recent_fills.first().cloned(),
            pending_order: None,
            probability: None,
            closed_positions: 0,
        };
        let cycle_txt = format_cycle_report(&cycle);
        assert!(cycle_txt.contains("EXECUTED paper-1"));
    }

    #[tokio::test]
    async fn should_execute_cli_commands_init_tick_status_positions_logs_and_panic_close() {
        let temp_dir = std::env::temp_dir().join(format!(
            "trade_bot_cli_test_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(1)
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg_path = temp_dir.join("config.toml");

        // 1. InitConfig
        let out_init = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::InitConfig { force: false },
        })
        .await
        .unwrap();
        assert!(out_init.contains("Initialized default configuration"));

        // Customize data_dir & unreachable public_base_url so paper tick runs deterministically offline
        let mut cfg = load_config_file(&cfg_path).unwrap();
        cfg.data_dir = temp_dir.join(".trade-bot").to_string_lossy().to_string();
        cfg.exchange.live_base_url = "http://127.0.0.1:9".to_string();
        cfg.strategy.min_confidence = 0.50;
        cfg.strategy.decision_policy = DecisionPolicy::ConfirmByChat;
        save_config_file(&cfg_path, &cfg).unwrap();

        // 2. TickOnce (with ConfirmByChat -> holds pending proposal)
        let out_tick = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::TickOnce,
        })
        .await
        .unwrap();
        assert!(out_tick.contains("Symbol=BTCUSDT"));

        // 2b. Approve pending order via CLI
        let out_approve = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::Approve,
        })
        .await
        .unwrap();
        assert!(out_approve.contains("Approved & executed pending order"));

        // 2c. Reject when no pending order
        let out_reject = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::Reject,
        })
        .await
        .unwrap();
        assert!(out_reject.contains("No pending order proposal"));

        // 3. Status
        let out_status = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::Status,
        })
        .await
        .unwrap();
        assert!(out_status.contains("STOPPED"));

        // 4. Positions
        let out_pos = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::Positions,
        })
        .await
        .unwrap();
        assert!(out_pos.contains("Active Futures Positions"));

        // 5. Logs
        let out_logs = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::Logs {
                lines: 10,
                follow: false,
            },
        })
        .await
        .unwrap();
        assert!(out_logs.contains("Symbol=BTCUSDT"));

        // 5b. Monitor (bounded 2 frames)
        let out_mon = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::Monitor {
                refresh_ms: 10,
                max_frames: Some(2),
            },
        })
        .await
        .unwrap();
        assert!(out_mon.contains("TRADE-BOT LIVE ACTIVITY MONITOR"));

        // 6. PanicClose & Stop
        let out_stop = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::Stop,
        })
        .await
        .unwrap();
        assert!(out_stop.contains("not currently running"));

        let out_panic = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::PanicClose,
        })
        .await
        .unwrap();
        assert!(out_panic.contains("Panic close complete"));

        // 7. ConfigInfo, ActualSkill, CheckConfig & Wizard (with SQLite .db config)
        let out_cfg_info = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::ConfigInfo,
        })
        .await
        .unwrap();
        assert!(out_cfg_info.contains("strategy.decision_policy"));
        assert!(out_cfg_info.contains("risk.sizing_mode"));
        assert!(out_cfg_info.contains("risk.require_positive_ev"));
        assert!(out_cfg_info.contains("exchange.provider"));
        assert!(out_cfg_info.contains("telegram.enabled"));

        let out_skill = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::ActualSkill,
        })
        .await
        .unwrap();
        assert!(out_skill.contains("Trade-Bot Learned Skills & Context"));

        let out_check = execute_cli(Cli {
            config: cfg_path.clone(),
            worker_id: "default".to_string(),
            command: Commands::CheckConfig,
        })
        .await
        .unwrap();
        assert!(out_check.contains("SEMUA REQUIREMENT TERPENUHI"));

        let sqlite_cfg_path = temp_dir.join("bot_config.db");
        let out_wizard = execute_cli(Cli {
            config: sqlite_cfg_path.clone(),
            worker_id: "worker-2".to_string(),
            command: Commands::Wizard {
                non_interactive: true,
            },
        })
        .await
        .unwrap();
        assert!(out_wizard.contains("Wizard Setup Selesai"));
        assert!(sqlite_cfg_path.exists());

        // 8. Telegram command handlers (with disabled Telegram adapter so no network call is made)
        let store = Arc::new(JsonStateStore::new(&cfg.data_dir));
        let tg = TelegramBotAdapter::new(cfg.telegram.clone(), "".to_string(), "".to_string());
        assert!(!handle_telegram_command(TelegramCommand::Status, &cfg, Arc::clone(&store), &tg).await);
        assert!(!handle_telegram_command(TelegramCommand::Positions, &cfg, Arc::clone(&store), &tg).await);
        assert!(!handle_telegram_command(TelegramCommand::Logs, &cfg, Arc::clone(&store), &tg).await);
        assert!(!handle_telegram_command(TelegramCommand::Config, &cfg, Arc::clone(&store), &tg).await);
        assert!(!handle_telegram_command(TelegramCommand::ActualSkill, &cfg, Arc::clone(&store), &tg).await);
        assert!(!handle_telegram_command(TelegramCommand::Approve, &cfg, Arc::clone(&store), &tg).await);
        assert!(!handle_telegram_command(TelegramCommand::Reject, &cfg, Arc::clone(&store), &tg).await);
        assert!(!handle_telegram_command(TelegramCommand::Help, &cfg, Arc::clone(&store), &tg).await);
        assert!(handle_telegram_command(TelegramCommand::Panic, &cfg, Arc::clone(&store), &tg).await);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn should_format_live_dashboard_frame_and_elapsed_durations() {
        assert_eq!(spinner_glyph(0), "⠋");
        assert_eq!(spinner_glyph(1), "⠙");
        assert_eq!(spinner_glyph(10), "⠋");

        assert_eq!(format_elapsed_duration(None, 10_000), "never");
        assert_eq!(format_elapsed_duration(Some(5_000), 12_000), "7s");
        assert_eq!(format_elapsed_duration(Some(0), 125_000), "2m 5s");
        assert_eq!(format_elapsed_duration(Some(0), 3_725_000), "1h 2m 5s");

        let cfg = BotConfig::default();
        let mut state = BotState::new(&cfg);
        state.started_at_ms = Some(1_000);
        state.last_tick_ms = Some(8_000);

        let logs = vec!["[2026-11-15] Symbol=BTCUSDT | Decision=Long".to_string()];
        let running_frame =
            format_live_dashboard_frame(&cfg, &state, Some(8888), &logs, 0, 12_000);
        assert!(running_frame.contains("[ACTIVE / RUNNING] (PID: 8888)"));
        assert!(running_frame.contains("Last tick: 4s ago"));
        assert!(running_frame.contains("Uptime: 11s"));

        let stopped_frame = format_live_dashboard_frame(&cfg, &state, None, &[], 1, 12_000);
        assert!(stopped_frame.contains("[STOPPED]"));
    }
}

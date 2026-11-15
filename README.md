# Trade-Bot — Binance USDⓈ-M Futures Hybrid Trading Bot Agent

Bot trading berjangka (**Binance USDⓈ-M Futures / USDT-Margined**) berbasis **Rust** dengan antarmuka **CLI** dan **Built-in Background Daemon**.

Menggunakan arsitektur **Hybrid Agent** yang menggabungkan:
1. **Indikator Teknikal Murni (`EMA`, `RSI`, `ATR`)** — identifikasi tren, filter momentum *overbought/oversold*, serta volatilitas.
2. **AI / LLM Market Advisor Opsional** — konfirmasi sinyal atau veto terhadap kondisi pasar anomali melalui endpoint kompatibel OpenAI Chat Completions.
3. **Risk Guardrails & Circuit Breaker** — perhitungan ukuran posisi otomatis berdasarkan `% risiko per trade`, *Stop-Loss* & *Take-Profit* berbasis kelipatan `ATR`, batas *leverage*, serta pengaman *Max Daily Drawdown*.

---

## Fitur Utama

- **3 Mode Eksekusi:**
  - `paper`: Simulasi lokal (saldo virtual & eksekusi SL/TP otomatis menggunakan data klines publik atau sintetis). Aman untuk percobaan tanpa risiko dana.
  - `testnet`: Terhubung ke **Binance Futures Testnet** (`https://testnet.binancefuture.com`).
  - `live`: Terhubung ke **Binance USDⓈ-M Futures Mainnet** (`https://fapi.binance.com`).
- **Jalan di Background (Built-in Daemon):**
  - Tidak perlu `tmux`/`nohup` manual. Perintah `start --daemon` otomatis menjalankan worker di background, mencatat `PID` ke `.trade-bot/bot.pid`, menyimpan state ke `.trade-bot/bot_state.json`, dan menulis log ke `.trade-bot/bot.log`.
- **Tombol Darurat (`panic-close`):**
  - Menghentikan daemon background sekaligus menutup seluruh posisi terbuka secara instan (*Market Reduce-Only*).

---

## Prasyarat

- **Rust Toolchain** (`rustc >= 1.85`, `cargo`)
- **OS:** macOS atau Linux
- *(Opsional untuk mode `testnet` / `live`)* API Key & Secret Binance Futures (pastikan posisi akun diatur ke **One-Way Mode**).

---

## 1. Cara Build

```bash
# Build mode debug (cepat untuk development)
cargo build

# Atau build mode release (dioptimasi untuk produksi)
cargo build --release
```

Binary akan tersedia di:
- Debug: `./target/debug/trade-bot`
- Release: `./target/release/trade-bot`

---

## 2. Inisialisasi & Konfigurasi

Buat file konfigurasi awal (`config.json`):

```bash
./target/release/trade-bot init-config
```

Jika ingin menimpa konfigurasi yang sudah ada:

```bash
./target/release/trade-bot init-config --force
```

### Contoh Isi `config.json`

```json
{
  "mode": "paper",
  "symbol": "BTCUSDT",
  "interval": "5m",
  "kline_limit": 100,
  "poll_interval_secs": 15,
  "data_dir": ".trade-bot",
  "strategy": {
    "fast_ema_period": 9,
    "slow_ema_period": 21,
    "rsi_period": 14,
    "rsi_overbought": 70.0,
    "rsi_oversold": 30.0,
    "atr_period": 14,
    "min_confidence": 0.6
  },
  "risk": {
    "leverage": 3,
    "max_leverage": 10,
    "risk_per_trade_pct": 0.01,
    "atr_sl_multiplier": 1.5,
    "atr_tp_multiplier": 3.0,
    "max_daily_drawdown_pct": 0.05,
    "max_open_positions": 1,
    "initial_paper_balance_usdt": 1000.0
  },
  "ai": {
    "enabled": false,
    "endpoint_url": "https://api.openai.com/v1/chat/completions",
    "model": "gpt-4o-mini",
    "api_key_env": "AI_API_KEY",
    "weight": 0.35
  },
  "exchange": {
    "api_key_env": "BINANCE_API_KEY",
    "api_secret_env": "BINANCE_API_SECRET",
    "testnet_base_url": "https://testnet.binancefuture.com",
    "live_base_url": "https://fapi.binance.com",
    "recv_window_ms": 5000
  }
}
```

### Mengatur Kredensial (Environment Variables)

Kredensial **tidak pernah** disimpan di dalam file konfigurasi demi keamanan. Set melalui environment variable sebelum menjalankan mode `testnet` atau `live`:

```bash
export BINANCE_API_KEY="api_key_binance_anda"
export BINANCE_API_SECRET="api_secret_binance_anda"

# Opsional jika "ai.enabled": true
export AI_API_KEY="api_key_openai_atau_llm_anda"
```

---

## 3. Cara Menjalankan Bot (CLI & Background Daemon)

### A. Menjalankan di Background (Daemon Mode — Default)
```bash
./target/release/trade-bot start --daemon
```
Contoh output:
```text
Trading bot daemon started in background (PID: 56598, Mode: paper, Symbol: BTCUSDT, Log: .trade-bot/bot.log)
```

*(Jika ingin menjalankan di foreground terminal untuk melihat proses secara langsung, gunakan `./target/release/trade-bot start --foreground`).*

### B. Mengecek Status Bot & Keputusan Terakhir
```bash
./target/release/trade-bot status
```
Contoh output:
```text
=== Binance Futures Trading Bot Status ===
Status          : RUNNING (PID 56598)
Mode            : paper
Symbol          : BTCUSDT (5m)
Leverage        : 3x (Max 10x)
Wallet Balance  : 1000.00 USDT (Day Start: 1000.00 USDT)
Realized PnL    : +0.00 USDT
Total Trades    : 1
Open Positions  : 1
Last Decision   : Long (confidence 0.72) — Bullish trend (EMA9=66480.12 > EMA21=66390.45, RSI=61.4, ATR=92.50)
```

### C. Melihat Posisi Aktif & Riwayat Order Fill
```bash
./target/release/trade-bot positions
```

### D. Melihat Log Background Daemon
```bash
# Menampilkan 30 baris log terakhir (default)
./target/release/trade-bot logs

# Menampilkan 100 baris log terakhir
./target/release/trade-bot logs -n 100
```

### E. Menjalankan 1 Siklus Evaluasi Secara Instan (`tick-once`)
Berguna untuk mengetes konfigurasi atau strategi tanpa harus menyalakan loop daemon:
```bash
./target/release/trade-bot tick-once
```

### F. Menghentikan Background Daemon
Menghentikan proses daemon di background secara bersih (`SIGTERM`), tanpa menutup posisi yang sedang terbuka di market:
```bash
./target/release/trade-bot stop
```

### G. Penutupan Darurat (`panic-close`)
Menghentikan daemon background **dan** langsung menutup seluruh posisi terbuka dengan order `MARKET` (*reduce-only*):
```bash
./target/release/trade-bot panic-close
```

### Menggunakan File Konfigurasi Custom
Seluruh perintah mendukung flag global `--config <PATH>` (atau `-c`):
```bash
./target/release/trade-bot --config my-eth-config.json start --daemon
./target/release/trade-bot --config my-eth-config.json status
```

---

## 4. Struktur Direktori Proyek

```text
.
├── Cargo.toml
├── README.md
├── src/
│   ├── lib.rs
│   ├── main.rs                 # Entrypoint binary CLI
│   ├── cli.rs                  # Definisi subcommands & handler CLI
│   ├── core/                   # Core logic murni & agnostik (100% bebas I/O)
│   │   ├── domain.rs           # Tipe data domain, config, state, dan error
│   │   ├── ports.rs            # Trait abstraksi (ExchangePort, AiAdvisorPort, StateStorePort, ClockPort)
│   │   ├── indicators.rs       # Kalkulasi EMA, RSI, ATR, dan evaluasi sinyal teknikal
│   │   ├── risk.rs             # Position sizing, harga SL/TP berbasis ATR, validasi drawdown
│   │   └── agent.rs            # Hybrid Agent Decision Engine & orchestrator siklus
│   └── adapters/               # Adapter I/O tipis
│       ├── binance.rs          # REST client Binance USDⓈ-M Futures + HMAC-SHA256 signer
│       ├── paper.rs            # Simulator Paper Trading lokal
│       ├── ai_advisor.rs       # HTTP client untuk AI/LLM Advisor
│       ├── state_store.rs      # Penyimpanan atomik bot_state.json & config.json
│       └── daemon.rs           # Manajemen proses background (PID file, log, SIGTERM)
└── docs/
    └── tasks/                  # Dokumentasi teknis & diagram alur
```

---

## 5. Pengujian & Linter

```bash
# Menjalankan seluruh unit test dan doc-test
cargo test

# Menjalankan linter Clippy
cargo clippy -- -D warnings
```

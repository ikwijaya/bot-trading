# Cara Kerja Trade-Bot (Arsitektur & Alur Mermaid)

Dokumen ini menjelaskan cara kerja **Trade-Bot** secara menyeluruh mulai dari siklus hidup proses *background daemon*, perhitungan indikator teknikal (`EMA`, `RSI`, `ATR`), penggabungan sinyal dengan **AI Advisor**, kalkulasi manajemen risiko (*Position Sizing*, *Stop-Loss*, *Take-Profit*, *Daily Drawdown Circuit Breaker*), hingga pemantauan dan kontrol jarak jauh melalui **Bot Telegram**.

---

## 1. Gambaran Umum Arsitektur (Ports & Adapters)

Trade-Bot dirancang menggunakan pola **Hexagonal Architecture (Ports & Adapters)** yang memisahkan **Core Logic** (murni, deterministik, 100% bebas I/O) dari **Adapters** (komunikasi HTTP ke Binance, LLM, Telegram, dan penyimpanan file lokal).

```mermaid
flowchart LR
    subgraph CLI_And_Daemon["CLI & Daemon Controller"]
        CLI["CLI (trade-bot)"]
        Worker["Background Worker Loop"]
    end

    subgraph Core_Engine["Core Engine (src/core/ - Pure & Agnostic)"]
        Indicators["Technical Indicators (EMA, RSI, ATR)"]
        Combiner["Signal Combiner (Tech + AI)"]
        Risk["Risk Engine (Position Size, SL/TP, Drawdown)"]
        Agent["AgentEngine::execute_cycle"]
    end

    subgraph Adapters["I/O Adapters (src/adapters/)"]
        Binance["BinanceFuturesClient (Testnet / Live)"]
        Paper["PaperExchangeAdapter (Local Sim)"]
        AI["HttpAiAdvisor (OpenAI / LLM)"]
        Telegram["TelegramBotAdapter (Notify & Commands)"]
        Store["JsonStateStore (.trade-bot/bot_state.json)"]
    end

    CLI --> Worker
    Worker --> Agent
    Agent --> Indicators
    Agent --> Combiner
    Agent --> Risk
    Agent --> Binance
    Agent --> Paper
    Agent --> AI
    Agent --> Store
    Worker --> Telegram
```

---

## 2. Alur Siklus Hidup Background Daemon (`start --daemon`)

Saat pengguna menjalankan `trade-bot start --daemon`, proses utama CLI tidak mengunci terminal melainkan men-*spawn* proses anak (*detached worker*) di background:

```mermaid
flowchart TD
    StartCmd["User menjalankan: trade-bot start --daemon"] --> LoadEnv["Muat file .env (jika ada) & baca config.json"]
    LoadEnv --> CheckPID{"Cek .trade-bot/bot.pid"}
    CheckPID -- "PID ada & proses hidup (kill -0)" --> Reject["Tolak duplikasi: tampilkan PID yang sedang aktif"]
    CheckPID -- "Tidak ada / Stale PID" --> Spawn["Spawn proses background: trade-bot run-worker"]
    Spawn --> WriteFiles["Simpan PID ke .trade-bot/bot.pid & redirect log ke .trade-bot/bot.log"]
    WriteFiles --> NotifyStart["Kirim notifikasi start ke Bot Telegram (jika aktif)"]
    NotifyStart --> Loop["Loop setiap poll_interval_secs detik"]
    Loop --> ExecCycle["Jalankan 1 Siklus Evaluasi (AgentEngine::execute_cycle)"]
    ExecCycle --> CheckTgAlert{"Ada Order / notify_on_cycle?"}
    CheckTgAlert -- "Ya" --> SendTg["Kirim Alert ke Telegram"]
    CheckTgAlert -- "Tidak" --> PollTgCmd["Polling perintah masuk Telegram (getUpdates)"]
    SendTg --> PollTgCmd
    PollTgCmd --> SleepWait["Tunggu poll_interval_secs atau sinyal SIGTERM / Ctrl+C"]
    SleepWait -- "Lanjut" --> Loop
    SleepWait -- "SIGTERM (trade-bot stop / /panic)" --> CleanExit["Bersihkan PID, update state running=false, berhenti"]
```

---

## 3. Sequence Diagram: Satu Siklus Evaluasi Trading (`execute_cycle`)

Berikut urutan interaksi antar komponen pada setiap satu siklus evaluasi:

```mermaid
sequenceDiagram
    participant W as Background Worker
    participant A as AgentEngine (Core)
    participant S as StateStore (.trade-bot)
    participant E as Exchange (Paper/Testnet/Live)
    participant I as Indicators & Risk (Core)
    participant L as AI Advisor (LLM)
    participant T as Telegram Bot

    W->>A: execute_cycle(cfg)
    A->>S: load_state()
    A->>E: fetch_klines(symbol, interval, kline_limit)
    E-->>A: Vec<Candle> (OHLCV)
    A->>I: evaluate_technical_snapshot(candles, strategy_cfg)
    I-->>A: TechnicalSignal (EMA9, EMA21, RSI, ATR, action, confidence)
    A->>E: fetch_account(symbol)
    E-->>A: AccountSnapshot (balance, open_positions)

    alt ai.enabled == true
        A->>L: assess_market(symbol, technical_signal, positions)
        L-->>A: Option<AiAssessment> (action, confidence, reasoning)
    end

    A->>I: combine_signals(tech, ai, ai_weight, min_confidence)
    I-->>A: AgentDecision (LONG / SHORT / HOLD / CLOSE)

    opt Posisi terbuka berlawanan arah ATAU Decision == CLOSE
        A->>E: close_all_positions(symbol)
        E-->>A: Vec<OrderFill> (Realized PnL)
    end

    opt Decision == LONG atau SHORT
        A->>I: validate_risk_limits(state, account, risk_cfg)
        alt Risk Valid (Drawdown aman & slot posisi tersedia)
            A->>I: compute_sl_tp_prices(last_price, atr, side, risk_cfg)
            A->>I: calculate_position_size(available_balance, entry, sl, risk_cfg)
            A->>E: set_leverage(symbol, leverage)
            A->>E: place_order(MARKET + STOP_MARKET SL + TAKE_PROFIT_MARKET TP)
            E-->>A: OrderFill
        end
    end

    A->>S: save_state(updated_state)
    A-->>W: CycleReport
    opt telegram.enabled == true
        W->>T: send_notification(alert) & fetch_updates()
    end
```

---

## 4. Logika Evaluasi Sinyal Teknikal & Penggabungan AI (`Decision Tree`)

### A. Sinyal Indikator Teknikal (`evaluate_technical_snapshot`)
1. **Exponential Moving Average (`fast_ema` vs `slow_ema`):** Menentukan arah tren utama.
2. **Relative Strength Index (`rsi`):** Mencegah beli di pucuk (*overbought*) atau jual di dasar (*oversold*).
3. **Average True Range (`atr`):** Mengukur volatilitas rata-rata dalam satuan harga untuk menentukan jarak *Stop-Loss* dan *Take-Profit* yang adaptif.

### B. Penggabungan dengan AI Advisor (`combine_signals`)

```mermaid
flowchart TD
    Candles["Data Candlestick OHLCV"] --> Calc["Hitung Fast EMA, Slow EMA, RSI, ATR"]
    Calc --> CheckTrend{"Cek Kondisi Tren & Momentum"}

    CheckTrend -- "Fast EMA > Slow EMA & 45 <= RSI < Overbought & Price >= Slow EMA" --> TechLong["Technical Signal: LONG"]
    CheckTrend -- "Fast EMA < Slow EMA & Oversold < RSI <= 55 & Price <= Slow EMA" --> TechShort["Technical Signal: SHORT"]
    CheckTrend -- "Kondisi Lainnya / Sideways" --> TechHold["Technical Signal: HOLD"]

    TechLong --> CheckAI{"AI Advisor Aktif?"}
    TechShort --> CheckAI
    TechHold --> CheckAI

    CheckAI -- "Tidak Aktif" --> ConfCheck1{"Tech Confidence >= min_confidence?"}
    ConfCheck1 -- "Ya" --> FinalExec["Keputusan: Eksekusi Sinyal Teknikal"]
    ConfCheck1 -- "Tidak" --> FinalHold["Keputusan: HOLD"]

    CheckAI -- "Aktif" --> AICall["Evaluasi Konteks oleh AI Advisor"]
    AICall --> CheckAIClose{"AI Action == CLOSE & Conf >= min?"}
    CheckAIClose -- "Ya" --> FinalClose["Keputusan: CLOSE Posisi"]
    CheckAIClose -- "Tidak" --> CheckAgree{"AI Setuju dengan Arah Teknikal?"}

    CheckAgree -- "Setuju (Misal Tech=LONG & AI=LONG)" --> Blend["Hitung Blended Confidence = (1 - w)*Tech + w*AI"]
    CheckAgree -- "Berbeda Arah (AI Veto)" --> FinalHold

    Blend --> ConfCheck2{"Blended Confidence >= min_confidence?"}
    ConfCheck2 -- "Ya" --> FinalExec
    ConfCheck2 -- "Tidak" --> FinalHold
```

---

## 5. Mekanisme Kebijakan Keputusan (`decision_policy`), Batas Pembelian & Kalkulasi Probabilitas Anti-Rugi (`Expected Value`)

Setiap kali sinyal `LONG` atau `SHORT` lolos dari evaluasi strategi, order diproses melalui **Risk & Probability Engine** (`src/core/risk.rs`) dan **Decision Policy** (`src/core/agent.rs`):

```mermaid
flowchart TD
    Signal["Sinyal LONG / SHORT"] --> PolicySkip{"strategy.decision_policy == 'skip'?"}
    PolicySkip -- "Ya" --> SkipOrder["Skip Pembukaan Posisi Baru (Monitoring Only)"]
    PolicySkip -- "Tidak" --> Guard1{"1. Cek Leverage, Daily Drawdown & Max Open Positions"}
    Guard1 -- "Gagal" --> Block["Order Diblokir"]
    Guard1 -- "Lolos" --> CalcSLTP["2. Hitung Harga SL & TP Berbasis ATR"]
    CalcSLTP --> CalcProb["3. Hitung Win Probability, Break-Even Win Rate, Expected Value (EV) setelah Fee 0.08% & Half-Kelly"]
    CalcProb --> CheckEV{"risk.require_positive_ev == true & EV <= 0?"}
    CheckEV -- "Rugi Secara Matematis (EV <= 0)" --> Block
    CheckEV -- "Menguntungkan (EV > 0)" --> CalcSize["4. Hitung Ukuran Order sesuai sizing_mode ('probability_ev' / 'risk_pct' / 'max_balance' / 'fixed_min') + Clamp [min_order_usdt..max_order_usdt]"]
    CalcSize --> CheckPolicy{"5. Cek strategy.decision_policy"}
    CheckPolicy -- "'auto'" --> AutoExec["Otomatis Eksekusi Order ke Exchange"]
    CheckPolicy -- "'confirm_by_chat'" --> HoldChat["Tahan Order (pending_order) & Kirim Proposal ke Telegram/CLI -> Tunggu /approve atau /reject"]
```

### A. 3 Mode Pengambilan Keputusan (`strategy.decision_policy`)
1. **`"auto"` (Otomatis Beli):** Begitu sinyal valid dan lolos kalkulasi probabilitas & risiko, bot langsung mengeksekusi order ke exchange.
2. **`"confirm_by_chat"` (Hold & Konfirmasi via Chat/CLI):** Bot **menahan order** di `pending_order`, mengirimkan info lengkap ke Telegram (harga entry, SL, TP, nilai USDT, probabilitas menang, *break-even win rate*, dan *Expected Value*), lalu menunggu Anda mengetik **`/approve`** (atau `/buy`) di chat Telegram (atau `./target/release/trade-bot approve` di CLI) untuk membeli, atau **`/reject`** untuk membatalkan.
3. **`"skip"` (Skip Eksekusi):** Bot tetap menganalisis pasar dan mengirim laporan sinyal, tetapi melewati semua pembukaan posisi baru.

### B. 4 Mode Ukuran Pembelian (`risk.sizing_mode`) & Batas Nominal (`min_order_usdt` / `max_order_usdt`)
- **`"probability_ev"` (Default — Hitung Probabilitas & Half-Kelly):** Menyesuaikan ukuran pembelian secara proporsional dengan keunggulan probabilitas (*Half-Kelly Criterion*) setelah memperhitungkan *round-trip taker fee* `0.08%`.
- **`"risk_pct"`:** Berdasarkan persentase risiko saldo (`risk_per_trade_pct`) terhadap jarak *Stop-Loss*.
- **`"max_balance"`:** Menggunakan daya beli maksimum sesuai saldo tersedia & *leverage* (atau dibatasi oleh `max_order_usdt`).
- **`"fixed_min"`:** Menggunakan batas nominal pembelian minimum tetap (`min_order_usdt`).
- Seluruh mode di atas selalu di-*clamp* di antara **`risk.min_order_usdt`** (batas bawah pembelian, misal `10.0` USDT) dan **`risk.max_order_usdt`** (batas atas pembelian; `0.0` = otomatis mengikuti batas saldo & leverage).

### C. Rumus Anti-Rugi (`evaluate_trade_probability` & `require_positive_ev`)
1. **Net Reward & Net Risk (setelah estimasi fee transaksi round-trip `0.08%`):**
   - $\text{Net Reward \%} = \max\left(0,\; \frac{|\text{Take Profit} - \text{Entry}|}{\text{Entry}} - 0.0008\right)$
   - $\text{Net Risk \%} = \frac{|\text{Entry} - \text{Stop Loss}|}{\text{Entry}} + 0.0008$
   - $\text{Net R:R} = \frac{\text{Net Reward \%}}{\text{Net Risk \%}}$
2. **Break-Even Win Rate & Expected Value (EV):**
   - $\text{Break-Even Win Rate} = \frac{1}{1 + \text{Net R:R}}$
   - $\text{Expected Value (R)} = (p \times \text{Net R:R}) - (1 - p)$
   - Jika `risk.require_positive_ev = true`, maka transaksi **wajib** memiliki $\text{EV} > 0$ dan $p > \text{Break-Even Win Rate}$ agar tidak merugi secara ekspektasi matematis.

---

## 6. Alur Pemantauan & Kontrol Interaktif via Bot Telegram

Saat `telegram.enabled = true`, daemon menjalankan dua fungsi komunikasi dengan Telegram:
1. **Push Notifications:** Mengirim pesan otomatis ke Telegram Anda ketika daemon menyala, ketika ada order baru dieksekusi (`notify_on_trade = true`), ketika posisi ditutup, atau setiap siklus (`notify_on_cycle = true`).
2. **Interactive Command Polling (`listen_commands = true`):** Daemon mengambil pesan masuk dari Telegram (`getUpdates`), memverifikasi bahwa `chat.id` pengirim sama dengan `TELEGRAM_CHAT_ID` Anda, lalu mengeksekusi perintah:

```mermaid
flowchart TD
    UserTg["Anda mengirim pesan di aplikasi Telegram"] --> Poll["Daemon memanggil getUpdates"]
    Poll --> Auth{"is_authorized_chat(chat_id, TELEGRAM_CHAT_ID)?"}
    Auth -- "Tidak Sah (Orang Lain)" --> Ignore["Abaikan Pesan (Keamanan Terjaga)"]
    Auth -- "Sah (Milik Anda)" --> ParseCmd{"parse_telegram_command"}

    ParseCmd -- "/status" --> ReplyStatus["Balas status daemon, saldo USDT, PnL, dan sinyal terakhir"]
    ParseCmd -- "/positions" --> ReplyPos["Balas daftar posisi Futures terbuka & 5 order fill terakhir"]
    ParseCmd -- "/logs" --> ReplyLogs["Balas 10 baris log terakhir dari .trade-bot/bot.log"]
    ParseCmd -- "/config" --> ReplyCfg["Balas ringkasan konfigurasi aktif"]
    ParseCmd -- "/panic" --> ExecPanic["Tutup semua posisi terbuka (Market Reduce-Only) + Hentikan Daemon + Kirim Konfirmasi"]
    ParseCmd -- "/help atau /start" --> ReplyHelp["Balas daftar perintah Telegram yang tersedia"]
```

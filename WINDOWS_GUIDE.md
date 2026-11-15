# 🪟 Panduan Lengkap Menjalankan & Export `trade-bot.exe` di Windows

Dokumen ini menjelaskan cara meng-export aplikasi menjadi file executable mandiri (**`trade-bot.exe`**) untuk Windows, cara menjalankan **Setup Wizard Interaktif** (termasuk pemilihan lokasi config `.json` atau database **SQLite `.db`**), pengecekan kelayakan konfigurasi (**`check-config`**), serta penggunaan **Multi-Worker** dan **Multi-Exchange** (`Binance`, `Bybit`, `OKX`).

---

## 1. Cara Export / Build Aplikasi Menjadi `trade-bot.exe`

Aplikasi ini menggunakan `rustls` (murni Rust tanpa ketergantungan OpenSSL eksternal) dan SQLite terintegrasi (`rusqlite`), sehingga hasil kompilasi berupa **satu file `trade-bot.exe` mandiri (portable)**.

### Opsi A — Build Langsung di PC/Server Windows (Direkomendasikan)

1. **Install Rust Toolchain untuk Windows:**
   - Unduh dan jalankan installer resmi dari [https://rustup.rs](https://rustup.rs) (`rustup-init.exe`).
   - Pastikan **Desktop development with C++ (Visual Studio Build Tools)** tercentang saat instalasi jika diminta.
2. **Buka PowerShell atau Command Prompt di folder proyek, lalu jalankan:**
   ```powershell
   cargo build --release
   ```
3. **Ambil file `.exe` hasil export:**
   - File binary siap pakai tersedia di:
     ```text
     target\release\trade-bot.exe
     ```
   - Anda dapat menyalin `trade-bot.exe` (bersama file `.env` jika ada) ke folder mana saja di Windows (misal `C:\TradeBot\trade-bot.exe`).

---

### Opsi B — Cross-Compile `trade-bot.exe` dari macOS atau Linux

Jika Anda ingin meng-export file `.exe` Windows langsung dari laptop macOS/Linux:

1. **Tambahkan target Windows MSVC & `cargo-xwin`:**
   ```bash
   rustup target add x86_64-pc-windows-msvc
   cargo install cargo-xwin
   ```
2. **Build binary release untuk Windows:**
   ```bash
   cargo xwin build --release --target x86_64-pc-windows-msvc
   ```
3. **File hasil export berada di:**
   ```text
   target/x86_64-pc-windows-msvc/release/trade-bot.exe
   ```

---

## 2. Setup Konfigurasi dengan Wizard Interaktif (`wizard`)

Anda tidak perlu mengedit file konfigurasi secara manual. Jalankan perintah **`wizard`** untuk mengatur lokasi file konfigurasi (bisa berupa `.json` maupun database **SQLite `.db`**), memilih bursa (`binance` / `bybit` / `okx`), mode eksekusi, hingga batas pembelian:

```powershell
# Menjalankan wizard interaktif (akan menanyakan lokasi path config & pengaturan utama)
.\trade-bot.exe wizard

# Atau langsung menentukan lokasi database SQLite custom:
.\trade-bot.exe --config C:\TradeBot\my_config.db wizard
```

### Pertanyaan yang Ditampilkan oleh Wizard:
1. **Lokasi penyimpanan file konfigurasi**: Bisa diisi `config.json` (format JSON) atau `config.db` / `config.sqlite` (otomatis disimpan ke tabel `bot_configs` di database **SQLite**).
2. **Exchange Provider**: Pilih `binance` (Binance USDⓈ-M Futures), `bybit` (Bybit V5 Linear USDT), atau `okx` (OKX V5 USDT Swap).
3. **Mode Eksekusi**: Pilih `paper` (simulasi aman tanpa API key), `testnet`, atau `live`.
4. **Simbol Kontrak**: Contoh `BTCUSDT`, `ETHUSDT`, `SOLUSDT`.
5. **Kebijakan Keputusan (`decision_policy`)**:
   - `auto`: Otomatis eksekusi beli/jual saat sinyal & probabilitas terpenuhi.
   - `confirm_by_chat`: Tahan order, kirim info ke Telegram/CLI, dan tunggu perintah `/approve` (atau `trade-bot.exe approve`).
   - `skip`: Hanya pantau sinyal tanpa eksekusi order.
6. **Mode Ukuran Pembelian (`sizing_mode`)**: `probability_ev` (otomatis hitung probabilitas & Half-Kelly), `risk_pct`, `max_balance`, atau `fixed_min`.
7. **Batas Minimum & Maksimum Pembelian (`min_order_usdt` & `max_order_usdt`)**.

Begitu selesai, Wizard otomatis menyimpan konfigurasi dan menjalankan **Validasi Requirement Konfigurasi**.

---

## 3. Pengecekan Kelayakan Konfigurasi (`check-config`)

Kapan saja sebelum menjalankan bot, Anda dapat memeriksa apakah seluruh parameter konfigurasi dan API key di `.env` sudah memenuhi syarat:

```powershell
# Cek file config default (config.json)
.\trade-bot.exe check-config

# Cek konfigurasi dari database SQLite
.\trade-bot.exe --config C:\TradeBot\my_config.db check-config
```

Contoh tampilan hasil `check-config`:
```text
========================================================================
🔍 HASIL PENGECEKAN REQUIREMENT KONFIGURASI (C:\TradeBot\my_config.db)
========================================================================
 ✅ [LOLOS] symbol                   : Simbol pasangan kontrak valid: BTCUSDT (Exchange: binance)
 ✅ [LOLOS] strategy.ema_periods     : Periode EMA valid: fast=9 < slow=21
 ✅ [LOLOS] kline_limit              : Jumlah candlestick (100) mencukupi untuk kalkulasi indikator (min 27)
 ✅ [LOLOS] strategy.rsi_bounds      : Batas RSI valid: oversold=30.0, overbought=70.0
 ✅ [LOLOS] risk.leverage            : Leverage aman: 3x (batas maksimum: 10x)
 ✅ [LOLOS] risk.order_bounds_usdt   : Batas pembelian valid: Min=$15.00 USDT, Max=Sesuai Saldo (Auto) (Mode: probability_ev)
 ✅ [LOLOS] exchange.credentials     : Mode PAPER aktif — tidak memerlukan API Key Exchange untuk simulasi
 ✅ [LOLOS] ai.credentials           : AI Advisor dinonaktifkan (ai.enabled = false) — bot menggunakan pure teknikal + learned skills
 ✅ [LOLOS] telegram.credentials     : Notifikasi Telegram dinonaktifkan (telegram.enabled = false)
------------------------------------------------------------------------
 🎉 STATUS AKHIR: SEMUA REQUIREMENT TERPENUHI (SIAP DIJALANKAN)
========================================================================
```

---

## 4. Menjalankan Banyak Worker Sekaligus (`--worker-id`) Tanpa Bentrok

Jika Anda ingin menjalankan **banyak worker secara paralel** (misalnya Worker 1 untuk `BTCUSDT` dan Worker 2 untuk `ETHUSDT`), gunakan flag global **`--worker-id <nama_worker>`**:

```powershell
# Jalankan Worker Utama (default) menggunakan config.db
.\trade-bot.exe --config config.db start --daemon

# Jalankan Worker Kedua (terisolasi di .trade-bot/workers/eth-worker/)
.\trade-bot.exe --config config_eth.db --worker-id eth-worker start --daemon

# Memantau status & live dashboard masing-masing worker:
.\trade-bot.exe --config config.db status
.\trade-bot.exe --config config_eth.db --worker-id eth-worker monitor
```

### Mengapa Multi-Worker Ini Aman dari Kekeliruan?
1. **Isolasi State, PID, Log, dan `skills.md`**: Setiap `--worker-id` menyimpan `bot_state.json`, `bot.pid`, `bot.log`, dan `skills.md` di sub-folder terpisah (`.trade-bot/workers/<worker-id>/`), sehingga tidak akan pernah saling menimpa saldo atau posisi.
2. **Pencegahan Bentrok Polling Telegram (`HTTP 409 Conflict`)**: Hanya worker utama (`--worker-id default`) yang melakukan long-polling `getUpdates` ke Telegram, sedangkan worker tambahan tetap dapat mengirim notifikasi alert ke Telegram tanpa memicu duplikasi eksekusi perintah chat.
3. **Tips Penting untuk Akun Live Exchange**: Jika menjalankan banyak worker pada satu akun bursa Live yang sama, pastikan setiap worker menangani **simbol pasangan berbeda** (mis. Worker A = `BTCUSDT`, Worker B = `ETHUSDT`) agar tidak saling menutup posisi di kontrak yang sama.

---

## 5. Melihat Hasil Pembelajaran AI (`skills.md` / `/actual-skill`)

Pada setiap siklus evaluasi, bot secara otomatis mempelajari rezim pasar (Bullish/Bearish), menghitung Realized Win-Rate, mencatat rata-rata Expected Value (+EV), menyimpan insight AI terakhir, dan memperbarui aturan adaptif di file **`.trade-bot/skills.md`**. Isi file ini juga otomatis disuntikkan kembali ke prompt AI pada siklus berikutnya sebagai **Continuous Learning Context**.

- **Lihat lewat CLI di Windows/macOS/Linux:**
  ```powershell
  .\trade-bot.exe actual-skill
  ```
- **Lihat langsung dari Chat Bot Telegram:**
  Ketik perintah:
  ```text
  /actual-skill
  ```

---

## 6. Menjalankan Sebagai Desktop App (GUI Visual) & Tanya AI (`/query`)

Aplikasi ini dilengkapi **Built-in Desktop App GUI** tanpa memerlukan instalasi runtime tambahan:

1. **Cara Membuka Desktop App GUI di Windows / macOS / Linux:**
   - **Cara 1 (Double-Click):** Klik 2x langsung file **`trade-bot.exe`** di Windows Explorer (ketika dijalankan tanpa argumen, aplikasi otomatis menyalakan server lokal `127.0.0.1:8787` dan membuka jendela **Trade-Bot Desktop App**).
   - **Cara 2 (Lewat Terminal / PowerShell):**
     ```powershell
     .\trade-bot.exe gui --port 8787
     ```
2. **Fitur di Dalam Desktop App GUI:**
   - **Tombol 1-Klik:** `▶ Start Daemon`, `⏹ Stop Daemon`, `⚡ Tick Once`, `✅ Approve Pending Order`, `🚫 Reject Pending Order`, dan `🚨 Panic Close`.
   - **Kotak Tanya AI Langsung (`/query`):** Ketik pertanyaan seputar posisi, alasan belum beli, atau log terakhir langsung dari layar Desktop App.
   - **Form Editor Konfigurasi & Check-Config:** Ubah Exchange (`binance`/`bybit`/`okx`), Mode, Symbol, Decision Policy, Min/Max USDT, dan simpan langsung ke `config.json` atau `config.db`.
3. **Tanya AI Langsung dari Telegram (`/query <teks>`):**
   - Pastikan `"ai": { "enabled": true }` di config dan `AI_API_KEY` (serta opsional `AI_SYSTEM_PROMPT`) sudah diisi di `.env`.
   - Dari ruang chat Bot Telegram Anda, kirim perintah:
     ```text
     /query kenapa siklus terakhir belum beli dan bagaimana kondisi RSI saat ini?
     ```
   - Atau dari CLI:
     ```powershell
     .\trade-bot.exe query "analisa posisi terbuka dan log terakhir saya"
     ```


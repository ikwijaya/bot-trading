# Panduan Lengkap Menjalankan `trade-bot` 24 Jam di Android & iPhone

Dokumen ini menjelaskan langkah-demi-langkah cara menjadikan **HP Android** sebagai server trading 24 jam hemat listrik (tanpa perlu root) menggunakan **Termux**, serta cara menginstal Dashboard Web App ke layar utama **Android (Chrome)** maupun **iPhone (Safari iOS)**.

---

## Bagian 1: Menjalankan Worker 24 Jam Langsung di Dalam HP Android (Via Termux)

Karena `trade-bot` ditulis dengan Rust murni + `rustls` (tanpa bergantung pada OpenSSL sistem) dan SQLite terbundel (`rusqlite` `bundled`), bot ini dapat dikompilasi dan dijalankan langsung di HP Android 64-bit (`aarch64`).

Ada **dua mode** menjalankan bot di Android:

- **Opsi A (Rekomendasi): Binary Pre-Built dari Laptop Dev** — cross-compile binary ARM64 Android di laptop Anda (sekali saja), lalu hanya zip + transfer folder `dist/android-termux/` ke HP. Tidak perlu `cargo build` di HP. Lihat **[Opsi A](#opsi-a-rekomendasi-binary-pre-built-dari-laptop-dev-tanpa-build-di-hp)** di bawah.
- **Opsi B (Fallback): Build On-Device** — build Rust langsung di HP via Termux (~5-15 menit, butuh ~1.5 GB storage untuk toolchain + dependencies). Lihat **[Opsi B](#opsi-b-fallback-build-on-device-di-termux)** di bawah.

---

### Opsi A (Rekomendasi): Binary Pre-Built dari Laptop Dev (Tanpa Build di HP)

#### Langkah A1 — Setup Android NDK + cargo-ndk di Laptop Dev (Sekali Saja)

Di laptop macOS/Linux Anda:

```bash
# 1. Install Android NDK (pilih salah satu)
brew install --cask android-ndk
# ATAU install via Android Studio: SDK Manager → SDK Tools → NDK (Side by side)

# 2. Set env var ANDROID_NDK_HOME (sesuaikan path NDK Anda)
export ANDROID_NDK_HOME="/opt/homebrew/share/android-ndk"
# Tambahkan baris di atas ke ~/.zshrc atau ~/.bashrc agar persisten.

# 3. Install cargo-ndk + Rust target aarch64-linux-android
cargo install cargo-ndk
rustup target add aarch64-linux-android
```

#### Langkah A2 — Cross-Compile & Package di Laptop Dev (Setiap Kali Build)

```bash
cd /path/to/trade

# Cross-compile ke Android ARM64 (SQLite otomatis ter-bundle via `cc` crate + NDK clang)
cargo ndk -t arm64-v8a build --release
# Hasil: target/aarch64-linux-android/release/trade-bot

# Package: binary ARM64 otomatis ter-copy ke dist/android-termux/trade-bot
./target/release/trade-bot package-app

# Zip folder self-contained
cd dist && zip -r android-termux.zip android-termux
```

Isi `dist/android-termux/` setelah langkah di atas:
- `trade-bot` — Binary ARM64 Android ELF (pre-built, siap jalan).
- `setup-termux.sh`, `start-daemon-android.sh`, `start-gui-android.sh`, `stop-daemon-android.sh` — launcher scripts.
- `config.json`, `.env.example`, `README-ANDROID.md`.

#### Langkah A3 — Transfer & Jalankan di HP Android

1. Kirim `android-termux.zip` ke HP (via USB, Telegram Saved Messages, Google Drive, dll).
2. Pastikan izin baterai **Termux** disetel ke **Unrestricted** (lihat **Langkah 2** di Opsi B).
3. Di Termux, unzip & masuk folder:
   ```bash
   termux-setup-storage          # (sekali saja, agar Termux bisa akses ~/storage/shared)
   cd ~/storage/shared/Download  # atau lokasi tempat Anda simpan zip
   unzip android-termux.zip -d ~/android-termux
   cd ~/android-termux
   ```
4. Jalankan setup (otomatis deteksi `./trade-bot` dan **skip build**, hanya install `termux-api` untuk wake-lock):
   ```bash
   bash setup-termux.sh
   ```
5. Konfigurasi `.env` (lihat **Langkah 4** di Opsi B).
6. Jalankan daemon 24 jam:
   ```bash
   ./start-daemon-android.sh
   ```

---

### Opsi B (Fallback): Build On-Device di Termux

Gunakan opsi ini jika laptop dev Anda tidak punya NDK, atau Anda ingin build langsung di HP.

### Langkah 1 — Install Aplikasi Termux & Termux:API
1. Unduh dan instal aplikasi **Termux** serta **Termux:API** dari **F-Droid** atau halaman GitHub Releases resmi Termux (`https://github.com/termux/termux-app/releases`).
   > **Catatan:** Jangan menginstal versi Termux lama dari Google Play Store karena repositori paketnya sudah dihentikan; gunakan versi dari F-Droid atau GitHub Releases.

### Langkah 2 — Matikan Optimasi Baterai Android untuk Termux (Wajib agar Jalan 24 Jam)
Agar sistem operasi Android tidak membekukan Termux saat layar HP mati:
1. Buka **Settings (Pengaturan)** di HP Android Anda.
2. Masuk ke **Apps (Aplikasi) → Termux → Battery (Baterai)**.
3. Pilih **Unrestricted (Tidak Dibatasi / Jangan Optimalkan)**.
4. Lakukan hal yang sama untuk aplikasi **Termux:API**.

### Langkah 3 — Siapkan Paket & Build `trade-bot` di Termux
Buka aplikasi **Termux** di HP Android Anda, lalu jalankan perintah berikut:

```bash
# 1. Update package manager & instal Rust, Clang, Git, dan Termux-API
pkg update -y && pkg upgrade -y
pkg install -y rust clang make git pkg-config termux-api

# 2. Masuk ke folder project trade-bot (bisa melalui git clone atau menyalin folder project)
# Contoh jika menyalin dari penyimpanan internal HP:
# termux-setup-storage
# cp -r ~/storage/shared/Download/trade ~/trade
cd ~/trade

# 3. Build binary release (SQLite otomatis ter-bundle di dalam binary)
cargo build --release
```

Atau jika Anda sudah menyalin hasil `./target/release/trade-bot package-app` (`dist/android-termux/`), cukup jalankan:
```bash
bash setup-termux.sh
```

### Langkah 4 — Konfigurasi `.env` & Deteksi ID Telegram Otomatis
1. Salin file `.env.example` menjadi `.env`:
   ```bash
   cp .env.example .env
   nano .env
   ```
2. Isi `TELEGRAM_BOT_TOKEN` Anda, lalu simpan (`Ctrl+O`, `Enter`, `Ctrl+X`).
3. Kirim pesan `/start` ke bot Anda dari Telegram pribadi (dan `/status` di Grup Telegram Anda), lalu jalankan deteksi ID otomatis:
   ```bash
   ./target/release/trade-bot telegram-whoami
   ```
4. Salin baris `TELEGRAM_OWNER_ID=...` dan `TELEGRAM_CHAT_ID=...` yang muncul ke dalam file `.env`.

### Langkah 5 — Menjalankan Background Worker 24 Jam dengan Wake-Lock
Agar CPU HP Android tetap aktif menjalankan siklus bot meskipun layar HP dimatikan:

```bash
# Aktifkan Wake-Lock (muncul notifikasi Termux "Wake lock held")
termux-wake-lock

# Jalankan daemon di background
./target/release/trade-bot start --daemon

# Cek status & live monitor di layar Termux
./target/release/trade-bot status
./target/release/trade-bot monitor
```

Atau gunakan skrip otomatis dari `dist/android-termux/`:
```bash
./start-daemon-android.sh
```

Untuk menghentikan daemon dan melepas wake-lock:
```bash
./stop-daemon-android.sh
```

---

## Bagian 2: Membuka & Menginstal Aplikasi GUI di Layar Utama Android & iPhone (PWA)

Selain dikontrol lewat **Telegram Bot**, `trade-bot` memiliki **Web App GUI bawaan** yang mendukung mode **Full-Screen Standalone App (PWA)** di Android dan iPhone.

### A. Jika Bot Jalan di Dalam HP Android (Termux)
1. Di Termux, jalankan:
   ```bash
   ./target/release/trade-bot gui --host 0.0.0.0 --port 8787 --no-open
   ```
   *(Atau jalankan `./start-gui-android.sh`)*
2. Buka browser **Chrome** di HP Android Anda ke alamat:
   ```text
   http://127.0.0.1:8787
   ```
3. Tekan ikon **Menu (⋮)** di pojok kanan atas Chrome → pilih **"Add to Home screen" (Tambahkan ke Layar Utama)**.

### B. Jika Bot Jalan di Mac / PC Windows dan Diakses dari iPhone atau Android (Wi-Fi Sama)
1. Di Mac atau PC Windows tempat bot berjalan, jalankan:
   ```bash
   ./target/release/trade-bot gui --host 0.0.0.0 --port 8787
   ```
2. Cek alamat IP Wi-Fi Mac/PC Anda (misal di Mac: `ipconfig getifaddr en0`, contoh hasilnya `192.168.1.25`).
3. **Di iPhone (Safari):**
   - Buka `http://192.168.1.25:8787` di **Safari**.
   - Tekan tombol **Share (Bagikan)** di bagian bawah Safari → pilih **"Add to Home Screen" (Tambahkan ke Layar Utama)** → tekan **Add**.
   - Ikon **TradeBot** akan muncul di Home Screen iPhone Anda dan berjalan **Full-Screen seperti aplikasi iOS native** (berkat meta tag `apple-mobile-web-app-capable`).
4. **Saat Berada di Luar Rumah (4G/5G):**
   - Gunakan aplikasi **Telegram** di iPhone/Android Anda untuk memantau dan mengontrol bot dari mana saja (`/status`, `/positions`, `/query <pertanyaan>`, `/approve`, `/reject`, `/panic`).

# windows-notch

Notch ala Dynamic Island untuk Windows 11, ditulis dengan Rust. Menampilkan lagu yang sedang diputar
(Spotify, Apple Music, YouTube di browser, dll.) beserta lirik tersinkron.

## Cara kerja

| Bagian | Teknologi |
|---|---|
| Info lagu & kontrol (play/pause/next/prev) | Windows SMTC (`GlobalSystemMediaTransportControlsSessionManager`) lewat crate `windows` |
| Lirik tersinkron (LRC) | [LRCLIB](https://lrclib.net), gratis tanpa API key |
| UI | `eframe` / `egui` (jendela transparan, tanpa border, always-on-top) |

Karena memakai SMTC, tidak perlu login atau API key Spotify / Apple Music.

## Build & jalankan

Butuh [Rust](https://rustup.rs) (toolchain MSVC) di Windows 10/11.

```powershell
cargo run --release
```

Hasil build ada di `target\release\windows-notch.exe` (tanpa jendela konsol).

## Penggunaan

- **Idle**: pil kecil di tengah atas layar.
- **Lagu diputar**: pil menampilkan baris lirik aktif (atau judul - artis bila lirik tidak ada).
- **Arahkan mouse** atau **ganti lagu**: notch melebar menampilkan judul, artis, lirik + baris berikutnya,
  progress, dan tombol kontrol. Tombol x menutup aplikasi.

## Konfigurasi cepat (`src/main.rs`)

- `LYRIC_LEAD_MS`: geser waktu lirik (naikkan jika lirik terasa telat).
- `COLLAPSED`, `EXPANDED`, `TOP_MARGIN`: ukuran dan posisi.

## Catatan & batasan

- Lirik dicari berdasarkan judul + artis + album + durasi. Lagu yang tidak ada di LRCLIB akan
  menampilkan "Lirik tidak ditemukan".
- Font sistem (Segoe UI, YaHei, Yu Gothic, Malgun) dimuat otomatis agar lirik CJK tampil benar.
- Jendela always-on-top tidak muncul di atas aplikasi fullscreen eksklusif.
- Progress/lirik mengandalkan timeline yang dilaporkan aplikasi musik; sebagian aplikasi melaporkannya
  kurang akurat.

## Ide pengembangan

Seek lewat klik progress bar (`TryChangePlaybackPosition`), cover album (`props.Thumbnail()`),
autostart saat login, dan tema warna dari cover.

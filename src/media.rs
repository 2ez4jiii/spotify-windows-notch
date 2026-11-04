//! Membaca "now playing" dari Windows System Media Transport Controls (SMTC).
//! Spotify, Apple Music, browser, dll. semuanya terdaftar di sini, jadi tidak
//! perlu API khusus per aplikasi.
//!
//! Di non-Windows disediakan stub dengan lagu demo supaya UI bisa di-compile/diuji.

use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

/// Artwork album yang sudah didecode ke RGBA. `id` naik setiap gambar baru,
/// supaya UI tahu kapan harus membuat ulang texture.
#[derive(Clone, Debug)]
pub struct Artwork {
    pub id: u64,
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
    /// Warna dominan cover (RGB), dipakai UI untuk menyesuaikan warna latar.
    pub accent: [u8; 3],
}

/// Cari warna dominan: piksel dikelompokkan ke 512 "ember" warna; piksel yang
/// hampir hitam / hampir putih dilewati, dan warna jenuh diberi bobot lebih
/// supaya hasilnya tidak berupa cokelat keruh dari rata-rata semua warna.
pub fn dominant_color(rgba: &[u8]) -> [u8; 3] {
    let mut weight = [0f32; 512];
    let mut sum = [[0f32; 3]; 512];
    for px in rgba.chunks_exact(4).step_by(3) {
        if px[3] < 128 {
            continue;
        }
        let (r, g, b) = (px[0] as f32, px[1] as f32, px[2] as f32);
        let mx = r.max(g).max(b);
        let mn = r.min(g).min(b);
        let v = mx / 255.0;
        let s = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
        if v < 0.12 || (v > 0.92 && s < 0.12) {
            continue;
        }
        let w = 0.15 + s;
        let i = (((px[0] >> 5) as usize) << 6) | (((px[1] >> 5) as usize) << 3) | (px[2] >> 5) as usize;
        weight[i] += w;
        sum[i][0] += r * w;
        sum[i][1] += g * w;
        sum[i][2] += b * w;
    }
    let best = weight
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);
    if weight[best] <= 0.0 {
        return [24, 24, 24]; // cover hampir hitam/putih/abu: pakai abu gelap
    }
    let w = weight[best];
    [
        (sum[best][0] / w) as u8,
        (sum[best][1] / w) as u8,
        (sum[best][2] / w) as u8,
    ]
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub track: Option<Track>,
    pub playing: bool,
    /// Posisi (ms) pada saat `sampled_at`.
    pub position_ms: u64,
    pub sampled_at: Option<Instant>,
    pub artwork: Option<Arc<Artwork>>,
    #[allow(dead_code)]
    pub source: String,
}

impl Snapshot {
    /// Posisi saat ini, diinterpolasi sejak sampel terakhir.
    pub fn position_now_ms(&self) -> u64 {
        let mut p = self.position_ms;
        if self.playing {
            if let Some(t) = self.sampled_at {
                p += t.elapsed().as_millis() as u64;
            }
        }
        match &self.track {
            Some(t) if t.duration_ms > 0 => p.min(t.duration_ms),
            _ => p,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Command {
    PlayPause,
    Next,
    Prev,
}

#[cfg(windows)]
mod sys {
    use super::*;
    use std::sync::{mpsc::Receiver, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use windows::core::Interface;
    use windows::Foundation::IAsyncOperation;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionMediaProperties as Props,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    };
    use windows::Storage::Streams::{DataReader, IRandomAccessStream};

    /// Cache artwork per lagu.
    #[derive(Default)]
    struct ArtState {
        key: String,
        art: Option<Arc<Artwork>>,
        attempts: u32,
        next_id: u64,
    }

    /// 100-ns tick -> ms
    fn ticks_to_ms(t: i64) -> u64 {
        (t.max(0) / 10_000) as u64
    }

    /// Waktu sekarang dalam format Windows FILETIME (100ns sejak 1601-01-01).
    fn now_filetime() -> i64 {
        let unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        (unix.as_nanos() / 100) as i64 + 116_444_736_000_000_000
    }

    /// Baca thumbnail dari SMTC lalu decode (JPEG/PNG) dan kecilkan ke maks. 256px.
    fn load_artwork(props: &Props) -> Option<(usize, usize, Vec<u8>)> {
        let reference = props.Thumbnail().ok()?;
        let stream: IRandomAccessStream =
            reference.OpenReadAsync().ok()?.get().ok()?.cast().ok()?;
        let size = stream.Size().ok()?;
        if size == 0 || size > 8 * 1024 * 1024 {
            return None;
        }
        let input = stream.GetInputStreamAt(0).ok()?;
        let reader = DataReader::CreateDataReader(&input).ok()?;
        let load: IAsyncOperation<u32> = reader.LoadAsync(size as u32).ok()?.cast().ok()?;
        load.get().ok()?;
        let mut buf = vec![0u8; size as usize];
        reader.ReadBytes(&mut buf).ok()?;

        let img = image::load_from_memory(&buf).ok()?.thumbnail(256, 256).to_rgba8();
        let (w, h) = img.dimensions();
        Some((w as usize, h as usize, img.into_raw()))
    }

    fn read(manager: &Manager, art: &mut ArtState) -> windows::core::Result<Snapshot> {
        let session = manager.GetCurrentSession()?;
        let props = session.TryGetMediaPropertiesAsync()?.get()?;
        let timeline = session.GetTimelineProperties()?;
        let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;

        let start = ticks_to_ms(timeline.StartTime()?.Duration);
        let end = ticks_to_ms(timeline.EndTime()?.Duration);
        let mut position = ticks_to_ms(timeline.Position()?.Duration).saturating_sub(start);

        // Timeline hanya diperbarui saat ada perubahan; kompensasi selisih waktu.
        if playing {
            let since = now_filetime() - timeline.LastUpdatedTime()?.UniversalTime;
            position += ticks_to_ms(since);
        }
        let duration = end.saturating_sub(start);
        if duration > 0 {
            position = position.min(duration);
        }

        let title = props.Title()?.to_string();
        let mut artwork = None;
        let track = if title.is_empty() {
            None
        } else {
            let t = Track {
                title,
                artist: props.Artist()?.to_string(),
                album: props.AlbumTitle()?.to_string(),
                duration_ms: duration,
            };

            // Lagu baru -> reset cache. Thumbnail kadang datang terlambat, jadi
            // dicoba ulang beberapa kali, dan sekali lagi di percobaan ke-8
            // untuk menimpa gambar lagu sebelumnya yang mungkin masih nyangkut.
            let key = format!("{}\u{1}{}\u{1}{}", t.artist, t.title, t.album);
            if key != art.key {
                art.key = key;
                art.art = None;
                art.attempts = 0;
            }
            if art.attempts < 12 {
                art.attempts += 1;
                if art.art.is_none() || art.attempts == 8 {
                    if let Some((w, h, rgba)) = load_artwork(&props) {
                        art.next_id += 1;
                        let accent = dominant_color(&rgba);
                        art.art = Some(Arc::new(Artwork {
                            id: art.next_id,
                            size: [w, h],
                            rgba,
                            accent,
                        }));
                    }
                }
            }
            artwork = art.art.clone();
            Some(t)
        };

        Ok(Snapshot {
            track,
            playing,
            position_ms: position,
            sampled_at: Some(Instant::now()),
            artwork,
            source: session.SourceAppUserModelId()?.to_string(),
        })
    }

    /// Jalankan thread polling. `on_update` dipanggil tiap snapshot baru (untuk repaint UI).
    pub fn spawn(
        state: Arc<Mutex<Snapshot>>,
        commands: Receiver<Command>,
        on_update: impl Fn() + Send + 'static,
    ) {
        thread::spawn(move || {
            let manager = loop {
                match Manager::RequestAsync().and_then(|op| op.get()) {
                    Ok(m) => break m,
                    Err(_) => thread::sleep(Duration::from_secs(2)),
                }
            };
            let mut art = ArtState::default();

            loop {
                while let Ok(cmd) = commands.try_recv() {
                    if let Ok(session) = manager.GetCurrentSession() {
                        let _ = match cmd {
                            Command::PlayPause => session.TryTogglePlayPauseAsync().map(|_| ()),
                            Command::Next => session.TrySkipNextAsync().map(|_| ()),
                            Command::Prev => session.TrySkipPreviousAsync().map(|_| ()),
                        };
                    }
                }

                // Tidak ada sesi aktif -> snapshot kosong.
                let snap = read(&manager, &mut art).unwrap_or_default();
                if let Ok(mut s) = state.lock() {
                    *s = snap;
                }
                on_update();
                thread::sleep(Duration::from_millis(250));
            }
        });
    }
}

#[cfg(not(windows))]
mod sys {
    use super::*;
    use std::sync::{mpsc::Receiver, Mutex};
    use std::thread;
    use std::time::Duration;

    pub fn spawn(
        state: Arc<Mutex<Snapshot>>,
        commands: Receiver<Command>,
        on_update: impl Fn() + Send + 'static,
    ) {
        thread::spawn(move || {
            // Gradien demo sebagai pengganti cover album.
            let mut rgba = Vec::with_capacity(64 * 64 * 4);
            for y in 0..64u32 {
                for x in 0..64u32 {
                    rgba.extend_from_slice(&[(x * 4) as u8, (y * 4) as u8, 180, 255]);
                }
            }
            let mut snap = Snapshot {
                track: Some(Track {
                    title: "Demo Song".into(),
                    artist: "Demo Artist".into(),
                    album: "Demo Album".into(),
                    duration_ms: 200_000,
                }),
                playing: true,
                position_ms: 0,
                sampled_at: Some(Instant::now()),
                artwork: Some(Arc::new(Artwork {
                    id: 1,
                    size: [64, 64],
                    accent: dominant_color(&rgba),
                    rgba,
                })),
                source: "demo".into(),
            };
            loop {
                while let Ok(cmd) = commands.try_recv() {
                    if let Command::PlayPause = cmd {
                        snap.position_ms = snap.position_now_ms();
                        snap.sampled_at = Some(Instant::now());
                        snap.playing = !snap.playing;
                    }
                }
                if let Ok(mut s) = state.lock() {
                    *s = snap.clone();
                }
                on_update();
                thread::sleep(Duration::from_millis(250));
            }
        });
    }
}

pub use sys::spawn;
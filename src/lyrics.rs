//! Lirik tersinkron (format LRC) dari LRCLIB (https://lrclib.net) - gratis, tanpa API key.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

const API: &str = "https://lrclib.net/api";
const USER_AGENT: &str = "windows-notch/0.1 (Dynamic Island style notch)";

#[derive(Clone, Debug)]
pub struct Line {
    pub at_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug)]
pub enum Lyrics {
    Idle,
    Loading,
    Synced(Vec<Line>),
    /// Lirik ada, tapi tanpa timestamp.
    Plain,
    Instrumental,
    NotFound,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    synced_lyrics: Option<String>,
    plain_lyrics: Option<String>,
    instrumental: Option<bool>,
    duration: Option<f64>,
}

/// "mm:ss.xx" -> milidetik. Mengembalikan None untuk tag metadata ([ar:...], [ti:...]).
fn parse_stamp(s: &str) -> Option<u64> {
    let (m, sec) = s.split_once(':')?;
    let m: u64 = m.trim().parse().ok()?;
    let sec: f64 = sec.trim().parse().ok()?;
    Some((m as f64 * 60_000.0 + sec * 1000.0).round() as u64)
}

pub fn parse_lrc(src: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in src.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        let mut skip = false;
        while let Some(s) = rest.strip_prefix('[') {
            let Some(end) = s.find(']') else { break };
            match parse_stamp(&s[..end]) {
                Some(ms) => stamps.push(ms),
                None => {
                    skip = true;
                    break;
                }
            }
            rest = &s[end + 1..];
        }
        if skip {
            continue;
        }
        let text = rest.trim();
        for at_ms in stamps {
            out.push(Line { at_ms, text: text.to_string() });
        }
    }
    out.sort_by_key(|l| l.at_ms);
    out
}

fn classify(item: &Item) -> Option<Lyrics> {
    if let Some(s) = item.synced_lyrics.as_deref().filter(|s| !s.trim().is_empty()) {
        let lines = parse_lrc(s);
        if !lines.is_empty() {
            return Some(Lyrics::Synced(lines));
        }
    }
    if item.instrumental == Some(true) {
        return Some(Lyrics::Instrumental);
    }
    if item.plain_lyrics.as_deref().is_some_and(|s| !s.trim().is_empty()) {
        return Some(Lyrics::Plain);
    }
    None
}

fn fetch(title: &str, artist: &str, album: &str, duration_ms: u64) -> Lyrics {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .user_agent(USER_AGENT)
        .build();
    let dur_s = (duration_ms as f64 / 1000.0).round() as u64;

    // 1) /get - cocok persis. Coba beberapa kombinasi karena LRCLIB ketat soal album & durasi.
    let attempts = [(true, true), (false, true), (false, false)];
    let mut fallback: Option<Lyrics> = None;
    for (with_album, with_dur) in attempts {
        if with_album && album.is_empty() || with_dur && dur_s == 0 {
            continue;
        }
        let mut req = agent
            .get(&format!("{API}/get"))
            .query("track_name", title)
            .query("artist_name", artist);
        if with_album {
            req = req.query("album_name", album);
        }
        if with_dur {
            req = req.query("duration", &dur_s.to_string());
        }
        if let Ok(resp) = req.call() {
            if let Ok(item) = resp.into_json::<Item>() {
                match classify(&item) {
                    Some(l @ Lyrics::Synced(_)) => return l,
                    Some(other) => fallback = fallback.or(Some(other)),
                    None => {}
                }
            }
        }
    }

    // 2) /search - ambil hasil bertimestamp dengan durasi paling dekat.
    if let Ok(resp) = agent
        .get(&format!("{API}/search"))
        .query("track_name", title)
        .query("artist_name", artist)
        .call()
    {
        if let Ok(items) = resp.into_json::<Vec<Item>>() {
            let target = duration_ms as f64 / 1000.0;
            let best = items
                .iter()
                .filter(|i| i.synced_lyrics.as_deref().is_some_and(|s| !s.trim().is_empty()))
                .min_by(|a, b| {
                    let da = (a.duration.unwrap_or(f64::MAX) - target).abs();
                    let db = (b.duration.unwrap_or(f64::MAX) - target).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                });
            if let Some(l) = best.and_then(classify) {
                return l;
            }
        }
    }

    fallback.unwrap_or(Lyrics::NotFound)
}

struct Inner {
    key: String,
    current: Lyrics,
    cache: HashMap<String, Lyrics>,
}

/// Menyimpan lirik lagu aktif + cache; pengambilan dilakukan di thread terpisah.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Mutex<Inner>>,
    on_ready: Arc<dyn Fn() + Send + Sync>,
}

impl Store {
    pub fn new(on_ready: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                key: String::new(),
                current: Lyrics::Idle,
                cache: HashMap::new(),
            })),
            on_ready: Arc::new(on_ready),
        }
    }

    pub fn request(&self, key: &str, title: &str, artist: &str, album: &str, duration_ms: u64) {
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if g.key == key {
                return;
            }
            g.key = key.to_string();
            if let Some(hit) = g.cache.get(key).cloned() {
                g.current = hit;
                return;
            }
            g.current = Lyrics::Loading;
        }

        let this = self.clone();
        let (key, title, artist, album) =
            (key.to_string(), title.to_string(), artist.to_string(), album.to_string());
        thread::spawn(move || {
            let result = fetch(&title, &artist, &album, duration_ms);
            {
                let mut g = this.inner.lock().unwrap_or_else(|e| e.into_inner());
                g.cache.insert(key.clone(), result.clone());
                if g.key == key {
                    g.current = result;
                }
            }
            (this.on_ready)();
        });
    }

    pub fn with<R>(&self, f: impl FnOnce(&Lyrics) -> R) -> R {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&g.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc() {
        let src = "[ar:Someone]\n[00:01.50]Halo\n[00:03.00][00:10.25]Dunia\n[00:05.00]\n";
        let l = parse_lrc(src);
        assert_eq!(l.len(), 4);
        assert_eq!((l[0].at_ms, l[0].text.as_str()), (1500, "Halo"));
        assert_eq!((l[1].at_ms, l[1].text.as_str()), (3000, "Dunia"));
        assert_eq!(l[2].text, "");
        assert_eq!(l[3].at_ms, 10250);
    }
}

// Sembunyikan jendela konsol pada build release di Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod lyrics;
mod media;

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, CursorIcon, FontFamily, FontId, Galley, Painter, Pos2, Rect, Sense, Shape,
    Stroke, Vec2,
};
use lyrics::Lyrics;
use media::{Command, Snapshot};

const IDLE: Vec2 = Vec2::new(150.0, 32.0);
const COLLAPSED: Vec2 = Vec2::new(320.0, 44.0);
const EXPANDED: Vec2 = Vec2::new(440.0, 190.0);
const TOP_MARGIN: f32 = 8.0;
/// Tampilkan baris lirik sedikit lebih awal agar terasa pas dengan suara.
const LYRIC_LEAD_MS: u64 = 250;
const GREEN: Color32 = Color32::from_rgb(30, 215, 96);

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(IDLE)
            .with_position(Pos2::new(800.0, TOP_MARGIN))
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "windows-notch",
        options,
        Box::new(|cc| Ok(Box::new(Notch::new(cc)))),
    )
}

// ---------------------------------------------------------------------------
// Pengaturan (disimpan di %APPDATA%\windows-notch\settings.txt)
// ---------------------------------------------------------------------------

fn settings_path() -> Option<PathBuf> {
    let dir = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(dir).join("windows-notch").join("settings.txt"))
}

/// Apakah notch terakhir kali dikunci?
fn load_locked() -> bool {
    settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|txt| txt.lines().any(|l| l.trim() == "locked=true"))
}

fn save_locked(locked: bool) {
    if let Some(path) = settings_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format!("locked={locked}\n"));
    }
}

/// Tambahkan font sistem Windows sebagai fallback (CJK, simbol) supaya lirik
/// Jepang/Korea/Mandarin tidak tampil sebagai kotak kosong.
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let candidates = ["segoeui.ttf", "msyh.ttc", "YuGothR.ttc", "malgun.ttf", "seguisym.ttf"];
    for (i, file) in candidates.iter().enumerate() {
        let Ok(bytes) = std::fs::read(format!("{dir}\\Fonts\\{file}")) else { continue };
        let name = format!("sys-{file}");
        fonts.font_data.insert(name.clone(), egui::FontData::from_owned(bytes));
        let family = fonts.families.entry(FontFamily::Proportional).or_default();
        if i == 0 {
            family.insert(0, name); // Segoe UI sebagai font utama
        } else {
            family.push(name);
        }
    }
    ctx.set_fonts(fonts);
}

struct LyricView {
    current: String,
    next: String,
    synced: bool,
}

struct Notch {
    state: Arc<Mutex<Snapshot>>,
    tx: mpsc::Sender<Command>,
    lyrics: lyrics::Store,
    size: Vec2,
    sent: Vec2,
    placed: bool,
    last_key: String,
    pop_until: Instant,
    hover_until: Instant,
    art_tex: Option<egui::TextureHandle>,
    art_id: u64,
    /// Terkunci = tetap expand walau mouse pergi.
    locked: bool,
    /// Warna latar saat ini (dianimasikan menuju warna artwork).
    bg: [f32; 3],
}

impl Notch {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        setup_fonts(&ctx);

        let state = Arc::new(Mutex::new(Snapshot::default()));
        let (tx, rx) = mpsc::channel();

        let c1 = ctx.clone();
        media::spawn(state.clone(), rx, move || c1.request_repaint());
        let c2 = ctx.clone();
        let lyrics = lyrics::Store::new(move || c2.request_repaint());

        let locked = load_locked();
        let now = Instant::now();
        Self {
            state,
            tx,
            lyrics,
            size: IDLE,
            sent: Vec2::ZERO,
            placed: false,
            last_key: String::new(),
            pop_until: now,
            hover_until: now,
            art_tex: None,
            art_id: 0,
            locked,
            bg: [0.0; 3],
        }
    }

    fn lyric_view(&self, pos_ms: u64) -> LyricView {
        self.lyrics.with(|l| match l {
            Lyrics::Synced(lines) => {
                let idx = lines.partition_point(|x| x.at_ms <= pos_ms + LYRIC_LEAD_MS);
                let text = |i: usize| lines.get(i).map(|x| x.text.clone()).unwrap_or_default();
                let nonempty = |s: String| if s.is_empty() { "♪".to_string() } else { s };
                if idx == 0 {
                    LyricView { current: "♪".into(), next: text(0), synced: true }
                } else {
                    LyricView { current: nonempty(text(idx - 1)), next: text(idx), synced: true }
                }
            }
            Lyrics::Loading => msg("Memuat lirik…"),
            Lyrics::NotFound => msg("Lirik tidak ditemukan"),
            Lyrics::Plain => msg("Lirik tidak tersinkron"),
            Lyrics::Instrumental => msg("♪ Instrumental ♪"),
            Lyrics::Idle => msg(""),
        })
    }
}

fn msg(s: &str) -> LyricView {
    LyricView { current: s.into(), next: String::new(), synced: false }
}

impl eframe::App for Notch {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let snap = self.state.lock().map(|s| s.clone()).unwrap_or_default();
        let has_track = snap.track.is_some();

        let monitor_w = ctx.input(|i| i.viewport().monitor_size).map(|m| m.x);

        // Sinkronkan texture artwork dengan snapshot (buat ulang hanya bila gambar berganti).
        match snap.artwork.as_ref() {
            Some(a) if a.id != self.art_id => {
                let img = egui::ColorImage::from_rgba_unmultiplied(a.size, &a.rgba);
                self.art_tex = Some(ctx.load_texture("artwork", img, egui::TextureOptions::LINEAR));
                self.art_id = a.id;
            }
            None if self.art_id != 0 => {
                self.art_tex = None;
                self.art_id = 0;
            }
            _ => {}
        }

        // Lagu berganti -> ambil lirik & lebarkan notch sebentar.
        if let Some(t) = &snap.track {
            let key = format!("{}\u{1}{}\u{1}{}", t.artist, t.title, t.album);
            if key != self.last_key {
                self.lyrics.request(&key, &t.title, &t.artist, &t.album, t.duration_ms);
                self.last_key = key;
                self.pop_until = now + Duration::from_secs(4);
            }
        } else {
            self.last_key.clear();
        }

        // Hover -> melebar. Sedikit jeda sebelum menciut agar tidak berkedip.
        if ctx.input(|i| i.pointer.hover_pos().is_some()) {
            self.hover_until = now + Duration::from_millis(350);
        }
        // Terkunci -> tetap expand; selain itu otomatis collapse.
        let expanded =
            has_track && (self.locked || now < self.hover_until || now < self.pop_until);

        // Animasi ukuran (eksponensial, terasa seperti Dynamic Island).
        let target = if expanded {
            EXPANDED
        } else if has_track {
            COLLAPSED
        } else {
            IDLE
        };
        let dt = ctx.input(|i| i.stable_dt).min(0.05);
        let diff = target - self.size;
        let animating = diff.length() > 0.4;
        if animating {
            self.size += diff * (1.0 - (-dt * 16.0).exp());
        } else {
            self.size = target;
        }

        // Warna latar mengikuti artwork (digelapkan agar teks putih tetap terbaca),
        // berpindah halus antar lagu. Tanpa artwork -> hitam.
        let accent = snap.artwork.as_ref().map(|a| a.accent);
        let target_bg = accent.map(tone).unwrap_or([0.0; 3]);
        let k = 1.0 - (-dt * 5.0).exp();
        let mut bg_animating = false;
        for i in 0..3 {
            let d = target_bg[i] - self.bg[i];
            if d.abs() > 0.5 {
                self.bg[i] += d * k;
                bg_animating = true;
            } else {
                self.bg[i] = target_bg[i];
            }
        }
        let bg_color = Color32::from_rgb(
            self.bg[0].round() as u8,
            self.bg[1].round() as u8,
            self.bg[2].round() as u8,
        );
        let accent_bright = accent.map(bright).unwrap_or(GREEN);

        // Ukuran jendela + posisi (selalu di tengah atas layar).
        if !self.placed || (self.size - self.sent).length() > 0.2 {
            let screen_w = monitor_w.unwrap_or(1920.0);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(self.size));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(Pos2::new(
                (screen_w - self.size.x) / 2.0,
                TOP_MARGIN,
            )));
            self.sent = self.size;
            self.placed = monitor_w.is_some();
        }

        // 0 = collapsed, 1 = expanded
        let p = ((self.size.y - COLLAPSED.y) / (EXPANDED.y - COLLAPSED.y)).clamp(0.0, 1.0);
        let pos_ms = snap.position_now_ms();
        let view = self.lyric_view(pos_ms);
        let time = ctx.input(|i| i.time);
        let tx = self.tx.clone();
        let art_tex = self.art_tex.clone();
        let ew = EXPANDED.x;
        let locked = self.locked;

        // Hasil interaksi dari dalam closure UI.
        let mut toggle_lock = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter().clone();
                painter.rect_filled(rect, (rect.height() / 2.0).min(28.0), bg_color);

                let Some(track) = &snap.track else {
                    draw_eq(&painter, Pos2::new(rect.min.x + 20.0, rect.center().y), false, time, GREEN);
                    let g = fit(ctx, "Tidak ada musik", FontId::proportional(12.0), Color32::from_gray(150), rect.width() - 46.0);
                    painter.galley(Pos2::new(rect.min.x + 36.0, rect.center().y - g.size().y / 2.0), g, Color32::WHITE);
                    return;
                };

                // ---------- Collapsed: [artwork] [lirik] ----------
                if p < 0.6 {
                    let a = 1.0 - p / 0.6;
                    let art_rect = Rect::from_center_size(
                        Pos2::new(rect.min.x + 9.0 + 15.0, rect.center().y),
                        Vec2::splat(30.0),
                    );
                    draw_art(ui, art_tex.as_ref(), art_rect, 8.0, a);

                    let text = if view.synced && !view.current.is_empty() {
                        view.current.clone()
                    } else {
                        format!("{} · {}", track.title, track.artist)
                    };
                    let tx0 = art_rect.max.x + 11.0;
                    let g = fit(ctx, &text, FontId::proportional(13.5), Color32::WHITE.gamma_multiply(a), rect.max.x - tx0 - 18.0);
                    painter.galley(Pos2::new(tx0, rect.center().y - g.size().y / 2.0), g, Color32::WHITE);
                }

                // ---------- Expanded: artwork kiri, info + lirik kanan ----------
                if p > 0.4 {
                    let a = ((p - 0.4) / 0.6).clamp(0.0, 1.0);
                    let white = Color32::WHITE.gamma_multiply(a);
                    let grey = Color32::from_gray(170).gamma_multiply(a);
                    let dim = Color32::from_gray(120).gamma_multiply(a);
                    let interactive = p > 0.95;

                    // Tata letak relatif ke tengah-atas, agar tidak "bergeser" saat animasi.
                    let base = Pos2::new(rect.center().x - ew / 2.0, rect.min.y);
                    let x0 = base.x + 20.0;
                    let inner_w = ew - 40.0;

                    // Artwork (kiri)
                    let art_rect = Rect::from_min_size(Pos2::new(x0, base.y + 18.0), Vec2::splat(100.0));
                    draw_art(ui, art_tex.as_ref(), art_rect, 14.0, a);

                    // Kolom kanan: judul, artis, lirik
                    let rx = art_rect.max.x + 18.0;
                    let rw = base.x + ew - 20.0 - rx;

                    let g = fit(ctx, &track.title, FontId::proportional(15.0), white, rw - 62.0);
                    painter.galley(Pos2::new(rx, base.y + 18.0), g, Color32::WHITE);
                    let g = fit(ctx, &track.artist, FontId::proportional(12.0), grey, rw - 62.0);
                    painter.galley(Pos2::new(rx, base.y + 38.0), g, Color32::WHITE);

                    // Tombol tutup
                    let close = Rect::from_center_size(Pos2::new(base.x + ew - 30.0, base.y + 28.0), Vec2::splat(22.0));
                    let close_resp = interactive.then(|| {
                        ui.interact(close, egui::Id::new("close"), Sense::click())
                            .on_hover_cursor(CursorIcon::PointingHand)
                    });
                    let (cc, s) = (close.center(), 4.5);
                    let col = if close_resp.as_ref().is_some_and(|r| r.hovered()) { white } else { dim };
                    painter.line_segment([cc + Vec2::new(-s, -s), cc + Vec2::new(s, s)], Stroke::new(1.6, col));
                    painter.line_segment([cc + Vec2::new(-s, s), cc + Vec2::new(s, -s)], Stroke::new(1.6, col));
                    if close_resp.is_some_and(|r| r.clicked()) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }

                    // Tombol gembok: kunci agar tetap expand
                    let lock = Rect::from_center_size(Pos2::new(base.x + ew - 58.0, base.y + 28.0), Vec2::splat(22.0));
                    let lock_resp = interactive.then(|| {
                        ui.interact(lock, egui::Id::new("lock"), Sense::click())
                            .on_hover_cursor(CursorIcon::PointingHand)
                    });
                    let lcol = if locked {
                        accent_bright.gamma_multiply(a)
                    } else if lock_resp.as_ref().is_some_and(|r| r.hovered()) {
                        white
                    } else {
                        dim
                    };
                    draw_lock(&painter, lock.center(), locked, lcol);
                    if lock_resp.is_some_and(|r| r.clicked()) {
                        toggle_lock = true;
                    }

                    // Lirik aktif (membungkus maks. ~2 baris) + baris berikutnya
                    let clip = Rect::from_min_max(Pos2::new(rx - 2.0, base.y + 64.0), Pos2::new(rx + rw + 2.0, base.y + 112.0));
                    let lp = painter.with_clip_rect(clip.intersect(rect));
                    let cur = ctx.fonts(|f| f.layout(view.current.clone(), FontId::proportional(17.5), white, rw));
                    lp.galley(Pos2::new(rx, base.y + 66.0), cur, Color32::WHITE);
                    if !view.next.is_empty() {
                        let g = fit(ctx, &view.next, FontId::proportional(12.5), dim, rw);
                        painter.galley(Pos2::new(rx, base.y + 116.0), g, Color32::WHITE);
                    }

                    // Progress
                    let dur = track.duration_ms;
                    let frac = if dur > 0 { (pos_ms as f32 / dur as f32).clamp(0.0, 1.0) } else { 0.0 };
                    let cy = base.y + 146.0;
                    let g = ctx.fonts(|f| f.layout_no_wrap(fmt_time(pos_ms), FontId::proportional(10.5), grey));
                    painter.galley(Pos2::new(x0, cy - g.size().y / 2.0), g, Color32::WHITE);
                    let g = ctx.fonts(|f| f.layout_no_wrap(fmt_time(dur), FontId::proportional(10.5), grey));
                    painter.galley(Pos2::new(x0 + inner_w - g.size().x, cy - g.size().y / 2.0), g, Color32::WHITE);
                    let (bx0, bx1) = (x0 + 40.0, x0 + inner_w - 40.0);
                    let track_r = Rect::from_min_max(Pos2::new(bx0, cy - 1.5), Pos2::new(bx1, cy + 1.5));
                    painter.rect_filled(track_r, 1.5, Color32::from_gray(60).gamma_multiply(a));
                    let fill_r = Rect::from_min_max(track_r.min, Pos2::new(bx0 + (bx1 - bx0) * frac, cy + 1.5));
                    painter.rect_filled(fill_r, 1.5, white);

                    // Kontrol
                    let ccy = base.y + 170.0;
                    let ccx = base.x + ew / 2.0;
                    for (i, (id, cmd)) in [("prev", Command::Prev), ("play", Command::PlayPause), ("next", Command::Next)].into_iter().enumerate() {
                        let c = Pos2::new(ccx + (i as f32 - 1.0) * 52.0, ccy);
                        let r = Rect::from_center_size(c, Vec2::splat(30.0));
                        let resp = if interactive {
                            Some(ui.interact(r, egui::Id::new(id), Sense::click()).on_hover_cursor(CursorIcon::PointingHand))
                        } else {
                            None
                        };
                        if resp.as_ref().is_some_and(|r| r.hovered()) {
                            painter.circle_filled(c, 15.0, Color32::from_gray(38));
                        }
                        match cmd {
                            Command::Prev => draw_skip(&painter, c, -1.0, white),
                            Command::Next => draw_skip(&painter, c, 1.0, white),
                            Command::PlayPause => draw_play_pause(&painter, c, snap.playing, white),
                        }
                        if resp.is_some_and(|r| r.clicked()) {
                            let _ = tx.send(cmd);
                        }
                    }
                }
            });

        if toggle_lock {
            self.locked = !self.locked;
            save_locked(self.locked);
        }

        // Jadwal repaint
        if animating || bg_animating {
            ctx.request_repaint();
        } else if expanded {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else if snap.playing {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}

// ---------------------------------------------------------------------------
// Helper gambar
// ---------------------------------------------------------------------------

/// Gambar artwork dengan sudut membulat; bila belum ada, tampilkan placeholder not musik.
fn draw_art(ui: &mut egui::Ui, tex: Option<&egui::TextureHandle>, rect: Rect, radius: f32, alpha: f32) {
    match tex {
        Some(t) => {
            ui.put(
                rect,
                egui::Image::new(t)
                    .fit_to_exact_size(rect.size())
                    .rounding(radius)
                    .tint(Color32::WHITE.gamma_multiply(alpha)),
            );
        }
        None => {
            let painter = ui.painter();
            painter.rect_filled(rect, radius, Color32::from_gray(38).gamma_multiply(alpha));
            let g = painter.layout_no_wrap(
                "♪".to_string(),
                FontId::proportional(rect.height() * 0.45),
                Color32::from_gray(140).gamma_multiply(alpha),
            );
            painter.galley(rect.center() - g.size() / 2.0, g, Color32::WHITE);
        }
    }
}

/// Gelapkan warna cover agar teks putih tetap terbaca; cover yang sangat gelap
/// (tapi bukan hitam) dicerahkan sedikit supaya warnanya masih terlihat.
fn tone(c: [u8; 3]) -> [f32; 3] {
    let [r, g, b] = c.map(|v| v as f32);
    let lum = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0;
    let f = if lum > 0.30 {
        0.30 / lum
    } else if lum > 0.01 && lum < 0.10 {
        (0.10 / lum).min(2.5)
    } else {
        1.0
    };
    [(r * f).min(255.0), (g * f).min(255.0), (b * f).min(255.0)]
}

/// Versi terang warna cover (campur 50% putih), untuk aksen kecil seperti ikon gembok.
fn bright(c: [u8; 3]) -> Color32 {
    let m = |v: u8| ((v as f32 + 255.0) / 2.0) as u8;
    Color32::from_rgb(m(c[0]), m(c[1]), m(c[2]))
}

/// Ikon gembok: tertutup bila `locked`, terbuka (gelang terangkat) bila tidak.
fn draw_lock(p: &Painter, c: Pos2, locked: bool, col: Color32) {
    let body = Rect::from_center_size(Pos2::new(c.x, c.y + 3.0), Vec2::new(11.0, 8.5));
    p.rect_filled(body, 2.0, col);

    let r = 3.4;
    let dy = if locked { 0.0 } else { -2.5 };
    let arc_cy = c.y - 3.5 + dy;
    let body_top = body.min.y;

    let mut pts = vec![Pos2::new(c.x - r, body_top)];
    pts.push(Pos2::new(c.x - r, arc_cy));
    for i in 0..=8 {
        let th = std::f32::consts::PI * (1.0 + i as f32 / 8.0);
        pts.push(Pos2::new(c.x + r * th.cos(), arc_cy + r * th.sin()));
    }
    // Kaki kanan: menyambung ke badan saat terkunci, menggantung saat terbuka.
    pts.push(Pos2::new(c.x + r, if locked { body_top } else { arc_cy + 1.5 }));
    p.add(Shape::line(pts, Stroke::new(1.6, col)));
}

/// Layout teks satu baris; dipotong dengan "…" bila melebihi `max_w`.
fn fit(ctx: &egui::Context, text: &str, font: FontId, color: Color32, max_w: f32) -> Arc<Galley> {
    let layout = |s: String| ctx.fonts(|f| f.layout_no_wrap(s, font.clone(), color));
    let g = layout(text.to_string());
    if g.size().x <= max_w.max(1.0) {
        return g;
    }
    // Perkiraan awal proporsional, lalu rapikan satu per satu.
    let chars: Vec<char> = text.chars().collect();
    let mut n = ((chars.len() as f32) * max_w / g.size().x) as usize;
    n = n.min(chars.len().saturating_sub(1));
    loop {
        let s: String = chars[..n].iter().collect();
        let g = layout(format!("{}…", s.trim_end()));
        if g.size().x <= max_w || n == 0 {
            return g;
        }
        n -= 1;
    }
}

fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

fn draw_eq(p: &Painter, c: Pos2, playing: bool, t: f64, col: Color32) {
    for i in 0..4 {
        let h = if playing {
            4.0 + 10.0 * (0.5 + 0.5 * ((t * 6.5 + i as f64 * 1.9).sin() as f32))
        } else {
            3.0
        };
        let x = c.x + (i as f32 - 1.5) * 5.5;
        p.rect_filled(Rect::from_center_size(Pos2::new(x, c.y), Vec2::new(3.0, h)), 1.5, col);
    }
}

fn tri(p: &Painter, c: Pos2, s: f32, dir: f32, col: Color32) {
    let mut pts = vec![
        Pos2::new(c.x - s * 0.7 * dir, c.y - s),
        Pos2::new(c.x + s * 0.9 * dir, c.y),
        Pos2::new(c.x - s * 0.7 * dir, c.y + s),
    ];
    if dir < 0.0 {
        pts.reverse(); // jaga urutan titik tetap searah jarum jam
    }
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn draw_skip(p: &Painter, c: Pos2, dir: f32, col: Color32) {
    let s = 7.0;
    tri(p, Pos2::new(c.x - dir * 0.2 * s, c.y), s, dir, col);
    p.rect_filled(
        Rect::from_center_size(Pos2::new(c.x + dir * s * 1.0, c.y), Vec2::new(2.6, 2.0 * s)),
        1.0,
        col,
    );
}

fn draw_play_pause(p: &Painter, c: Pos2, playing: bool, col: Color32) {
    if playing {
        for dx in [-4.5, 4.5] {
            p.rect_filled(Rect::from_center_size(Pos2::new(c.x + dx, c.y), Vec2::new(4.0, 15.0)), 1.2, col);
        }
    } else {
        tri(p, Pos2::new(c.x + 1.0, c.y), 9.0, 1.0, col);
    }
}
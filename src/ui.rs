//! Panadapter ekranı: üst bilgi şeridi, spektrum, frekans ölçeği, şelale.
//!
//! Yatay eksen 48 kHz: 800 piksel × 60 Hz. Ekranın ortası (x=400) QMX'in
//! LO frekansıdır; QMX'te LO, ayar (dial) frekansının 12 kHz altında durur,
//! bu yüzden ayar işareti x=600'dedir.

use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_6X10};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::{IntoStorage, Rgb565};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use u8g2_fonts::{fonts, FontRenderer};

use crate::dsp::N;
use crate::lcd::{Framebuffer, W};

pub const HDR_H: usize = 44;
pub const SPEC_Y: usize = HDR_H;
pub const SPEC_H: usize = 156;
pub const SCALE_Y: usize = SPEC_Y + SPEC_H;
pub const SCALE_H: usize = 20;
pub const WF_Y: usize = SCALE_Y + SCALE_H;
pub const WF_H: usize = 480 - WF_Y;

pub const HZ_PER_PX: i64 = 60;
/// QMX'te LO, ayar frekansının bu kadar altında
pub const LO_OFFSET: i64 = 12_000;
pub const DIAL_X: i32 = (W as i64 / 2 + LO_OFFSET / HZ_PER_PX) as i32;
/// Spektrumda gösterilen dB aralığı
const RANGE_DB: f32 = 70.0;

pub struct Ui {
    big: FontRenderer,
    /// Her piksel sütunu için yumuşatılmış dB
    smooth: Vec<f32>,
    /// Görüntü alt sınırı (gürültü tabanını yavaşça izler)
    floor: f32,
    palette: [u16; 256],
    /// Şelalenin çizildiği ayar frekansı (kaydırma için)
    wf_dial: Option<u64>,
    wf_acc: Vec<f32>,
    // Kare başına geçici tamponlar (yığın yerine yığıt belleğinde)
    col: Vec<f32>,
    sorted: Vec<f32>,
    ty: Vec<usize>,
    seg: Vec<(usize, usize)>,
    // Bir önceki karede çizilen iz (yalnız değişen pikselleri yazmak için)
    pty: Vec<usize>,
    pseg: Vec<(usize, usize)>,
    full_redraw: bool,
    /// Bu karede baştan sona yeniden çizilecek sütunlar (imleç taşınınca)
    force: Vec<bool>,
    /// Sürükleme imleci (piksel)
    cursor: Option<usize>,
    /// Geçiş bandı piksel aralığı
    pb: Option<(usize, usize)>,
    wf_count: u32,
    t_spec_us: u64,
    t_wf_us: u64,
    wf_lines: u32,
}

/// Şelaleye kaç spektrum karesinde bir satır eklenir
const WF_EVERY: u32 = 2;

/// Üst şeritteki mod düğmeleri (QMX CAT kodları: MD1 LSB, MD2 USB, MD3 CW, MD6 DIGI)
pub const MODES: [&str; 4] = ["LSB", "USB", "CW", "DIGI"];
const MODE_X: i32 = 230;
const MODE_W: i32 = 60;
const MODE_GAP: i32 = 6;

/// Üst şeritte dokunulan nokta bir mod düğmesiyse onu döndürür.
pub fn mode_button_at(x: i32, y: i32) -> Option<&'static str> {
    if !(4..=40).contains(&y) {
        return None;
    }
    MODES.iter().enumerate().find_map(|(i, m)| {
        let x0 = MODE_X + i as i32 * (MODE_W + MODE_GAP);
        (x >= x0 && x < x0 + MODE_W).then_some(*m)
    })
}

/// Seçili modun alıcı penceresi, ayar frekansına göre Hz cinsinden (alt, üst).
/// Genişlik QMX'ten (FW) okunamazsa varsayılanlar: SSB/DIGI 2700, CW 300 Hz.
pub fn passband(mode: &str, width: Option<u32>) -> Option<(i64, i64)> {
    let w = |def: u32| width.filter(|&w| w > 0).unwrap_or(def) as i64;
    match mode {
        "USB" | "DIGI" => Some((0, w(2700))),
        "LSB" | "DIGI-R" => Some((-w(2700), 0)),
        "CW" | "CW-R" => Some((-w(300) / 2, w(300) / 2)),
        "AM" => Some((-w(6000) / 2, w(6000) / 2)),
        "FM" => Some((-w(10000) / 2, w(10000) / 2)),
        _ => None,
    }
}

/// Şelale renkleri: siyah → lacivert → mavi → camgöbeği → sarı → kırmızı → beyaz
fn palette() -> [u16; 256] {
    let stops: [(f32, (f32, f32, f32)); 7] = [
        (0.00, (0.0, 0.0, 0.0)),
        (0.15, (0.0, 0.0, 0.35)),
        (0.35, (0.0, 0.25, 0.85)),
        (0.55, (0.0, 0.85, 0.85)),
        (0.72, (1.0, 0.95, 0.0)),
        (0.88, (1.0, 0.15, 0.0)),
        (1.00, (1.0, 1.0, 1.0)),
    ];
    let mut p = [0u16; 256];
    for (i, out) in p.iter_mut().enumerate() {
        let t = i as f32 / 255.0;
        let k = stops.windows(2).position(|w| t <= w[1].0).unwrap_or(5);
        let (t0, c0) = stops[k];
        let (t1, c1) = stops[k + 1];
        let f = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
        let mix = |a: f32, b: f32| a + (b - a) * f;
        let (r, g, b) = (mix(c0.0, c1.0), mix(c0.1, c1.1), mix(c0.2, c1.2));
        *out = Rgb565::new((r * 31.0) as u8, (g * 63.0) as u8, (b * 31.0) as u8).into_storage();
    }
    p
}

pub fn fmt_freq(hz: u64) -> String {
    let mhz = hz / 1_000_000;
    let khz = (hz / 1000) % 1000;
    let h = hz % 1000;
    format!("{mhz}.{khz:03}.{h:03}")
}

fn text(fb: &mut Framebuffer, s: &str, x: i32, y: i32, c: Rgb565) {
    let _ = Text::new(s, Point::new(x, y), MonoTextStyle::new(&FONT_10X20, c)).draw(fb);
}

fn fill(fb: &mut Framebuffer, x: usize, y: usize, w: usize, h: usize, c: Rgb565) {
    let _ = Rectangle::new(Point::new(x as i32, y as i32), Size::new(w as u32, h as u32))
        .into_styled(PrimitiveStyle::with_fill(c))
        .draw(fb);
}

impl Ui {
    pub fn new(fb: &mut Framebuffer) -> Self {
        fb.buf.fill(0);
        Self {
            big: FontRenderer::new::<fonts::u8g2_font_logisoso28_tn>(),
            smooth: vec![-120.0; W],
            floor: -110.0,
            palette: palette(),
            wf_dial: None,
            wf_acc: vec![f32::MIN; W],
            col: vec![0.0; W],
            sorted: vec![0.0; W],
            ty: vec![0; W],
            seg: vec![(0, 0); W],
            pty: vec![0; W],
            pseg: vec![(0, 0); W],
            full_redraw: true,
            force: vec![false; W],
            cursor: None,
            pb: None,
            wf_count: 0,
            t_spec_us: 0,
            t_wf_us: 0,
            wf_lines: 0,
        }
    }

    /// Üst şerit: frekans, mod düğmeleri, durum
    pub fn header(&self, fb: &mut Framebuffer, dial: Option<u64>, mode: &str, status: &str, ok: bool) {
        fill(fb, 0, 0, W, HDR_H, Rgb565::new(2, 4, 3));
        for (i, m) in MODES.iter().enumerate() {
            let x = MODE_X + i as i32 * (MODE_W + MODE_GAP);
            let r = Rectangle::new(Point::new(x, 6), Size::new(MODE_W as u32, 32));
            let sel = *m == mode;
            if sel {
                let _ = r.into_styled(PrimitiveStyle::with_fill(Rgb565::CYAN)).draw(fb);
            } else {
                let _ = r
                    .into_styled(PrimitiveStyle::with_stroke(Rgb565::new(6, 20, 14), 1))
                    .draw(fb);
            }
            let tx = x + (MODE_W - m.len() as i32 * 10) / 2;
            let c = if sel { Rgb565::BLACK } else { Rgb565::CSS_LIGHT_GRAY };
            text(fb, m, tx, 28, c);
        }
        if let Some(f) = dial {
            let _ = self.big.render_aligned(
                fmt_freq(f).as_str(),
                Point::new(14, 38),
                VerticalPosition::Baseline,
                HorizontalAlignment::Left,
                FontColor::Transparent(Rgb565::WHITE),
                fb,
            );
        } else {
            text(fb, "QMX not connected", 14, 30, Rgb565::CSS_ORANGE);
        }
        if !MODES.contains(&mode) && !mode.is_empty() {
            let x = MODE_X + MODES.len() as i32 * (MODE_W + MODE_GAP);
            text(fb, mode, x, 28, Rgb565::CYAN);
        }
        let c = if ok { Rgb565::GREEN } else { Rgb565::CSS_ORANGE };
        let x = W as i32 - 14 - status.len() as i32 * 10;
        text(fb, status, x, 30, c);
    }

    /// Frekans ölçeği: 5 kHz'de bir etiket, 1 kHz'de bir çentik
    pub fn scale(&self, fb: &mut Framebuffer, dial: Option<u64>) {
        fill(fb, 0, SCALE_Y, W, SCALE_H, Rgb565::new(3, 6, 4));
        let Some(dial) = dial else { return };
        let lo = dial as i64 - LO_OFFSET;
        let left = lo - (W as i64 / 2) * HZ_PER_PX;
        let first = (left / 1000 + 1) * 1000;
        let style = MonoTextStyle::new(&FONT_6X10, Rgb565::CSS_LIGHT_GRAY);
        let mut f = first;
        while f < left + W as i64 * HZ_PER_PX {
            let x = ((f - left) / HZ_PER_PX) as usize;
            let major = f % 5000 == 0;
            let h = if major { 6 } else { 3 };
            for y in SCALE_Y..SCALE_Y + h {
                fb.put(x, y, Rgb565::CSS_LIGHT_GRAY);
            }
            if major {
                let s = format!("{}.{:03}", f / 1_000_000, (f / 1000) % 1000);
                let tx = x as i32 - s.len() as i32 * 3;
                let _ = Text::new(&s, Point::new(tx, SCALE_Y as i32 + 17), style).draw(fb);
            }
            f += 1000;
        }
        // Ayar işareti: kırmızı üçgen
        for dy in 0..6 {
            for dx in -(5 - dy as i32)..=(5 - dy as i32) {
                fb.put((DIAL_X + dx) as usize, SCALE_Y + dy, Rgb565::RED);
            }
        }
    }

    /// Sürükleme imlecini taşır/kaldırır (eski ve yeni sütun yeniden çizilir).
    pub fn set_cursor(&mut self, x: Option<usize>) {
        if x == self.cursor {
            return;
        }
        for c in [self.cursor, x].into_iter().flatten() {
            if c < W {
                self.force[c] = true;
            }
        }
        self.cursor = x.filter(|&c| c < W);
    }

    /// Geçiş bandını ayar frekansına göre Hz cinsinden ayarlar.
    pub fn set_passband(&mut self, pb: Option<(i64, i64)>) {
        let px = pb.map(|(a, b)| {
            let x0 = (DIAL_X as i64 + a / HZ_PER_PX).clamp(0, W as i64 - 1) as usize;
            let x1 = (DIAL_X as i64 + b / HZ_PER_PX).clamp(0, W as i64 - 1) as usize;
            (x0, x1)
        });
        if px != self.pb {
            self.pb = px;
            self.full_redraw = true;
        }
    }

    /// Yeni bir spektrum karesini çizer ve şelaleye bir satır ekler.
    pub fn frame(&mut self, fb: &mut Framebuffer, db: &mut [f32], dial: Option<u64>) {
        // LO sızıntısını (merkezdeki tepe ve ±328/±656 Hz yan çizgileri) komşu
        // bin'lerin ortalamasıyla ört: ±15 bin = ±700 Hz
        let c = N / 2;
        let edge = (db[c - 17] + db[c - 16] + db[c + 16] + db[c + 17]) / 4.0;
        for v in &mut db[c - 15..=c + 15] {
            *v = edge;
        }

        // DİKKAT: ana görev yığını 16 KB; 800 elemanlı diziler yığında değil,
        // Ui içindeki önceden ayrılmış Vec'lerde tutulur.
        // Bin'leri piksel sütunlarına indir (sütun başına en büyük değer)
        for x in 0..W {
            let b0 = x * N / W;
            let b1 = ((x + 1) * N / W).max(b0 + 1);
            self.col[x] = db[b0..b1].iter().cloned().fold(f32::MIN, f32::max);
            self.smooth[x] = self.smooth[x] * 0.5 + self.col[x] * 0.5;
        }

        // Gürültü tabanı: sütunların %20'lik dilimi; görüntü sınırı onu yavaşça izler
        self.sorted.copy_from_slice(&self.col);
        self.sorted.sort_unstable_by(|a, b| a.total_cmp(b));
        let nf = self.sorted[W / 5];
        self.floor += (nf - 6.0 - self.floor) * 0.05;
        let bottom = self.floor;
        let top = bottom + RANGE_DB;

        let t0 = std::time::Instant::now();
        self.draw_spectrum(fb, bottom, top);
        self.t_spec_us += t0.elapsed().as_micros() as u64;

        // Şelale satırı her kareye değil, her WF_EVERY karede bir (PSRAM bant genişliği):
        // aradaki karelerin en büyük değeri biriktirilir, kısa sinyaller kaybolmaz
        for x in 0..W {
            self.wf_acc[x] = self.wf_acc[x].max(self.col[x]);
        }
        self.wf_count += 1;
        if self.wf_count >= WF_EVERY {
            let t1 = std::time::Instant::now();
            let acc = core::mem::take(&mut self.wf_acc);
            self.waterfall_line(fb, &acc, dial);
            self.wf_acc = acc;
            self.wf_acc.fill(f32::MIN);
            self.wf_count = 0;
            self.t_wf_us += t1.elapsed().as_micros() as u64;
            self.wf_lines += 1;
        }
    }

    /// Son çağrıdan beri spektrum ve şelale sürelerini (ms) ve satır sayısını verir.
    pub fn take_timing(&mut self) -> (f32, f32, u32) {
        let r = (
            self.t_spec_us as f32 / 1000.0,
            self.t_wf_us as f32 / 1000.0,
            self.wf_lines,
        );
        self.t_spec_us = 0;
        self.t_wf_us = 0;
        self.wf_lines = 0;
        r
    }

    /// Spektrumu çizer. PSRAM'e yazmak pahalı olduğu için her sütunda yalnızca
    /// önceki kareye göre değişebilecek piksel aralığı yeniden yazılır.
    fn draw_spectrum(&mut self, fb: &mut Framebuffer, bottom: f32, _top: f32) {
        let bg = Rgb565::new(1, 2, 2).into_storage();
        let grid = Rgb565::new(4, 9, 5).into_storage();
        let dialc = Rgb565::new(20, 0, 0).into_storage();
        let fillc = Rgb565::new(3, 22, 8).into_storage();
        let linec = Rgb565::new(16, 63, 20).into_storage();

        let to_y = |v: f32| -> usize {
            let t = ((v - bottom) / RANGE_DB).clamp(0.0, 1.0);
            SPEC_Y + SPEC_H - 1 - (t * (SPEC_H - 1) as f32) as usize
        };
        // Bir önceki karenin durumunu sakla, yenisini hesapla
        core::mem::swap(&mut self.ty, &mut self.pty);
        core::mem::swap(&mut self.seg, &mut self.pseg);
        let mut prev = to_y(self.smooth[0]);
        for x in 0..W {
            let y = to_y(self.smooth[x]);
            self.ty[x] = y;
            self.seg[x] = if prev < y { (prev, y) } else { (y, prev) };
            prev = y;
        }

        // Izgara alt sınırdan itibaren her 10 dB'de bir sabit piksel satırında
        // (dB değerine bağlı değil, böylece taban kayınca ızgara yer değiştirmez)
        let is_grid = |y: usize| {
            let from_bottom = (SPEC_Y + SPEC_H - 1 - y) as f32;
            let step = SPEC_H as f32 * 10.0 / RANGE_DB;
            let r = from_bottom % step;
            from_bottom > 0.0 && r < 1.0
        };
        let pb_bg = Rgb565::new(3, 7, 7).into_storage();
        let pb_fill = Rgb565::new(5, 30, 13).into_storage();
        let pb_edge = Rgb565::new(14, 28, 14).into_storage();
        let cursorc = Rgb565::CYAN.into_storage();

        let dx = DIAL_X as usize;
        let full = self.full_redraw;
        self.full_redraw = false;
        let cursor = self.cursor;
        let (pb0, pb1) = self.pb.unwrap_or((usize::MAX, usize::MAX));

        for x in 0..W {
            let (s0, s1) = self.seg[x];
            let ty = self.ty[x];
            let (lo, hi) = if full || self.force[x] {
                (SPEC_Y, SPEC_Y + SPEC_H - 1)
            } else {
                let (p0, p1) = self.pseg[x];
                (s0.min(p0), s1.max(p1))
            };
            self.force[x] = false;
            let grid_col = x % 3 == 0;
            let in_pb = pb0 != usize::MAX && x >= pb0 && x <= pb1;
            let is_edge = x == pb0 || x == pb1;
            let (bgc, fc) = if in_pb { (pb_bg, pb_fill) } else { (bg, fillc) };
            let is_cursor = cursor == Some(x);
            for y in lo..=hi {
                fb.buf[y * W + x] = if is_cursor {
                    cursorc
                } else if y >= s0 && y <= s1 {
                    linec
                } else if x == dx {
                    dialc
                } else if y > ty {
                    fc
                } else if is_edge {
                    pb_edge
                } else if grid_col && is_grid(y) {
                    grid
                } else {
                    bgc
                };
            }
        }
    }

    fn waterfall_line(&mut self, fb: &mut Framebuffer, col: &[f32], dial: Option<u64>) {
        // Frekans değiştiyse eski satırları yatayda kaydır ki sinyaller hizalı kalsın.
        // Kayma pikselin tam katı kadar yapılır, artan Hz'ler bir sonrakine kalır.
        match (dial, self.wf_dial) {
            (Some(d), Some(old)) => {
                let shift = (old as i64 - d as i64) / HZ_PER_PX;
                if shift != 0 {
                    shift_rows(fb, shift);
                    self.wf_dial = Some((old as i64 - shift * HZ_PER_PX) as u64);
                }
            }
            (Some(d), None) => self.wf_dial = Some(d),
            (None, _) => {}
        }

        // Bir satır aşağı kaydır ve en üste yeni satırı yaz
        let start = WF_Y * W;
        let end = (WF_Y + WF_H - 1) * W;
        fb.buf.copy_within(start..end, start + W);
        let lo = self.floor + 3.0;
        let span = RANGE_DB * 0.75;
        for x in 0..W {
            let t = ((col[x] - lo) / span).clamp(0.0, 1.0);
            fb.buf[start + x] = self.palette[(t * 255.0) as usize];
        }
    }
}

/// Şelaleyi yatayda `shift` piksel kaydırır (+ sağa), açılan yeri siyah yapar.
fn shift_rows(fb: &mut Framebuffer, shift: i64) {
    let s = shift.unsigned_abs() as usize;
    for y in WF_Y..WF_Y + WF_H {
        let row = &mut fb.buf[y * W..(y + 1) * W];
        if s >= W {
            row.fill(0);
        } else if shift > 0 {
            row.copy_within(0..W - s, s);
            row[..s].fill(0);
        } else {
            row.copy_within(s..W, 0);
            row[W - s..].fill(0);
        }
    }
}

/// Dokunulan x koordinatının ayar frekansına karşılığı
pub fn x_to_freq(x: i32, dial: u64) -> u64 {
    let lo = dial as i64 - LO_OFFSET;
    (lo + (x as i64 - W as i64 / 2) * HZ_PER_PX) as u64
}

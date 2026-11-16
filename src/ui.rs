//! Panadapter ekranı (A · panelli düzen, QRP DDS renk dili).
//!
//! ```text
//!   0 ┌ üst şerit: QMXPan · durum · WiFi ───────────────────────────────┐
//!  32 │ büyük frekans          │ band kartı │ S-metre kartı              │
//! 114 │ mod çipleri · BW · SPAN                                          │
//! 148 │ spektrum paneli (dB ızgara, geçiş bandı, LO bölgesi, ayar etiketi)│
//! 290 │ frekans ölçeği                                                    │
//! 310 │ şelale                                                            │
//! 442 │ tuş çubuğu: BAND · ZOOM · DISPLAY · MEM · WIFI · MENU            │
//!     └───────────────────────────────────────────────────────────────────┘
//! ```
//! Yatay eksen 48 kHz: 800 piksel × 60 Hz. Ekranın ortası (x=400) QMX'in LO
//! frekansıdır; QMX'te LO, ayar frekansının 12 kHz altında durur, bu yüzden
//! ayar işareti x=600'dedir.

use crate::dsp::N;
use crate::fonts::*;
use crate::gfx::{self, blend, rrect, text, text_c, text_r, th};
use crate::lcd::{Framebuffer, W};
use crate::settings::Settings;

// --------------------------------------------------------------- yerleşim

pub const TOP_H: i32 = 30;
const FREQ_Y: i32 = 100;
const CARD_Y: i32 = 36;
const CARD_H: i32 = 72;
const BAND_X: i32 = 330;
const BAND_W: i32 = 96;
const SM_X: i32 = 434;
const SM_W: i32 = 358;
const CHIP_Y: i32 = 116;
const CHIP_H: i32 = 26;
const MODE_X: i32 = 12;
const MODE_W: i32 = 56;
const MODE_STEP: i32 = 62;

pub const SPEC_Y: usize = 148;
pub const SPEC_H: usize = 142;
pub const SCALE_Y: usize = SPEC_Y + SPEC_H;
pub const SCALE_H: usize = 20;
pub const WF_Y: usize = SCALE_Y + SCALE_H;
pub const WF_H: usize = 128;
const KEY_Y: i32 = 444;
const KEY_H: i32 = 30;

pub const HZ_PER_PX: i64 = 60;
/// QMX'te LO, ayar frekansının bu kadar altında
pub const LO_OFFSET: i64 = 12_000;
pub const DIAL_X: i32 = (W as i64 / 2 + LO_OFFSET / HZ_PER_PX) as i32;
/// LO sızıntısının bastırıldığı bölge (±700 Hz ≈ ±12 piksel): "veri yok"
const LO_X0: usize = W / 2 - 12;
const LO_X1: usize = W / 2 + 12;

/// Üst şeritteki mod çipleri (QMX CAT kodları: MD1 LSB, MD2 USB, MD3 CW, MD6 DIGI)
pub const MODES: [&str; 4] = ["LSB", "USB", "CW", "DIGI"];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    Band,
    Cw,
    Ft8,
    Display,
    Wifi,
    Menu,
}
/// (tuş, yazı, etkin mi)
const KEYS: [(Key, &str, bool); 6] = [
    (Key::Band, "BAND", true),
    (Key::Cw, "CW", true),
    (Key::Ft8, "FT8", false),
    (Key::Display, "DISPLAY", true),
    (Key::Wifi, "WIFI", false),
    (Key::Menu, "MENU", false),
];

/// Şelalenin altındaki metin paneli (CW / FT8) yüksekliği
const PANEL_H: usize = 52;

fn key_x(i: usize) -> i32 {
    8 + (i as i32 * 263) / 2
}

/// Tuş çubuğunda dokunulan (etkin) tuş
pub fn key_at(x: i32, y: i32) -> Option<Key> {
    if y < KEY_Y - 4 {
        return None;
    }
    (0..KEYS.len()).find_map(|i| {
        let x0 = key_x(i);
        (x >= x0 && x < x0 + 125 && KEYS[i].2).then_some(KEYS[i].0)
    })
}

/// Üst bölümde dokunulan nokta bir mod çipiyse onu döndürür.
pub fn mode_chip_at(x: i32, y: i32) -> Option<&'static str> {
    if !(CHIP_Y - 4..=CHIP_Y + CHIP_H + 4).contains(&y) {
        return None;
    }
    MODES.iter().enumerate().find_map(|(i, m)| {
        let x0 = MODE_X + i as i32 * MODE_STEP;
        (x >= x0 && x < x0 + MODE_W).then_some(*m)
    })
}

/// Frekans yazısına ya da band kartına dokunuldu mu (band penceresini açar)
pub fn freq_area_at(x: i32, y: i32) -> bool {
    (TOP_H..CHIP_Y - 4).contains(&y) && x < SM_X
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

/// S birimi: S9 = -73 dBm, S birimi başına 6 dB (S1 = -121 dBm)
pub fn s_units(dbm: f32) -> String {
    if dbm > -73.0 {
        format!("S9+{:.0}", dbm + 73.0)
    } else {
        let s = (9.0 + (dbm + 73.0) / 6.0).floor().max(0.0);
        format!("S{s:.0}")
    }
}

pub fn fmt_freq(hz: u64) -> String {
    let mhz = hz / 1_000_000;
    let khz = (hz / 1000) % 1000;
    let h = hz % 1000;
    format!("{mhz}.{khz:03}.{h:03}")
}

/// Dokunulan x koordinatının ayar frekansına karşılığı
pub fn x_to_freq(x: i32, dial: u64) -> u64 {
    let lo = dial as i64 - LO_OFFSET;
    (lo + (x as i64 - W as i64 / 2) * HZ_PER_PX) as u64
}

// ---------------------------------------------------------------- paletler

type Stops = &'static [(f32, (f32, f32, f32))];
pub const PALETTES: [(&str, Stops); 3] = [
    // siyah → lacivert → mavi → camgöbeği → sarı → kırmızı → beyaz
    (
        "Classic",
        &[
            (0.00, (0.0, 0.0, 0.0)),
            (0.15, (0.0, 0.0, 0.35)),
            (0.35, (0.0, 0.25, 0.85)),
            (0.55, (0.0, 0.85, 0.85)),
            (0.72, (1.0, 0.95, 0.0)),
            (0.88, (1.0, 0.15, 0.0)),
            (1.00, (1.0, 1.0, 1.0)),
        ],
    ),
    ("Gray", &[(0.0, (0.0, 0.0, 0.0)), (1.0, (1.0, 1.0, 1.0))]),
    (
        "Amber",
        &[
            (0.00, (0.0, 0.0, 0.0)),
            (0.40, (0.35, 0.12, 0.0)),
            (0.75, (1.0, 0.6, 0.0)),
            (1.00, (1.0, 1.0, 0.7)),
        ],
    ),
];

fn palette(idx: usize) -> [u16; 256] {
    let stops = PALETTES[idx.min(PALETTES.len() - 1)].1;
    let mut p = [0u16; 256];
    for (i, out) in p.iter_mut().enumerate() {
        let t = i as f32 / 255.0;
        let k = stops
            .windows(2)
            .position(|w| t <= w[1].0)
            .unwrap_or(stops.len() - 2);
        let (t0, c0) = stops[k];
        let (t1, c1) = stops[k + 1];
        let f = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
        let mix = |a: f32, b: f32| a + (b - a) * f;
        let (r, g, b) = (mix(c0.0, c1.0), mix(c0.1, c1.1), mix(c0.2, c1.2));
        let hex = ((r * 255.0) as u32) << 16 | ((g * 255.0) as u32) << 8 | (b * 255.0) as u32;
        *out = gfx::rgb(hex);
    }
    p
}

// ------------------------------------------------------------- üst bölüm

/// Üst bölümde gösterilen her şey; yalnızca değişen parça yeniden çizilir.
#[derive(Clone, PartialEq, Default)]
pub struct Hdr {
    pub dial: Option<u64>,
    pub mode: String,
    pub bw: Option<u32>,
    pub status: String,
    pub ok: bool,
    pub net: String,
    /// dBm, tam sayıya yuvarlanmış (1 dB değişimde yeniden çizilir)
    pub smeter: Option<i32>,
}

pub struct Ui {
    /// Her piksel sütunu için yumuşatılmış dB
    smooth: Vec<f32>,
    /// Gürültü tabanı (yavaşça izlenir)
    floor: f32,
    /// Spektrumun alt sınırı: 5 dB'ye yuvarlanmış, ızgara etiketleri buna göre
    bottom: f32,
    palette: [u16; 256],
    set: Settings,
    spectrum_hidden: bool,
    /// Şelalenin çizildiği ayar frekansı (yatay kaydırma için)
    wf_dial: Option<u64>,
    wf_acc: Vec<f32>,
    wf_count: u32,
    // Kare başına geçici tamponlar (ana görev yığını küçük; burada tutulur)
    col: Vec<f32>,
    sorted: Vec<f32>,
    ty: Vec<usize>,
    seg: Vec<(usize, usize)>,
    pty: Vec<usize>,
    pseg: Vec<(usize, usize)>,
    full_redraw: bool,
    force: Vec<bool>,
    cursor: Option<usize>,
    /// Geçiş bandı piksel aralığı
    pb: Option<(usize, usize)>,
    // Spektrum katmanları (SPEC_H satır): zemin, zemin+dolgu karışımı, üst katman
    bg: Framebuffer,
    bgfill: Framebuffer,
    over: Framebuffer,
    /// Katmanların hangi durum için hazırlandığı
    layers_for: Option<(i32, Option<(usize, usize)>, i32)>,
    over_for: Option<u64>,
    last: Hdr,
    /// Şelalenin o anki yüksekliği (metin paneli açıksa kısalır)
    wf_h: usize,
    /// CAL ofseti (dB): ızgara etiketleri dBm gösterir
    pub cal: f32,
    t_spec_us: u64,
    t_wf_us: u64,
    wf_lines: u32,
}

impl Ui {
    pub fn new(fb: &mut Framebuffer) -> Self {
        fb.buf.fill(th::BG);
        let ui = Self {
            smooth: vec![-130.0; W],
            floor: -125.0,
            bottom: -135.0,
            palette: palette(0),
            set: Settings::default(),
            spectrum_hidden: false,
            wf_dial: None,
            wf_acc: vec![f32::MIN; W],
            wf_count: 0,
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
            bg: Framebuffer::layer(SPEC_H, th::SPEC_BG),
            bgfill: Framebuffer::layer(SPEC_H, th::SPEC_BG),
            over: Framebuffer::layer(SPEC_H, 0),
            layers_for: None,
            over_for: None,
            last: Hdr::default(),
            wf_h: WF_H,
            cal: 0.0,
            t_spec_us: 0,
            t_wf_us: 0,
            wf_lines: 0,
        };
        gfx::fill(fb, 0, WF_Y as i32, W as i32, WF_H as i32, 0);
        ui.draw_keys(fb, &[]);
        ui.draw_topbar(fb, "", false, "");
        ui.draw_freq(fb, None);
        ui.draw_band_card(fb, None);
        ui.draw_smeter(fb, None);
        ui.draw_chips(fb, "", None);
        ui
    }

    // ------------------------------------------------------------ üst bölüm

    pub fn header(&mut self, fb: &mut Framebuffer, h: &Hdr) {
        let old = core::mem::take(&mut self.last);
        if h.status != old.status || h.ok != old.ok || h.net != old.net {
            self.draw_topbar(fb, &h.status, h.ok, &h.net);
        }
        if h.dial != old.dial {
            self.draw_freq(fb, h.dial);
            let band = |d: Option<u64>| d.map(|f| (crate::bands::band_of(f), crate::bands::segment(f)));
            if band(h.dial) != band(old.dial) {
                self.draw_band_card(fb, h.dial);
            }
        }
        if h.smeter != old.smeter {
            self.draw_smeter(fb, h.smeter);
        }
        if h.mode != old.mode || h.bw != old.bw {
            self.draw_chips(fb, &h.mode, h.bw);
        }
        self.last = h.clone();
    }

    fn draw_topbar(&self, fb: &mut Framebuffer, status: &str, ok: bool, net: &str) {
        gfx::fill(fb, 0, 0, W as i32, TOP_H, th::PN);
        let x = text(fb, &F_B16, 12, 21, "QMXPan", th::CY);
        text(fb, &F_B14, x + 12, 20, "PANADAPTER", th::TX);
        // Durum: renkli nokta + yazı, ağ bilgisi en sağda
        let nw = gfx::text_w(&F_R14, net);
        let net_x = W as i32 - 12 - nw;
        if !net.is_empty() {
            text(fb, &F_R14, net_x, 20, net, th::MU);
        }
        let sw = gfx::text_w(&F_B14, status);
        let sx = net_x - 22 - sw;
        let dot = if ok { th::GR } else { th::AM };
        rrect(fb, sx - 13, 11, 8, 8, 4, dot);
        text(fb, &F_B14, sx, 20, status, th::TX);
    }

    fn draw_freq(&self, fb: &mut Framebuffer, dial: Option<u64>) {
        gfx::fill(fb, 0, TOP_H, BAND_X - 4, CHIP_Y - 4 - TOP_H, th::BG);
        text(fb, &F_B12, 14, 50, "VFO A · CAT", th::MU);
        match dial {
            Some(f) => {
                let s = fmt_freq(f);
                let (main, tail) = s.split_at(s.len() - 4);
                let x = text(fb, &F_D56, 12, FREQ_Y, main, th::TX);
                let x = text(fb, &F_D36, x, FREQ_Y, tail, th::TX);
                text(fb, &F_B14, x + 6, FREQ_Y, "MHz", th::MU);
            }
            None => {
                text(fb, &F_B16, 14, 84, "Waiting for QMX...", th::AM);
            }
        }
    }

    fn draw_band_card(&self, fb: &mut Framebuffer, dial: Option<u64>) {
        gfx::fill(fb, BAND_X, CARD_Y, BAND_W, CARD_H, th::BG);
        rrect(fb, BAND_X, CARD_Y, BAND_W, CARD_H, 8, th::PN);
        let cx = BAND_X + BAND_W / 2;
        match dial.and_then(|f| crate::bands::band_of(f).map(|i| (i, f))) {
            Some((i, f)) => {
                text_c(fb, &F_B30, cx, CARD_Y + 38, &format!("{}m", crate::bands::BANDS[i].name), th::AM);
                text_c(fb, &F_B12, cx, CARD_Y + 60, crate::bands::segment(f), th::GR);
            }
            None => {
                text_c(fb, &F_B30, cx, CARD_Y + 38, "—", th::MU);
                let sub = if dial.is_some() { "out of band" } else { "" };
                text_c(fb, &F_B12, cx, CARD_Y + 60, sub, th::RD);
            }
        }
    }

    fn draw_smeter(&self, fb: &mut Framebuffer, dbm: Option<i32>) {
        gfx::fill(fb, SM_X, CARD_Y, SM_W, CARD_H, th::BG);
        rrect(fb, SM_X, CARD_Y, SM_W, CARD_H, 8, th::PN);
        text(fb, &F_B12, SM_X + 14, CARD_Y + 18, "SIGNAL", th::MU);
        // 22 bölme × 4 dB: S1 (-121 dBm) … S9 (-73) … S9+40 (-33)
        let n = 22;
        let bx = SM_X + 150;
        let bw = SM_W - 164;
        let lit = dbm.map_or(0, |d| ((d + 121 + 4) / 4).clamp(0, n));
        for i in 0..n {
            let x0 = bx + i * bw / n;
            let x1 = bx + (i + 1) * bw / n - 2;
            let c = if i < lit {
                if i < 12 { th::GR } else { th::RD }
            } else {
                th::LN
            };
            rrect(fb, x0, CARD_Y + 30, x1 - x0, 14, 2, c);
        }
        for (i, l) in [(0, "1"), (3, "3"), (6, "5"), (9, "7"), (12, "9"), (17, "+20"), (21, "+40")] {
            let cx = bx + i * bw / n + bw / n / 2 - 1;
            text_c(fb, &F_R11, cx, CARD_Y + 60, l, th::MU);
        }
        match dbm {
            Some(d) => {
                let x = text(fb, &F_B30, SM_X + 14, CARD_Y + 56, &s_units(d as f32), th::TX);
                text(fb, &F_R14, x.max(SM_X + 60) + 8, CARD_Y + 56, &format!("{}{} dBm", if d < 0 { "−" } else { "" }, d.abs()), th::MU);
            }
            None => {
                text(fb, &F_B30, SM_X + 14, CARD_Y + 56, "—", th::MU);
            }
        }
    }

    fn draw_chips(&self, fb: &mut Framebuffer, mode: &str, bw: Option<u32>) {
        gfx::fill(fb, 0, CHIP_Y - 2, W as i32, CHIP_H + 4, th::BG);
        for (i, m) in MODES.iter().enumerate() {
            let x = MODE_X + i as i32 * MODE_STEP;
            let sel = *m == mode;
            rrect(fb, x, CHIP_Y, MODE_W, CHIP_H, 13, if sel { th::CY } else { th::PN2 });
            text_c(fb, &F_B14, x + MODE_W / 2, CHIP_Y + 18, m, if sel { th::ON_CY } else { th::TX });
        }
        let chip = |fb: &mut Framebuffer, x: i32, w: i32, l: &str, v: &str, c: u16| {
            rrect(fb, x, CHIP_Y, w, CHIP_H, 13, th::PN);
            text(fb, &F_B12, x + 12, CHIP_Y + 17, l, th::MU);
            text_r(fb, &F_B14, x + w - 12, CHIP_Y + 18, v, c);
        };
        let bwv = passband(mode, bw).map(|(a, b)| b - a);
        let bws = match bwv {
            Some(v) if v >= 1000 => format!("{:.1} kHz", v as f32 / 1000.0),
            Some(v) => format!("{v} Hz"),
            None => "—".into(),
        };
        chip(fb, 268, 112, "BW", &bws, th::CY);
        chip(fb, 388, 112, "SPAN", "48 kHz", th::TX);
        if !MODES.contains(&mode) && !mode.is_empty() {
            chip(fb, 508, 112, "MODE", mode, th::AM);
        }
    }

    /// Tuş çubuğu; `active` içindeki tuşlar camgöbeği (açık) gösterilir.
    pub fn draw_keys(&self, fb: &mut Framebuffer, active: &[Key]) {
        gfx::fill(fb, 0, KEY_Y - 4, W as i32, 480 - KEY_Y + 4, th::BG);
        for (i, (k, label, enabled)) in KEYS.iter().enumerate() {
            let x = key_x(i);
            let on = active.contains(k);
            let (bg, fg) = match (enabled, on) {
                (true, true) => (th::CY, th::ON_CY),
                (true, false) => (th::PN2, th::TX),
                _ => (th::PN, th::LN),
            };
            rrect(fb, x, KEY_Y, 125, KEY_H, 7, bg);
            text_c(fb, &F_B14, x + 62, KEY_Y + 20, label, fg);
        }
    }

    /// Şelalenin altında metin panelini açar/kapatır (şelale o kadar kısalır).
    pub fn set_text_panel(&mut self, fb: &mut Framebuffer, on: bool) {
        let h = if on { WF_H - PANEL_H } else { WF_H };
        if h != self.wf_h {
            self.wf_h = h;
            gfx::fill(fb, 0, (WF_Y + h) as i32, W as i32, (WF_H - h) as i32, if on { th::BG } else { 0 });
        }
    }

    /// CW paneli: durum satırı ve çözülen metnin son kısmı (sağdan akar).
    pub fn draw_cw_panel(&self, fb: &mut Framebuffer, txt: &str, st: &crate::cw::Stat, dial: Option<u64>) {
        let y = (WF_Y + WF_H - PANEL_H) as i32 + 2;
        let h = PANEL_H as i32 - 4;
        gfx::fill(fb, 0, y - 2, W as i32, h + 4, th::BG);
        rrect(fb, 4, y, W as i32 - 8, h, 8, th::PN);
        // Sol: CW rozeti, ton ışığı ve hız
        rrect(fb, 12, y + 7, 38, 18, 9, th::CY);
        text_c(fb, &F_B12, 31, y + 20, "CW", th::ON_CY);
        rrect(fb, 58, y + 11, 10, 10, 5, if st.key { th::GR } else { th::LN });
        let info = if st.wpm > 0 {
            let rx = dial.map(|d| d as i64 + st.tone_hz as i64 - 700);
            match rx {
                Some(f) => format!("{} WPM · {} dB · {}", st.wpm, st.snr_db, fmt_freq(f as u64)),
                None => format!("{} WPM · {} dB", st.wpm, st.snr_db),
            }
        } else {
            "listening...".to_string()
        };
        text(fb, &F_R11, 12, y + 41, &info, th::MU);
        // Metin: son kısmı, en fazla panel genişliği kadar
        let maxw = W as i32 - 8 - 260;
        let mut start = txt.len();
        for (i, _) in txt.char_indices().rev() {
            if gfx::text_w(&F_B16, &txt[i..]) > maxw {
                break;
            }
            start = i;
        }
        text_r(fb, &F_B16, W as i32 - 18, y + 30, &txt[start..], th::TX);
    }

    // ------------------------------------------------------------- ölçek

    /// Frekans ölçeği: 5 kHz'de bir etiket, 1 kHz'de bir çentik
    pub fn scale(&self, fb: &mut Framebuffer, dial: Option<u64>) {
        gfx::fill(fb, 0, SCALE_Y as i32, W as i32, SCALE_H as i32, th::BG);
        let Some(dial) = dial else { return };
        let lo = dial as i64 - LO_OFFSET;
        let left = lo - (W as i64 / 2) * HZ_PER_PX;
        let mut f = (left / 1000 + 1) * 1000;
        while f < left + W as i64 * HZ_PER_PX {
            let x = ((f - left) / HZ_PER_PX) as i32;
            let major = f % 5000 == 0;
            gfx::fill(fb, x, SCALE_Y as i32, 1, if major { 5 } else { 3 }, th::MU);
            if major {
                let s = format!("{}.{:03}", f / 1_000_000, (f / 1000) % 1000);
                text_c(fb, &F_R11, x, SCALE_Y as i32 + 17, &s, th::MU);
            }
            f += 1000;
        }
        // Ayar işareti: kehribar üçgen
        for dy in 0..6 {
            gfx::fill(fb, DIAL_X - 5 + dy, SCALE_Y as i32 + dy, 11 - 2 * dy, 1, th::AM);
        }
    }

    // --------------------------------------------------- ayarlar ve durum

    pub fn redraw_all(&mut self) {
        self.full_redraw = true;
    }

    /// S-metre kalibrasyonu değişince ızgara etiketleri (dBm) yeniden üretilir.
    pub fn set_cal(&mut self, cal: f32) {
        if cal != self.cal {
            self.cal = cal;
            self.layers_for = None;
        }
    }

    pub fn settings(&self) -> Settings {
        self.set
    }

    pub fn apply_settings(&mut self, s: Settings) {
        if s.palette != self.set.palette {
            self.palette = palette(s.palette as usize);
        }
        if s.spec_range != self.set.spec_range {
            self.full_redraw = true;
            self.layers_for = None;
        }
        self.set = s;
    }

    pub fn set_spectrum_hidden(&mut self, hidden: bool) {
        self.spectrum_hidden = hidden;
        if !hidden {
            self.full_redraw = true;
        }
    }

    /// Şelaleyi tamamen siler (band değişince eski bandın izleri kalmasın).
    pub fn clear_waterfall(&mut self, fb: &mut Framebuffer) {
        fb.buf[WF_Y * W..(WF_Y + self.wf_h) * W].fill(0);
        self.wf_dial = None;
    }

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
        }
    }

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

    // --------------------------------------------------- spektrum katmanları

    /// Zemin katmanı: panel, dB ızgarası ve etiketleri, geçiş bandı, LO bölgesi.
    /// Dolgu katmanı: aynısının üstüne %14 camgöbeği.
    fn build_layers(&mut self) {
        let grid_rows = self.grid_rows();
        let bg = &mut self.bg;
        bg.buf.fill(th::BG);
        rrect(bg, 0, 0, W as i32, SPEC_H as i32, 8, th::SPEC_BG);
        // Geçiş bandı: %13 kehribar
        if let Some((p0, p1)) = self.pb {
            for y in 0..SPEC_H {
                for x in p0..=p1 {
                    let i = y * W + x;
                    bg.buf[i] = blend(bg.buf[i], th::AM, 34);
                }
            }
        }
        // LO bölgesi: koyu zemin üstüne çapraz tarama
        for y in 0..SPEC_H {
            for x in LO_X0..=LO_X1 {
                let hatch = (x + y) % 7 == 0;
                bg.buf[y * W + x] = if hatch { th::LO_HATCH } else { th::LO_BG };
            }
        }
        text_c(bg, &F_R11, W as i32 / 2, 14, "LO", th::MU);
        // 10 dB'de bir ızgara çizgisi (etiketleri üst katmanda, iz üstlerini örtmesin)
        for y in grid_rows {
            for x in 0..W {
                if !(LO_X0..=LO_X1).contains(&x) {
                    let i = y * W + x;
                    bg.buf[i] = blend(bg.buf[i], th::LN, 200);
                }
            }
        }
        // Geçiş bandı kenarları
        if let Some((p0, p1)) = self.pb {
            for y in 0..SPEC_H {
                for x in [p0, p1] {
                    let i = y * W + x;
                    bg.buf[i] = blend(bg.buf[i], th::AM, 140);
                }
            }
        }
        for i in 0..W * SPEC_H {
            self.bgfill.buf[i] = blend(self.bg.buf[i], th::CY, 36);
        }
    }

    /// Izgara çizgilerinin katman içindeki satırları ve dB değerleri (10 dB'de bir)
    fn grid(&self) -> Vec<(usize, f32)> {
        let bottom = self.bottom;
        let range = self.set.spec_range as f32;
        // draw_spectrum'daki to_y ile aynı ölçek (katman koordinatı)
        let to_y = |v: f32| (SPEC_H - 3) as f32 - (v - bottom) / range * (SPEC_H - 8) as f32;
        let mut out = Vec::new();
        let mut g = (bottom / 10.0).ceil() * 10.0;
        while g < bottom + range {
            let y = to_y(g).round();
            if y >= 6.0 && y < (SPEC_H - 2) as f32 {
                out.push((y as usize, g));
            }
            g += 10.0;
        }
        out
    }

    fn grid_rows(&self) -> Vec<usize> {
        self.grid().into_iter().map(|(y, _)| y).collect()
    }

    /// Üst katman: dBm etiketleri (koyu zeminli), ayar frekansında kesikli
    /// kehribar çizgi ve frekans etiketi. İzin üstünde kalır.
    fn build_overlay(&mut self, dial: Option<u64>) {
        // Eski etiketin kapladığı sütunlar yeniden çizilsin
        for x in DIAL_X as usize - 45..=DIAL_X as usize + 45 {
            self.force[x] = true;
        }
        let grid = self.grid();
        let cal = self.cal;
        let o = &mut self.over;
        o.buf.fill(0);
        for (y, g) in grid {
            let dbm = (g + cal).round() as i32;
            let s = format!("{}{}", if dbm < 0 { "−" } else { "" }, dbm.abs());
            let w = gfx::text_w(&F_R11, &s) + 8;
            let yy = y as i32;
            gfx::fill(o, 2, yy - 7, w, 13, th::SPEC_BG);
            rrect(o, 2, yy - 7, w, 13, 4, th::PN);
            text(o, &F_R11, 6, yy + 4, &s, th::MU);
        }
        for y in 20..SPEC_H {
            if y % 7 < 4 {
                o.buf[y * W + DIAL_X as usize] = th::AM;
            }
        }
        if let Some(d) = dial {
            let s = fmt_freq(d);
            let w = gfx::text_w(&F_B12, &s) + 14;
            // Etiket önce spektrum zemin rengine boyanır ki yuvarlak köşelerin
            // yumuşatması siyaha (saydam) değil zemine karışsın
            gfx::fill(o, DIAL_X - w / 2, 3, w, 15, th::SPEC_BG);
            rrect(o, DIAL_X - w / 2, 3, w, 15, 7, th::AM);
            text_c(o, &F_B12, DIAL_X, 15, &s, th::ON_AM);
        }
    }

    // ------------------------------------------------------------- kare

    /// Yeni bir spektrum karesini çizer ve şelaleye bir satır ekler.
    pub fn frame(&mut self, fb: &mut Framebuffer, db: &mut [f32], dial: Option<u64>) {
        // LO sızıntısını komşu bin'lerin ortalamasıyla ört (şelalede de görünmesin)
        let c = N / 2;
        let edge = (db[c - 17] + db[c - 16] + db[c + 16] + db[c + 17]) / 4.0;
        for v in &mut db[c - 15..=c + 15] {
            *v = edge;
        }

        let a = self.set.smooth as f32 / 100.0;
        for x in 0..W {
            let b0 = x * N / W;
            let b1 = ((x + 1) * N / W).max(b0 + 1);
            self.col[x] = db[b0..b1].iter().cloned().fold(f32::MIN, f32::max);
            self.smooth[x] = self.smooth[x] * a + self.col[x] * (1.0 - a);
        }

        // Gürültü tabanı: sütunların %20'lik dilimi; yavaşça izlenir
        self.sorted.copy_from_slice(&self.col);
        self.sorted.sort_unstable_by(|a, b| a.total_cmp(b));
        let nf = self.sorted[W / 5];
        self.floor += (nf - self.floor) * 0.05;
        // Alt sınır gürültü tabanının ~15 dB altında (iz panelin dibine yapışmasın),
        // 5 dB adımlarla ve histerezisle değişir (ızgara etiketleri oynamasın)
        let target = self.floor - 15.0;
        let want = (target / 5.0).floor() * 5.0;
        if (want - self.bottom).abs() >= 5.0 && (target - self.bottom - 2.5).abs() > 4.0 {
            self.bottom = want;
        }

        let key = (self.bottom as i32, self.pb, self.set.spec_range);
        let layers_changed = self.layers_for != Some(key);
        if layers_changed {
            self.build_layers();
            self.layers_for = Some(key);
            self.full_redraw = true;
        }
        if layers_changed || self.over_for != Some(dial.unwrap_or(0)) {
            self.build_overlay(dial);
            self.over_for = Some(dial.unwrap_or(0));
        }

        if !self.spectrum_hidden {
            let t0 = std::time::Instant::now();
            self.draw_spectrum(fb);
            self.t_spec_us += t0.elapsed().as_micros() as u64;
        }

        // Şelale satırı her wf_every karede bir; aradaki karelerin en büyüğü
        for x in 0..W {
            self.wf_acc[x] = self.wf_acc[x].max(self.col[x]);
        }
        self.wf_count += 1;
        if self.wf_count >= self.set.wf_every as u32 {
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

    /// Spektrumu çizer: her sütunda yalnız önceki kareye göre değişebilecek
    /// pikseller yazılır; renkler önceden hazırlanmış katmanlardan gelir.
    fn draw_spectrum(&mut self, fb: &mut Framebuffer) {
        let range = self.set.spec_range as f32;
        let bottom = self.bottom;
        let y_bot = SPEC_Y + SPEC_H - 3;
        let to_y = |v: f32| -> usize {
            let t = ((v - bottom) / range).clamp(0.0, 1.0);
            y_bot - (t * (SPEC_H - 8) as f32) as usize
        };
        core::mem::swap(&mut self.ty, &mut self.pty);
        core::mem::swap(&mut self.seg, &mut self.pseg);
        let mut prev = to_y(self.smooth[0]);
        for x in 0..W {
            if (LO_X0..=LO_X1).contains(&x) {
                // LO bölgesinde iz yok
                self.ty[x] = SPEC_Y + SPEC_H;
                self.seg[x] = (usize::MAX, 0);
                prev = to_y(self.smooth[(LO_X1 + 1).min(W - 1)]);
                continue;
            }
            let y = to_y(self.smooth[x]);
            self.ty[x] = y;
            self.seg[x] = if prev < y { (prev, y) } else { (y, prev) };
            prev = y;
        }

        let full = self.full_redraw;
        self.full_redraw = false;
        let cursor = self.cursor;
        for x in 0..W {
            let (s0, s1) = self.seg[x];
            let ty = self.ty[x];
            let (lo, hi) = if full || self.force[x] {
                (SPEC_Y, SPEC_Y + SPEC_H - 1)
            } else {
                let (p0, p1) = self.pseg[x];
                let lo = s0.min(p0).max(SPEC_Y);
                let hi = s1.max(p1).max(ty.min(self.pty[x])).min(SPEC_Y + SPEC_H - 1);
                if lo > hi {
                    continue;
                }
                (lo, hi)
            };
            self.force[x] = false;
            let is_cursor = cursor == Some(x);
            for y in lo..=hi {
                let li = (y - SPEC_Y) * W + x;
                let ov = self.over.buf[li];
                fb.buf[y * W + x] = if is_cursor {
                    th::CY
                } else if ov != 0 {
                    ov
                } else if y >= s0 && y <= s1 {
                    th::CY
                } else if y > ty {
                    self.bgfill.buf[li]
                } else {
                    self.bg.buf[li]
                };
            }
        }
    }

    fn waterfall_line(&mut self, fb: &mut Framebuffer, col: &[f32], dial: Option<u64>) {
        // Frekans değiştiyse eski satırları yatayda kaydır ki sinyaller hizalı kalsın
        match (dial, self.wf_dial) {
            (Some(d), Some(old)) => {
                let shift = (old as i64 - d as i64) / HZ_PER_PX;
                if shift != 0 {
                    shift_rows(fb, shift, self.wf_h);
                    self.wf_dial = Some((old as i64 - shift * HZ_PER_PX) as u64);
                }
            }
            (Some(d), None) => self.wf_dial = Some(d),
            (None, _) => {}
        }
        let start = WF_Y * W;
        let end = (WF_Y + self.wf_h - 1) * W;
        fb.buf.copy_within(start..end, start + W);
        let lo = self.floor - 3.0 - self.set.wf_bright as f32;
        let span = self.set.wf_span as f32;
        for x in 0..W {
            fb.buf[start + x] = if (LO_X0..=LO_X1).contains(&x) {
                th::LO_BG
            } else {
                let t = ((col[x] - lo) / span).clamp(0.0, 1.0);
                self.palette[(t * 255.0) as usize]
            };
        }
    }
}

/// Şelaleyi yatayda `shift` piksel kaydırır (+ sağa), açılan yeri siyah yapar.
fn shift_rows(fb: &mut Framebuffer, shift: i64, h: usize) {
    let s = shift.unsigned_abs() as usize;
    for y in WF_Y..WF_Y + h {
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

// ------------------------------------------------------------ band penceresi
// Spektrum + ölçek + şelale alanının üstünde açılır; açıkken o alan çizilmez.

const BP_X: i32 = 16;
const BP_Y: i32 = SPEC_Y as i32 + 22;
const BP_W: i32 = W as i32 - 32;
const BP_H: i32 = 210;
const BB_W: i32 = 114;
const BB_H: i32 = 62;
const BB_GAP: i32 = 12;
const BB_COLS: usize = 6;

/// Band penceresindeki "SET" (görüntü ayarları) düğmesinin sırası
pub const SET_BUTTON: usize = crate::bands::BANDS.len();

fn band_button_rect(i: usize) -> (i32, i32) {
    let col = (i % BB_COLS) as i32;
    let row = (i / BB_COLS) as i32;
    (BP_X + 14 + col * (BB_W + BB_GAP), BP_Y + 52 + row * (BB_H + BB_GAP))
}

pub fn draw_band_popup(fb: &mut Framebuffer, current: Option<usize>) {
    gfx::fill(fb, 0, SPEC_Y as i32, W as i32, (WF_Y + WF_H - SPEC_Y) as i32, th::BG);
    rrect(fb, BP_X - 1, BP_Y - 1, BP_W + 2, BP_H + 2, 11, th::CY);
    rrect(fb, BP_X, BP_Y, BP_W, BP_H, 10, th::PN);
    text(fb, &F_B16, BP_X + 16, BP_Y + 30, "Band", th::TX);
    text_r(fb, &F_R14, BP_X + BP_W - 16, BP_Y + 30, "tap outside to close", th::MU);
    for (i, b) in crate::bands::BANDS.iter().enumerate() {
        let (x, y) = band_button_rect(i);
        let sel = current == Some(i);
        rrect(fb, x, y, BB_W, BB_H, 8, if sel { th::AM } else { th::PN2 });
        text_c(fb, &F_B30, x + BB_W / 2, y + 36, b.name, if sel { th::ON_AM } else { th::TX });
        text_c(fb, &F_R11, x + BB_W / 2, y + 54, "m", if sel { th::ON_AM } else { th::MU });
    }
    let (x, y) = band_button_rect(SET_BUTTON);
    rrect(fb, x, y, BB_W, BB_H, 8, th::PN2);
    text_c(fb, &F_B16, x + BB_W / 2, y + 30, "DISPLAY", th::CY);
    text_c(fb, &F_R11, x + BB_W / 2, y + 48, "settings", th::MU);
}

/// Band penceresinde dokunulan yer: Some(Some(i)) düğme, Some(None) pencere içi boşluk,
/// None pencere dışı.
pub fn band_popup_hit(x: i32, y: i32) -> Option<Option<usize>> {
    if x < BP_X || x >= BP_X + BP_W || y < BP_Y || y >= BP_Y + BP_H {
        return None;
    }
    Some((0..=SET_BUTTON).find(|&i| {
        let (bx, by) = band_button_rect(i);
        x >= bx && x < bx + BB_W && y >= by && y < by + BB_H
    }))
}

/// Pencere kapanınca altında kalan alanı temizler (spektrum ve şelale yeniden dolar).
pub fn clear_band_popup(fb: &mut Framebuffer) {
    gfx::fill(fb, 0, SPEC_Y as i32, W as i32, (WF_Y + WF_H - SPEC_Y) as i32, th::BG);
    gfx::fill(fb, 0, WF_Y as i32, W as i32, WF_H as i32, 0);
}

// ------------------------------------------------------------- ayar paneli
// Spektrum + ölçek alanının üstünde açılır; şelale altta akmaya devam eder.

const SP_Y: i32 = SPEC_Y as i32;
const SP_H: i32 = (WF_Y - SPEC_Y) as i32 - 2;
const SP_ROW0: i32 = SP_Y + 34;
const SP_ROW_H: i32 = 42;
const SP_COL_W: i32 = 392;
const SP_MINUS_X: i32 = 150;
const SP_PLUS_X: i32 = 316;
const SP_BTN: (i32, i32) = (48, 34);
const SP_RESET: (i32, i32, i32, i32) = (W as i32 - 112, SP_Y + 6, 100, 24);

const SET_LABELS: [&str; 6] = [
    "WF bright",
    "WF contrast",
    "Spec range",
    "WF speed",
    "Palette",
    "Smoothing",
];

pub enum SettingsHit {
    Step(usize, bool),
    Reset,
    Inside,
    Outside,
}

fn set_cell(i: usize) -> (i32, i32) {
    (8 + (i % 2) as i32 * SP_COL_W, SP_ROW0 + (i / 2) as i32 * SP_ROW_H)
}

fn set_value(s: &Settings, i: usize) -> String {
    match i {
        0 => format!("{:+} dB", s.wf_bright),
        1 | 2 => format!("{} dB", s.get(i)),
        3 => format!("{:.0} /s", 23.4 / s.wf_every as f32),
        4 => PALETTES[s.palette as usize].0.to_string(),
        _ => format!("{}%", s.smooth),
    }
}

pub fn draw_settings(fb: &mut Framebuffer, s: &Settings) {
    gfx::fill(fb, 0, SP_Y, W as i32, SP_H + 2, th::BG);
    rrect(fb, 0, SP_Y, W as i32, SP_H, 10, th::CY);
    rrect(fb, 1, SP_Y + 1, W as i32 - 2, SP_H - 2, 9, th::PN);
    text(fb, &F_B16, 14, SP_Y + 24, "Display settings", th::TX);
    text(fb, &F_R14, 170, SP_Y + 24, "tap the waterfall to close", th::MU);
    let (rx, ry, rw, rh) = SP_RESET;
    rrect(fb, rx, ry, rw, rh, 12, th::PN2);
    text_c(fb, &F_B14, rx + rw / 2, ry + 17, "Reset", th::TX);

    for i in 0..6 {
        let (x, y) = set_cell(i);
        text(fb, &F_B14, x + 8, y + 23, SET_LABELS[i], th::MU);
        for (bx, sym) in [(SP_MINUS_X, if i == 4 { "<" } else { "−" }), (SP_PLUS_X, if i == 4 { ">" } else { "+" })] {
            rrect(fb, x + bx, y, SP_BTN.0, SP_BTN.1, 8, th::PN2);
            text_c(fb, &F_B16, x + bx + SP_BTN.0 / 2, y + 23, sym, th::CY);
        }
        let v = set_value(s, i);
        let cx = x + (SP_MINUS_X + SP_BTN.0 + SP_PLUS_X) / 2;
        text_c(fb, &F_B16, cx, y + 23, &v, th::TX);
    }
}

pub fn settings_hit(x: i32, y: i32) -> SettingsHit {
    if y < SP_Y || y >= SP_Y + SP_H {
        return SettingsHit::Outside;
    }
    let (rx, ry, rw, rh) = SP_RESET;
    if x >= rx && x < rx + rw && y >= ry - 4 && y < ry + rh + 4 {
        return SettingsHit::Reset;
    }
    for i in 0..6 {
        let (cx, cy) = set_cell(i);
        for (bx, up) in [(SP_MINUS_X, false), (SP_PLUS_X, true)] {
            if x >= cx + bx - 8 && x < cx + bx + SP_BTN.0 + 8 && y >= cy - 4 && y < cy + SP_BTN.1 + 4 {
                return SettingsHit::Step(i, up);
            }
        }
    }
    SettingsHit::Inside
}

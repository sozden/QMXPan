//! PROPAGATION sayfası: gri hatlı dünya haritası, güneş verisi kartları ve
//! band grupları için gündüz/gece yayılım tahmini (N0NBH).
//!
//! Harita: Natural Earth 1:110m kara poligonlarından (kamu malı) üretilmiş
//! 520x260 eşdikdörtgen kara maskesi (tools/landmask.ps1). Gündüz/gece sınırı
//! her dakika güneşin tepe noktasından hesaplanır.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::fonts::{F_B12, F_B14, F_B16, F_B30, F_R11};
use crate::gfx::{blend, rgb, rrect, text, text_c, text_r, th};
use crate::lcd::{Framebuffer, W};
use crate::meters::{draw_tabs, Tab};
use crate::solar::{self, Solar};

const LAND: &[u8] = include_bytes!("../assets/land_520x260.bin");
const MW: i32 = 520;
const MH: i32 = 260;
const MX: i32 = 8;
const MY: i32 = 38;

/// Sağdaki kartlar
const CX: i32 = 538;
const CW: i32 = 124;
const CH: i32 = 61;
/// Alttaki band tablosu
const TY: i32 = 308;

fn is_land(x: i32, y: i32) -> bool {
    let i = (y * 65 + x / 8) as usize;
    LAND[i] & (0x80 >> (x % 8)) != 0
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|t| t.as_secs()).unwrap_or(0)
}

fn time_ok(t: u64) -> bool {
    t > 1_735_689_600
}

pub struct Prop {
    map: Vec<u16>,
    map_min: u64,
    map_qth: String,
    gen: u32,
    state: String,
}

impl Prop {
    pub fn new() -> Prop {
        Prop {
            map: vec![0; (MW * MH) as usize],
            map_min: u64::MAX,
            map_qth: String::new(),
            gen: u32::MAX,
            state: String::new(),
        }
    }

    pub fn enter(&mut self, fb: &mut Framebuffer) {
        fb.buf.fill(th::BG);
        crate::gfx::fill(fb, 0, 0, W as i32, 30, th::PN);
        let x = text(fb, &F_B16, 12, 21, "QMXPan", th::CY);
        text(fb, &F_B14, x + 12, 20, "PROPAGATION", th::TX);
        rrect(fb, MX - 2, MY - 2, MW + 4, MH + 4, 6, th::LN);
        draw_tabs(fb, Tab::Prop);
        self.map_min = u64::MAX;
        self.gen = u32::MAX;
    }

    pub fn update(&mut self, fb: &mut Framebuffer) {
        let t = now();
        let qth = solar::QTH.lock().unwrap().clone();
        if t / 60 != self.map_min || qth != self.map_qth {
            self.map_min = t / 60;
            self.map_qth = qth.clone();
            self.draw_map(fb, t, &qth);
        }
        let gen = solar::GEN.load(std::sync::atomic::Ordering::SeqCst);
        // Veri yokken bağlantı durumu değiştikçe de yenile
        let st = solar::STATE.lock().unwrap().clone();
        if gen != self.gen || (gen == 0 && st != self.state) {
            self.gen = gen;
            self.state = st;
            let s = solar::DATA.lock().unwrap().clone();
            draw_cards(fb, s.as_ref());
            draw_table(fb, s.as_ref());
        }
    }

    fn draw_map(&mut self, fb: &mut Framebuffer, t: u64, qth: &str) {
        let ok = time_ok(t);
        let (dec, slon) = solar::subsolar(t);
        let rad = core::f32::consts::PI / 180.0;
        let (sd, cd) = ((dec * rad).sin(), (dec * rad).cos());
        // Sütun başına cos(boylam farkı), satır başına sin/cos(enlem)
        let cosdl: Vec<f32> = (0..MW)
            .map(|x| {
                let lon = (x as f32 + 0.5) / MW as f32 * 360.0 - 180.0;
                ((lon - slon) * rad).cos()
            })
            .collect();
        let (oc_d, ld_d) = (0x1b3a5e, 0x3d6b4c);
        let (oc_n, ld_n) = (0x0a1428, 0x172a22);
        let grey = rgb(0xffb347);
        let grid = rgb(0x8494b2);
        for y in 0..MH {
            let lat = 90.0 - (y as f32 + 0.5) / MH as f32 * 180.0;
            let (sl, cl) = ((lat * rad).sin(), (lat * rad).cos());
            let row = (y * MW) as usize;
            for x in 0..MW {
                let land = is_land(x, y);
                let c = if ok {
                    // Güneşin yükseklik açısı (derece, ufka yakın yerde yeterli yaklaşım)
                    let s = sl * sd + cl * cd * cosdl[x as usize];
                    let e = s.clamp(-1.0, 1.0).asin() / rad;
                    let k = ((e + 6.0) / 6.5).clamp(0.0, 1.0);
                    let base = if land { mix(ld_n, ld_d, k) } else { mix(oc_n, oc_d, k) };
                    let mut c = rgb(base);
                    // Gri hat: alacakaranlık şeridinde kehribar ışıltı
                    let g = 1.0 - ((e + 3.0).abs() / 4.0);
                    if g > 0.0 {
                        c = blend(c, grey, (g * 70.0) as u32);
                    }
                    c
                } else {
                    rgb(if land { ld_d } else { oc_d })
                };
                self.map[row + x as usize] = c;
            }
        }
        // Enlem/boylam ızgarası (30°), ekvator biraz daha belirgin
        for gx in (0..=360).step_by(30) {
            let x = (gx * MW / 360).min(MW - 1);
            for y in 0..MH {
                let i = (y * MW + x) as usize;
                self.map[i] = blend(self.map[i], grid, 40);
            }
        }
        for gy in (0..=180).step_by(30) {
            let y = (gy * MH / 180).min(MH - 1);
            let a = if gy == 90 { 80 } else { 40 };
            for x in 0..MW {
                let i = (y * MW + x) as usize;
                self.map[i] = blend(self.map[i], grid, a);
            }
        }
        let to_px = |lat: f32, lon: f32| ((lon + 180.0) / 360.0 * MW as f32, (90.0 - lat) / 180.0 * MH as f32);
        // Güneşin tepe noktası
        if ok {
            let (sx, sy) = to_px(dec, slon);
            self.dot(sx, sy, 9.0, rgb(0xffd23f), 50);
            self.dot(sx, sy, 5.0, rgb(0xffd23f), 255);
            self.dot(sx, sy, 2.5, rgb(0xfff4c0), 255);
        }
        // Kendi konumumuz
        if let Some((lat, lon)) = solar::locator(qth) {
            let (qx, qy) = to_px(lat, lon);
            self.dot(qx, qy, 6.0, th::CY, 90);
            self.dot(qx, qy, 3.5, th::CY, 255);
            self.dot(qx, qy, 1.5, th::ON_CY, 255);
        }
        for y in 0..MH {
            let o = (MY + y) as usize * W + MX as usize;
            let s = (y * MW) as usize;
            fb.buf[o..o + MW as usize].copy_from_slice(&self.map[s..s + MW as usize]);
        }
    }

    fn dot(&mut self, cx: f32, cy: f32, r: f32, c: u16, amax: u32) {
        let (x0, x1) = ((cx - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32);
        let (y0, y1) = ((cy - r - 1.0).floor() as i32, (cy + r + 1.0).ceil() as i32);
        for y in y0.max(0)..=y1.min(MH - 1) {
            for xx in x0..=x1 {
                // Harita doğu-batı yönünde sarar
                let x = xx.rem_euclid(MW);
                let (dx, dy) = (xx as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let cov = (r + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let i = (y * MW + x) as usize;
                    self.map[i] = blend(self.map[i], c, (cov * amax as f32) as u32);
                }
            }
        }
    }
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let x = ((a >> s) & 0xFF) as f32;
        let y = ((b >> s) & 0xFF) as f32;
        ((x + (y - x) * t + 0.5) as u32).min(255) << s
    };
    ch(16) | ch(8) | ch(0)
}

fn num(s: &str) -> Option<f32> {
    s.trim().parse::<f32>().ok()
}

/// Değere göre renk: iyi yeşil, orta kehribar, kötü kırmızı
fn grade(good: bool, fair: bool) -> u16 {
    if good {
        th::GR
    } else if fair {
        th::AM
    } else {
        th::RD
    }
}

fn card(fb: &mut Framebuffer, i: i32, label: &str, val: &str, unit: &str, c: u16) {
    let x = CX + (i % 2) * (CW + 6);
    let y = MY - 2 + (i / 2) * (CH + 5);
    rrect(fb, x, y, CW, CH, 8, th::PN);
    text(fb, &F_B12, x + 10, y + 17, label, th::MU);
    let e = text(fb, &F_B30, x + 10, y + 50, val, c);
    if !unit.is_empty() {
        text(fb, &F_B12, e + 4, y + 50, unit, th::MU);
    }
}

fn draw_cards(fb: &mut Framebuffer, s: Option<&Solar>) {
    let Some(s) = s else {
        for (i, l) in ["SOLAR FLUX", "SUNSPOTS", "A INDEX", "K INDEX", "X-RAY", "SOLAR WIND", "BZ", "NOISE"].iter().enumerate() {
            card(fb, i as i32, l, "—", "", th::MU);
        }
        return;
    };
    let dash = |v: &str| if v.is_empty() { "—".to_string() } else { v.to_string() };
    let sfi = num(&s.sfi);
    card(fb, 0, "SOLAR FLUX", &dash(&s.sfi), "", sfi.map_or(th::MU, |v| grade(v >= 120.0, v >= 90.0)));
    card(fb, 1, "SUNSPOTS", &dash(&s.ssn), "", th::TX);
    let a = num(&s.a);
    card(fb, 2, "A INDEX", &dash(&s.a), "", a.map_or(th::MU, |v| grade(v < 8.0, v < 20.0)));
    let k = num(&s.k);
    card(fb, 3, "K INDEX", &dash(&s.k), "", k.map_or(th::MU, |v| grade(v <= 2.0, v <= 4.0)));
    let xc = s.xray.chars().next().unwrap_or(' ');
    card(fb, 4, "X-RAY", &dash(&s.xray), "", grade(matches!(xc, 'A' | 'B'), xc == 'C'));
    let w = num(&s.wind);
    card(
        fb,
        5,
        "SOLAR WIND",
        &w.map_or("—".into(), |v| format!("{v:.0}")),
        "km/s",
        w.map_or(th::MU, |v| grade(v < 450.0, v < 600.0)),
    );
    let bz = num(&s.bz);
    card(
        fb,
        6,
        "BZ",
        &bz.map_or("—".into(), |v| format!("{v:.1}")),
        "nT",
        bz.map_or(th::MU, |v| grade(v > -5.0, v > -10.0)),
    );
    card(fb, 7, "NOISE", &dash(&s.noise), "", th::TX);
}

fn pill(fb: &mut Framebuffer, x: i32, y: i32, w: i32, cond: &str) {
    let (bg, fg) = match cond {
        "Good" => (th::GR, th::ON_CY),
        "Fair" => (th::AM, th::ON_AM),
        "Poor" => (th::RD, rgb(0x2a0808)),
        _ => (th::PN2, th::MU),
    };
    rrect(fb, x, y, w, 30, 15, bg);
    let l = if cond.is_empty() { "—" } else { cond };
    text_c(fb, &F_B14, x + w / 2, y + 20, l, fg);
}

fn draw_table(fb: &mut Framebuffer, s: Option<&Solar>) {
    crate::gfx::fill(fb, 0, TY, W as i32, 440 - TY - 2, th::BG);
    rrect(fb, 8, TY, 560, 118, 10, th::PN);
    // Başlıklar
    let col = |i: i32| 100 + i * 116;
    text(fb, &F_B12, 22, TY + 22, "BANDS", th::MU);
    for (i, g) in solar::GROUPS.iter().enumerate() {
        text_c(fb, &F_B14, col(i as i32) + 52, TY + 22, g, th::TX);
    }
    text(fb, &F_B14, 22, TY + 56, "DAY", th::AM);
    text(fb, &F_B14, 22, TY + 96, "NIGHT", th::VI);
    for i in 0..4 {
        let (d, n) = s.map(|s| (s.bands[i].0.as_str(), s.bands[i].1.as_str())).unwrap_or(("", ""));
        pill(fb, col(i as i32), TY + 36, 104, d);
        pill(fb, col(i as i32), TY + 76, 104, n);
    }
    // Sağda: jeomanyetik alan, aurora, MUF ve kaynak
    let x = 578;
    rrect(fb, x, TY, 214, 118, 10, th::PN);
    let row = |fb: &mut Framebuffer, y: i32, l: &str, v: &str, c: u16| {
        text(fb, &F_B12, x + 12, y, l, th::MU);
        text_r(fb, &F_B14, x + 202, y, v, c);
    };
    match s {
        Some(s) => {
            let gm = s.geomag.as_str();
            let gc = match gm {
                "VR QUIET" | "QUIET" | "INACTIVE" => th::GR,
                "UNSETTLD" | "ACTIVE" => th::AM,
                _ => th::RD,
            };
            row(fb, TY + 24, "GEOMAG FIELD", gm, gc);
            row(fb, TY + 48, "AURORA", &s.aurora, th::TX);
            let muf = match s.muf.as_str() {
                "" | "NoRpt" => "no report".to_string(),
                m => format!("{m} MHz"),
            };
            row(fb, TY + 72, "MUF", &muf, th::TX);
            let upd = s.updated.split_whitespace().rev().nth(1).unwrap_or("").to_string();
            text(fb, &F_R11, x + 12, TY + 104, &format!("N0NBH · {upd} UTC"), th::MU);
        }
        None => {
            let st = solar::STATE.lock().unwrap().clone();
            text(fb, &F_B14, x + 12, TY + 30, "No data yet", th::AM);
            text(fb, &F_R11, x + 12, TY + 52, &st, th::MU);
            text(fb, &F_R11, x + 12, TY + 104, "Source: N0NBH (hamqsl.com)", th::MU);
        }
    }
}

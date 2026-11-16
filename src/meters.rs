//! METERS sayfası: klasik analog kadranlar (SWR, S-metre, güç), Nixie tüplü
//! frekans ve UTC saati, kehribar spektrum şeridi.
//!
//! Durağan her şey (zemin, kadranlar, ölçekler, yazılar) açılışta bir kez ekran
//! dışı bir katmana çizilir. Sonra yalnızca ibreler, tüpler ve değerler
//! güncellenir. İbre, kadranın bir kopyası üzerinde hazırlanıp tek seferde
//! ekrana kopyalanır; böylece titremez.
//!
//! Nixie rakamları bir yazı tipinden gelmez: her rakam, buradaki çizgi ve yay
//! tanımlarından (tüpteki katot tellerine benzer biçimde) üretilir.

use std::f32::consts::{PI, TAU};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::fonts::{F_B12, F_B14, F_B16, F_B30, F_R11};
use crate::gfx::{blend, rgb, text, text_c, text_r, th};
use crate::lcd::{Framebuffer, H, W};

// ------------------------------------------------------------ renk yardımcıları

/// İki 0xRRGGBB renk arasında doğrusal geçiş
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |s: u32| {
        let x = ((a >> s) & 0xFF) as f32;
        let y = ((b >> s) & 0xFF) as f32;
        ((x + (y - x) * t + 0.5) as u32).min(255) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// Sayfa zemini (yukarıdan aşağı koyulaşan fırçalanmış metal)
fn bg_at(y: i32) -> u32 {
    mix(0x1a1d24, 0x0d0f13, y as f32 / H as f32)
}

/// Yuvarlak köşeli dikdörtgenin (x, y noktasında) kaplama oranı 0..1
fn rr_cov(x: f32, y: f32, rx: f32, ry: f32, w: f32, h: f32, r: f32) -> f32 {
    let qx = (x - (rx + w / 2.0)).abs() - (w / 2.0 - r);
    let qy = (y - (ry + h / 2.0)).abs() - (h / 2.0 - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    let d = (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r;
    (0.5 - d).clamp(0.0, 1.0)
}

/// Bir noktanın doğru parçasına uzaklığı
#[inline]
fn seg_dist(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let (qx, qy) = (px - x0, py - y0);
    let l2 = (dx * dx + dy * dy).max(1e-6);
    let t = ((qx * dx + qy * dy) / l2).clamp(0.0, 1.0);
    let (ex, ey) = (qx - t * dx, qy - t * dy);
    (ex * ex + ey * ey).sqrt()
}

// ------------------------------------------------------------ çizim yüzeyi

/// Ekran koordinatlarıyla çizilen bir tampon (tam ekran ya da bir parçası)
struct Cv<'a> {
    b: &'a mut [u16],
    w: i32,
    h: i32,
    ox: i32,
    oy: i32,
}

impl<'a> Cv<'a> {
    fn of(fb: &'a mut Framebuffer) -> Self {
        let h = fb.h as i32;
        Cv { b: &mut *fb.buf, w: W as i32, h, ox: 0, oy: 0 }
    }

    #[inline]
    fn px(&mut self, x: i32, y: i32, c: u16, a: u32) {
        let (lx, ly) = (x - self.ox, y - self.oy);
        if lx >= 0 && ly >= 0 && lx < self.w && ly < self.h {
            let i = (ly * self.w + lx) as usize;
            self.b[i] = blend(self.b[i], c, a);
        }
    }

    /// Tampona düşen kutu sınırları (x0, x1, y0, y1; sonlar dahil)
    fn clip(&self, x0: f32, x1: f32, y0: f32, y1: f32) -> (i32, i32, i32, i32) {
        (
            (x0.floor() as i32).max(self.ox),
            (x1.ceil() as i32).min(self.ox + self.w - 1),
            (y0.floor() as i32).max(self.oy),
            (y1.ceil() as i32).min(self.oy + self.h - 1),
        )
    }

    /// Kalınlığı w olan yumuşatılmış doğru parçası (amax: en yüksek örtme)
    fn seg(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, c: u16, amax: f32) {
        let r = w / 2.0 + 1.0;
        let (bx0, bx1, by0, by1) = self.clip(x0.min(x1) - r, x0.max(x1) + r, y0.min(y1) - r, y0.max(y1) + r);
        for y in by0..=by1 {
            for x in bx0..=bx1 {
                let d = seg_dist(x as f32 + 0.5, y as f32 + 0.5, x0, y0, x1, y1);
                let cov = (w / 2.0 + 0.5 - d).clamp(0.0, 1.0) * amax;
                if cov > 0.0 {
                    self.px(x, y, c, (cov * 255.0) as u32);
                }
            }
        }
    }

    /// Merkezi (cx, cy) olan yayın üst yarısı, a0..a1 açıları arasında (radyan,
    /// y aşağı: π..2π üst yarıdır)
    fn arc(&mut self, cx: f32, cy: f32, r: f32, w: f32, a0: f32, a1: f32, c: u16) {
        let ro = r + w / 2.0 + 1.0;
        let (bx0, bx1, by0, by1) = self.clip(cx - ro, cx + ro, cy - ro, cy + 1.0);
        for y in by0..=by1 {
            for x in bx0..=bx1 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let d = (dx * dx + dy * dy).sqrt();
                let cov = (w / 2.0 + 0.5 - (d - r).abs()).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let mut a = dy.atan2(dx);
                if a < 0.0 {
                    a += TAU;
                }
                if a >= a0 && a <= a1 {
                    self.px(x, y, c, (cov * 255.0) as u32);
                }
            }
        }
    }

    /// Yumuşatılmış dolu daire
    fn disc(&mut self, cx: f32, cy: f32, r: f32, c: u16, amax: f32) {
        let (bx0, bx1, by0, by1) = self.clip(cx - r - 1.0, cx + r + 1.0, cy - r - 1.0, cy + r + 1.0);
        for y in by0..=by1 {
            for x in bx0..=bx1 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let cov = (r + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0) * amax;
                if cov > 0.0 {
                    self.px(x, y, c, (cov * 255.0) as u32);
                }
            }
        }
    }

    /// Neon ışıltısı: yumuşak turuncu hale + parlak çekirdek
    fn neon(&mut self, cx: f32, cy: f32, r: f32) {
        let gr = r * 3.2;
        let (bx0, bx1, by0, by1) = self.clip(cx - gr, cx + gr, cy - gr, cy + gr);
        let glow = rgb(0xff5a00);
        for y in by0..=by1 {
            for x in bx0..=bx1 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let d = (dx * dx + dy * dy).sqrt();
                let a = 0.55 * (-(d - r).max(0.0) / (r * 0.9)).exp();
                self.px(x, y, glow, (a * 255.0) as u32);
            }
        }
        self.disc(cx, cy, r, rgb(0xff8a2a), 1.0);
        self.disc(cx - r * 0.2, cy - r * 0.2, r * 0.45, rgb(0xffd0a0), 0.8);
    }
}

/// Base katmanın bir dikdörtgenini ekrana kopyalar (eski çizimleri siler)
fn restore(base: &Framebuffer, fb: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32) {
    let x0 = x.max(0) as usize;
    let x1 = (x + w).min(W as i32).max(0) as usize;
    for yy in y.max(0)..(y + h).min(H as i32) {
        let o = yy as usize * W;
        fb.buf[o + x0..o + x1].copy_from_slice(&base.buf[o + x0..o + x1]);
    }
}

// ------------------------------------------------------------ Nixie rakamları

/// Elips yayı üzerindeki noktalar (derece; y aşağı, 270° tepe)
fn arc_pts(cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
    let n = ((a1 - a0).abs() / 10.0).ceil().max(2.0) as i32;
    (0..=n)
        .map(|i| {
            let a = (a0 + (a1 - a0) * i as f32 / n as f32) * PI / 180.0;
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

/// Bir rakamın katot telleri: 10 × 16 birimlik kutuda çoklu çizgiler
fn glyph(d: u8) -> Vec<Vec<(f32, f32)>> {
    match d {
        0 => vec![arc_pts(5.0, 8.0, 4.2, 7.2, 0.0, 360.0)],
        1 => vec![vec![(3.0, 3.0), (5.2, 0.8), (5.2, 15.2)]],
        2 => {
            let mut p = arc_pts(5.0, 4.8, 4.0, 4.0, 195.0, 385.0);
            p.extend([(0.8, 15.2), (9.2, 15.2)]);
            vec![p]
        }
        3 => vec![
            arc_pts(5.0, 4.3, 3.7, 3.5, 200.0, 450.0),
            arc_pts(5.0, 11.5, 4.1, 3.7, 270.0, 520.0),
        ],
        4 => vec![vec![(7.2, 15.2), (7.2, 0.8), (0.8, 11.0), (9.4, 11.0)]],
        5 => {
            let mut p = vec![(9.0, 0.8), (1.8, 0.8)];
            p.extend(arc_pts(5.0, 10.8, 4.3, 4.4, 210.0, 500.0));
            vec![p]
        }
        6 => vec![
            arc_pts(5.0, 11.0, 4.2, 4.2, 0.0, 360.0),
            arc_pts(11.5, 11.0, 10.4, 10.4, 185.0, 245.0),
        ],
        7 => vec![vec![(0.8, 0.8), (9.2, 0.8), (3.8, 15.2)]],
        8 => vec![
            arc_pts(5.0, 4.1, 3.6, 3.3, 0.0, 360.0),
            arc_pts(5.0, 11.4, 4.2, 3.8, 0.0, 360.0),
        ],
        // 9: ters çevrilmiş 6
        _ => glyph(6)
            .into_iter()
            .map(|l| l.into_iter().map(|(x, y)| (10.0 - x, 16.0 - y)).collect())
            .collect(),
    }
}

/// Rakam tellerini piksel koordinatına taşır ve doğru parçalarına böler
fn glyph_segs(d: u8, ox: f32, oy: f32, sx: f32, sy: f32) -> Vec<[f32; 4]> {
    let mut v = Vec::new();
    for line in glyph(d) {
        for w in line.windows(2) {
            v.push([ox + w[0].0 * sx, oy + w[0].1 * sy, ox + w[1].0 * sx, oy + w[1].1 * sy]);
        }
    }
    v
}

fn segs_dist(segs: &[[f32; 4]], x: f32, y: f32) -> f32 {
    segs.iter()
        .map(|s| seg_dist(x, y, s[0], s[1], s[2], s[3]))
        .fold(f32::MAX, f32::min)
}

/// Bir boy Nixie tüpü: 0-9 yanık rakamlar ve 10 = sönük tüp, hazır resimler
struct Tubes {
    w: i32,
    sh: i32,
    spr: Vec<Vec<u16>>,
}

impl Tubes {
    /// y0: tüplerin ekrandaki üst kenarı (zemin rengi buna göre)
    fn new(w: i32, h: i32, y0: i32) -> Self {
        let sh = h + 4;
        let gh = h as f32 * 0.60;
        let gw = gh * 10.0 / 16.0;
        let ox = (w as f32 - gw) / 2.0;
        let oy = h as f32 * 0.18;
        let (sx, sy) = (gw / 10.0, gh / 16.0);
        let sw = h as f32 * 0.028;
        let ghosts = [glyph_segs(8, ox, oy, sx, sy), glyph_segs(0, ox, oy, sx, sy)];
        let (wf, hf) = (w as f32, h as f32);
        let r = wf * 0.42;
        let mut spr = Vec::new();
        for d in 0..=10u8 {
            let lit = (d < 10).then(|| glyph_segs(d, ox, oy, sx, sy));
            let mut img = vec![0u16; (w * sh) as usize];
            for y in 0..sh {
                for x in 0..w {
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let mut col = bg_at(y0 + y);
                    let inside = rr_cov(fx, fy, 0.0, 0.0, wf, hf, r);
                    if inside > 0.0 {
                        let t = fy / hf;
                        let glass = if t < 0.5 {
                            mix(0x2a1608, 0x140a04, t * 2.0)
                        } else {
                            mix(0x140a04, 0x241306, (t - 0.5) * 2.0)
                        };
                        let inner = rr_cov(fx, fy, 1.3, 1.3, wf - 2.6, hf - 2.6, r - 1.3);
                        let mut g = mix(0x4a2a12, glass, inner);
                        if inner > 0.5 {
                            // anot teli örgüsü
                            if (x + y) % 5 == 0 || (x - y).rem_euclid(5) == 0 {
                                g = mix(g, 0xff8c3c, 0.07);
                            }
                            // arkadaki sönük katotlar
                            let gd = segs_dist(&ghosts[0], fx, fy).min(segs_dist(&ghosts[1], fx, fy));
                            g = mix(g, 0xa05a28, 0.18 * (sw / 2.0 + 0.5 - gd).clamp(0.0, 1.0));
                            if let Some(segs) = lit.as_ref() {
                                let dd = segs_dist(segs, fx, fy);
                                g = mix(g, 0xff5a00, 0.60 * (-dd / (hf * 0.045)).exp());
                                g = mix(g, 0xff7a1a, (sw * 0.9 + 0.5 - dd).clamp(0.0, 1.0));
                                g = mix(g, 0xffc28a, (sw * 0.45 + 0.5 - dd).clamp(0.0, 1.0));
                            }
                            // cam yansıması (sol kenar)
                            let refl = 0.10 * (1.0 - fx / (wf * 0.35));
                            if refl > 0.0 {
                                g = mix(g, 0xffffff, refl);
                            }
                        }
                        col = mix(col, g, inside);
                    }
                    // alt soket
                    let sock = rr_cov(fx, fy, 3.0, hf - 4.0, wf - 6.0, 7.0, 3.0);
                    if sock > 0.0 && inside < 1.0 {
                        col = mix(col, 0x1a1a1a, sock);
                    }
                    img[(y * w + x) as usize] = rgb(col);
                }
            }
            spr.push(img);
        }
        Tubes { w, sh, spr }
    }

    fn draw(&self, fb: &mut Framebuffer, x: i32, y: i32, i: usize) {
        let img = &self.spr[i.min(10)];
        for r in 0..self.sh {
            let o = (y + r) as usize * W + x as usize;
            let s = (r * self.w) as usize;
            fb.buf[o..o + self.w as usize].copy_from_slice(&img[s..s + self.w as usize]);
        }
    }
}

/// Bir tüp sırası: tüplerin ve ayırıcı noktaların konumları
struct Row {
    xs: Vec<i32>,
    y: i32,
    shown: Vec<u8>,
}

impl Row {
    /// `pattern`: 'd' tüp, '.' nokta, ':' iki nokta
    fn layout(cv: &mut Cv, x: i32, y: i32, t: &Tubes, h: i32, gap: i32, pattern: &str) -> Row {
        let mut xs = Vec::new();
        let mut cx = x;
        for ch in pattern.chars() {
            match ch {
                '.' => {
                    cv.neon(cx as f32 + 2.0, (y + h - 9) as f32, 3.0);
                    cx += 8;
                }
                ':' => {
                    cv.neon(cx as f32 + 3.0, y as f32 + h as f32 * 0.36, 2.5);
                    cv.neon(cx as f32 + 3.0, y as f32 + h as f32 * 0.66, 2.5);
                    cx += 8;
                }
                _ => {
                    xs.push(cx);
                    cx += t.w + gap;
                }
            }
        }
        let n = xs.len();
        Row { xs, y, shown: vec![255; n] }
    }

    /// Rakamlar: 0-9, 10 = sönük. Yalnızca değişen tüpler yeniden çizilir.
    fn show(&mut self, fb: &mut Framebuffer, t: &Tubes, digits: &[u8]) {
        for (i, &d) in digits.iter().enumerate().take(self.xs.len()) {
            if self.shown[i] != d {
                t.draw(fb, self.xs[i], self.y, d as usize);
                self.shown[i] = d;
            }
        }
    }

    fn end(&self, t: &Tubes) -> i32 {
        self.xs.last().map_or(0, |x| x + t.w)
    }
}

// ------------------------------------------------------------ analog kadran

struct Scale {
    title: &'static str,
    unit: &'static str,
    /// Kırmızı bölgenin başladığı oran
    red: f32,
    minor: i32,
    majors: &'static [(f32, &'static str)],
    /// İkinci (iç) ölçek; boşsa yok
    sub: &'static [(f32, &'static str)],
    big: bool,
}

const SWR: Scale = Scale {
    title: "SWR",
    unit: "RATIO",
    red: 0.62,
    minor: 20,
    majors: &[(0.0, "1"), (0.28, "1.5"), (0.5, "2"), (0.72, "3"), (0.87, "5"), (1.0, "INF")],
    sub: &[],
    big: false,
};
const SMETER: Scale = Scale {
    title: "S-UNITS",
    unit: "dBm",
    red: 0.545,
    minor: 22,
    majors: &[(0.0, "1"), (0.136, "3"), (0.273, "5"), (0.41, "7"), (0.545, "9"), (0.773, "+20"), (1.0, "+40")],
    sub: &[(0.0, "-121"), (0.273, "-97"), (0.545, "-73"), (0.773, "-53"), (1.0, "-33")],
    big: true,
};
const WATTS: Scale = Scale {
    title: "WATTS",
    unit: "QRP",
    red: 0.84,
    minor: 24,
    majors: &[(0.0, "0"), (0.167, "1"), (0.333, "2"), (0.5, "3"), (0.667, "4"), (0.833, "5"), (1.0, "6")],
    sub: &[],
    big: false,
};

/// SWR değerini ölçek oranına çevirir (büyük çentiklerden geçen parçalı doğru)
fn swr_frac(s: f32) -> f32 {
    const P: [(f32, f32); 6] = [(1.0, 0.0), (1.5, 0.28), (2.0, 0.5), (3.0, 0.72), (5.0, 0.87), (20.0, 1.0)];
    if s <= 1.0 {
        return 0.0;
    }
    for w in P.windows(2) {
        if s <= w[1].0 {
            return w[0].1 + (w[1].1 - w[0].1) * (s - w[0].0) / (w[1].0 - w[0].0);
        }
    }
    1.0
}

const INK: u32 = 0x1b1b1b;
const RED: u32 = 0xc4231c;

struct Meter {
    fx: i32,
    fy: i32,
    fw: i32,
    fh: i32,
    px: f32,
    py: f32,
    r: f32,
    a0: f32,
    a1: f32,
    /// göbek kapağı elipsi (merkez y, yarı eksenler)
    hub: (f32, f32, f32),
    scratch: Vec<u16>,
    drawn: f32,
    // yaylı ibre
    pos: f32,
    vel: f32,
}

impl Meter {
    /// Çerçeve (x, y, w, h); kadranı ve ölçeği `base` katmanına çizer
    fn new(base: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32, s: &Scale) -> Meter {
        let (fx, fy, fw, fh) = (x + 18, y + 16, w - 36, h - 32);
        let px = fx as f32 + fw as f32 / 2.0;
        let py = fy as f32 + fh as f32 * 0.98;
        let r = fh as f32 * 0.78;
        let a0 = PI * 1.21;
        let a1 = PI * 1.79;
        let hub = (fy as f32 + fh as f32 + 2.0, fw as f32 * 0.17, 22.0);
        let m = Meter {
            fx,
            fy,
            fw,
            fh,
            px,
            py,
            r,
            a0,
            a1,
            hub,
            scratch: vec![0; (fw * fh) as usize],
            drawn: f32::NAN,
            pos: 0.0,
            vel: 0.0,
        };
        m.paint_static(base, x, y, w, h, s);
        m
    }

    fn ang(&self, f: f32) -> f32 {
        self.a0 + (self.a1 - self.a0) * f
    }

    fn paint_static(&self, base: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32, s: &Scale) {
        let (fx, fy, fw, fh) = (self.fx as f32, self.fy as f32, self.fw as f32, self.fh as f32);
        // Siyah çerçeve
        for yy in y..y + h {
            let c = mix(0x2b2b2e, 0x0e0e10, (yy - y) as f32 / h as f32);
            for xx in x..x + w {
                let i = yy as usize * W + xx as usize;
                let cov = rr_cov(xx as f32 + 0.5, yy as f32 + 0.5, x as f32, y as f32, w as f32, h as f32, 10.0);
                if cov > 0.0 {
                    base.buf[i] = blend(base.buf[i], rgb(c), (cov * 255.0) as u32);
                }
            }
        }
        // Fildişi kadran: alttan yayılan sıcak ışık
        let (gcx, gcy) = (fx + fw / 2.0, fy + fh * 0.8);
        for yy in self.fy..self.fy + self.fh {
            for xx in self.fx..self.fx + self.fw {
                let (pxf, pyf) = (xx as f32 + 0.5, yy as f32 + 0.5);
                let cov = rr_cov(pxf, pyf, fx, fy, fw, fh, 6.0);
                if cov <= 0.0 {
                    continue;
                }
                let inner = rr_cov(pxf, pyf, fx + 1.0, fy + 1.0, fw - 2.0, fh - 2.0, 5.0);
                let (dx, dy) = (pxf - gcx, (pyf - gcy) * 1.25);
                let t = (dx * dx + dy * dy).sqrt() / (fw * 0.72);
                let face = if t < 0.55 {
                    mix(0xfff6d8, 0xf3dfa8, t / 0.55)
                } else {
                    mix(0xf3dfa8, 0xc9a865, (t - 0.55) / 0.45)
                };
                // cam parlaması: sol üstten çapraz
                let gt = ((pxf - fx) / fw + (pyf - fy) / fh) / 2.0;
                let face = if gt < 0.32 { mix(face, 0xffffff, 0.22 * (1.0 - gt / 0.32)) } else { face };
                let c = mix(0x5a4a2a, face, inner);
                let i = yy as usize * W + xx as usize;
                base.buf[i] = blend(base.buf[i], rgb(c), (cov * 255.0) as u32);
            }
        }
        // Vidalar
        {
            let mut cv = Cv::of(base);
            for (sx, sy) in [(x + 10, y + 10), (x + w - 10, y + 10), (x + 10, y + h - 10), (x + w - 10, y + h - 10)] {
                let (sx, sy) = (sx as f32, sy as f32);
                cv.disc(sx, sy, 4.2, rgb(0x5a5a5e), 1.0);
                cv.disc(sx - 0.8, sy - 0.8, 2.6, rgb(0xa8a8ac), 0.8);
                cv.seg(sx - 3.0, sy, sx + 3.0, sy, 1.0, rgb(0x222222), 1.0);
            }
        }
        let (px, py, r) = (self.px, self.py, self.r);
        let red_a = self.ang(s.red);
        let mut cv = Cv::of(base);
        // Ölçek yayları
        cv.arc(px, py, r, 1.6, self.a0, self.a1, rgb(INK));
        cv.arc(px, py, r + 3.0, 5.0, red_a, self.a1, rgb(RED));
        if !s.sub.is_empty() {
            cv.arc(px, py, r * 0.78, 1.0, self.a0, self.a1, rgb(INK));
        }
        // Çentikler
        for i in 0..=s.minor {
            let f = i as f32 / s.minor as f32;
            let a = self.ang(f);
            let big = s.majors.iter().any(|m| (m.0 - f).abs() < 0.01);
            let (ca, sa) = (a.cos(), a.sin());
            let l = if big { 11.0 } else { 6.0 };
            let c = if f >= s.red - 0.001 { RED } else { INK };
            cv.seg(px + ca * r, py + sa * r, px + ca * (r - l), py + sa * (r - l), if big { 1.8 } else { 1.0 }, rgb(c), 1.0);
        }
        drop(cv);
        // Büyük çentik yazıları
        let font = if s.big { &F_B14 } else { &F_B12 };
        for &(f, l) in s.majors {
            let a = self.ang(f);
            let (lx, ly) = (px + a.cos() * (r - 22.0), py + a.sin() * (r - 22.0));
            let c = if f >= s.red - 0.001 { rgb(0xb01e18) } else { rgb(INK) };
            if l == "INF" {
                // Sonsuz işareti: yan yana iki küçük halka
                let mut cv = Cv::of(base);
                for dx in [-3.4f32, 3.4] {
                    cv.arc(lx + dx, ly - 1.0, 3.0, 1.6, 0.0, TAU, c);
                    // arc yalnız üst yarıyı çizer; alt yarı için ters yönde bir yay
                    for k in 0..24 {
                        let t0 = PI * k as f32 / 24.0;
                        let t1 = PI * (k + 1) as f32 / 24.0;
                        cv.seg(lx + dx + 3.0 * t0.cos(), ly - 1.0 + 3.0 * t0.sin(), lx + dx + 3.0 * t1.cos(), ly - 1.0 + 3.0 * t1.sin(), 1.6, c, 1.0);
                    }
                }
            } else {
                text_c(base, font, lx as i32, ly as i32 + 5, l, c);
            }
        }
        for &(f, l) in s.sub {
            let a = self.ang(f);
            let rr = r * 0.78 - 12.0;
            text_c(base, &F_R11, (px + a.cos() * rr) as i32, (py + a.sin() * rr) as i32 + 4, l, rgb(0x3a3a3a));
        }
        // Kadran yazıları
        text_c(base, &F_B16, px as i32, (fy + fh * 0.62) as i32, s.title, rgb(INK));
        text(base, &F_R11, self.fx + 8, self.fy + self.fh - 6, "QMXPan", rgb(0x7a6038));
        text_r(base, &F_R11, self.fx + self.fw - 8, self.fy + self.fh - 6, s.unit, rgb(0x7a6038));
        // Bakalit göbek kapağı (ibrenin dibini örter)
        let (hcy, hrx, hry) = self.hub;
        for yy in (hcy - hry) as i32 - 1..=hcy as i32 {
            for xx in (px - hrx) as i32 - 1..=(px + hrx) as i32 + 1 {
                let (dx, dy) = ((xx as f32 + 0.5 - px) / hrx, (yy as f32 + 0.5 - hcy) / hry);
                let e = (dx * dx + dy * dy).sqrt();
                let cov = ((1.0 - e) * hry + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 && (yy as f32) < hcy {
                    let c = mix(0x3a3a3c, 0x111111, (dx + 1.0) / 2.0 * 0.7 + (dy + 1.0) / 2.0 * 0.3);
                    let i = yy as usize * W + xx as usize;
                    base.buf[i] = blend(base.buf[i], rgb(c), (cov * 255.0) as u32);
                }
            }
        }
        let mut cv = Cv::of(base);
        let sy = (self.fy + self.fh) as f32 - 8.0;
        cv.disc(px, sy, 5.0, rgb(0x6a6a6a), 1.0);
        cv.disc(px - 1.2, sy - 1.2, 2.6, rgb(0xdddddd), 0.8);
    }

    /// Yaylı ibre: hedefe biraz aşarak oturur (dt adım sayısı kadar)
    fn step(&mut self, target: f32, steps: u32) {
        for _ in 0..steps {
            self.vel = (self.vel + (target - self.pos) * 0.09) * 0.72;
            self.pos += self.vel;
        }
    }

    /// Kadranın kopyasına ibreyi çizip ekrana aktarır (değiştiyse)
    fn draw(&mut self, base: &Framebuffer, fb: &mut Framebuffer, force: bool) {
        let f = self.pos.clamp(-0.02, 1.02);
        if !force && (f - self.drawn).abs() < 0.0015 {
            return;
        }
        self.drawn = f;
        let (fw, fh) = (self.fw as usize, self.fh as usize);
        for r in 0..fh {
            let o = (self.fy as usize + r) * W + self.fx as usize;
            self.scratch[r * fw..(r + 1) * fw].copy_from_slice(&base.buf[o..o + fw]);
        }
        let a = self.ang(f);
        let (ca, sa) = (a.cos(), a.sin());
        // İbre göbek kapağının dışından başlar
        let (hcy, hrx, hry) = self.hub;
        let d0 = 1.0 / ((ca / hrx).powi(2) + (sa / hry).powi(2)).sqrt() + (self.py - hcy).abs() + 1.0;
        let (px, py, r) = (self.px, self.py, self.r);
        {
            let mut cv = Cv { b: &mut self.scratch, w: self.fw, h: self.fh, ox: self.fx, oy: self.fy };
            cv.seg(px + ca * d0 + 3.0, py + sa * d0 + 3.0, px + ca * (r + 4.0) + 3.0, py + sa * (r + 4.0) + 3.0, 2.4, 0, 0.22);
            cv.seg(px + ca * d0, py + sa * d0, px + ca * r * 0.6, py + sa * r * 0.6, 2.2, rgb(0x121212), 1.0);
            cv.seg(px + ca * r * 0.6, py + sa * r * 0.6, px + ca * (r + 4.0), py + sa * (r + 4.0), 1.1, rgb(0x121212), 1.0);
        }
        for r in 0..fh {
            let o = (self.fy as usize + r) * W + self.fx as usize;
            fb.buf[o..o + fw].copy_from_slice(&self.scratch[r * fw..(r + 1) * fw]);
        }
    }
}

// ------------------------------------------------------------ sayfa

/// Sayfaya her güncellemede verilen değerler
pub struct Data {
    pub dial: Option<u64>,
    /// Üst şeritteki bilgi ("40m · CW · 300 Hz")
    pub info: String,
    /// S-metre (dBm, kalibrasyonlu); I/Q yoksa None
    pub dbm: Option<f32>,
    pub tx: bool,
    /// Gönderirken okunan güç (W) ve SWR
    pub pwr: Option<f32>,
    pub swr: Option<f32>,
}

const KEY_Y: i32 = 444;
const KEY_H: i32 = 30;
const LCD_Y: i32 = 358;
const LCD_H: i32 = 44;

fn key_x(i: usize) -> i32 {
    8 + (i as i32 * 263) / 2
}

/// BACK tuşuna dokunuldu mu
pub fn back_at(x: i32, y: i32) -> bool {
    y >= KEY_Y - 4 && x >= key_x(5)
}

pub struct Meters {
    base: Framebuffer,
    freq: Row,
    clock: Row,
    big: Tubes,
    small: Tubes,
    swr: Meter,
    sm: Meter,
    pw: Meter,
    last_step: Instant,
    shown_info: String,
    shown_tx: Option<bool>,
    shown_lcd: [String; 3],
    /// Son gönderimin en yüksek gücü/SWR'si ve ne zaman
    peak: Option<(f32, f32, Instant)>,
}

impl Meters {
    pub fn new() -> Meters {
        let t0 = Instant::now();
        let mut base = Framebuffer::layer(H, 0);
        for y in 0..H as i32 {
            let c = rgb(bg_at(y));
            base.buf[y as usize * W..(y as usize + 1) * W].fill(c);
        }
        // Üst şerit
        crate::gfx::fill(&mut base, 0, 0, W as i32, 30, th::PN);
        let x = text(&mut base, &F_B16, 12, 21, "QMXPan", th::CY);
        text(&mut base, &F_B14, x + 12, 20, "METERS", th::TX);
        text(&mut base, &F_B12, 14, 46, "VFO A", th::MU);
        text(&mut base, &F_B12, 563, 56, "UTC", th::MU);

        let big = Tubes::new(44, 76, 50);
        let small = Tubes::new(32, 58, 62);
        let (freq, clock) = {
            let mut cv = Cv::of(&mut base);
            let f = Row::layout(&mut cv, 14, 50, &big, 76, 4, "dd.ddd.ddd");
            let c = Row::layout(&mut cv, 563, 62, &small, 58, 3, "dd:dd:dd");
            (f, c)
        };
        text(&mut base, &F_B14, freq.end(&big) + 8, 122, "MHz", rgb(0xc98a55));

        let swr = Meter::new(&mut base, 10, 156, 236, 182, &SWR);
        let sm = Meter::new(&mut base, 254, 146, 292, 202, &SMETER);
        let pw = Meter::new(&mut base, 554, 156, 236, 182, &WATTS);
        // Tuşlar: panadapter tuşları burada kapalı, yalnız BACK
        for (i, l) in ["BAND", "CW", "FT8", "DISPLAY", "ZOOM", "BACK"].iter().enumerate() {
            let x = key_x(i);
            let (bg, fg) = if i == 5 { (th::CY, th::ON_CY) } else { (th::PN, th::LN) };
            crate::gfx::rrect(&mut base, x, KEY_Y, 125, KEY_H, 7, bg);
            text_c(&mut base, &F_B14, x + 62, KEY_Y + 20, l, fg);
        }
        log::info!("METERS sayfası hazırlandı: {} ms", t0.elapsed().as_millis());
        Meters {
            base,
            freq,
            clock,
            big,
            small,
            swr,
            sm,
            pw,
            last_step: Instant::now(),
            shown_info: String::new(),
            shown_tx: None,
            shown_lcd: Default::default(),
            peak: None,
        }
    }

    /// Sayfayı açar: tüm ekranı baştan çizer
    pub fn enter(&mut self, fb: &mut Framebuffer) {
        fb.buf.copy_from_slice(&*self.base.buf);
        self.freq.shown.fill(255);
        self.clock.shown.fill(255);
        self.shown_info.clear();
        self.shown_tx = None;
        self.shown_lcd = Default::default();
        for m in [&mut self.swr, &mut self.sm, &mut self.pw] {
            m.drawn = f32::NAN;
        }
        self.last_step = Instant::now();
    }

    pub fn update(&mut self, fb: &mut Framebuffer, d: &Data) {
        // Frekans: 8 tüp, 10 MHz altında baştaki tüp sönük
        let digits: Vec<u8> = match d.dial {
            Some(f) => {
                let s = format!("{:08}", f.min(99_999_999));
                let mut v: Vec<u8> = s.bytes().map(|b| b - b'0').collect();
                if v[0] == 0 {
                    v[0] = 10;
                }
                v
            }
            None => vec![10; 8],
        };
        self.freq.show(fb, &self.big, &digits);
        // UTC saati (SNTP ayarlamadıysa sönük)
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|t| t.as_secs()).unwrap_or(0);
        let clock: Vec<u8> = if now > 1_735_689_600 {
            let s = now % 86_400;
            let (h, m, sec) = ((s / 3600) as u8, ((s / 60) % 60) as u8, (s % 60) as u8);
            vec![h / 10, h % 10, m / 10, m % 10, sec / 10, sec % 10]
        } else {
            vec![10; 6]
        };
        self.clock.show(fb, &self.small, &clock);

        // Üst şerit: band/mod ve TX lambası
        if d.info != self.shown_info {
            restore(&self.base, fb, 300, 0, 430, 30);
            text_r(fb, &F_B14, 730, 20, &d.info, th::MU);
            self.shown_info = d.info.clone();
        }
        if self.shown_tx != Some(d.tx) {
            restore(&self.base, fb, 740, 0, 60, 30);
            text_r(fb, &F_B14, 760, 20, "TX", if d.tx { th::RD } else { th::MU });
            let mut cv = Cv::of(fb);
            if d.tx {
                cv.neon(778.0, 15.0, 6.5);
                cv.disc(778.0, 15.0, 6.5, rgb(0xe0281e), 1.0);
                cv.disc(776.5, 13.5, 3.0, rgb(0xffd0c8), 0.9);
            } else {
                cv.disc(778.0, 15.0, 7.0, rgb(0x2a0a08), 1.0);
                cv.disc(777.0, 14.0, 4.0, rgb(0x5a1a16), 0.9);
            }
            self.shown_tx = Some(d.tx);
        }

        // Gönderim tepe değerleri 5 sn tutulur
        if d.tx {
            if let (Some(p), s) = (d.pwr, d.swr.unwrap_or(1.0)) {
                let (pp, ps) = match self.peak {
                    Some((pp, ps, t)) if t.elapsed().as_millis() < 400 => (pp.max(p), ps.max(s)),
                    _ => (p, s),
                };
                self.peak = Some((pp, ps, Instant::now()));
            }
        }
        let held = self.peak.filter(|p| d.tx || p.2.elapsed().as_secs() < 5);

        // İbreler (yaylı hareket, ~16 ms'lik adımlarla)
        let steps = (self.last_step.elapsed().as_millis() / 16) as u32;
        if steps > 0 {
            self.last_step = Instant::now();
            let steps = steps.min(10);
            let s_t = d.dbm.map_or(-0.02, |v| (v + 121.0) / 88.0);
            let (p_t, w_t) = if d.tx {
                (d.pwr.unwrap_or(0.0) / 6.0, swr_frac(d.swr.unwrap_or(1.0)))
            } else {
                (0.0, 0.0)
            };
            self.sm.step(s_t, steps);
            self.pw.step(p_t, steps);
            self.swr.step(w_t, steps);
            self.sm.draw(&self.base, fb, false);
            self.pw.draw(&self.base, fb, false);
            self.swr.draw(&self.base, fb, false);
        }

        // Kehribar LCD okumaları: solda küçük başlık, sağda büyük değer
        let swr_s = match held {
            Some((_, s, _)) => (format!("{s:.2}"), String::new()),
            None => ("—".into(), String::new()),
        };
        let sm_s = match d.dbm {
            Some(v) => (crate::ui::s_units(v), format!("{:.0} dBm", v)),
            None => ("—".into(), String::new()),
        };
        let pw_s = match held {
            Some((p, _, _)) => (format!("{p:.1}"), "W".into()),
            None => ("—".into(), "W".into()),
        };
        let boxes = [("SWR", swr_s, 28, 200), ("SIGNAL", sm_s, 272, 256), ("POWER", pw_s, 572, 200)];
        for (i, (label, (val, small), x, w)) in boxes.into_iter().enumerate() {
            let key = format!("{val}|{small}");
            if key == self.shown_lcd[i] {
                continue;
            }
            crate::gfx::rrect(fb, x, LCD_Y, w, LCD_H, 6, rgb(0x3a2a12));
            crate::gfx::rrect(fb, x + 1, LCD_Y + 1, w - 2, LCD_H - 2, 5, rgb(0x1a1206));
            text(fb, &F_B12, x + 12, LCD_Y + 18, label, rgb(0xa07030));
            let amber = rgb(0xffb347);
            let base_y = LCD_Y + 34;
            let mut right = x + w - 12;
            if !small.is_empty() {
                text_r(fb, &F_B16, right, base_y, &small, rgb(0xd08a40));
                right -= crate::gfx::text_w(&F_B16, &small) + 8;
            }
            text_r(fb, &F_B30, right, base_y, &val, amber);
            self.shown_lcd[i] = key;
        }
    }
}

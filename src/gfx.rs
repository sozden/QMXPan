//! Tema renkleri ve Ã§izim yardÄ±mcÄ±larÄ±: yumuÅŸatÄ±lmÄ±ÅŸ (4 bit) yazÄ±, yuvarlak kÃ¶ÅŸeli
//! paneller, RGB565 renk karÄ±ÅŸÄ±mÄ±. Renk dili QRP DDS projesinden.

use crate::lcd::{Framebuffer, W};

/// 0xRRGGBB â†’ RGB565
pub const fn rgb(hex: u32) -> u16 {
    let r = (hex >> 16) & 0xFF;
    let g = (hex >> 8) & 0xFF;
    let b = hex & 0xFF;
    (((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)) as u16
}

pub mod th {
    use super::rgb;
    pub const BG: u16 = rgb(0x0a1020);
    pub const PN: u16 = rgb(0x141d30);
    pub const PN2: u16 = rgb(0x1d2a44);
    pub const LN: u16 = rgb(0x2a3a58);
    pub const TX: u16 = rgb(0xe8eef8);
    pub const MU: u16 = rgb(0x8494b2);
    pub const CY: u16 = rgb(0x22d3ee);
    pub const AM: u16 = rgb(0xfbbf24);
    pub const GR: u16 = rgb(0x34d399);
    pub const RD: u16 = rgb(0xf87171);
    /// Spektrum panelinin zemini
    pub const SPEC_BG: u16 = rgb(0x0c1424);
    /// LO (veri yok) bÃ¶lgesi ve tarama Ã§izgileri
    pub const LO_BG: u16 = rgb(0x111a2c);
    pub const LO_HATCH: u16 = rgb(0x26324a);
    /// CamgÃ¶beÄŸi zemin Ã¼stÃ¼ndeki koyu yazÄ±
    pub const ON_CY: u16 = rgb(0x00121a);
    /// Kehribar zemin Ã¼stÃ¼ndeki koyu yazÄ±
    pub const ON_AM: u16 = rgb(0x2a1c02);
}

/// Ä°ki RGB565 rengi karÄ±ÅŸtÄ±rÄ±r: a = 0 â†’ bg, 255 â†’ fg
#[inline]
pub fn blend(bg: u16, fg: u16, a: u32) -> u16 {
    if a == 0 {
        return bg;
    }
    if a >= 255 {
        return fg;
    }
    let (br, bgc, bb) = ((bg >> 11) as u32, ((bg >> 5) & 0x3F) as u32, (bg & 0x1F) as u32);
    let (fr, fg_, fb) = ((fg >> 11) as u32, ((fg >> 5) & 0x3F) as u32, (fg & 0x1F) as u32);
    let r = (br * (255 - a) + fr * a) / 255;
    let g = (bgc * (255 - a) + fg_ * a) / 255;
    let b = (bb * (255 - a) + fb * a) / 255;
    ((r << 11) | (g << 5) | b) as u16
}

pub struct Glyph {
    pub off: u32,
    pub w: u8,
    pub h: u8,
    pub xo: i8,
    pub yo: i8,
    pub adv: u8,
}

pub struct Font {
    pub bmp: &'static [u8],
    pub gl: &'static [Glyph],
    pub cp: &'static [u16],
    pub top: u8,
    pub bottom: u8,
}

impl Font {
    fn glyph(&self, c: char) -> Option<&Glyph> {
        let cp = c as u32;
        if cp > 0xFFFF {
            return None;
        }
        self.cp.binary_search(&(cp as u16)).ok().map(|i| &self.gl[i])
    }
}

/// YazÄ±nÄ±n piksel geniÅŸliÄŸi
pub fn text_w(f: &Font, s: &str) -> i32 {
    s.chars()
        .map(|c| f.glyph(c).or_else(|| f.glyph('?')).map_or(0, |g| g.adv as i32))
        .sum()
}

/// YazÄ±yÄ± (x, taban Ã§izgisi y) noktasÄ±ndan Ã§izer, bitiÅŸ x'ini dÃ¶ndÃ¼rÃ¼r.
pub fn text(fb: &mut Framebuffer, f: &Font, x: i32, y: i32, s: &str, color: u16) -> i32 {
    let mut cx = x;
    for c in s.chars() {
        let Some(g) = f.glyph(c).or_else(|| f.glyph('?')) else { continue };
        let gx = cx + g.xo as i32;
        let gy = y + g.yo as i32;
        let row_bytes = (g.w as usize).div_ceil(2);
        for r in 0..g.h as i32 {
            let py = gy + r;
            if py < 0 || py >= fb.h as i32 {
                continue;
            }
            let base = g.off as usize + r as usize * row_bytes;
            for col in 0..g.w as i32 {
                let px = gx + col;
                if px < 0 || px >= W as i32 {
                    continue;
                }
                let byte = f.bmp[base + col as usize / 2];
                let lev = if col % 2 == 0 { byte >> 4 } else { byte & 0x0F } as u32;
                if lev == 0 {
                    continue;
                }
                let i = py as usize * W + px as usize;
                fb.buf[i] = blend(fb.buf[i], color, lev * 17);
            }
        }
        cx += g.adv as i32;
    }
    cx
}

/// OrtalanmÄ±ÅŸ yazÄ±
pub fn text_c(fb: &mut Framebuffer, f: &Font, cx: i32, y: i32, s: &str, color: u16) {
    let w = text_w(f, s);
    text(fb, f, cx - w / 2, y, s, color);
}

/// SaÄŸa dayalÄ± yazÄ± (x = saÄŸ kenar)
pub fn text_r(fb: &mut Framebuffer, f: &Font, x: i32, y: i32, s: &str, color: u16) {
    let w = text_w(f, s);
    text(fb, f, x - w, y, s, color);
}

pub fn fill(fb: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32, c: u16) {
    let x0 = x.max(0) as usize;
    let x1 = (x + w).min(W as i32).max(0) as usize;
    let y0 = y.max(0) as usize;
    let y1 = (y + h).min(fb.h as i32).max(0) as usize;
    if x0 >= x1 {
        return;
    }
    for yy in y0..y1 {
        fb.buf[yy * W + x0..yy * W + x1].fill(c);
    }
}

/// Yuvarlak kÃ¶ÅŸeli dolu dikdÃ¶rtgen; kÃ¶ÅŸeler yumuÅŸatÄ±lmÄ±ÅŸ.
pub fn rrect(fb: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32, r: i32, c: u16) {
    let r = r.min(w / 2).min(h / 2).max(0);
    if r == 0 {
        fill(fb, x, y, w, h, c);
        return;
    }
    // Orta gÃ¶vde
    fill(fb, x, y + r, w, h - 2 * r, c);
    fill(fb, x + r, y, w - 2 * r, r, c);
    fill(fb, x + r, y + h - r, w - 2 * r, r, c);
    // DÃ¶rt kÃ¶ÅŸe: piksel merkezinin kÃ¶ÅŸe Ã§emberine uzaklÄ±ÄŸÄ±na gÃ¶re kaplama
    let rf = r as f32;
    for dy in 0..r {
        for dx in 0..r {
            let fx = rf - dx as f32 - 0.5;
            let fy = rf - dy as f32 - 0.5;
            let d = (fx * fx + fy * fy).sqrt();
            let cov = (rf - d + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let a = (cov * 255.0) as u32;
            for (px, py) in [
                (x + dx, y + dy),
                (x + w - 1 - dx, y + dy),
                (x + dx, y + h - 1 - dy),
                (x + w - 1 - dx, y + h - 1 - dy),
            ] {
                if px >= 0 && py >= 0 && (px as usize) < W && (py as usize) < fb.h {
                    let i = py as usize * W + px as usize;
                    fb.buf[i] = blend(fb.buf[i], c, a);
                }
            }
        }
    }
}

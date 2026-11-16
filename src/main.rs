//! QMXPan — 1. adım: ekran ve dokunmatik testi.
//!
//! Ekrana renk çubukları, renk geçişleri ve ızgara çizer; dokunulan yere
//! sarı nokta koyar ve koordinatı hem ekrana hem seri porta yazar.
//! Sağ üstteki "CLEAR" kutusuna dokunmak ekranı yeniden çizer.

mod board;
mod lcd;
mod touch;

use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::extra as x;
use log::info;

use lcd::{Framebuffer, H, W};

const CLEAR_BOX: Rectangle = Rectangle::new(Point::new(680, 430), Size::new(110, 40));

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    info!("QMXPan {} başlıyor", env!("CARGO_PKG_VERSION"));

    let p = Peripherals::take()?;
    let mut i2c = I2cDriver::new(
        p.i2c0,
        p.pins.gpio8,
        p.pins.gpio9,
        &I2cConfig::new().baudrate(Hertz(400_000)),
    )?;

    let mut exp = board::Expander::init(&mut i2c)?;
    info!("CH422G hazır (USB_SEL = USB portu)");
    board::reset_touch(&mut i2c, &mut exp, p.pins.gpio4.into())?;
    let mut tp = touch::Gt911::probe(&mut i2c);

    let mut fb = lcd::init()?;
    let (psram, psram_free) = unsafe {
        (
            x::esp_psram_get_size(),
            x::heap_caps_get_free_size(x::MALLOC_CAP_SPIRAM),
        )
    };
    info!(
        "Ekran hazır. PSRAM: {} KB, boş {} KB",
        psram / 1024,
        psram_free / 1024
    );

    draw_test_screen(&mut fb, psram / 1024, tp.is_some());

    let mut last: Option<(u16, u16)> = None;
    loop {
        if let Some(t) = tp.as_mut() {
            if let Some((tx, ty)) = t.read(&mut i2c) {
                let pt = Point::new(tx as i32, ty as i32);
                if CLEAR_BOX.contains(pt) {
                    draw_test_screen(&mut fb, psram / 1024, true);
                } else {
                    let _ = Circle::with_center(pt, 9)
                        .into_styled(PrimitiveStyle::with_fill(Rgb565::YELLOW))
                        .draw(&mut fb);
                    show_touch(&mut fb, tx, ty);
                }
                if last != Some((tx, ty)) {
                    info!("Dokunma: x={tx} y={ty}");
                    last = Some((tx, ty));
                }
            }
        }
        FreeRtos::delay_ms(15);
    }
}

fn text(fb: &mut Framebuffer, s: &str, x: i32, y: i32, c: Rgb565) {
    let _ = Text::new(s, Point::new(x, y), MonoTextStyle::new(&FONT_10X20, c)).draw(fb);
}

fn show_touch(fb: &mut Framebuffer, tx: u16, ty: u16) {
    let _ = Rectangle::new(Point::new(10, 432), Size::new(300, 30))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(fb);
    text(fb, &format!("Touch: x={tx:3} y={ty:3}"), 14, 455, Rgb565::YELLOW);
}

fn draw_test_screen(fb: &mut Framebuffer, psram_kb: usize, touch_ok: bool) {
    fb.buf.fill(0);

    // 1) Sekiz düz renk çubuğu (üst 80 piksel)
    let bars = [
        Rgb565::WHITE,
        Rgb565::YELLOW,
        Rgb565::CYAN,
        Rgb565::GREEN,
        Rgb565::MAGENTA,
        Rgb565::RED,
        Rgb565::BLUE,
        Rgb565::new(8, 16, 8),
    ];
    for (i, c) in bars.iter().enumerate() {
        let _ = Rectangle::new(Point::new(i as i32 * 100, 0), Size::new(100, 80))
            .into_styled(PrimitiveStyle::with_fill(*c))
            .draw(fb);
    }

    // 2) Kırmızı, yeşil, mavi ve gri geçişleri: veri hattı hatası varsa
    //    geçişte basamak/renk atlaması görünür
    for x in 0..W {
        let r = (x * 32 / W) as u8;
        let g = (x * 64 / W) as u8;
        for y in 0..30 {
            fb.put(x, 90 + y, Rgb565::new(r, 0, 0));
            fb.put(x, 125 + y, Rgb565::new(0, g, 0));
            fb.put(x, 160 + y, Rgb565::new(0, 0, r));
            fb.put(x, 195 + y, Rgb565::new(r, g, r));
        }
    }

    // 3) 50 piksellik ızgara ve beyaz çerçeve (kenarlar tam görünmeli)
    let grid = Rgb565::new(6, 12, 6);
    for y in (240..H).step_by(50) {
        for x in 0..W {
            fb.put(x, y, grid);
        }
    }
    for x in (0..W).step_by(50) {
        for y in 240..H {
            fb.put(x, y, grid);
        }
    }
    let _ = Rectangle::new(Point::zero(), Size::new(W as u32, H as u32))
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::WHITE, 1))
        .draw(fb);

    // 4) Bilgi yazıları
    text(fb, "QMXPan - display & touch test", 14, 270, Rgb565::WHITE);
    text(
        fb,
        &format!("800x480 RGB565   PSRAM: {psram_kb} KB"),
        14,
        300,
        Rgb565::CSS_LIGHT_GRAY,
    );
    text(
        fb,
        if touch_ok {
            "Touch: GT911 OK - draw on the screen"
        } else {
            "Touch: GT911 NOT FOUND"
        },
        14,
        330,
        if touch_ok { Rgb565::GREEN } else { Rgb565::RED },
    );

    let _ = CLEAR_BOX
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CYAN, 2))
        .draw(fb);
    text(fb, "CLEAR", 710, 457, Rgb565::CYAN);
}

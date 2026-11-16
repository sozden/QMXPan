//! QMXPan — 2. adım: QMX USB testi.
//!
//! Kart USB host olur; QMX takılınca ses (I/Q) arayüzünü açar, örnek hızını
//! ve I/Q seviyelerini ölçer, CAT portundan saniyede bir "FA;" gönderip
//! yanıtı ekrana yazar. Ayrıntılı tanımlayıcı dökümü seri porta gider.

mod board;
mod lcd;
mod touch;
mod usb;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::units::Hertz;
use log::{info, warn};

use lcd::{Framebuffer, W};
use usb::{Cat, Ev, IqStats, IqStream};

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
    board::reset_touch(&mut i2c, &mut exp, p.pins.gpio4.into())?;
    let _tp = touch::Gt911::probe(&mut i2c);
    let mut fb = lcd::init()?;
    fb.buf.fill(0);
    text(&mut fb, "QMXPan - QMX USB test", 14, 30, Rgb565::WHITE);

    let events = usb::start()?;
    let stats = Arc::new(Mutex::new(IqStats::default()));

    let mut dev: Option<(u16, u16)> = None;
    let mut iq: Option<IqStream> = None;
    let mut cat: Option<Cat> = None;
    let mut audio_ifaces: Vec<u8> = Vec::new();
    let mut cat_retry: Option<Instant> = None;
    let mut last_fa = Instant::now();
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let boot = Instant::now();

    loop {
        while let Ok(ev) = events.try_recv() {
            match ev {
                Ev::NewDev(vid, pid) => {
                    info!("USB aygıtı takıldı: {vid:04X}:{pid:04X}");
                    dev = Some((vid, pid));
                    // Ses sürücüsü arayüzleri bildirsin diye biraz bekleyip CAT'i ara
                    cat_retry = Some(Instant::now() + Duration::from_millis(1500));
                }
                Ev::UacRx { addr, iface } => {
                    info!("Ses giriş arayüzü: adres {addr}, arayüz {iface}");
                    audio_ifaces.push(iface);
                    match IqStream::open(addr, iface, stats.clone()) {
                        Ok(s) => iq = Some(s),
                        Err(e) => warn!("I/Q akışı açılamadı: {e}"),
                    }
                }
                Ev::UacGone(h) => {
                    if iq.as_ref().is_some_and(|s| s.handle() == h) {
                        iq.take().unwrap().close();
                    }
                    info!("Ses arayüzü çıkarıldı");
                    *stats.lock().unwrap() = IqStats::default();
                    audio_ifaces.clear();
                    dev = None;
                }
                Ev::CdcGone(h) => {
                    if cat.as_ref().is_some_and(|c| c.handle() == h) {
                        cat.take().unwrap().close();
                    }
                    info!("CAT portu çıkarıldı");
                    usb::CAT_RX.lock().unwrap().clear();
                }
            }
        }

        // CAT portunu bul: ses arayüzü olmayan ilk CDC arayüzü
        if cat.is_none() && cat_retry.is_some_and(|t| Instant::now() >= t) {
            cat_retry = None;
            for iface in 0..8u8 {
                if audio_ifaces.contains(&iface) {
                    continue;
                }
                match Cat::open(iface) {
                    Ok(c) => {
                        info!("CAT portu açıldı: arayüz {iface}");
                        // I/Q modunu aç (yalnız bu oturum için; QMX kapanınca normale döner)
                        let _ = c.send("Q9 1;");
                        FreeRtos::delay_ms(200);
                        let _ = c.send("Q9;");
                        cat = Some(c);
                        break;
                    }
                    Err(e) => info!("Arayüz {iface} CDC değil ({e})"),
                }
            }
        }

        if let Some(c) = cat.as_ref() {
            if last_fa.elapsed() >= Duration::from_secs(1) {
                last_fa = Instant::now();
                if let Err(e) = c.send("FA;") {
                    warn!("CAT gönderilemedi: {e}");
                }
                info!(
                    "CAT: {} {}",
                    last_cat_reply("FA").unwrap_or_default(),
                    last_cat_reply("Q9").unwrap_or_default()
                );
            }
        }

        if last_draw.elapsed() >= Duration::from_millis(500) {
            last_draw = Instant::now();
            let s = stats.lock().unwrap().clone();
            draw_status(&mut fb, dev, &s, cat.as_ref().map(|c| c.iface));
            // Kart QMX bağlıyken resetlenirse QMX'in USB'si takılı kalıyor;
            // kablo çıkarmak yetmiyor, QMX'in kapatılıp açılması gerekiyor
            if dev.is_none() && boot.elapsed() > Duration::from_secs(5) {
                line(&mut fb, 8, "No QMX? If it is plugged in, power-cycle the QMX.", Rgb565::CSS_ORANGE);
            } else {
                line(&mut fb, 8, "", Rgb565::BLACK);
            }
        }
        FreeRtos::delay_ms(20);
    }
}

fn text(fb: &mut Framebuffer, s: &str, x: i32, y: i32, c: Rgb565) {
    let _ = Text::new(s, Point::new(x, y), MonoTextStyle::new(&FONT_10X20, c)).draw(fb);
}

fn line(fb: &mut Framebuffer, row: i32, s: &str, c: Rgb565) {
    let y = 70 + row * 36;
    let _ = Rectangle::new(Point::new(0, y - 22), Size::new(W as u32, 32))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(fb);
    text(fb, s, 14, y, c);
}

/// CAT'ten gelen, verilen önekle başlayan son yanıt (ör. "FA" → "FA00014074000;")
fn last_cat_reply(prefix: &str) -> Option<String> {
    let rx = usb::CAT_RX.lock().unwrap();
    rx.split(';')
        .map(str::trim)
        .filter(|r| r.starts_with(prefix))
        .last()
        .map(|r| format!("{r};"))
}

fn draw_status(fb: &mut Framebuffer, dev: Option<(u16, u16)>, s: &IqStats, cat: Option<u8>) {
    let ok = Rgb565::GREEN;
    let bad = Rgb565::CSS_ORANGE;
    match dev {
        Some((v, p)) => line(fb, 0, &format!("USB device: {v:04X}:{p:04X}"), ok),
        None => line(fb, 0, "USB device: waiting for QMX...", bad),
    }
    if s.format.is_empty() {
        line(fb, 1, "Audio: -", bad);
    } else {
        line(fb, 1, &format!("Audio: {}", s.format), ok);
    }
    if s.running {
        line(fb, 2, &format!("Stream: {:.0} frames/s", s.rate), ok);
        line(
            fb,
            3,
            &format!("Level: I {:.1} dBFS   Q {:.1} dBFS", s.i_dbfs, s.q_dbfs),
            Rgb565::WHITE,
        );
        let (msg, c) = if s.corr.abs() > 0.95 {
            ("L = R : plain audio, not I/Q?", bad)
        } else {
            ("I and Q differ : looks like I/Q", ok)
        };
        line(fb, 4, &format!("Corr {:+.2}  {msg}", s.corr), c);
    } else {
        line(fb, 2, "Stream: -", bad);
        line(fb, 3, "", Rgb565::WHITE);
        line(fb, 4, "", Rgb565::WHITE);
    }
    match (cat, last_cat_reply("FA")) {
        (Some(i), Some(r)) => line(fb, 5, &format!("CAT (if {i}): {r}"), ok),
        (Some(i), None) => line(fb, 5, &format!("CAT (if {i}): no reply yet"), bad),
        _ => line(fb, 5, "CAT: -", bad),
    }
    match last_cat_reply("Q9") {
        Some(r) if r == "Q91;" || r == "Q9 1;" => line(fb, 6, &format!("IQ mode: ON ({r})"), ok),
        Some(r) => line(fb, 6, &format!("IQ mode: OFF ({r})"), bad),
        None => line(fb, 6, "IQ mode: unknown", bad),
    }
}

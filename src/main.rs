//! QMXPan — QMX/QMX+ için panadapter (3. adım: spektrum + şelale).
//!
//! Kart USB host olur; QMX'in I/Q sesini alır, FFT ile spektrum ve şelale
//! çizer. CAT ile frekans/mod okunur; spektruma ya da şelaleye dokunmak
//! QMX'i o frekansa ayarlar.

mod board;
mod dsp;
mod lcd;
mod touch;
mod ui;
mod usb;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::units::Hertz;
use log::{info, warn};

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
    let mut tp = touch::Gt911::probe(&mut i2c);
    let mut fb = lcd::init()?;
    let mut ui = ui::Ui::new(&mut fb);

    let events = usb::start()?;
    let stats = Arc::new(Mutex::new(IqStats::default()));

    let mut dev: Option<(u16, u16)> = None;
    let mut iq: Option<IqStream> = None;
    let mut cat: Option<Cat> = None;
    let mut audio_ifaces: Vec<u8> = Vec::new();
    let mut cat_retry: Option<Instant> = None;

    let mut dial: Option<u64> = None;
    let mut mode = String::new();
    let mut last_poll = Instant::now();
    let mut polls = 0u32;
    // QMX'ten son okunan I/Q modu (tampon taşınca yanıt kaybolmasın diye saklanır)
    let mut iq_on = false;
    let mut last_hdr = Instant::now() - Duration::from_secs(1);
    let mut scale_for: Option<Option<u64>> = None;
    let mut last_seq = 0u32;
    let mut pressed = false;
    // Kendi gönderdiğimiz FA'dan sonra eski yanıtlar ekranı geri almasın
    let mut hold_dial_until = Instant::now();

    // Başarım ölçümü
    let mut frames = 0u32;
    let mut draw_us = 0u64;
    let mut perf_t = Instant::now();
    let boot = Instant::now();
    let mut last_peaks = Instant::now();

    loop {
        while let Ok(ev) = events.try_recv() {
            match ev {
                Ev::NewDev(vid, pid) => {
                    info!("USB aygıtı takıldı: {vid:04X}:{pid:04X}");
                    dev = Some((vid, pid));
                    cat_retry = Some(Instant::now() + Duration::from_millis(1500));
                }
                Ev::UacRx { addr, iface } => {
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
                    dial = None;
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

        // CAT portunu bul ve I/Q modunu aç
        if cat.is_none() && cat_retry.is_some_and(|t| Instant::now() >= t) {
            cat_retry = None;
            for iface in (0..8u8).filter(|i| !audio_ifaces.contains(i)) {
                if let Ok(c) = Cat::open(iface) {
                    info!("CAT portu açıldı: arayüz {iface}");
                    let _ = c.send("Q9 1;");
                    FreeRtos::delay_ms(200);
                    let _ = c.send("Q9;FA;MD;");
                    cat = Some(c);
                    break;
                }
            }
        }

        // Frekans ve modu yarım saniyede bir sor
        if let Some(c) = cat.as_ref() {
            if last_poll.elapsed() >= Duration::from_millis(500) {
                last_poll = Instant::now();
                polls += 1;
                // 5 sn'de bir I/Q modunu da doğrula (QMX menüsü ya da MU; kapatabilir)
                let _ = c.send(if polls % 10 == 0 { "FA;MD;Q9;" } else { "FA;MD;" });
            }
            if let Some(q) = cat_value("Q9") {
                iq_on = q.trim() == "1";
            }
            if Instant::now() >= hold_dial_until {
                if let Some(f) = cat_value("FA").and_then(|v| v.parse::<u64>().ok()) {
                    dial = Some(f);
                }
            }
            if let Some(m) = cat_value("MD") {
                mode = mode_name(&m).to_string();
            }
        }

        // Dokunarak ayar: parmak değdiği anda o frekansa git
        if let Some(t) = tp.as_mut() {
            match t.poll(&mut i2c) {
                Some(Some((x, y))) => {
                    if !pressed && (y as usize) >= ui::SPEC_Y {
                        if let (Some(c), Some(d)) = (cat.as_ref(), dial) {
                            let snap = if mode.starts_with("CW") { 10 } else { 100 };
                            let f = (ui::x_to_freq(x as i32, d) + snap / 2) / snap * snap;
                            info!("Dokunarak ayar: {}", ui::fmt_freq(f));
                            usb::CAT_RX.lock().unwrap().clear();
                            let _ = c.send(&format!("FA{f:011};"));
                            dial = Some(f);
                            hold_dial_until = Instant::now() + Duration::from_millis(700);
                        }
                    }
                    pressed = true;
                }
                Some(None) => pressed = false,
                None => {}
            }
        }

        // Yeni spektrum geldiyse çiz
        let spec = {
            let s = dsp::SPECTRUM.lock().unwrap();
            (s.seq != last_seq && !s.db.is_empty()).then(|| {
                last_seq = s.seq;
                s.db.clone()
            })
        };
        if let Some(db) = spec {
            if last_peaks.elapsed() >= Duration::from_secs(3) {
                last_peaks = Instant::now();
                log_peaks(&db, dial);
            }
            let t0 = Instant::now();
            ui.frame(&mut fb, &db, dial);
            draw_us += t0.elapsed().as_micros() as u64;
            frames += 1;
        }

        if scale_for != Some(dial) {
            ui.scale(&mut fb, dial);
            scale_for = Some(dial);
        }

        if last_hdr.elapsed() >= Duration::from_millis(300) {
            last_hdr = Instant::now();
            let s = stats.lock().unwrap().clone();
            if cat.is_none() {
                iq_on = false;
            }
            let (status, ok) = match (dev, s.running, iq_on) {
                (None, _, _) if boot.elapsed() > Duration::from_secs(5) => {
                    ("no QMX - power-cycle it".to_string(), false)
                }
                (None, _, _) => ("waiting for QMX".to_string(), false),
                (Some(_), false, _) => ("no I/Q stream".to_string(), false),
                (Some(_), true, false) => ("IQ mode OFF".to_string(), false),
                (Some(_), true, true) => (format!("IQ {:.1}k", s.rate / 1000.0), true),
            };
            ui.header(&mut fb, dial, &mode, &status, ok);
        }

        if perf_t.elapsed() >= Duration::from_secs(10) {
            if frames > 0 {
                let (spec_ms, wf_ms, lines) = ui.take_timing();
                info!(
                    "Ekran: {:.1} kare/s, kare ort. {:.1} ms (spektrum {:.1} ms, şelale satırı {:.1} ms)",
                    frames as f32 / perf_t.elapsed().as_secs_f32(),
                    draw_us as f32 / frames as f32 / 1000.0,
                    spec_ms / frames as f32,
                    wf_ms / lines.max(1) as f32
                );
            }
            frames = 0;
            draw_us = 0;
            perf_t = Instant::now();
        }
        FreeRtos::delay_ms(5);
    }
}

/// Tanı için: spektrumdaki en güçlü 8 tepeyi mutlak frekans ve LO'ya göre
/// konumuyla kayda yazar (ayna / iç parazit / gerçek sinyal ayrımı için).
fn log_peaks(db: &[f32], dial: Option<u64>) {
    let n = db.len();
    let mut sorted = db.to_vec();
    sorted.sort_unstable_by(|a, b| a.total_cmp(b));
    let median = sorted[n / 2];
    let bin_hz = 48_000.0 / n as f32;
    let mut peaks: Vec<(f32, usize)> = (2..n - 2)
        .filter(|&k| {
            db[k] > median + 12.0
                && db[k] >= db[k - 1]
                && db[k] >= db[k + 1]
                && db[k] >= db[k - 2]
                && db[k] >= db[k + 2]
        })
        .map(|k| (db[k], k))
        .collect();
    peaks.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
    let lo = dial.map(|d| d as i64 - ui::LO_OFFSET);
    let list: Vec<String> = peaks
        .iter()
        .take(8)
        .map(|&(v, k)| {
            let off = ((k as f32 - n as f32 / 2.0) * bin_hz) as i64;
            match lo {
                Some(lo) => format!("{}(LO{off:+}) {:.0}dB", lo + off, v - median),
                None => format!("LO{off:+} {:.0}dB", v - median),
            }
        })
        .collect();
    info!("Tepeler [ayar {:?}, taban {:.0} dBFS]: {}", dial, median, list.join(", "));
}

/// CAT'ten gelen, verilen önekle başlayan son yanıtın değeri (ör. "FA" → "00014074000")
fn cat_value(prefix: &str) -> Option<String> {
    let rx = usb::CAT_RX.lock().unwrap();
    rx.split(';')
        .map(str::trim)
        .filter(|r| r.starts_with(prefix) && r.len() > prefix.len())
        .last()
        .map(|r| r[prefix.len()..].to_string())
}

fn mode_name(md: &str) -> &'static str {
    match md.trim() {
        "1" => "LSB",
        "2" => "USB",
        "3" => "CW",
        "4" => "FM",
        "5" => "AM",
        "6" => "DIGI",
        "7" => "CW-R",
        "9" => "DIGI-R",
        _ => "?",
    }
}

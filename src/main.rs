//! QMXPan — QMX/QMX+ için panadapter.
//!
//! Kart USB host olur; QMX'in I/Q sesini alır, FFT ile spektrum ve şelale
//! çizer. CAT ile frekans/mod okunur; spektruma ya da şelaleye dokunmak
//! QMX'i o frekansa ayarlar. CW çözücü ve FT8 alma da vardır.

mod audio;
mod bands;
mod board;
mod cw;
mod dsp;
mod fonts;
mod ft8;
mod morse;
mod gfx;
mod lcd;
mod net;
mod settings;
mod task;
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

    // Ana döngü (ekran/dokunma/CAT) varsayılan 1 önceliğinde kalırsa öncelik
    // 5'teki her iş parçacığı onu bekletir; 4'e çıkar
    unsafe { esp_idf_svc::sys::vTaskPrioritySet(core::ptr::null_mut(), 4) };

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

    let nvs_part = esp_idf_svc::nvs::EspDefaultNvsPartition::take()?;
    let band_mem = bands::Memory::new(nvs_part.clone());
    let set_store = settings::Store::new(nvs_part.clone());
    ui.apply_settings(set_store.load());
    let mut settings_open = false;
    // Şelale alanındaki panel (CW metni / FT8 listesi) ve en son çizilen içerik
    let mut panel = ui::Panel::None;
    // FT8: 0 kapalı, 1 liste, 2 şelale + spektrum etiketleri
    let mut ft8_mode: u8 = 0;
    let mut cw_drawn: Option<(String, cw::Stat, Option<u64>)> = None;
    let mut ft8_drawn: Option<(u32, bool)> = None;
    ft8::start();
    // Ayar değişince NVS'ye hemen değil, 2 sn sonra bir kez yazılır
    let mut settings_dirty: Option<Instant> = None;
    net::start(
        p.modem,
        esp_idf_svc::eventloop::EspSystemEventLoop::take()?,
        nvs_part,
    );
    let mut marked_valid = false;
    let mut band_popup = false;
    // S-metre (dBFS, geçiş bandındaki en güçlü bin; hızlı yükselir, yavaş düşer)
    let mut smeter: Option<f32> = None;

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
    // Sürükleme: parmak spektrum/şelaledeyken son x; bırakınca o frekansa gidilir
    let mut drag_x: Option<u16> = None;
    let mut pressed = false;
    // Bir pencereyi kapatan dokunuş, parmak kalkana kadar başka iş yapmasın
    let mut swallow = false;
    // Kendi gönderdiğimiz FA/MD'den sonra eski yanıtlar ekranı geri almasın
    let mut hold_dial_until = Instant::now();
    let mut hold_mode_until = Instant::now();
    let mut fw: Option<u32> = None;
    let mut last_fw_raw: Option<String> = None;
    let mut last_q9_fix = Instant::now() - Duration::from_secs(10);

    // Başarım ölçümü
    let mut frames = 0u32;
    let mut draw_us = 0u64;
    let mut perf_t = Instant::now();
    let mut loop_t = Instant::now();
    let mut max_gap_ms = 0u32;
    let boot = Instant::now();
    let mut last_peaks = Instant::now();

    loop {
        // OTA bitti ya da WiFi ayarı değişti: QMX'i düzgünce bırakıp yeniden başla
        if net::RESTART.load(std::sync::atomic::Ordering::SeqCst) {
            info!("Yeniden başlatılıyor: önce QMX bağlantısı kapatılıyor");
            if let Some(c) = cat.take() {
                c.close();
            }
            if let Some(s) = iq.take() {
                s.close();
            }
            FreeRtos::delay_ms(300);
            usb::power_down_port();
            FreeRtos::delay_ms(700);
            unsafe { esp_idf_svc::sys::esp_restart() };
        }
        // 15 sn sorunsuz çalıştıysa bu yazılımı kalıcı yap (OTA geri dönüşünü iptal et)
        if !marked_valid && boot.elapsed() >= Duration::from_secs(15) {
            net::mark_valid();
            marked_valid = true;
        }

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
                // 5 sn'de bir I/Q modunu ve filtre genişliğini de sor
                let _ = c.send(if polls % 10 == 0 { "FA;MD;Q9;FW;" } else { "FA;MD;" });
            }
            if let Some(q) = cat_value("Q9") {
                iq_on = q.trim() == "1";
                // QMX menüsü ya da bazı komutlar I/Q modunu kapatabilir: yeniden aç
                if !iq_on && last_q9_fix.elapsed() >= Duration::from_secs(4) {
                    warn!("QMX I/Q modu kapanmış, yeniden açılıyor");
                    let _ = c.send("Q9 1;Q9;");
                    last_q9_fix = Instant::now();
                }
            }
            if Instant::now() >= hold_dial_until {
                if let Some(f) = cat_value("FA").and_then(|v| v.parse::<u64>().ok()) {
                    dial = Some(f);
                }
            }
            if Instant::now() >= hold_mode_until {
                if let Some(m) = cat_value("MD") {
                    mode = mode_name(&m).to_string();
                }
            }
            let fw_raw = cat_value("FW");
            if fw_raw != last_fw_raw {
                info!("QMX FW yanıtı: {:?}", fw_raw);
                last_fw_raw = fw_raw.clone();
            }
            fw = fw_raw.and_then(|v| v.trim().parse::<u32>().ok()).filter(|&w| (50..=10_000).contains(&w));
        }
        ui.set_passband(ui::passband(&mode, fw));

        // Dokunma: üst şeritte mod düğmeleri ve frekans (band penceresi);
        // spektrum/şelalede sürükleyip bırakınca ayar
        if let Some(t) = tp.as_mut() {
            match t.poll(&mut i2c) {
                Some(Some((x, y))) if settings_open => {
                    if !pressed {
                        let mut s = ui.settings();
                        match ui::settings_hit(x as i32, y as i32) {
                            ui::SettingsHit::Step(i, up) => s.step(i, up),
                            ui::SettingsHit::Reset => s = settings::Settings::default(),
                            ui::SettingsHit::Inside => {}
                            ui::SettingsHit::Outside => {
                                settings_open = false;
                                ui.set_spectrum_hidden(false);
                                scale_for = None;
                                swallow = true;
                            }
                        }
                        if settings_open && s != ui.settings() {
                            ui.apply_settings(s);
                            ui::draw_settings(&mut fb, &s);
                            settings_dirty = Some(Instant::now());
                        }
                    }
                    pressed = true;
                }
                Some(Some((x, y))) if band_popup => {
                    if !pressed {
                        match ui::band_popup_hit(x as i32, y as i32) {
                            Some(Some(ui::SET_BUTTON)) => {
                                band_popup = false;
                                ui::clear_band_popup(&mut fb);
                                settings_open = true;
                                ui.set_spectrum_hidden(true);
                                ui::draw_settings(&mut fb, &ui.settings());
                            }
                            Some(Some(i)) => {
                                if let (Some(c), Some(d)) = (cat.as_ref(), dial) {
                                    // Ayrılan bandın frekansını hatırla
                                    if let Some(cur) = bands::band_of(d) {
                                        band_mem.set(cur, d);
                                    }
                                    let f = band_mem.get(i);
                                    info!("Band {} m: {}", bands::BANDS[i].name, ui::fmt_freq(f));
                                    usb::CAT_RX.lock().unwrap().clear();
                                    let _ = c.send(&format!("FA{f:011};"));
                                    dial = Some(f);
                                    hold_dial_until = Instant::now() + Duration::from_millis(900);
                                    // Eski bandın şelalesi yeni frekanslarla karışmasın
                                    ui.clear_waterfall(&mut fb);
                                }
                                band_popup = false;
                            }
                            Some(None) => {}
                            None => band_popup = false,
                        }
                        if !band_popup && !settings_open {
                            ui::clear_band_popup(&mut fb);
                            ui.redraw_all();
                            scale_for = None;
                            cw_drawn = None;
                            ft8_drawn = None;
                            // Pencere şelale alanını sildi: panel düzenini yeniden kur
                            let p = panel;
                            ui.set_panel(&mut fb, ui::Panel::None);
                            ui.set_panel(&mut fb, p);
                        }
                        swallow = true;
                    }
                    pressed = true;
                }
                Some(Some(_)) if swallow => pressed = true,
                Some(Some((x, y))) => {
                    if (ui::SPEC_Y..ui::WF_Y + ui::WF_H).contains(&(y as usize)) {
                        if dial.is_some() {
                            drag_x = Some(x);
                            ui.set_cursor(Some(x as usize));
                        }
                    } else if !pressed {
                        if let Some(k) = ui::key_at(x as i32, y as i32) {
                            info!("Tuş: {k:?}");
                            match k {
                                ui::Key::Band if dial.is_some() => {
                                    band_popup = true;
                                    ui::draw_band_popup(&mut fb, dial.and_then(bands::band_of));
                                }
                                ui::Key::Display => {
                                    settings_open = true;
                                    ui.set_spectrum_hidden(true);
                                    ui::draw_settings(&mut fb, &ui.settings());
                                }
                                ui::Key::Zoom => {
                                    // ×1 → ×2 → ×4 → ×8 → ×1
                                    let z = match ui.view().zoom {
                                        1 => 2,
                                        2 => 4,
                                        4 => 8,
                                        _ => 1,
                                    };
                                    info!("Büyütme x{z}");
                                    dsp::ZOOM.store(z, std::sync::atomic::Ordering::Relaxed);
                                    ui.set_zoom(&mut fb, z);
                                    scale_for = None;
                                    ui.draw_keys(&mut fb, &keys_active(panel, ft8_mode));
                                    cw_drawn = None;
                                    ft8_drawn = None;
                                }
                                ui::Key::Cw | ui::Key::Ft8 => {
                                    // CW ve FT8 aynı anda açık olmaz. FT8 tuşu üç durum
                                    // arasında döner: liste → şelale + etiketler → kapalı.
                                    let (np, nf) = match k {
                                        ui::Key::Cw if panel == ui::Panel::Cw => (ui::Panel::None, 0),
                                        ui::Key::Cw => (ui::Panel::Cw, 0),
                                        _ => match ft8_mode {
                                            0 => (ui::Panel::Ft8, 1),
                                            1 => (ui::Panel::None, 2),
                                            _ => (ui::Panel::None, 0),
                                        },
                                    };
                                    panel = np;
                                    ft8_mode = nf;
                                    cw::ON.store(panel == ui::Panel::Cw, std::sync::atomic::Ordering::Relaxed);
                                    ft8::ON.store(ft8_mode > 0, std::sync::atomic::Ordering::Relaxed);
                                    if panel == ui::Panel::Cw {
                                        cw::TEXT.lock().unwrap().clear();
                                    }
                                    ui.set_panel(&mut fb, panel);
                                    ui.set_ft8_key(match ft8_mode {
                                        1 => "FT8 LIST",
                                        2 => "FT8 WF",
                                        _ => "FT8",
                                    });
                                    ui.draw_keys(&mut fb, &keys_active(panel, ft8_mode));
                                    cw_drawn = None;
                                    ft8_drawn = None;
                                }
                                _ => {}
                            }
                        } else if let (Some(m), Some(c)) =
                            (ui::mode_chip_at(x as i32, y as i32), cat.as_ref())
                        {
                            let code = match m {
                                "LSB" => 1,
                                "USB" => 2,
                                "CW" => 3,
                                _ => 6,
                            };
                            info!("Mod: {m}");
                            usb::CAT_RX.lock().unwrap().clear();
                            let _ = c.send(&format!("MD{code};MD;FW;"));
                            mode = m.to_string();
                            fw = None;
                            hold_mode_until = Instant::now() + Duration::from_millis(800);
                            last_hdr = Instant::now() - Duration::from_secs(1);
                        } else if ui::freq_area_at(x as i32, y as i32) && dial.is_some() {
                            band_popup = true;
                            ui::draw_band_popup(&mut fb, dial.and_then(bands::band_of));
                        }
                    }
                    pressed = true;
                }
                Some(None) => {
                    pressed = false;
                    swallow = false;
                    if let (Some(x), Some(c), Some(d)) = (drag_x.take(), cat.as_ref(), dial) {
                        let snap = match mode.as_str() {
                            "CW" | "CW-R" => 10,
                            "AM" | "FM" => 1000,
                            _ => 100,
                        };
                        let f = (ui.view().x_to_freq(x as i32, d) + snap / 2) / snap * snap;
                        info!("Dokunarak ayar: {}", ui::fmt_freq(f));
                        usb::CAT_RX.lock().unwrap().clear();
                        let _ = c.send(&format!("FA{f:011};"));
                        dial = Some(f);
                        hold_dial_until = Instant::now() + Duration::from_millis(700);
                    }
                    ui.set_cursor(None);
                }
                None => {}
            }
        }

        // Yeni spektrum geldiyse çiz
        let spec = {
            let s = dsp::SPECTRUM.lock().unwrap();
            (s.seq != last_seq && !s.db.is_empty()).then(|| {
                last_seq = s.seq;
                (s.db.clone(), s.zoom, s.center_hz, s.bin_hz)
            })
        };
        if let Some((mut db, szoom, center_hz, bin_hz)) = spec {
            if szoom == 1 && last_peaks.elapsed() >= Duration::from_secs(10) {
                last_peaks = Instant::now();
                log_peaks(&db, dial);
            }
            // S-metre: geçiş bandındaki en güçlü bin (SSB/DIGI varsayılanı 2.7 kHz)
            let (a, b) = ui::passband(&mode, fw).unwrap_or((0, 2700));
            let p = dsp::peak_in(&db, center_hz, bin_hz, ui::LO_OFFSET + a, ui::LO_OFFSET + b);
            let v = match smeter {
                Some(s) if p < s => s + (p - s) * 0.15, // yavaş düşüş
                _ => p,                                 // anında yükseliş
            };
            smeter = Some(v);
            net::SMETER_DBFS_TENTHS
                .store((v * 10.0) as i32, std::sync::atomic::Ordering::Relaxed);

            // Band penceresi açıkken spektrum/şelale çizilmez (pencereyi bozmasın)
            if !band_popup {
                let t0 = Instant::now();
                ui.frame(&mut fb, &mut db, szoom, dial);
                draw_us += t0.elapsed().as_micros() as u64;
                frames += 1;
            }
        }

        if !settings_open && scale_for != Some(dial) {
            ui.scale(&mut fb, dial);
            scale_for = Some(dial);
        }
        // CW paneli: metin ya da durum değiştiyse yeniden çiz
        if panel == ui::Panel::Cw && !band_popup {
            let now = (cw::TEXT.lock().unwrap().clone(), *cw::STAT.lock().unwrap(), dial);
            if cw_drawn.as_ref() != Some(&now) {
                ui.draw_cw_panel(&mut fb, &now.0, &now.1, now.2);
                cw_drawn = Some(now);
            }
        }
        // FT8 listesi: yeni çözüm geldiyse ya da saat durumu değiştiyse
        ft8::DIAL.store(dial.unwrap_or(0) as u32, std::sync::atomic::Ordering::Relaxed);
        if ft8_mode > 0 && !band_popup {
            let st = ft8::STATUS.lock().unwrap().clone();
            let now = (ft8::GEN.load(std::sync::atomic::Ordering::SeqCst), st.time_ok);
            if ft8_drawn != Some(now) {
                let msgs = ft8::MSGS.lock().unwrap().clone();
                if panel == ui::Panel::Ft8 {
                    ui.draw_ft8_panel(&mut fb, &msgs, &st, MYCALL);
                }
                // Son dilimde çözülen istasyonlar spektrumda etiketlenir
                let labels = msgs
                    .iter()
                    .filter(|m| m.slot == st.last_slot && m.dial != 0)
                    .filter_map(|m| {
                        let call = m.sender()?;
                        let color = if m.text.contains(MYCALL) {
                            gfx::th::AM
                        } else if m.text.starts_with("CQ ") {
                            gfx::th::GR
                        } else {
                            gfx::th::TX
                        };
                        Some(ui::Label { rf: m.dial + m.freq as u64, text: call, color })
                    })
                    .collect();
                ui.set_labels(labels);
                ft8_drawn = Some(now);
            }
        } else if ft8_mode == 0 {
            ui.set_labels(Vec::new());
        }
        if settings_dirty.is_some_and(|t| t.elapsed() >= Duration::from_secs(2)) {
            set_store.save(&ui.settings());
            info!("Görüntü ayarları kaydedildi: {:?}", ui.settings());
            settings_dirty = None;
        }

        if last_hdr.elapsed() >= Duration::from_millis(200) {
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
                (Some(_), true, true) => (format!("IQ {:.1} kHz", s.rate / 1000.0), true),
            };
            let ota = net::OTA.load(std::sync::atomic::Ordering::Relaxed);
            let (status, ok) = if (1..=101).contains(&ota) {
                (format!("OTA {}%", ota.min(100)), true)
            } else {
                (status, ok)
            };
            let st = net::STATUS.lock().unwrap().clone();
            let net_line = match st.mode {
                net::Mode::Station => st.ip.clone(),
                net::Mode::SetupAp => format!("Setup: {} -> {}", st.ssid, st.ip),
                net::Mode::Connecting => format!("WiFi {}...", st.ssid),
                net::Mode::Off => String::new(),
            };
            let cal = net::CAL_TENTHS.load(std::sync::atomic::Ordering::Relaxed) as f32 / 10.0;
            ui.set_cal(cal);
            let sm = if ok && s.running && iq_on {
                smeter.map(|v| (v + cal).round() as i32)
            } else {
                None
            };
            let hdr = ui::Hdr {
                dial,
                mode: mode.clone(),
                bw: fw,
                status,
                ok,
                net: net_line,
                smeter: sm,
                zoom: ui.view().zoom,
            };
            ui.header(&mut fb, &hdr);
        }

        if perf_t.elapsed() >= Duration::from_secs(10) {
            if frames > 0 {
                let (spec_ms, wf_ms, lines) = ui.take_timing();
                let line = format!(
                    "{:.1} fps, frame {:.1} ms (spectrum {:.1}, wf line {:.1}), max loop gap {} ms",
                    frames as f32 / perf_t.elapsed().as_secs_f32(),
                    draw_us as f32 / frames as f32 / 1000.0,
                    spec_ms / frames as f32,
                    wf_ms / lines.max(1) as f32,
                    max_gap_ms
                );
                info!("Ekran: {line}");
                *net::PERF.lock().unwrap() = line;
            }
            frames = 0;
            draw_us = 0;
            max_gap_ms = 0;
            perf_t = Instant::now();
        }
        FreeRtos::delay_ms(5);
        // Döngünün en uzun duraklaması (takılma ölçüsü)
        max_gap_ms = max_gap_ms.max(loop_t.elapsed().as_millis() as u32);
        loop_t = Instant::now();
    }
}

/// FT8 listesinde vurgulanan çağrı işareti
const MYCALL: &str = "TA4SO";

/// Tuş çubuğunda açık (camgöbeği) gösterilecek tuşlar
fn keys_active(panel: ui::Panel, ft8_mode: u8) -> Vec<ui::Key> {
    let mut v = Vec::new();
    if panel == ui::Panel::Cw {
        v.push(ui::Key::Cw);
    }
    if ft8_mode > 0 {
        v.push(ui::Key::Ft8);
    }
    v
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

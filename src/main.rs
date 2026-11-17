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
mod dx;
mod fonts;
mod ft8;
mod morse;
mod gfx;
mod lcd;
mod meters;
mod net;
mod prop;
mod settings;
mod solar;
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
    // OTA geri dönüşünü iptal et: ekran başlamadan ÖNCE yapılmalı. İşaretleme
    // flash'ı belleğe eşlerken önbelleği bir an dondurur; ekran çalışıyorsa
    // RGB kesmesi PSRAM'deki görüntüyü okumaya çalışıp kartı çökertiyor.
    net::mark_valid();
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
    let mut ft8_labels: Vec<ui::Label> = Vec::new();
    let mut labels_t = Instant::now();
    let mut cw_drawn: Option<(String, cw::Stat, Option<u64>)> = None;
    let mut ft8_drawn: Option<(u32, bool)> = None;
    ft8::start();
    // Ayar değişince NVS'ye hemen değil, 2 sn sonra bir kez yazılır
    let mut settings_dirty: Option<Instant> = None;
    dx::start(nvs_part.clone());
    solar::start();
    meters::prebuild();
    net::start(
        p.modem,
        esp_idf_svc::eventloop::EspSystemEventLoop::take()?,
        nvs_part,
    );
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
    // Bant isteği: QMX gerçekten o banda geçti mi (bazı QMX'lerde bazı bantlar yok)
    let mut band_check: Option<(usize, Instant)> = None;
    // Üst şeritte birkaç saniye gösterilen uyarı
    let mut notice: Option<(String, Instant)> = None;
    let mut last_kick = Instant::now();
    let mut kicks = 0u32;

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
    let mut drag_start: Option<(i32, i32)> = None;
    let mut pressed = false;
    // Bir pencereyi kapatan dokunuş, parmak kalkana kadar başka iş yapmasın
    let mut swallow = false;
    // Kendi gönderdiğimiz FA/MD'den sonra eski yanıtlar ekranı geri almasın
    let mut hold_dial_until = Instant::now();
    let mut hold_mode_until = Instant::now();
    let mut fw: Option<u32> = None;
    // QMX'in CW offset'i (CW'de LO'yu bu kadar daha aşağı koyar); QMX'ten okunur
    let mut cw_offset: u32 = 700;
    let mut last_fw_raw: Option<String> = None;
    let mut last_q9_fix = Instant::now() - Duration::from_secs(10);

    // METERS sayfası (ilk açılışta hazırlanır) ve gönderim okumaları
    let mut meters: Option<meters::Meters> = None;
    let mut prop: Option<prop::Prop> = None;
    // Alt sayfa açık mı (METERS ya da PROPAGATION) ve hangisi
    let mut meters_open = false;
    let mut sub_tab = meters::Tab::Meters;
    let mut last_meter_poll = Instant::now();
    let mut tx_on = false;
    let mut tx_pwr: Option<f32> = None;
    let mut tx_swr: Option<f32> = None;
    let mut tx_raw: (Option<String>, Option<String>, Option<String>) = (None, None, None);

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
                        Err(e) => warn!("I/Q akışı açılamadı: {e} (boş iç RAM {} B)", free_internal()),
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

        // QMX yokken USB portu 6 sn'de bir kapatılıp açılır: başarısız bir tanıma
        // denemesinden sonra port, QMX kapatılıp açılsa bile yeni bağlantıyı görmüyor
        if dev.is_none() && boot.elapsed() >= Duration::from_secs(8) && last_kick.elapsed() >= Duration::from_secs(6) {
            last_kick = Instant::now();
            kicks += 1;
            if kicks % 10 == 1 {
                info!("QMX yok: USB portu yeniden açılıyor ({kicks}. kez)");
            }
            usb::kick_port();
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
                // 5 sn'de bir I/Q modunu ve filtre genişliğini, CW'de ayrıca
                // CW offset'i sor (QMX'in menüsünden değiştirilebilir)
                let is_cw = mode.starts_with("CW");
                let q = match polls % 10 {
                    0 => "FA;MD;Q9;FW;",
                    5 if is_cw => "FA;MD;MMCW|CW offset;",
                    _ => "FA;MD;",
                };
                let _ = c.send(q);
            }
            // "MM700;": QMX'in CW offset'i (500..950 Hz)
            if let Some(v) = cat_value("MM").and_then(|v| v.trim().parse::<u32>().ok()) {
                if (500..=950).contains(&v) && v != cw_offset {
                    info!("QMX CW offset: {v} Hz");
                    cw_offset = v;
                }
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
                // FA yanıtı 11 hanedir; tampon kırpılınca yarım kalan "FA0" gibi
                // parçalar 0 Hz okunuyordu
                if let Some(f) = cat_value("FA")
                    .filter(|v| v.len() == 11)
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    // Tanı: QMX'in bildirdiği frekans büyük ölçüde değiştiyse kayda yaz
                    if dial.is_some_and(|d| d.abs_diff(f) > 100_000) {
                        info!("QMX frekansı (CAT FA): {} → {}", ui::fmt_freq(dial.unwrap()), ui::fmt_freq(f));
                    }
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

            // METERS açıkken gönderim durumu, güç ve SWR 5 kez/sn sorulur.
            // Yanıtlar okununca tampondan silinir: eski değer yeniden okunmasın.
            if meters_open && sub_tab == meters::Tab::Meters && last_meter_poll.elapsed() >= Duration::from_millis(200) {
                last_meter_poll = Instant::now();
                let _ = c.send("TQ;PC;SW;");
            }
            let raw = (cat_take("TQ"), cat_take("PC"), cat_take("SW"));
            if raw.0.is_some() || raw.1.is_some() || raw.2.is_some() {
                if raw != tx_raw {
                    info!("QMX TX yanıtı: TQ {:?} PC {:?} SW {:?}", raw.0, raw.1, raw.2);
                    tx_raw = raw.clone();
                }
                if let Some(t) = &raw.0 {
                    tx_on = t.trim() == "1";
                }
                if let Some(v) = raw.1 {
                    tx_pwr = v.trim().parse::<f32>().ok().filter(|p| (0.0..=20.0).contains(p));
                }
                if let Some(v) = raw.2 {
                    tx_swr = v.trim().parse::<f32>().ok().filter(|s| *s >= 1.0);
                }
            }
        }
        // CW'de QMX, LO'yu CW offset kadar daha aşağı koyar: ayar frekansı
        // ana bantta 12000 + CW offset'tedir (diğer modlarda 12000)
        let if_hz = ui::LO_OFFSET + if mode.starts_with("CW") { cw_offset as i64 } else { 0 };
        if if_hz != ui.view().if_hz {
            info!("IF {if_hz} Hz ({mode})");
            dsp::IF_HZ.store(if_hz as u32, std::sync::atomic::Ordering::Relaxed);
            ui.set_if(&mut fb, if_hz);
            scale_for = None;
        }
        ui.set_passband(ui::passband(&mode, fw));

        // Web'den sayfa değiştirme (uzaktan ekran denetimi)
        let mut close_sub = false;
        let mut want_zoom: Option<u32> = None;
        match net::GOTO.swap(0, std::sync::atomic::Ordering::SeqCst) {
            1 if meters_open => close_sub = true,
            g @ (2 | 3) => {
                if !meters_open {
                    if band_popup || settings_open {
                        band_popup = false;
                        settings_open = false;
                        ui.set_spectrum_hidden(false);
                    }
                    meters_open = true;
                    ui.suspend();
                }
                sub_tab = if g == 3 { meters::Tab::Prop } else { meters::Tab::Meters };
                open_sub(&mut fb, sub_tab, &mut meters, &mut prop);
            }
            _ => {}
        }

        // Dokunma: üst şeritte mod düğmeleri ve frekans (band penceresi);
        // spektrum/şelalede sürükleyip bırakınca ayar
        if let Some(t) = tp.as_mut() {
            match t.poll(&mut i2c) {
                Some(Some((x, y))) if meters_open => {
                    let tab = if pressed { None } else { meters::tab_at(x as i32, y as i32) };
                    match tab {
                        Some(t @ (meters::Tab::Meters | meters::Tab::Prop)) if t != sub_tab => {
                            info!("Sayfa: {t:?}");
                            sub_tab = t;
                            open_sub(&mut fb, sub_tab, &mut meters, &mut prop);
                        }
                        _ => {}
                    }
                    if tab == Some(meters::Tab::Back) {
                        close_sub = true;
                        swallow = true;
                    }
                    pressed = true;
                }
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
                                    band_check = Some((i, Instant::now()));
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
                            if drag_x.is_none() {
                                drag_start = Some((x as i32, y as i32));
                            }
                            drag_x = Some(x);
                            ui.set_cursor(Some(x as usize));
                        }
                    } else if !pressed {
                        if let Some(k) = ui::key_at(x as i32, y as i32) {
                            info!("Tuş: {k:?}");
                            match k {
                                ui::Key::Band if dial.is_some() => {
                                    band_popup = true;
                                    ui::draw_band_popup(&mut fb, dial.and_then(bands::band_of), band_mem.missing());
                                }
                                ui::Key::Menu => {
                                    info!("Alt sayfa açıldı: {sub_tab:?}");
                                    meters_open = true;
                                    ui.suspend();
                                    open_sub(&mut fb, sub_tab, &mut meters, &mut prop);
                                    swallow = true;
                                }
                                ui::Key::Display => {
                                    settings_open = true;
                                    ui.set_spectrum_hidden(true);
                                    ui::draw_settings(&mut fb, &ui.settings());
                                }
                                ui::Key::Zoom => {
                                    // ×1 → ×2 → ×4 → ×8 → ×16 → ×1
                                    let z = match ui.view().zoom {
                                        1 => 2,
                                        2 => 4,
                                        4 => 8,
                                        8 => 16,
                                        _ => 1,
                                    };
                                    want_zoom = Some(z);
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
                                    // FT8 şelalesine geçerken ×16: ayarın 0..3 kHz üstü tüm ekranı kaplar
                                    if nf == 2 && ui.view().zoom != 16 {
                                        want_zoom = Some(16);
                                    }
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
                            ui::draw_band_popup(&mut fb, dial.and_then(bands::band_of), band_mem.missing());
                        }
                    }
                    pressed = true;
                }
                Some(None) => {
                    pressed = false;
                    swallow = false;
                    let start = drag_start.take();
                    if let (Some(x), Some(c), Some(d)) = (drag_x.take(), cat.as_ref(), dial) {
                        let snap = match mode.as_str() {
                            "CW" | "CW-R" => 10,
                            "AM" | "FM" => 1000,
                            _ => 100,
                        };
                        // Bir etikete kısa dokunuş: tam o istasyonun frekansına git
                        let on_label = start
                            .filter(|&(sx, _)| (sx - x as i32).abs() < 12)
                            .and_then(|(sx, sy)| ui.label_at(sx, sy));
                        let f = on_label
                            .unwrap_or_else(|| (ui.view().x_to_freq(x as i32, d) + snap / 2) / snap * snap);
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

        // Seçilen banda 1,5 sn içinde geçilmediyse QMX o bandı desteklemiyor:
        // işaretle (band penceresinde soluk) ve üst şeritte uyar
        if let Some((i, t)) = band_check {
            // QMX bağlantısı koptuysa karar verilmez (yanlışlıkla "yok" işaretlenmesin)
            if cat.is_none() || dial.is_none() {
                band_check = None;
            } else if t.elapsed() >= Duration::from_millis(1500) {
                band_check = None;
                let mask = band_mem.missing();
                let now_in = dial.and_then(bands::band_of) == Some(i);
                let new = if now_in { mask & !(1 << i) } else { mask | (1 << i) };
                if !now_in {
                    warn!("QMX {} m bandına geçmedi", bands::BANDS[i].name);
                    notice = Some((format!("QMX: {} m not available", bands::BANDS[i].name), Instant::now()));
                    last_hdr = Instant::now() - Duration::from_secs(1);
                }
                if new != mask {
                    band_mem.set_missing(new);
                }
            }
        }

        // Büyütme değişimi (ZOOM tuşu ya da FT8 şelalesine geçiş)
        if let Some(z) = want_zoom.take() {
            info!("Büyütme x{z}");
            dsp::ZOOM.store(z, std::sync::atomic::Ordering::Relaxed);
            ui.set_zoom(&mut fb, z);
            scale_for = None;
            ui.draw_keys(&mut fb, &keys_active(panel, ft8_mode));
            cw_drawn = None;
            ft8_drawn = None;
        }
        // FT8 açıkken büyütülmüş görünümün ortası ayar + 1500 Hz (USB tarafı;
        // LSB'de −1500): dijital bant ekranın ortasına gelir
        let ofs: i64 = if ft8_mode == 0 {
            0
        } else {
            match mode.as_str() {
                "USB" | "DIGI" => 1500,
                "LSB" | "DIGI-R" => -1500,
                _ => 0,
            }
        };
        if ofs != ui.view().ofs && !meters_open {
            info!("Görünüm kayması {ofs:+} Hz");
            dsp::ZOOM_OFS.store(ofs as i32, std::sync::atomic::Ordering::Relaxed);
            ui.set_ofs(&mut fb, ofs);
            scale_for = None;
        }

        // Alt sayfadan panadapter'a dönüş: ekranı baştan kur
        if close_sub && meters_open {
            info!("Alt sayfa kapandı");
            meters_open = false;
            ui.repaint(&mut fb);
            ui.set_panel(&mut fb, panel);
            ui.draw_keys(&mut fb, &keys_active(panel, ft8_mode));
            scale_for = None;
            cw_drawn = None;
            ft8_drawn = None;
            last_hdr = Instant::now() - Duration::from_secs(1);
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
            let ifh = ui.view().if_hz;
            let p = dsp::peak_in(&db, center_hz, bin_hz, ifh + a, ifh + b);
            let v = match smeter {
                Some(s) if p < s => s + (p - s) * 0.15, // yavaş düşüş
                _ => p,                                 // anında yükseliş
            };
            smeter = Some(v);
            net::SMETER_DBFS_TENTHS
                .store((v * 10.0) as i32, std::sync::atomic::Ordering::Relaxed);

            // Band penceresi ya da METERS açıkken spektrum/şelale çizilmez
            if !band_popup && !meters_open {
                let t0 = Instant::now();
                ui.frame(&mut fb, &mut db, szoom, dial);
                draw_us += t0.elapsed().as_micros() as u64;
                frames += 1;
            }
        }

        if meters_open && sub_tab == meters::Tab::Prop {
            if let Some(p) = prop.as_mut() {
                p.update(&mut fb);
            }
        }
        if meters_open && sub_tab == meters::Tab::Meters {
            if let Some(m) = meters.as_mut() {
                let s = stats.lock().unwrap().clone();
                let cal = net::CAL_TENTHS.load(std::sync::atomic::Ordering::Relaxed) as f32 / 10.0;
                let band = dial.and_then(bands::band_of).map(|i| format!("{}m · ", bands::BANDS[i].name));
                let bw = match ui::passband(&mode, fw).map(|(a, b)| b - a) {
                    Some(v) if v >= 1000 => format!(" · {:.1} kHz", v as f32 / 1000.0),
                    Some(v) => format!(" · {v} Hz"),
                    None => String::new(),
                };
                let data = meters::Data {
                    dial,
                    info: format!("{}{}{}", band.unwrap_or_default(), mode, bw),
                    dbm: smeter.filter(|_| s.running && iq_on).map(|v| v + cal),
                    tx: tx_on,
                    pwr: tx_pwr,
                    swr: tx_swr,
                };
                m.update(&mut fb, &data);
            }
        }
        // Band penceresi açıkken ölçek çizilmez (pencerenin alt sırasının üstüne biniyordu)
        if !settings_open && !meters_open && !band_popup && scale_for != Some(dial) {
            ui.scale(&mut fb, dial);
            scale_for = Some(dial);
        }
        // CW paneli: metin ya da durum değiştiyse yeniden çiz
        if panel == ui::Panel::Cw && !band_popup && !meters_open {
            let now = (cw::TEXT.lock().unwrap().clone(), *cw::STAT.lock().unwrap(), dial);
            if cw_drawn.as_ref() != Some(&now) {
                ui.draw_cw_panel(&mut fb, &now.0, &now.1, now.2);
                cw_drawn = Some(now);
            }
        }
        // FT8 listesi: yeni çözüm geldiyse ya da saat durumu değiştiyse
        ft8::DIAL.store(dial.unwrap_or(0) as u32, std::sync::atomic::Ordering::Relaxed);
        if ft8_mode > 0 && !band_popup && !meters_open {
            let st = ft8::STATUS.lock().unwrap().clone();
            let now = (ft8::GEN.load(std::sync::atomic::Ordering::SeqCst), st.time_ok);
            if ft8_drawn != Some(now) {
                let msgs = ft8::MSGS.lock().unwrap().clone();
                if panel == ui::Panel::Ft8 {
                    ui.draw_ft8_panel(&mut fb, &msgs, &st, MYCALL);
                }
                // Son dilimde çözülen istasyonlar spektrumda etiketlenir
                ft8_labels = msgs
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
                ft8_drawn = Some(now);
                labels_t = Instant::now() - Duration::from_secs(1);
            }
        } else if ft8_mode == 0 {
            ft8_labels.clear();
        }
        // Spektrum etiketleri: FT8 çözümleri + görünür aralıktaki DX cluster spotları
        if labels_t.elapsed() >= Duration::from_millis(500) {
            labels_t = Instant::now();
            let mut labels = ft8_labels.clone();
            if let Some(d) = dial {
                let v = ui.view();
                let c = v.center(d);
                let half = v.px_to_hz(ui::W_HALF);
                let lo = (c - half).max(0) as u64;
                let hi = (c + half).max(0) as u64;
                for s in dx::in_range(lo, hi) {
                    if !labels.iter().any(|l| l.text == s.call) {
                        labels.push(ui::Label { rf: s.freq_hz, text: s.call, color: gfx::th::VI });
                    }
                }
            }
            ui.set_labels(labels);
        }
        if settings_dirty.is_some_and(|t| t.elapsed() >= Duration::from_secs(2)) {
            set_store.save(&ui.settings());
            info!("Görüntü ayarları kaydedildi: {:?}", ui.settings());
            settings_dirty = None;
        }

        if !meters_open && last_hdr.elapsed() >= Duration::from_millis(200) {
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
            if notice.as_ref().is_some_and(|(_, t)| t.elapsed() >= Duration::from_secs(5)) {
                notice = None;
            }
            let (status, ok) = if (1..=101).contains(&ota) {
                (format!("OTA {}%", ota.min(100)), true)
            } else if let Some((n, _)) = notice.as_ref() {
                (n.clone(), false)
            } else {
                (status, ok)
            };
            let st = net::STATUS.lock().unwrap().clone();
            let net_line = match st.mode {
                net::Mode::Station => {
                    let n = dx::SPOTS.lock().unwrap().len();
                    if dx::STATE.lock().unwrap().as_str() == "connected" {
                        format!("DX {n} · {}", st.ip)
                    } else {
                        st.ip.clone()
                    }
                }
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
                utc: utc_now(),
            };
            ui.header(&mut fb, &hdr);
        }

        if perf_t.elapsed() >= Duration::from_secs(10) {
            if frames > 0 {
                let (spec_ms, wf_ms, lines) = ui.take_timing();
                let line = format!(
                    "{:.1} fps, frame {:.1} ms (spectrum {:.1}, wf line {:.1}), max loop gap {} ms, internal RAM free {} B",
                    frames as f32 / perf_t.elapsed().as_secs_f32(),
                    draw_us as f32 / frames as f32 / 1000.0,
                    spec_ms / frames as f32,
                    wf_ms / lines.max(1) as f32,
                    max_gap_ms,
                    free_internal()
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

/// Boş iç RAM (bayt): görev yığınları ve USB/WiFi buradan alır
fn free_internal() -> usize {
    unsafe { esp_idf_svc::sys::heap_caps_get_free_size(esp_idf_svc::sys::MALLOC_CAP_INTERNAL) }
}

/// Alt sayfayı (ilk açılışta hazırlayıp) ekrana çizer
fn open_sub(
    fb: &mut lcd::Framebuffer,
    tab: meters::Tab,
    meters: &mut Option<meters::Meters>,
    prop: &mut Option<prop::Prop>,
) {
    match tab {
        meters::Tab::Prop => prop.get_or_insert_with(prop::Prop::new).enter(fb),
        _ => meters.get_or_insert_with(meters::take_or_build).enter(fb),
    }
}

/// UTC saati "SS:DD:ss"; SNTP saati henüz ayarlamadıysa boş
fn utc_now() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if t < 1_735_689_600 {
        return String::new();
    }
    let s = t % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// cat_value gibi, ama bulunan yanıtları tampondan siler: aynı değer bir
/// sonraki soruda eskiden kalma olarak yeniden okunmaz.
fn cat_take(prefix: &str) -> Option<String> {
    let mut rx = usb::CAT_RX.lock().unwrap();
    if !rx.contains(prefix) {
        return None;
    }
    let mut parts: Vec<&str> = rx.split(';').collect();
    let tail = parts.pop().unwrap_or("");
    let mut val = None;
    let mut keep = String::new();
    for p in parts {
        let t = p.trim();
        if t.starts_with(prefix) && t.len() > prefix.len() {
            val = Some(t[prefix.len()..].to_string());
        } else {
            keep.push_str(p);
            keep.push(';');
        }
    }
    keep.push_str(tail);
    *rx = keep;
    val
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

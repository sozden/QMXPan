//! USB host: QMX'in I/Q ses akışı (UAC) ve CAT seri portu (CDC-ACM).
//!
//! Sürücü geri çağrıları kendi görevlerinde çalışır; olayları bir kanalla
//! ana döngüye iletiriz, ağır işler (aygıt açma/kapama) orada yapılır.

use core::ffi::c_void;
use core::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use esp_idf_svc::sys::{esp, extra as x, EspError};
use log::{info, warn};

pub enum Ev {
    /// Yeni USB aygıtı takıldı (VID, PID)
    NewDev(u16, u16),
    /// Aygıtta ses giriş (RX) arayüzü bulundu
    UacRx { addr: u8, iface: u8 },
    /// Ses arayüzü çıkarıldı (tutamaç)
    UacGone(usize),
    /// CAT portu çıkarıldı (tutamaç)
    CdcGone(usize),
}

static EV_TX: OnceLock<Mutex<Sender<Ev>>> = OnceLock::new();

fn send(e: Ev) {
    if let Some(tx) = EV_TX.get() {
        let _ = tx.lock().unwrap().send(e);
    }
}

/// CAT portundan gelen ham baytlar
pub static CAT_RX: Mutex<String> = Mutex::new(String::new());

// ------------------------------------------------------------------ başlatma

/// USB host kütüphanesini ve iki sınıf sürücüsünü kurar.
pub fn start() -> anyhow::Result<Receiver<Ev>> {
    let (tx, rx) = channel();
    let _ = EV_TX.set(Mutex::new(tx));

    unsafe {
        let mut cfg: x::usb_host_config_t = core::mem::zeroed();
        cfg.intr_flags = esp_idf_svc::sys::ESP_INTR_FLAG_LEVEL1 as _;
        esp!(x::usb_host_install(&cfg))?;
    }

    // USB host kütüphanesinin olay döngüsü
    std::thread::Builder::new()
        .name("usb_lib".into())
        .stack_size(4096)
        .spawn(|| loop {
            let mut flags: u32 = 0;
            unsafe {
                x::usb_host_lib_handle_events(u32::MAX, &mut flags);
            }
        })?;

    unsafe {
        let mut ucfg: x::uac_host_driver_config_t = core::mem::zeroed();
        ucfg.create_background_task = true;
        ucfg.task_priority = 5;
        ucfg.stack_size = 4096;
        ucfg.core_id = 0;
        ucfg.callback = Some(uac_driver_cb);
        esp!(x::uac_host_install(&ucfg))?;

        let mut ccfg: x::cdc_acm_host_driver_config_t = core::mem::zeroed();
        ccfg.driver_task_stack_size = 4096;
        ccfg.driver_task_priority = 10;
        ccfg.xCoreID = 0;
        ccfg.new_dev_cb = Some(new_dev_cb);
        esp!(x::cdc_acm_host_install(&ccfg))?;
    }
    info!("USB host hazır, QMX bekleniyor");
    Ok(rx)
}

/// Yeniden başlatmadan önce USB portunu kapatır (QMX bağlantının bittiğini görsün).
pub fn power_down_port() {
    unsafe {
        x::usb_host_lib_set_root_port_power(false);
    }
}

// ------------------------------------------------------------ geri çağrılar

unsafe extern "C" fn new_dev_cb(dev: x::usb_device_handle_t) {
    let mut dd: *const x::usb_device_desc_t = core::ptr::null();
    let (mut vid, mut pid) = (0, 0);
    if x::usb_host_get_device_descriptor(dev, &mut dd) == 0 && !dd.is_null() {
        vid = (*dd).__bindgen_anon_1.idVendor;
        pid = (*dd).__bindgen_anon_1.idProduct;
        x::usb_print_device_descriptor(dd);
    }
    let mut cd: *const x::usb_config_desc_t = core::ptr::null();
    if x::usb_host_get_active_config_descriptor(dev, &mut cd) == 0 && !cd.is_null() {
        x::usb_print_config_descriptor(cd, None);
    }
    send(Ev::NewDev(vid, pid));
}

unsafe extern "C" fn uac_driver_cb(
    addr: u8,
    iface: u8,
    event: x::uac_host_driver_event_t,
    _arg: *mut c_void,
) {
    if event == x::uac_host_driver_event_t_UAC_HOST_DRIVER_EVENT_RX_CONNECTED {
        send(Ev::UacRx { addr, iface });
    } else {
        info!("UAC: ses çıkış (TX) arayüzü {iface} var (kullanılmıyor)");
    }
}

unsafe extern "C" fn uac_device_cb(
    h: x::uac_host_device_handle_t,
    event: x::uac_host_device_event_t,
    _arg: *mut c_void,
) {
    if event == x::uac_host_device_event_t_UAC_HOST_DRIVER_EVENT_DISCONNECTED {
        send(Ev::UacGone(h as usize));
    } else if event == x::uac_host_device_event_t_UAC_HOST_DEVICE_EVENT_TRANSFER_ERROR {
        warn!("UAC aktarım hatası");
    }
}

unsafe extern "C" fn cdc_data_cb(data: *const u8, len: usize, _arg: *mut c_void) -> bool {
    let s = core::slice::from_raw_parts(data, len);
    let mut rx = CAT_RX.lock().unwrap();
    rx.push_str(&String::from_utf8_lossy(s));
    if rx.len() > 512 {
        let cut = rx.len() - 256;
        rx.drain(..cut);
    }
    true
}

unsafe extern "C" fn cdc_event_cb(ev: *const x::cdc_acm_host_dev_event_data_t, _arg: *mut c_void) {
    let ev = &*ev;
    if ev.type_ == x::cdc_acm_host_dev_event_t_CDC_ACM_HOST_DEVICE_DISCONNECTED {
        send(Ev::CdcGone(ev.data.cdc_hdl as usize));
    } else if ev.type_ == x::cdc_acm_host_dev_event_t_CDC_ACM_HOST_ERROR {
        warn!("CDC hata: {}", ev.data.error);
    }
}

// ------------------------------------------------------------------- CAT

pub struct Cat {
    h: x::cdc_acm_dev_hdl_t,
    pub iface: u8,
}

unsafe impl Send for Cat {}

impl Cat {
    /// Verilen arayüz numarasında CDC portu açmayı dener.
    pub fn open(iface: u8) -> Result<Self, EspError> {
        unsafe {
            let mut oc: x::cdc_acm_host_open_config_t = core::mem::zeroed();
            oc.interface_idx = iface;
            oc.connection_timeout_ms = 1000;
            oc.out_buffer_size = 64;
            oc.in_buffer_size = 64;
            oc.event_cb = Some(cdc_event_cb);
            oc.data_cb = Some(cdc_data_cb);
            let mut h: x::cdc_acm_dev_hdl_t = null_mut();
            esp!(x::cdc_acm_host_open_v2(&oc, &mut h))?;
            Ok(Self { h, iface })
        }
    }

    pub fn handle(&self) -> usize {
        self.h as usize
    }

    pub fn send(&self, cmd: &str) -> Result<(), EspError> {
        unsafe {
            esp!(x::cdc_acm_host_data_tx_blocking(
                self.h,
                cmd.as_ptr(),
                cmd.len(),
                200
            ))
        }
    }

    pub fn close(self) {
        unsafe {
            x::cdc_acm_host_close(self.h);
        }
    }
}

// ------------------------------------------------------------------- I/Q

/// Ses akışının son bir saniyelik ölçümü
#[derive(Clone, Default)]
pub struct IqStats {
    pub format: String,
    pub rate: f32,
    pub i_dbfs: f32,
    pub q_dbfs: f32,
    /// I ile Q arasındaki ilinti: ~1 ise iki kanal aynı (I/Q değil, düz ses)
    pub corr: f32,
    pub running: bool,
}

pub struct IqStream {
    h: usize,
    stop: Arc<AtomicBool>,
}

impl IqStream {
    pub fn handle(&self) -> usize {
        self.h
    }

    /// Ses arayüzünü açar, 48 kHz'e en yakın biçimi seçip akışı başlatır.
    pub fn open(addr: u8, iface: u8, stats: Arc<Mutex<IqStats>>) -> anyhow::Result<Self> {
        unsafe {
            let mut dc: x::uac_host_device_config_t = core::mem::zeroed();
            dc.addr = addr;
            dc.iface_num = iface;
            dc.buffer_size = 48_000 * 2 * 4 / 10; // ~100 ms
            dc.buffer_threshold = 0;
            dc.callback = Some(uac_device_cb);
            let mut h: x::uac_host_device_handle_t = null_mut();
            esp!(x::uac_host_device_open(&dc, &mut h))?;
            x::uac_host_printf_device_param(h);

            let mut info: x::uac_host_dev_info_t = core::mem::zeroed();
            esp!(x::uac_host_get_device_info(h, &mut info))?;

            // Uygun alternatif ayarı bul (48 kHz tercih)
            let mut chosen: Option<(u8, u8, u8, u32)> = None;
            for alt in 1..=info.iface_alt_num {
                let mut p: x::uac_host_dev_alt_param_t = core::mem::zeroed();
                if x::uac_host_get_device_alt_param(h, alt, &mut p) != 0 {
                    continue;
                }
                let freqs: Vec<u32> = if p.sample_freq_type > 0 {
                    let n = (p.sample_freq_type as usize).min(p.__bindgen_anon_1.sample_freq.len());
                    p.__bindgen_anon_1.sample_freq[..n].to_vec()
                } else {
                    let r = p.__bindgen_anon_1.__bindgen_anon_1;
                    if (r.sample_freq_lower..=r.sample_freq_upper).contains(&48_000) {
                        vec![48_000]
                    } else {
                        vec![r.sample_freq_upper]
                    }
                };
                info!(
                    "UAC alt {alt}: {} kanal, {} bit ({} bayt), frekanslar {:?}",
                    p.channels, p.bit_resolution, p.subframe_size, freqs
                );
                let f = if freqs.contains(&48_000) { 48_000 } else { freqs[0] };
                // Öncelik: 48 kHz stereo; yoksa ilk bulunan
                let ideal = |c: (u8, u8, u8, u32)| c.0 == 2 && c.3 == 48_000;
                let cand = (p.channels, p.bit_resolution, p.subframe_size, f);
                if chosen.map_or(true, |c| !ideal(c) && ideal(cand)) {
                    chosen = Some(cand);
                }
            }
            let (ch, bits, sub, freq) =
                chosen.ok_or_else(|| anyhow::anyhow!("uygun ses biçimi yok"))?;

            let mut sc: x::uac_host_stream_config_t = core::mem::zeroed();
            sc.channels = ch;
            sc.bit_resolution = bits;
            sc.sample_freq = freq;
            esp!(x::uac_host_device_start(h, &sc))?;
            let format = format!("{freq} Hz, {ch} ch, {bits} bit");
            info!("I/Q akışı başladı: {format}");

            let stop = Arc::new(AtomicBool::new(false));
            let st = stop.clone();
            let hu = h as usize;
            stats.lock().unwrap().format = format;
            // Çekirdek 1, yüksek öncelik: ses akışı hiç beklememeli
            crate::task::spawn(c"iq_read", 12 * 1024, 6, esp_idf_svc::hal::cpu::Core::Core1, move || {
                reader(hu, ch as usize, sub as usize, st, stats)
            })?;
            Ok(Self { h: hu, stop })
        }
    }

    pub fn close(self) {
        self.stop.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        unsafe {
            let h = self.h as x::uac_host_device_handle_t;
            // Akışı durdur (QMX'e "alt ayar 0" gider), sonra kapat
            x::uac_host_device_stop(h);
            x::uac_host_device_close(h);
        }
    }
}

fn sample(b: &[u8], sub: usize) -> f32 {
    match sub {
        2 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
        3 => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
        _ => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
    }
}

fn db(x: f64) -> f32 {
    (10.0 * (x.max(1e-20)).log10()) as f32
}

fn reader(h: usize, ch: usize, sub: usize, stop: Arc<AtomicBool>, stats: Arc<Mutex<IqStats>>) {
    let frame = ch * sub;
    let mut buf = vec![0u8; frame * 480]; // 10 ms
    let (mut n, mut si, mut sq, mut siq) = (0u64, 0f64, 0f64, 0f64);
    let mut t0 = Instant::now();
    let mut an = crate::dsp::Analyzer::new();
    // CW: ayar frekansındaki sinyal → 700 Hz ton, 6 kHz (±900 Hz pencere)
    let mut cw_demod = crate::audio::Demod::new(12_000.0, 900.0, 8, 700.0, 100.0);
    // Susturma 6 (~13 dB): bant gürültüsü ve parazitten sahte E/I/T daha az çıksın
    let mut cw_dec = crate::cw::Decoder::new(700.0, true, 6);
    let mut cw_t = Instant::now();
    // FT8: USB geçiş bandı (ayar + 0..3 kHz) → 12 kHz gerçek ses
    let mut ft8_demod = crate::audio::Demod::new(12_000.0 + 1_500.0, 1_700.0, 4, 1_500.0, 0.05);
    let mut ft8_buf: Vec<f32> = Vec::with_capacity(256);
    while !stop.load(Ordering::SeqCst) {
        let cw_on = crate::cw::ON.load(Ordering::Relaxed);
        let ft8_on = crate::ft8::ON.load(Ordering::Relaxed);
        let mut got: u32 = 0;
        let r = unsafe {
            x::uac_host_device_read(
                h as x::uac_host_device_handle_t,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut got,
                50,
            )
        };
        if r != 0 && r != esp_idf_svc::sys::ESP_ERR_TIMEOUT {
            std::thread::sleep(Duration::from_millis(20));
        }
        for f in buf[..got as usize - got as usize % frame].chunks_exact(frame) {
            let i = sample(&f[..sub], sub) as f64;
            let q = if ch > 1 { sample(&f[sub..2 * sub], sub) as f64 } else { 0.0 };
            an.push(i as f32, q as f32);
            if cw_on {
                if let Some(a) = cw_demod.push(i as f32, q as f32) {
                    cw_dec.push(a);
                }
            }
            if ft8_on {
                if let Some(a) = ft8_demod.push(i as f32, q as f32) {
                    ft8_buf.push(a);
                }
            }
            si += i * i;
            sq += q * q;
            siq += i * q;
            n += 1;
        }
        // FT8 sesini (zaman damgasıyla) çözücü görevine gönder
        // 50 ms'lik parçalar (kuyrukta 64 parça = 3.2 sn tampon)
        if ft8_on && ft8_buf.len() >= 600 {
            crate::ft8::feed(core::mem::take(&mut ft8_buf), crate::ft8::now_unix());
            ft8_buf.reserve(700);
        }
        // CW çözücü çıktısını ve durumunu ~10 kez/s paylaş
        if cw_on && cw_t.elapsed() >= Duration::from_millis(100) {
            cw_t = Instant::now();
            if !cw_dec.out.is_empty() {
                let mut t = crate::cw::TEXT.lock().unwrap();
                t.push_str(&cw_dec.out);
                if t.len() > 400 {
                    let cut = t.len() - 300;
                    let cut = (cut..t.len()).find(|&i| t.is_char_boundary(i)).unwrap_or(t.len());
                    t.drain(..cut);
                }
                cw_dec.out.clear();
            }
            *crate::cw::STAT.lock().unwrap() = crate::cw::Stat {
                wpm: cw_dec.wpm().round() as u32,
                tone_hz: cw_dec.freq.round() as u32,
                snr_db: cw_dec.snr_db.max(0.0).round() as u32,
                key: cw_dec.tone(),
            };
        }
        let dt = t0.elapsed().as_secs_f32();
        if dt >= 1.0 {
            let mut s = stats.lock().unwrap();
            s.rate = n as f32 / dt;
            s.running = n > 0;
            if n > 0 {
                s.i_dbfs = db(si / n as f64);
                s.q_dbfs = db(sq / n as f64);
                s.corr = (siq / (si * sq).sqrt().max(1e-30)) as f32;
            }
            log::debug!(
                "I/Q: {:.0} örnek/s, I {:.1} dBFS, Q {:.1} dBFS, ilinti {:.2}",
                s.rate, s.i_dbfs, s.q_dbfs, s.corr
            );
            (n, si, sq, siq) = (0, 0.0, 0.0, 0.0);
            t0 = Instant::now();
        }
    }
    stats.lock().unwrap().running = false;
}

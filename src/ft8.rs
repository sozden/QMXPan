//! FT8 alma: 12 kHz USB sesi 15 sn'lik UTC dilimlerine bölünür, her dilim
//! sonunda ft8_lib ile (components/ft8w sarmalayıcısı) çözülür.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use esp_idf_svc::sys::ft8 as c;
use log::{info, warn};

pub const RATE: usize = 12_000;
const SLOT: f64 = 15.0;
/// Dilimde çözüme başlamak için gereken en az ses (sn): FT8 iletimi 0.5..13.1 sn
const MIN_AUDIO_S: f64 = 13.0;
/// Panelde tutulan en fazla mesaj
const KEEP: usize = 40;

/// FT8 alma açık mı (FT8 tuşu)
pub static ON: AtomicBool = AtomicBool::new(false);
/// Gömülü test kaydını çöz (web: POST /api/ft8test)
pub static SELFTEST: AtomicBool = AtomicBool::new(false);
/// Sonuç listesi her değiştiğinde artar (ekran yeniden çizilsin)
pub static GEN: AtomicU32 = AtomicU32::new(0);
/// QMX'in o anki ayar frekansı (ana döngü yazar); çözülen mesajın RF frekansı için.
/// (Xtensa'da AtomicU64 yok; 54 MHz'e kadar u32 yeter.)
pub static DIAL: AtomicU32 = AtomicU32::new(0);

/// SNR kalibrasyonu: WSJT-X SNR'si (2500 Hz) ≈ SNR_SLOPE · sig_db + SNR_OFFSET.
/// Gömülü test kaydındaki 13 mesajın WSJT-X değerlerine doğrusal uydurma
/// (kalan sapma ±2.4 dB).
pub const SNR_SLOPE: f32 = 0.743;
pub const SNR_OFFSET: f32 = -24.4;

/// Arama ayarları (en az puan, aday sayısı, LDPC turu); web'den denenebilir
pub static PARAMS: Mutex<(i32, i32, i32)> = Mutex::new((10, 120, 20));

#[derive(Clone)]
pub struct Msg {
    /// Dilim başlangıcı "HHMMSS" (UTC) ya da "TEST"
    pub slot: String,
    pub snr: i32,
    /// Ton gücü − gürültü tabanı (bin başına, dB; kalibrasyon için)
    pub sig_db: f32,
    pub dt: f32,
    pub freq: f32,
    pub text: String,
    /// Çözüm anındaki ayar frekansı (RF = dial + freq); öz testte 0
    pub dial: u64,
}

impl Msg {
    /// Mesajı gönderen istasyon: "CQ [DX|POTA..] ÇAĞRI [GRID]" ya da "ALICI GÖNDEREN ..."
    pub fn sender(&self) -> Option<String> {
        let t: Vec<&str> = self.text.split_whitespace().collect();
        let raw = if t.first() == Some(&"CQ") {
            match t.len() {
                0..=1 => return None,
                2 | 3 => t[1],
                _ => t[2],
            }
        } else {
            *t.get(1)?
        };
        let c = raw.trim_matches(|ch| ch == '<' || ch == '>');
        (c.len() >= 3 && c != "...").then(|| c.to_string())
    }
}

#[derive(Clone, Default)]
pub struct Status {
    pub last_slot: String,
    pub count: usize,
    pub decode_ms: u32,
    /// Saat SNTP ile ayarlandı mı
    pub time_ok: bool,
}

/// En yeni en başta
pub static MSGS: Mutex<Vec<Msg>> = Mutex::new(Vec::new());
pub static STATUS: Mutex<Status> = Mutex::new(Status {
    last_slot: String::new(),
    count: 0,
    decode_ms: 0,
    time_ok: false,
});

static TX: OnceLock<SyncSender<(Vec<f32>, f64)>> = OnceLock::new();

/// I/Q okuyucudan: bir parça 12 kHz ses ve son örneğin UNIX zamanı (sn)
pub fn feed(samples: Vec<f32>, t_end: f64) {
    if let Some(tx) = TX.get() {
        let _ = tx.try_send((samples, t_end));
    }
}

pub fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn time_ok() -> bool {
    // 2025-01-01'den sonrası: SNTP saati ayarlamış demektir
    now_unix() > 1_735_689_600.0
}

fn hhmmss(t: f64) -> String {
    let s = t as u64 % 86_400;
    format!("{:02}{:02}{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// Çözücüye giden iş: bir dilimin sesi ve dilim adı
type Job = (Vec<f32>, String);

pub fn start() {
    let (tx, rx) = sync_channel::<(Vec<f32>, f64)>(64);
    let _ = TX.set(tx);
    let (jtx, jrx) = sync_channel::<Job>(1);
    use esp_idf_svc::hal::cpu::Core;
    // Toplayıcı hiç beklememeli (öncelik 4); çözücü saniyeler sürer, düşük
    // öncelikte boşta kalan zamanı kullanır. monitor_process yığında ~30 KB ister.
    let r1 = crate::task::spawn(c"ft8_rx", 8 * 1024, 4, Core::Core1, move || collect(rx, jtx));
    let r2 = crate::task::spawn(c"ft8_dec", 48 * 1024, 2, Core::Core1, move || decoder(jrx));
    if let Err(e) = r1.and(r2) {
        warn!("FT8 görevleri başlatılamadı: {e}");
    }
}

/// Çözücü görevi: dilimleri sırayla çözer; öz testi de burada çalışır
fn decoder(jobs: Receiver<Job>) {
    if unsafe { c::ft8w_init(200.0, 3000.0) } != 0 {
        warn!("FT8 çözücü başlatılamadı (bellek?)");
        return;
    }
    let block = unsafe { c::ft8w_block_size() } as usize;
    info!("FT8 çözücü hazır (blok {block} örnek)");
    loop {
        if SELFTEST.swap(false, Ordering::SeqCst) {
            selftest(block);
        }
        if let Ok((audio, slot)) = jobs.recv_timeout(Duration::from_millis(200)) {
            decode(&audio, block, &slot);
        }
    }
}

/// Toplayıcı görevi: sesi UTC dilimlerine böler, biten dilimi çözücüye verir
fn collect(rx: Receiver<(Vec<f32>, f64)>, jobs: SyncSender<Job>) {
    let mut buf: Vec<f32> = Vec::with_capacity(RATE * 15);
    let mut cur_slot: Option<i64> = None;
    let mut valid = false;

    loop {
        let (chunk, t_end) = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !ON.load(Ordering::Relaxed) {
            buf.clear();
            cur_slot = None;
            continue;
        }
        let ok = time_ok();
        STATUS.lock().unwrap().time_ok = ok;
        if !ok {
            continue;
        }
        let t_start = t_end - chunk.len() as f64 / RATE as f64;
        let slot = (t_start / SLOT).floor() as i64;
        if cur_slot != Some(slot) {
            // Biten dilimi çözücüye ver (çözücü hâlâ meşgulse bu dilim atlanır)
            if let Some(s) = cur_slot {
                if valid && buf.len() as f64 >= MIN_AUDIO_S * RATE as f64 {
                    let full = core::mem::replace(&mut buf, Vec::with_capacity(RATE * 15));
                    if jobs.try_send((full, hhmmss(s as f64 * SLOT))).is_err() {
                        warn!("FT8: çözücü meşgul, dilim atlandı");
                    }
                }
            }
            buf.clear();
            cur_slot = Some(slot);
            // Dilime geç girildiyse başı sıfırla doldur (zaman hizası korunur)
            let late = t_start - slot as f64 * SLOT;
            valid = late < 1.0;
            buf.resize((late.max(0.0) * RATE as f64) as usize, 0.0);
        }
        let room = RATE * 15 - buf.len();
        buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
}

fn decode(audio: &[f32], block: usize, slot: &str) {
    let t0 = Instant::now();
    let mut res: Vec<c::ft8w_result_t> = (0..60).map(|_| unsafe { core::mem::zeroed() }).collect();
    let (ms_, mc, li) = *PARAMS.lock().unwrap();
    let n = unsafe {
        c::ft8w_set_params(ms_, mc, li);
        c::ft8w_reset();
        for b in audio.chunks_exact(block) {
            c::ft8w_process(b.as_ptr());
        }
        c::ft8w_decode(res.as_mut_ptr(), res.len() as i32)
    } as usize;
    let ms = t0.elapsed().as_millis() as u32;
    let dial = if slot == "TEST" { 0 } else { DIAL.load(Ordering::Relaxed) as u64 };
    let mut new: Vec<Msg> = res[..n]
        .iter()
        .map(|r| Msg {
            slot: slot.to_string(),
            snr: (r.sig_db * SNR_SLOPE + SNR_OFFSET).round().clamp(-30.0, 40.0) as i32,
            sig_db: r.sig_db,
            // ft8_lib zamanı dilim başına göre; WSJT-X DT'si 0.5 sn kaydırılmış
            dt: r.time_sec - 0.5,
            freq: r.freq_hz,
            text: unsafe { core::ffi::CStr::from_ptr(r.text.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
            dial,
        })
        .collect();
    new.sort_by(|a, b| a.freq.total_cmp(&b.freq));
    info!("FT8 {slot}: {n} mesaj, {ms} ms");
    for m in &new {
        info!("  {} {:+3} {:+4.1} {:4.0} {}", m.slot, m.snr, m.dt, m.freq, m.text);
    }
    {
        let mut all = MSGS.lock().unwrap();
        let mut merged = new;
        merged.extend(all.drain(..));
        merged.truncate(KEEP);
        *all = merged;
    }
    *STATUS.lock().unwrap() = Status {
        last_slot: slot.to_string(),
        count: n,
        decode_ms: ms,
        time_ok: time_ok(),
    };
    GEN.fetch_add(1, Ordering::SeqCst);
}

/// Gömülü kayıt (ft8_lib test seti, WSJT-X 18 mesaj çözüyor) ile çözücü testi
fn selftest(block: usize) {
    static WAV: &[u8] = include_bytes!("../assets/ft8_test.wav");
    let data = &WAV[44..];
    let audio: Vec<f32> = data
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect();
    info!("FT8 öz testi: {} örnek", audio.len());
    decode(&audio, block, "TEST");
}

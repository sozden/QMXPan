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

#[derive(Clone)]
pub struct Msg {
    /// Dilim başlangıcı "HHMMSS" (UTC) ya da "TEST"
    pub slot: String,
    pub snr: i32,
    pub dt: f32,
    pub freq: f32,
    pub text: String,
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

pub fn start() {
    let (tx, rx) = sync_channel::<(Vec<f32>, f64)>(256);
    let _ = TX.set(tx);
    let r = std::thread::Builder::new()
        .name("ft8".into())
        // monitor_process yığında ~30 KB dizi kullanıyor
        .stack_size(48 * 1024)
        .spawn(move || run(rx));
    if let Err(e) = r {
        warn!("FT8 görevi başlatılamadı: {e}");
    }
}

fn run(rx: Receiver<(Vec<f32>, f64)>) {
    if unsafe { c::ft8w_init(200.0, 3000.0) } != 0 {
        warn!("FT8 çözücü başlatılamadı (bellek?)");
        return;
    }
    let block = unsafe { c::ft8w_block_size() } as usize;
    info!("FT8 çözücü hazır (blok {block} örnek)");
    let mut buf: Vec<f32> = Vec::with_capacity(RATE * 15);
    let mut cur_slot: Option<i64> = None;
    let mut valid = false;

    loop {
        if SELFTEST.swap(false, Ordering::SeqCst) {
            selftest(block);
        }
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
            // Önceki dilimi çöz
            if let Some(s) = cur_slot {
                if valid && buf.len() as f64 >= MIN_AUDIO_S * RATE as f64 {
                    decode(&buf, block, &hhmmss(s as f64 * SLOT));
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
    let mut res: Vec<c::ft8w_result_t> = (0..50).map(|_| unsafe { core::mem::zeroed() }).collect();
    let n = unsafe {
        c::ft8w_reset();
        for b in audio.chunks_exact(block) {
            c::ft8w_process(b.as_ptr());
        }
        c::ft8w_decode(res.as_mut_ptr(), res.len() as i32)
    } as usize;
    let ms = t0.elapsed().as_millis() as u32;
    let mut new: Vec<Msg> = res[..n]
        .iter()
        .map(|r| Msg {
            slot: slot.to_string(),
            snr: r.snr as i32,
            // ft8_lib zamanı dilim başına göre; WSJT-X DT'si 0.5 sn kaydırılmış
            dt: r.time_sec - 0.5,
            freq: r.freq_hz,
            text: unsafe { core::ffi::CStr::from_ptr(r.text.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
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

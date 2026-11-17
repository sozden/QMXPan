//! Full ve Lite sürümlerinin ikisinde de bulunan ortak durum: çağrı işareti,
//! S-metre kalibrasyonu, yeniden başlatma isteği, elle kurulan saat.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::sync::Mutex;

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use log::{info, warn};

/// Bu derlemede internet hizmetleri (WiFi, web, OTA, DX cluster, yayılım) var mı
pub const ONLINE: bool = cfg!(feature = "online");
/// Ekranda ve web sayfasında gösterilen paket adı
pub const EDITION: &str = if ONLINE { "Full" } else { "Lite" };

/// Kullanıcının çağrı işareti (FT8 vurgusu, DX cluster girişi); NVS "dxcall"
pub static MYCALL: Mutex<String> = Mutex::new(String::new());

/// OTA ilerlemesi: 0 = yok, 1..=100 = yazılıyor, 101 = bitti
pub static OTA: AtomicU8 = AtomicU8::new(0);
/// Web'den istenen sayfa: 0 yok, 1 panadapter, 2 METERS, 3 PROPAGATION
pub static GOTO: AtomicU8 = AtomicU8::new(0);
/// Ekran başarımı (ana döngü her 10 sn'de yazar)
pub static PERF: Mutex<String> = Mutex::new(String::new());
/// Ana döngüye "QMX'i bırak ve yeniden başlat" isteği
pub static RESTART: AtomicBool = AtomicBool::new(false);

/// S-metre kalibrasyonu: dBm = dBFS + CAL (onda bir dB). Varsayılan -2.0 dB.
pub static CAL_TENTHS: AtomicI32 = AtomicI32::new(-20);
/// Ana döngünün ölçtüğü son S-metre değeri (dBFS, onda bir dB)
pub static SMETER_DBFS_TENTHS: AtomicI32 = AtomicI32::new(i32::MIN);

/// Saat elle kuruldu (internet saati yok): FT8 zaman kayması ile ince ayar yapılır
pub static MANUAL_TIME: AtomicBool = AtomicBool::new(false);

const NS: &str = "qmxpan";

/// Açılışta NVS'den çağrı işareti ve kalibrasyonu okur
pub fn load(part: EspDefaultNvsPartition) {
    let Ok(nvs) = EspNvs::new(part, NS, true) else {
        warn!("Ortak ayarlar okunamadı");
        return;
    };
    let mut b = [0u8; 16];
    if let Ok(Some(s)) = nvs.get_str("dxcall", &mut b) {
        *MYCALL.lock().unwrap() = s.to_string();
    }
    if let Ok(Some(c)) = nvs.get_i32("cal") {
        CAL_TENTHS.store(c, Ordering::Relaxed);
        info!("S-metre kalibrasyonu: {:+.1} dB", c as f32 / 10.0);
    }
}

/// Çağrı işaretini kaydeder (boş olabilir)
pub fn save_call(part: EspDefaultNvsPartition, call: &str) {
    *MYCALL.lock().unwrap() = call.to_string();
    match EspNvs::new(part, NS, true).and_then(|n| n.set_str("dxcall", call)) {
        Ok(()) => info!("Çağrı işareti kaydedildi: '{call}'"),
        Err(e) => warn!("Çağrı işareti kaydedilemedi: {e}"),
    }
}

/// Geçerli bir çağrı işareti mi (3..12 karakter, harf/rakam/'/', en az bir rakam ve harf)
pub fn valid_call(c: &str) -> bool {
    (3..=12).contains(&c.len())
        && c.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '/')
        && c.chars().any(|ch| ch.is_ascii_digit())
        && c.chars().any(|ch| ch.is_ascii_uppercase())
}

/// Saat elle ayarlanırken kullanılan tarih: 2026-01-01 00:00 UTC. Tarih
/// bilinmez; FT8 ve saat göstergesi için günün saati yeter.
const MANUAL_EPOCH: i64 = 1_767_225_600;

/// Sistem saatini ayarlar (UNIX saniyesi + mikro saniye)
fn set_clock(sec: i64, usec: i64) {
    let tv = esp_idf_svc::sys::timeval { tv_sec: sec as _, tv_usec: usec as _ };
    unsafe { esp_idf_svc::sys::settimeofday(&tv, core::ptr::null()) };
}

pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Saat bilinmiyor mu (internet de elle kurulum da olmadı)
pub fn time_unknown() -> bool {
    now_us() < 1_735_689_600_000_000
}

/// Elle kurulum: verilen UTC saat:dakika, şu an tam :00 saniyesi
pub fn set_manual_time(hh: u32, mm: u32) {
    // Saat zaten biliniyorsa tarih korunur, yalnız günün saati değişir
    let now = now_us() / 1_000_000;
    let day = if time_unknown() { MANUAL_EPOCH } else { now - now.rem_euclid(86_400) };
    set_clock(day + hh as i64 * 3600 + mm as i64 * 60, 0);
    MANUAL_TIME.store(true, Ordering::SeqCst);
    info!("Saat elle kuruldu: {hh:02}:{mm:02}:00 UTC");
}

/// Saati `delta_ms` kadar kaydırır (FT8 zaman kayması düzeltmesi)
pub fn nudge_clock(delta_ms: i64) {
    let t = now_us() + delta_ms * 1000;
    set_clock(t.div_euclid(1_000_000), t.rem_euclid(1_000_000));
}

/// İnternet saati geldi: elle kurulum geçersiz
pub fn ntp_synced() {
    MANUAL_TIME.store(false, Ordering::SeqCst);
}

/// Yeni yazılım düzgün açıldıysa OTA geri dönüşünü iptal eder.
pub fn mark_valid() {
    match esp_idf_svc::ota::EspOta::new().and_then(|mut o| o.mark_running_slot_valid()) {
        Ok(()) => info!("Çalışan yazılım sağlam olarak işaretlendi"),
        Err(e) => warn!("Yazılım işaretlenemedi: {e}"),
    }
}

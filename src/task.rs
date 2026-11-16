//! Görevleri belirli çekirdeğe ve önceliğe yerleştirerek başlatma.
//!
//! Çekirdek planı:
//! - Çekirdek 0: ana döngü (ekran, dokunma, CAT), WiFi, USB sürücüleri
//! - Çekirdek 1: I/Q okuyucu (öncelik 6: FFT, CW, FT8 sesi; hiç beklememeli),
//!   FT8 çözücü (öncelik 2: saniyeler süren çözüm diğerlerini bekletmesin)

use std::ffi::CStr;
use std::thread::JoinHandle;

use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;

pub fn spawn<F>(name: &'static CStr, stack: usize, priority: u8, core: Core, f: F) -> std::io::Result<JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    let _ = ThreadSpawnConfiguration {
        name: Some(name),
        stack_size: stack,
        priority,
        pin_to_core: Some(core),
        ..Default::default()
    }
    .set();
    let r = std::thread::Builder::new().stack_size(stack).spawn(f);
    // Sonraki std::thread'ler varsayılanla başlasın
    let _ = ThreadSpawnConfiguration::default().set();
    r
}

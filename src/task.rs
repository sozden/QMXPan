//! Görevleri belirli çekirdeğe ve önceliğe yerleştirerek başlatma.
//!
//! Çekirdek planı:
//! - Çekirdek 0: ana döngü (ekran, dokunma, CAT), WiFi, USB sürücüleri
//! - Çekirdek 1: I/Q okuyucu (öncelik 6: FFT, CW, FT8 sesi; hiç beklememeli),
//!   FT8 çözücü (öncelik 2: saniyeler süren çözüm diğerlerini bekletmesin)
//!
//! İç RAM azdır (I/Q okuyucusu, USB ve WiFi ona muhtaç). Ağ işleri gibi
//! zamanı önemsiz görevlerin yığını `spawn_psram` ile PSRAM'e konur.

use std::ffi::CStr;
use std::thread::JoinHandle;

use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::task::thread::{MallocCap, ThreadSpawnConfiguration};

pub fn spawn<F>(name: &'static CStr, stack: usize, priority: u8, core: Core, f: F) -> std::io::Result<JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    spawn_in(name, stack, priority, core, false, f)
}

/// Yığını PSRAM'de olan görev (ağ, TLS gibi yavaş işler için)
pub fn spawn_psram<F>(name: &'static CStr, stack: usize, priority: u8, core: Core, f: F) -> std::io::Result<JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    spawn_in(name, stack, priority, core, true, f)
}

fn spawn_in<F>(name: &'static CStr, stack: usize, priority: u8, core: Core, psram: bool, f: F) -> std::io::Result<JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    let mut cfg = ThreadSpawnConfiguration {
        name: Some(name),
        stack_size: stack,
        priority,
        pin_to_core: Some(core),
        ..Default::default()
    };
    if psram {
        cfg.stack_alloc_caps = MallocCap::Spiram | MallocCap::Cap8bit;
    }
    let _ = cfg.set();
    let r = std::thread::Builder::new().stack_size(stack).spawn(f);
    // Sonraki std::thread'ler varsayılanla başlasın
    let _ = ThreadSpawnConfiguration::default().set();
    r
}

//! DX cluster istemcisi: telnet ile bağlanır, çağrı işaretiyle giriş yapar,
//! "DX de ..." satırlarından spotları toplar. Spotlar spektrumda etiketlenir.
//!
//! Varsayılan: dxc.ve7cc.net:23, çağrı işareti TA4SO (kullanıcı onayladı).
//! Ayarlar web sayfasından değişir (NVS: dxon, dxhost, dxport, dxcall).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use log::{info, warn};

/// Spotlar bu kadar süre tutulur
const KEEP: Duration = Duration::from_secs(20 * 60);
const MAX_SPOTS: usize = 300;

#[derive(Clone, Debug)]
pub struct Spot {
    pub freq_hz: u64,
    pub call: String,
    pub spotter: String,
    pub comment: String,
    pub at: Instant,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Cfg {
    pub on: bool,
    pub host: String,
    pub port: u16,
    pub call: String,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            on: true,
            host: "dxc.ve7cc.net".into(),
            port: 23,
            call: "TA4SO".into(),
        }
    }
}

pub static SPOTS: Mutex<Vec<Spot>> = Mutex::new(Vec::new());
/// Spot listesi değişince artar
pub static GEN: AtomicU32 = AtomicU32::new(0);
/// Bağlantı durumu (web sayfası ve üst şerit için)
pub static STATE: Mutex<String> = Mutex::new(String::new());
/// Ayarlar değişti: bağlantı yeni ayarlarla kurulsun
pub static RELOAD: AtomicBool = AtomicBool::new(false);

fn set_state(s: &str) {
    *STATE.lock().unwrap() = s.to_string();
}

pub fn load_cfg(nvs: &EspNvs<NvsDefault>) -> Cfg {
    let mut c = Cfg::default();
    if let Ok(Some(v)) = nvs.get_u8("dxon") {
        c.on = v != 0;
    }
    let mut b = [0u8; 64];
    if let Ok(Some(s)) = nvs.get_str("dxhost", &mut b) {
        if !s.is_empty() {
            c.host = s.to_string();
        }
    }
    if let Ok(Some(p)) = nvs.get_u16("dxport") {
        c.port = p;
    }
    let mut b = [0u8; 16];
    if let Ok(Some(s)) = nvs.get_str("dxcall", &mut b) {
        if !s.is_empty() {
            c.call = s.to_string();
        }
    }
    c
}

pub fn save_cfg(nvs: &EspNvs<NvsDefault>, c: &Cfg) -> anyhow::Result<()> {
    nvs.set_u8("dxon", c.on as u8)?;
    nvs.set_str("dxhost", &c.host)?;
    nvs.set_u16("dxport", c.port)?;
    nvs.set_str("dxcall", &c.call)?;
    Ok(())
}

pub fn start(part: EspDefaultNvsPartition) {
    let r = crate::task::spawn_psram(c"dx", 8 * 1024, 3, esp_idf_svc::hal::cpu::Core::Core0, move || {
        let nvs = match EspNvs::new(part, "qmxpan", true) {
            Ok(n) => n,
            Err(e) => {
                warn!("DX: NVS açılamadı: {e}");
                return;
            }
        };
        run(nvs)
    });
    if let Err(e) = r {
        warn!("DX görevi başlatılamadı: {e}");
    }
}

fn run(nvs: EspNvs<NvsDefault>) {
    loop {
        RELOAD.store(false, Ordering::SeqCst);
        let cfg = load_cfg(&nvs);
        if !cfg.on {
            set_state("off");
            wait_reload(Duration::from_secs(3600));
            continue;
        }
        if crate::net::STATUS.lock().unwrap().mode != crate::net::Mode::Station {
            set_state("waiting for WiFi");
            wait_reload(Duration::from_secs(5));
            continue;
        }
        set_state("connecting");
        match session(&cfg) {
            Ok(()) => info!("DX: bağlantı kapandı"),
            Err(e) => {
                warn!("DX: {e:#}");
                set_state(&format!("error: {e}"));
            }
        }
        // Yeniden bağlanmadan önce bekle (ayar değişirse hemen)
        wait_reload(Duration::from_secs(30));
    }
}

fn wait_reload(d: Duration) {
    let t0 = Instant::now();
    while t0.elapsed() < d && !RELOAD.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn session(cfg: &Cfg) -> anyhow::Result<()> {
    let addr = (cfg.host.as_str(), cfg.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("address not found"))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    info!("DX: {}:{} bağlandı", cfg.host, cfg.port);
    set_state("logging in");

    let mut buf = [0u8; 512];
    let mut line: Vec<u8> = Vec::with_capacity(256);
    let mut logged_in = false;
    let mut last_rx = Instant::now();
    let mut last_ka = Instant::now();
    let t0 = Instant::now();
    loop {
        if RELOAD.load(Ordering::SeqCst) {
            return Ok(());
        }
        match s.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                last_rx = Instant::now();
                let mut i = 0;
                while i < n {
                    let b = buf[i];
                    // Telnet seçenek görüşmesi (IAC x y): atla
                    if b == 0xFF {
                        i += 3;
                        continue;
                    }
                    i += 1;
                    match b {
                        b'\n' => {
                            handle_line(&String::from_utf8_lossy(&line));
                            line.clear();
                        }
                        b'\r' | 0x07 => {}
                        _ => {
                            if line.len() < 250 {
                                line.push(b);
                            }
                        }
                    }
                }
                // Giriş istemi satır sonu olmadan gelir
                if !logged_in {
                    let p = String::from_utf8_lossy(&line).to_ascii_lowercase();
                    if p.contains("call") || p.contains("login") {
                        s.write_all(format!("{}\r\n", cfg.call).as_bytes())?;
                        logged_in = true;
                        line.clear();
                        info!("DX: {} olarak giriş yapıldı", cfg.call);
                        set_state("connected");
                    }
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }
        // İstem gelmediyse 15 sn sonra yine de gir
        if !logged_in && t0.elapsed() > Duration::from_secs(15) {
            s.write_all(format!("{}\r\n", cfg.call).as_bytes())?;
            logged_in = true;
            set_state("connected");
        }
        // Bağlantı canlı kalsın: 5 dk sessizlikte boş satır
        if last_rx.elapsed() > Duration::from_secs(300) && last_ka.elapsed() > Duration::from_secs(300) {
            s.write_all(b"\r\n")?;
            last_ka = Instant::now();
        }
        if last_rx.elapsed() > Duration::from_secs(900) {
            anyhow::bail!("no data for 15 min");
        }
    }
}

/// "DX de SP5XYZ:     14025.0  JA1ABC       CW 22 dB 25 WPM CQ      1234Z"
fn parse(line: &str) -> Option<Spot> {
    let rest = line.trim_start().strip_prefix("DX de ")?;
    let (spotter, rest) = rest.split_once(':')?;
    let mut it = rest.split_whitespace();
    let khz: f64 = it.next()?.parse().ok()?;
    let call = it.next()?.to_string();
    let words: Vec<&str> = it.collect();
    // Son kelime saat ("1234Z") ise yorumdan çıkar
    let n = words.len();
    let comment = if n > 0 && words[n - 1].ends_with('Z') && words[n - 1].len() == 5 {
        words[..n - 1].join(" ")
    } else {
        words.join(" ")
    };
    if !(1_000.0..=60_000.0).contains(&khz) || call.len() < 3 {
        return None;
    }
    Some(Spot {
        freq_hz: (khz * 1000.0).round() as u64,
        call,
        spotter: spotter.trim().to_string(),
        comment,
        at: Instant::now(),
    })
}

fn handle_line(line: &str) {
    let Some(sp) = parse(line) else { return };
    let mut v = SPOTS.lock().unwrap();
    // Aynı istasyonun yakın frekanstaki eski spotunu yenisiyle değiştir
    v.retain(|o| !(o.call == sp.call && o.freq_hz.abs_diff(sp.freq_hz) < 2_000));
    v.retain(|o| o.at.elapsed() < KEEP);
    v.push(sp);
    if v.len() > MAX_SPOTS {
        let cut = v.len() - MAX_SPOTS;
        v.drain(..cut);
    }
    GEN.fetch_add(1, Ordering::SeqCst);
}

/// [lo, hi] Hz aralığındaki güncel spotlar
pub fn in_range(lo: u64, hi: u64) -> Vec<Spot> {
    SPOTS
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.freq_hz >= lo && s.freq_hz <= hi && s.at.elapsed() < KEEP)
        .cloned()
        .collect()
}

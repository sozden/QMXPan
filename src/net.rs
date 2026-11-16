//! WiFi, kurulum sayfası ve kablosuz yazılım güncelleme (OTA).
//!
//! - NVS'de kayıtlı ağ varsa ona bağlanır.
//! - Yoksa (ya da bağlanamazsa) "QMXPan-Setup" adında açık bir kurulum ağı
//!   açar; http://192.168.71.1 adresindeki sayfadan ağ adı ve şifre girilir.
//! - POST /api/ota: `espflash save-image` ile üretilen .bin yüklenir, bitince
//!   ana döngüden QMX bağlantısı düzgünce kapatılıp kart yeniden başlatılır.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Result};
use embedded_svc::http::Headers;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::http::server::{Configuration as HttpConfig, EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::ota::EspOta;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi,
};
use log::{error, info, warn};

pub const SETUP_SSID: &str = "QMXPan-Setup";
const NS: &str = "qmxpan";

#[derive(Clone, Default, PartialEq)]
pub enum Mode {
    #[default]
    Off,
    Connecting,
    Station,
    SetupAp,
}

#[derive(Clone, Default)]
pub struct Status {
    pub mode: Mode,
    pub ssid: String,
    pub ip: String,
}

pub static STATUS: Mutex<Status> = Mutex::new(Status {
    mode: Mode::Off,
    ssid: String::new(),
    ip: String::new(),
});

/// OTA ilerlemesi: 0 = yok, 1..=100 = yazılıyor, 101 = bitti
pub static OTA: AtomicU8 = AtomicU8::new(0);
/// Ana döngüye "QMX'i bırak ve yeniden başlat" isteği
pub static RESTART: AtomicBool = AtomicBool::new(false);

/// S-metre kalibrasyonu: dBm = dBFS + CAL (onda bir dB). Varsayılan -2.0 dB,
/// orijinal projenin "kukla yükte gürültü tabanı ≈ -130 dBm" ölçüsüne göre kaba tahmin.
pub static CAL_TENTHS: AtomicI32 = AtomicI32::new(-20);
/// Ana döngünün ölçtüğü son S-metre değeri (dBFS, onda bir dB); kalibrasyonda kullanılır
pub static SMETER_DBFS_TENTHS: AtomicI32 = AtomicI32::new(i32::MIN);

fn set_status(mode: Mode, ssid: &str, ip: &str) {
    *STATUS.lock().unwrap() = Status {
        mode,
        ssid: ssid.into(),
        ip: ip.into(),
    };
}

/// Ağ işini kendi görevinde başlatır; panadapter beklemez.
pub fn start(modem: Modem<'static>, sysloop: EspSystemEventLoop, nvs: EspDefaultNvsPartition) {
    let r = std::thread::Builder::new()
        .name("net".into())
        .stack_size(12 * 1024)
        .spawn(move || {
            if let Err(e) = run(modem, sysloop, nvs) {
                error!("Ağ görevi durdu: {e:#}");
            }
        });
    if let Err(e) = r {
        error!("Ağ görevi başlatılamadı: {e}");
    }
}

fn load_creds(nvs: &EspNvs<NvsDefault>) -> Option<(String, String)> {
    let mut b1 = [0u8; 64];
    let mut b2 = [0u8; 96];
    let ssid = nvs.get_str("ssid", &mut b1).ok().flatten()?.to_string();
    let pass = nvs.get_str("pass", &mut b2).ok().flatten().unwrap_or("").to_string();
    (!ssid.is_empty()).then_some((ssid, pass))
}

fn run(modem: Modem<'static>, sysloop: EspSystemEventLoop, part: EspDefaultNvsPartition) -> Result<()> {
    let nvs = EspNvs::new(part.clone(), NS, true)?;
    let creds = load_creds(&nvs);
    if let Ok(Some(c)) = nvs.get_i32("cal") {
        CAL_TENTHS.store(c, Ordering::Relaxed);
        info!("S-metre kalibrasyonu: {:+.1} dB", c as f32 / 10.0);
    }
    let nvs = std::sync::Arc::new(Mutex::new(nvs));

    let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, sysloop.clone(), Some(part))?, sysloop)?;
    let mut connected = false;

    if let Some((ssid, pass)) = &creds {
        set_status(Mode::Connecting, ssid, "");
        wifi.set_configuration(&Configuration::Client(ClientConfiguration {
            ssid: ssid.as_str().try_into().unwrap_or_default(),
            password: pass.as_str().try_into().unwrap_or_default(),
            auth_method: if pass.is_empty() { AuthMethod::None } else { AuthMethod::WPA2Personal },
            ..Default::default()
        }))?;
        wifi.start()?;
        for attempt in 1..=3 {
            info!("'{ssid}' ağına bağlanılıyor ({attempt}/3)");
            if wifi.connect().is_ok() && wifi.wait_netif_up().is_ok() {
                let ip = wifi.wifi().sta_netif().get_ip_info()?.ip;
                info!("WiFi bağlandı, IP: {ip}");
                set_status(Mode::Station, ssid, &ip.to_string());
                connected = true;
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        if !connected {
            warn!("Kayıtlı ağa bağlanılamadı, kurulum ağı açılıyor");
            wifi.stop()?;
        }
    }

    if !connected {
        wifi.set_configuration(&Configuration::AccessPoint(AccessPointConfiguration {
            ssid: SETUP_SSID.try_into().unwrap(),
            auth_method: AuthMethod::None,
            channel: 6,
            max_connections: 2,
            ..Default::default()
        }))?;
        wifi.start()?;
        wifi.wait_netif_up()?;
        let ip = wifi.wifi().ap_netif().get_ip_info()?.ip;
        info!("Kurulum ağı '{SETUP_SSID}' açık, adres http://{ip}");
        set_status(Mode::SetupAp, SETUP_SSID, &ip.to_string());
    }

    let _server = http_server(nvs)?;

    // Bağlantıyı izle, koparsa yeniden bağlan
    loop {
        std::thread::sleep(Duration::from_secs(5));
        if STATUS.lock().unwrap().mode != Mode::Station {
            continue;
        }
        if !wifi.is_connected().unwrap_or(false) {
            warn!("WiFi koptu, yeniden bağlanılıyor");
            let ssid = STATUS.lock().unwrap().ssid.clone();
            set_status(Mode::Connecting, &ssid, "");
            if wifi.connect().is_ok() && wifi.wait_netif_up().is_ok() {
                if let Ok(info) = wifi.wifi().sta_netif().get_ip_info() {
                    set_status(Mode::Station, &ssid, &info.ip.to_string());
                    info!("WiFi yeniden bağlandı");
                }
            }
        }
    }
}

type Req<'a, 'b> = Request<&'a mut EspHttpConnection<'b>>;

const PAGE_HEAD: &str = "<!doctype html><html><head><meta charset=utf-8>\
<meta name=viewport content='width=device-width,initial-scale=1'><title>QMXPan</title>\
<style>body{font-family:sans-serif;background:#111;color:#ddd;margin:16px;max-width:480px}\
input,button{font-size:16px;padding:8px;margin:4px 0;width:100%;box-sizing:border-box}\
h1{font-size:20px}code{color:#7fd}</style></head><body><h1>QMXPan</h1>";

fn index_page() -> String {
    let st = STATUS.lock().unwrap().clone();
    let wifi = match st.mode {
        Mode::Station => format!("Connected to <b>{}</b> ({})", html_escape(&st.ssid), st.ip),
        Mode::SetupAp => "Setup mode (no WiFi network saved or reachable)".to_string(),
        Mode::Connecting => "Connecting...".to_string(),
        Mode::Off => "Off".to_string(),
    };
    format!(
        "{PAGE_HEAD}<p>Version {ver}<br>WiFi: {wifi}</p>\
<h2>WiFi network</h2><form method=post action=/wifi>\
<input name=ssid placeholder='Network name (SSID)' required>\
<input name=pass type=password placeholder='Password'>\
<button>Save and connect</button></form>\
<h2>S-meter calibration</h2><p>Now reading: {now}<br>Offset: {cal:+.1} dB</p>\
<p>Feed a signal of known level into the QMX, tune it into the passband, \
then enter its level:</p><form method=post action=/cal>\
<input name=dbm type=number step=0.1 placeholder='Signal level in dBm, e.g. -73'>\
<button>Calibrate</button></form>\
<p>Firmware update: <code>tools\\ota.ps1 -Kart {ip}</code></p></body></html>",
        ver = env!("CARGO_PKG_VERSION"),
        ip = if st.ip.is_empty() { "IP" } else { &st.ip },
        now = now_dbm()
            .map(|d| format!("{d:.1} dBm ({})", crate::ui::s_units(d)))
            .unwrap_or_else(|| "-".into()),
        cal = CAL_TENTHS.load(Ordering::Relaxed) as f32 / 10.0,
    )
}

fn now_dbm() -> Option<f32> {
    let v = SMETER_DBFS_TENTHS.load(Ordering::Relaxed);
    (v != i32::MIN).then(|| (v + CAL_TENTHS.load(Ordering::Relaxed)) as f32 / 10.0)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// application/x-www-form-urlencoded çözümü
fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn form_value(body: &str, key: &str) -> Option<String> {
    body.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| url_decode(v))
    })
}

fn read_body(req: &mut Req, max: usize) -> Result<Vec<u8>> {
    let len = req.content_len().unwrap_or(0) as usize;
    if len > max {
        return Err(anyhow!("request too large"));
    }
    let mut body = vec![0u8; len];
    let mut off = 0;
    while off < len {
        let n = req.read(&mut body[off..])?;
        if n == 0 {
            break;
        }
        off += n;
    }
    body.truncate(off);
    Ok(body)
}

fn http_server(nvs: std::sync::Arc<Mutex<EspNvs<NvsDefault>>>) -> Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&HttpConfig {
        stack_size: 10 * 1024,
        ..Default::default()
    })?;

    server.fn_handler("/", Method::Get, |req| -> Result<()> {
        req.into_ok_response()?.write_all(index_page().as_bytes())?;
        Ok(())
    })?;

    let nvs_w = nvs.clone();
    server.fn_handler("/wifi", Method::Post, move |mut req| -> Result<()> {
        let nvs = &nvs_w;
        let body = read_body(&mut req, 1024)?;
        let body = String::from_utf8_lossy(&body);
        let ssid = form_value(&body, "ssid").unwrap_or_default();
        let pass = form_value(&body, "pass").unwrap_or_default();
        if ssid.is_empty() || ssid.len() > 32 || pass.len() > 64 {
            req.into_status_response(400)?.write_all(b"invalid network name or password")?;
            return Ok(());
        }
        {
            let n = nvs.lock().unwrap();
            n.set_str("ssid", &ssid)?;
            n.set_str("pass", &pass)?;
        }
        info!("WiFi ağı kaydedildi: '{ssid}', yeniden başlatılacak");
        req.into_ok_response()?.write_all(
            format!(
                "{PAGE_HEAD}<p>Saved. QMXPan restarts and joins <b>{}</b>. \
                 Its IP address is shown on the QMXPan screen.</p></body></html>",
                html_escape(&ssid)
            )
            .as_bytes(),
        )?;
        RESTART.store(true, Ordering::SeqCst);
        Ok(())
    })?;

    {
        let nvs = nvs.clone();
        server.fn_handler("/cal", Method::Post, move |mut req| -> Result<()> {
            let body = read_body(&mut req, 256)?;
            let body = String::from_utf8_lossy(&body);
            let dbm = form_value(&body, "dbm").and_then(|v| v.trim().parse::<f32>().ok());
            let meas = SMETER_DBFS_TENTHS.load(Ordering::Relaxed);
            let (Some(dbm), true) = (dbm, meas != i32::MIN) else {
                req.into_status_response(400)?
                    .write_all(b"need a dBm value and a running I/Q stream\n")?;
                return Ok(());
            };
            let cal = (dbm * 10.0).round() as i32 - meas;
            CAL_TENTHS.store(cal, Ordering::Relaxed);
            nvs.lock().unwrap().set_i32("cal", cal)?;
            info!("S-metre kalibre edildi: {dbm} dBm, ofset {:+.1} dB", cal as f32 / 10.0);
            req.into_ok_response()?.write_all(
                format!(
                    "{PAGE_HEAD}<p>Calibrated: offset {:+.1} dB.</p><p><a href=/>Back</a></p></body></html>",
                    cal as f32 / 10.0
                )
                .as_bytes(),
            )?;
            Ok(())
        })?;
    }

    server.fn_handler("/api/restart", Method::Post, |req| -> Result<()> {
        req.into_ok_response()?.write_all(b"restarting\n")?;
        RESTART.store(true, Ordering::SeqCst);
        Ok(())
    })?;

    server.fn_handler("/api/ota", Method::Post, |mut req| -> Result<()> {
        let len = req.content_len().unwrap_or(0) as usize;
        if !(64 * 1024..=3 * 1024 * 1024).contains(&len) {
            req.into_status_response(400)?.write_all(b"invalid image size\n")?;
            return Ok(());
        }
        OTA.store(1, Ordering::Relaxed);
        match receive_ota(&mut req, len) {
            Ok(()) => {
                OTA.store(101, Ordering::Relaxed);
                info!("OTA tamamlandı ({len} bayt)");
                req.into_ok_response()?.write_all(b"ok, restarting\n")?;
                RESTART.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                OTA.store(0, Ordering::Relaxed);
                error!("OTA başarısız: {e:#}");
                req.into_status_response(500)?
                    .write_all(format!("update failed: {e}\n").as_bytes())?;
            }
        }
        Ok(())
    })?;

    Ok(server)
}

fn receive_ota(req: &mut Req, len: usize) -> Result<()> {
    let mut ota = EspOta::new()?;
    let mut update = ota.initiate_update()?;
    let mut buf = vec![0u8; 8192];
    let mut done = 0usize;
    while done < len {
        let n = req.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // ESP32 uygulama imajları 0xE9 sihirli baytıyla başlar
        if done == 0 && buf[0] != 0xE9 {
            return Err(anyhow!("file is not an ESP32 application image"));
        }
        update.write(&buf[..n])?;
        done += n;
        OTA.store(1 + (done * 99 / len) as u8, Ordering::Relaxed);
    }
    if done != len {
        return Err(anyhow!("incomplete upload ({done}/{len})"));
    }
    // complete(): imajı doğrular, bir sonraki açılışta bu bölümü seçer
    update.complete()?;
    Ok(())
}

/// Yeni yazılım düzgün açıldıysa geri dönüşü iptal eder.
pub fn mark_valid() {
    match EspOta::new().and_then(|mut o| o.mark_running_slot_valid()) {
        Ok(()) => info!("Çalışan yazılım sağlam olarak işaretlendi"),
        Err(e) => warn!("Yazılım işaretlenemedi: {e}"),
    }
}

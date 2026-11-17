//! Güneş ve yayılım verisi: N0NBH'nin (hamqsl.com) ücretsiz XML akışı.
//! 30 dakikada bir HTTPS ile çekilir; isteğe hiçbir kişisel bilgi eklenmez
//! (kullanıcı onayladı). Sayfada kaynak olarak N0NBH yazılır.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use embedded_svc::http::client::Client;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use log::{info, warn};

const URL: &str = "https://www.hamqsl.com/solarxml.php";
const EVERY: Duration = Duration::from_secs(30 * 60);
const RETRY: Duration = Duration::from_secs(2 * 60);

/// Band grupları, XML'deki sırayla
pub const GROUPS: [&str; 4] = ["80m-40m", "30m-20m", "17m-15m", "12m-10m"];

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Solar {
    pub updated: String,
    pub sfi: String,
    pub ssn: String,
    pub a: String,
    pub k: String,
    pub xray: String,
    pub wind: String,
    pub bz: String,
    pub geomag: String,
    pub noise: String,
    pub aurora: String,
    pub muf: String,
    /// GROUPS sırasıyla (gündüz, gece): "Good" / "Fair" / "Poor"
    pub bands: [(String, String); 4],
}

pub static DATA: Mutex<Option<Solar>> = Mutex::new(None);
/// Veri her yenilendiğinde artar
pub static GEN: AtomicU32 = AtomicU32::new(0);
/// Durum (web sayfası ve tanı için)
pub static STATE: Mutex<String> = Mutex::new(String::new());

fn set_state(s: &str) {
    *STATE.lock().unwrap() = s.to_string();
}

pub fn start() {
    let r = crate::task::spawn(c"solar", 12 * 1024, 2, esp_idf_svc::hal::cpu::Core::Core0, || loop {
        if crate::net::STATUS.lock().unwrap().mode != crate::net::Mode::Station {
            set_state("waiting for WiFi");
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        set_state("fetching");
        let wait = match fetch().and_then(|x| parse(&x)) {
            Ok(s) => {
                info!("Güneş verisi: SFI {} A {} K {} ({})", s.sfi, s.a, s.k, s.updated);
                set_state(&format!("ok, updated {}", s.updated));
                *DATA.lock().unwrap() = Some(s);
                GEN.fetch_add(1, Ordering::SeqCst);
                EVERY
            }
            Err(e) => {
                // TLS iç RAM ister: hata olursa boş iç bellek de yazılsın (tanı için)
                let (free, big) = unsafe {
                    use esp_idf_svc::sys::{heap_caps_get_free_size, heap_caps_get_largest_free_block, MALLOC_CAP_INTERNAL};
                    (heap_caps_get_free_size(MALLOC_CAP_INTERNAL), heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL))
                };
                warn!("Güneş verisi alınamadı: {e} (iç RAM {free} B, en büyük blok {big} B)");
                set_state(&format!("error: {e} (internal RAM free {free}, largest {big})"));
                RETRY
            }
        };
        std::thread::sleep(wait);
    });
    if let Err(e) = r {
        warn!("solar iş parçacığı başlatılamadı: {e}");
    }
}

fn fetch() -> anyhow::Result<String> {
    let conn = EspHttpConnection::new(&Configuration {
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        timeout: Some(Duration::from_secs(20)),
        ..Default::default()
    })?;
    let mut client = Client::wrap(conn);
    let mut resp = client.get(URL)?.submit()?;
    let status = resp.status();
    if status != 200 {
        anyhow::bail!("HTTP {status}");
    }
    let mut body = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = resp.read(&mut buf).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        if n == 0 || body.len() > 32 * 1024 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// `<tag ...>değer</tag>` içindeki ilk değer
fn tag(x: &str, name: &str) -> String {
    // "<muf>" ararken "<muffactor>" yakalanmasın: ad tam eşleşmeli
    let i = match x.find(&format!("<{name}>")).or_else(|| x.find(&format!("<{name} "))) {
        Some(i) => i,
        None => return String::new(),
    };
    let Some(j) = x[i..].find('>') else { return String::new() };
    let start = i + j + 1;
    let Some(k) = x[start..].find('<') else { return String::new() };
    x[start..start + k].trim().to_string()
}

fn band(x: &str, group: &str, time: &str) -> String {
    let key = format!("<band name=\"{group}\" time=\"{time}\">");
    x.find(&key)
        .and_then(|i| {
            let s = i + key.len();
            x[s..].find('<').map(|k| x[s..s + k].trim().to_string())
        })
        .unwrap_or_default()
}

fn parse(x: &str) -> anyhow::Result<Solar> {
    if !x.contains("<solardata>") {
        anyhow::bail!("unexpected reply");
    }
    let mut s = Solar {
        updated: tag(x, "updated"),
        sfi: tag(x, "solarflux"),
        ssn: tag(x, "sunspots"),
        a: tag(x, "aindex"),
        k: tag(x, "kindex"),
        xray: tag(x, "xray"),
        wind: tag(x, "solarwind"),
        bz: tag(x, "magneticfield"),
        geomag: tag(x, "geomagfield"),
        noise: tag(x, "signalnoise"),
        aurora: tag(x, "aurora"),
        muf: tag(x, "muf"),
        bands: Default::default(),
    };
    for (i, g) in GROUPS.iter().enumerate() {
        s.bands[i] = (band(x, g, "day"), band(x, g, "night"));
    }
    Ok(s)
}

// ------------------------------------------------------------ güneş konumu

/// Unix zamanında (s) güneşin tam tepede olduğu nokta: (enlem, boylam) derece.
/// NOAA'nın kısaltılmış formülleri; birkaç dakikalık açı doğruluğu yeter.
pub fn subsolar(unix: u64) -> (f32, f32) {
    // 2000-01-01 12:00 UTC'den beri gün (f32 büyük sayılarda hassasiyet kaybeder,
    // fark tam sayıyla alınır)
    let days = (unix as i64 - 946_728_000) as f32 / 86_400.0;
    let rad = core::f32::consts::PI / 180.0;
    let g = (357.529 + 0.985_600_28 * days).rem_euclid(360.0) * rad;
    let q = (280.459 + 0.985_647_36 * days).rem_euclid(360.0);
    let l = (q + 1.915 * g.sin() + 0.020 * (2.0 * g).sin()) * rad;
    let e = (23.439 - 0.000_000_36 * days) * rad;
    let ra = (e.cos() * l.sin()).atan2(l.cos()) / rad;
    let dec = (e.sin() * l.sin()).asin() / rad;
    // Zaman denklemi (dakika)
    let mut eqt = (q - ra) * 4.0;
    eqt = (eqt + 720.0).rem_euclid(1440.0) - 720.0;
    let utc_min = (unix % 86_400) as f32 / 60.0;
    let lon = -(utc_min + eqt - 720.0) / 4.0;
    let lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
    (dec, lon)
}

/// Maidenhead konum kodu (4 ya da 6 karakter) → kare merkezinin (enlem, boylam)
pub fn locator(s: &str) -> Option<(f32, f32)> {
    let b: Vec<u8> = s.trim().to_ascii_uppercase().bytes().collect();
    if b.len() != 4 && b.len() != 6 {
        return None;
    }
    if !(b'A'..=b'R').contains(&b[0]) || !(b'A'..=b'R').contains(&b[1]) || !b[2].is_ascii_digit() || !b[3].is_ascii_digit() {
        return None;
    }
    let mut lon = (b[0] - b'A') as f32 * 20.0 - 180.0 + (b[2] - b'0') as f32 * 2.0;
    let mut lat = (b[1] - b'A') as f32 * 10.0 - 90.0 + (b[3] - b'0') as f32;
    if b.len() == 6 {
        if !(b'A'..=b'X').contains(&b[4]) || !(b'A'..=b'X').contains(&b[5]) {
            return None;
        }
        lon += (b[4] - b'A') as f32 * (2.0 / 24.0) + 1.0 / 24.0;
        lat += (b[5] - b'A') as f32 * (1.0 / 24.0) + 0.5 / 24.0;
    } else {
        lon += 1.0;
        lat += 0.5;
    }
    Some((lat, lon))
}

/// Kullanıcının konum kodu (NVS "qth"; web sayfasından girilir), boşsa yok
pub static QTH: Mutex<String> = Mutex::new(String::new());

//! Amatör HF bandları (QMX+ 160–6 m) ve band başına son frekans hafızası.

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use log::warn;

pub struct Band {
    pub name: &'static str,
    pub lo: u64,
    pub hi: u64,
    /// İlk ziyarette gidilen frekans (FT8 frekansı)
    pub default: u64,
}

pub const BANDS: [Band; 11] = [
    Band { name: "160", lo: 1_800_000, hi: 2_000_000, default: 1_840_000 },
    Band { name: "80", lo: 3_500_000, hi: 4_000_000, default: 3_573_000 },
    Band { name: "60", lo: 5_250_000, hi: 5_450_000, default: 5_357_000 },
    Band { name: "40", lo: 7_000_000, hi: 7_300_000, default: 7_074_000 },
    Band { name: "30", lo: 10_100_000, hi: 10_150_000, default: 10_136_000 },
    Band { name: "20", lo: 14_000_000, hi: 14_350_000, default: 14_074_000 },
    Band { name: "17", lo: 18_068_000, hi: 18_168_000, default: 18_100_000 },
    Band { name: "15", lo: 21_000_000, hi: 21_450_000, default: 21_074_000 },
    Band { name: "12", lo: 24_890_000, hi: 24_990_000, default: 24_915_000 },
    Band { name: "10", lo: 28_000_000, hi: 29_700_000, default: 28_074_000 },
    Band { name: "6", lo: 50_000_000, hi: 54_000_000, default: 50_313_000 },
];

pub fn band_of(f: u64) -> Option<usize> {
    BANDS.iter().position(|b| f >= b.lo && f <= b.hi)
}

/// Band kartında gösterilen bölüm adı (IARU R1 bant planına kabaca göre)
pub fn segment(f: u64) -> &'static str {
    let Some(i) = band_of(f) else { return "out of band" };
    let b = &BANDS[i];
    if f.abs_diff(b.default) <= 3_000 {
        return "FT8";
    }
    // CW bölümünün üst sınırı (kHz)
    let cw_top = [1_838, 3_570, 5_354, 7_040, 10_150, 14_070, 18_095, 21_070, 24_915, 28_070, 50_100][i] * 1000;
    if i == 4 {
        "CW · DIGI"
    } else if f < cw_top {
        "CW"
    } else {
        "SSB"
    }
}

/// Band başına son kullanılan frekans, NVS'de saklanır.
pub struct Memory {
    nvs: Option<EspNvs<NvsDefault>>,
}

impl Memory {
    pub fn new(part: EspDefaultNvsPartition) -> Self {
        let nvs = EspNvs::new(part, "qmxpan", true)
            .map_err(|e| warn!("Band hafızası açılamadı: {e}"))
            .ok();
        Self { nvs }
    }

    fn key(i: usize) -> String {
        format!("band{i}")
    }

    /// Bandın son frekansı; kayıt yoksa ya da band dışındaysa varsayılan.
    pub fn get(&self, i: usize) -> u64 {
        let b = &BANDS[i];
        self.nvs
            .as_ref()
            .and_then(|n| n.get_u32(&Self::key(i)).ok().flatten())
            .map(|f| f as u64)
            .filter(|&f| f >= b.lo && f <= b.hi)
            .unwrap_or(b.default)
    }

    /// QMX'in geçmeyi reddettiği bantlar (bit i = BANDS[i]); kart bunu kendisi öğrenir
    pub fn missing(&self) -> u16 {
        self.nvs
            .as_ref()
            .and_then(|n| n.get_u16("nobands").ok().flatten())
            .unwrap_or(0)
    }

    pub fn set_missing(&self, mask: u16) {
        if let Some(n) = self.nvs.as_ref() {
            if let Err(e) = n.set_u16("nobands", mask) {
                warn!("Bant listesi kaydedilemedi: {e}");
            }
        }
    }

    pub fn set(&self, i: usize, f: u64) {
        if let Some(n) = self.nvs.as_ref() {
            if let Err(e) = n.set_u32(&Self::key(i), f as u32) {
                warn!("Band frekansı kaydedilemedi: {e}");
            }
        }
    }
}

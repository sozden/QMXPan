//! Görüntü ayarları ve NVS'de saklanması.

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use log::warn;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    /// Şelale parlaklığı (dB): artınca zayıf sinyaller daha parlak
    pub wf_bright: i32,
    /// Şelale kontrastı: renk skalasının kapsadığı dB aralığı (küçük = sert)
    pub wf_span: i32,
    /// Spektrumda gösterilen dB aralığı
    pub spec_range: i32,
    /// Şelaleye kaç spektrum karesinde bir satır eklenir (1 = en hızlı)
    pub wf_every: i32,
    /// Renk paleti sırası (ui::PALETTES)
    pub palette: i32,
    /// Spektrum yumuşatma yüzdesi (0 = yok)
    pub smooth: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            wf_bright: 0,
            wf_span: 52,
            spec_range: 70,
            wf_every: 2,
            palette: 0,
            smooth: 50,
        }
    }
}

/// (anahtar, en küçük, en büyük, adım)
pub const LIMITS: [(&str, i32, i32, i32); 6] = [
    ("wfbright", -20, 20, 2),
    ("wfspan", 20, 90, 4),
    ("sprange", 40, 100, 5),
    ("wfevery", 1, 4, 1),
    ("palette", 0, 2, 1),
    ("smooth", 0, 90, 10),
];

impl Settings {
    pub fn get(&self, i: usize) -> i32 {
        [
            self.wf_bright,
            self.wf_span,
            self.spec_range,
            self.wf_every,
            self.palette,
            self.smooth,
        ][i]
    }

    fn slot(&mut self, i: usize) -> &mut i32 {
        match i {
            0 => &mut self.wf_bright,
            1 => &mut self.wf_span,
            2 => &mut self.spec_range,
            3 => &mut self.wf_every,
            4 => &mut self.palette,
            _ => &mut self.smooth,
        }
    }

    /// i. ayarı bir adım artırır/azaltır (palet döner, diğerleri sınırda durur).
    pub fn step(&mut self, i: usize, up: bool) {
        let (_, lo, hi, st) = LIMITS[i];
        let v = self.slot(i);
        let n = if up { *v + st } else { *v - st };
        *v = if i == 4 {
            if n > hi { lo } else if n < lo { hi } else { n }
        } else {
            n.clamp(lo, hi)
        };
    }
}

pub struct Store {
    nvs: Option<EspNvs<NvsDefault>>,
}

impl Store {
    pub fn new(part: EspDefaultNvsPartition) -> Self {
        let nvs = EspNvs::new(part, "qmxpan", true)
            .map_err(|e| warn!("Ayarlar açılamadı: {e}"))
            .ok();
        Self { nvs }
    }

    pub fn load(&self) -> Settings {
        let mut s = Settings::default();
        if let Some(n) = self.nvs.as_ref() {
            for (i, &(key, lo, hi, _)) in LIMITS.iter().enumerate() {
                if let Ok(Some(v)) = n.get_i32(key) {
                    *s.slot(i) = v.clamp(lo, hi);
                }
            }
        }
        s
    }

    pub fn save(&self, s: &Settings) {
        if let Some(n) = self.nvs.as_ref() {
            for (i, &(key, ..)) in LIMITS.iter().enumerate() {
                if let Err(e) = n.set_i32(key, s.get(i)) {
                    warn!("Ayar kaydedilemedi ({key}): {e}");
                }
            }
        }
    }
}

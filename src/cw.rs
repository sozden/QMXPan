//! CW çözücü: CW Keyer projesindeki (D:\CWKeyer, dsp.rs) çözücünün uyarlaması.
//! Giriş, QMX I/Q'sundan audio.rs ile üretilen 6 kHz gerçek sestir (ayar
//! frekansındaki sinyal ~700 Hz tona çevrilmiş). Goertzel zarfı -> uyarlanır
//! eşik -> uyarlanır hız ile nokta/çizgi -> metin.

#![allow(dead_code)]

use std::f32::consts::PI;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use crate::morse;

/// CW çözücü açık mı (CW tuşu)
pub static ON: AtomicBool = AtomicBool::new(false);
/// Çözülen metin (ekran son kısmını gösterir)
pub static TEXT: Mutex<String> = Mutex::new(String::new());

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Stat {
    pub wpm: u32,
    pub tone_hz: u32,
    pub snr_db: u32,
    pub key: bool,
}
pub static STAT: Mutex<Stat> = Mutex::new(Stat { wpm: 0, tone_hz: 0, snr_db: 0, key: false });

pub const FS: f32 = 6_000.0;
const DECIM: usize = 1;
/// Goertzel pencere uzunluğu ve adım (örnek, 5 kHz'de)
const N: usize = 64; // 12,8 ms
const HOP: usize = 32; // 6,4 ms
const HOP_MS: f32 = HOP as f32 * 1000.0 / FS;
/// Otomatik frekans taraması
const SCAN_LEN: usize = 512;
const SCAN_EVERY: u32 = 16; // adım (~100 ms)
/// Ton sayılmak için en düşük genlik (ADC basamağı; ~A/4 → ~12 basamak tepe, ~10 mV)
const MIN_MAG: f32 = 3.0;
/// Harf arası boşluk eşiği (nokta süresi cinsinden)
const LETTER_GAP: f32 = 2.3;
/// Kilitli istasyon bu kadar süre susmadan başka perdeye geçilmez
const HOLD_MS: f32 = 6000.0;

fn goertzel(samples: impl Iterator<Item = f32>, coeff: f32) -> f32 {
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for x in samples {
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0)
}

fn coeff_for(freq: f32) -> f32 {
    2.0 * (2.0 * PI * freq / FS).cos()
}

/// Örnekten metne kadar tüm çözücü durumu (donanımdan bağımsız, test edilebilir)
pub struct Decoder {
    // ön işleme
    acc: f32,
    acc_n: usize,
    dc: f32,
    ring: Vec<f32>, // son SCAN_LEN örnek
    pos: usize,
    since_hop: usize,
    hann: [f32; N],

    // ton
    pub freq: f32,
    coeff: f32,
    pub auto: bool,
    hops_since_scan: u32,
    /// Art arda aynı frekansta bulunan belirgin tepe sayısı
    lock: u32,
    quiet_scans: u32,
    /// Kilitli tondan son işaretten beri geçen süre
    since_tone_ms: f32,
    /// Kilitten beri çözülen, çizgi içeren geçerli harf sayısı
    good_letters: u32,
    /// Ham durum kaç adımdır farklı (kısa kopma süzgeci)
    flip: u32,

    // zarf
    hops: u32,
    noise: f32,
    peak: f32,
    pub level: f32, // 0..1
    pub snr_db: f32,
    pub squelch: u32,
    tone: bool,

    // zamanlama (ms)
    run_ms: f32,
    pub dit_ms: f32,
    marks: [f32; 16],
    mark_n: usize,
    pattern: String,
    /// Çözülmemiş işaretler: (işaret süresi, ardından gelen boşluk) ms
    elems: Vec<(f32, f32)>,
    word_open: bool,

    /// Çözülen metin buraya eklenir; çağıran boşaltır
    pub out: String,
    /// Test: adım adım [m, side, noise, peak, on]
    #[cfg(test)]
    pub trace: Vec<[f32; 5]>,
    #[cfg(test)]
    pub events: Vec<String>,
}

impl Decoder {
    pub fn new(freq: f32, auto: bool, squelch: u32) -> Self {
        let mut hann = [0.0; N];
        for (i, h) in hann.iter_mut().enumerate() {
            *h = 0.5 - 0.5 * (2.0 * PI * i as f32 / (N as f32 - 1.0)).cos();
        }
        Self {
            acc: 0.0,
            acc_n: 0,
            dc: 0.0,
            ring: vec![0.0; SCAN_LEN],
            pos: 0,
            since_hop: 0,
            hann,
            freq,
            coeff: coeff_for(freq),
            auto,
            hops_since_scan: 0,
            lock: 0,
            quiet_scans: 0,
            since_tone_ms: 0.0,
            good_letters: 0,
            flip: 0,
            hops: 0,
            noise: 1.0,
            peak: 1.0,
            level: 0.0,
            snr_db: 0.0,
            squelch,
            tone: false,
            run_ms: 0.0,
            dit_ms: 60.0,
            marks: [0.0; 16],
            mark_n: 0,
            pattern: String::new(),
            elems: Vec::new(),
            word_open: false,
            out: String::new(),
            #[cfg(test)]
            trace: Vec::new(),
            #[cfg(test)]
            events: Vec::new(),
        }
    }

    pub fn set_freq(&mut self, f: f32) {
        self.freq = f.clamp(300.0, 1200.0);
        self.coeff = coeff_for(self.freq);
    }

    pub fn tone(&self) -> bool {
        self.tone
    }

    pub fn wpm(&self) -> f32 {
        1200.0 / self.dit_ms
    }

    /// Ham 12 bit ADC örneği (20 kHz)
    pub fn push_raw(&mut self, x: u16) {
        self.acc += x as f32;
        self.acc_n += 1;
        if self.acc_n < DECIM {
            return;
        }
        let v = self.acc / DECIM as f32;
        self.acc = 0.0;
        self.acc_n = 0;
        self.push(v);
    }

    /// 5 kHz örnek
    pub fn push(&mut self, v: f32) {
        self.dc += (v - self.dc) * 0.002;
        self.ring[self.pos] = v - self.dc;
        self.pos = (self.pos + 1) % SCAN_LEN;
        self.since_hop += 1;
        if self.since_hop >= HOP {
            self.since_hop = 0;
            self.hop();
        }
    }

    /// Halkadaki son `n` örnek, eskiden yeniye
    fn last(&self, n: usize) -> impl Iterator<Item = f32> + '_ {
        let start = (self.pos + SCAN_LEN - n) % SCAN_LEN;
        (0..n).map(move |i| self.ring[(start + i) % SCAN_LEN])
    }

    /// Pencereli son N örnekte `freq` genliği
    fn mag(&self, freq: f32) -> f32 {
        let p = goertzel(self.last(N).zip(self.hann.iter()).map(|(x, h)| x * h), coeff_for(freq));
        p.sqrt() / N as f32
    }

    /// Aynı anda komşu frekanslardaki ortalama genlik (geniş bant gürültü tahmini).
    /// Gerçek CW tonu dar banttır; gürültü ise komşu frekanslarda da aynı güçtedir.
    fn side_noise(&self) -> f32 {
        let (mut sum, mut n) = (0.0, 0);
        for off in [-450.0, -250.0, 250.0, 450.0] {
            let f = self.freq + off;
            if (150.0..=2000.0).contains(&f) {
                sum += self.mag(f);
                n += 1;
            }
        }
        sum / n.max(1) as f32
    }

    fn hop(&mut self) {
        let m = self.mag(self.freq) + 1e-3;
        let side = self.side_noise() + 1e-3;

        // Gürültü tabanı: alt zarf (hızlı iner, ~2 s'de yavaşça çıkar). Ortalama
        // kullanılırsa yoğun/güçlü CW'de taban sinyalle şişip eşik sinyalin üstüne çıkar.
        if self.hops < 30 {
            self.hops += 1;
            self.noise = if self.hops == 1 { m } else { self.noise.min(m) };
        } else if m < self.noise {
            self.noise += (m - self.noise) * 0.3;
        } else if !self.tone {
            self.noise += (m - self.noise) * 0.004;
        }
        // Alt zarf rastgele gürültünün ortalamasının altında kalır; komşu frekans
        // tahmininden çok aşağı düşmesin
        self.noise = self.noise.max(side * 0.7);
        // Tepe: anında yükselir, ~1 s'de söner
        if m > self.peak {
            self.peak = m;
        } else {
            self.peak = (self.peak * 0.993).max(self.noise);
        }

        let snr = self.peak / self.noise;
        self.snr_db = 20.0 * snr.log10();
        self.level = (20.0 * (m / self.noise).log10() / 40.0).clamp(0.0, 1.0);
        let need = 1.5 + self.squelch as f32 * 0.5; // genlik oranı (~6..16 dB)
        let valid = snr >= need;

        let span = self.peak - self.noise;
        // Tepeye göre eşik + gürültü tabanına göre mutlak eşik (tepe sönerken
        // gürültünün ton sanılmasını önler)
        let floor_gate = self.noise * need * 0.9;
        // Dar bant şartı (komşu frekanslara göre) ve mutlak en düşük seviye
        // (giriş boşta ya da ses çok kısıkken ADC gürültüsünü çözmemek için)
        let narrow = need * 0.75;
        // Otomatik tonda: en güçlü tepe aynı frekansta kalıcı olmalı (ses/gürültü değil)
        let locked = !self.auto || self.lock >= 2;
        let on = if !valid || !locked {
            false
        } else if self.tone {
            m > self.noise + span * 0.35
                && m > floor_gate * 0.6
                && m > side * narrow * 0.5
                && m > MIN_MAG * 0.6
        } else {
            m > self.noise + span * 0.55 && m > floor_gate && m > side * narrow && m > MIN_MAG
        };
        #[cfg(test)]
        self.trace.push([m, side, self.noise, self.peak, if on { 1.0 } else { 0.0 }]);
        self.since_tone_ms = if self.tone { 0.0 } else { self.since_tone_ms + HOP_MS };
        // Tek adımlık (6,4 ms) kopma/parazit durum değiştirmesin. İki kenar da aynı
        // gecikmeyi aldığı için süreler bozulmaz.
        if on != self.tone {
            self.flip += 1;
            if self.flip >= 2 {
                self.flip = 0;
                self.edge(on);
            } else {
                self.edge(self.tone);
            }
        } else {
            self.flip = 0;
            self.edge(on);
        }

        if self.auto {
            self.hops_since_scan += 1;
            if self.hops_since_scan >= SCAN_EVERY {
                self.hops_since_scan = 0;
                self.scan();
            }
        }
    }

    /// 300..1200 Hz arasında en güçlü tonu ara
    fn scan(&mut self) {
        let mut best = (0.0f32, 0.0f32);
        let mut powers = [0.0f32; 91];
        for (i, pw) in powers.iter_mut().enumerate() {
            let f = 300.0 + i as f32 * 10.0;
            let p = goertzel(self.last(SCAN_LEN), coeff_for(f));
            *pw = p;
            if p > best.1 {
                best = (f, p);
            }
        }
        let mut sorted = powers;
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        let median = sorted[45].max(1e-6);
        // Belirgin tepe (~12 dB güç) ve şu anki tondan farklıysa kay
        if best.1 > median * 16.0 {
            let d = best.0 - self.freq;
            self.quiet_scans = 0;
            let holding = self.lock >= 2 && self.good_letters >= 3 && self.since_tone_ms < HOLD_MS;
            if d.abs() > 30.0 && holding {
                // Kilitli istasyon yakın zamanda çalıştı: aradaki başka perdedeki
                // (ör. yarışmada çağıran) istasyonlara atlama
                return;
            }
            if d.abs() <= 30.0 {
                self.lock = (self.lock + 1).min(5);
            } else {
                self.lock = 0;
                self.good_letters = 0;
            }
            if d.abs() > 60.0 {
                self.set_freq(best.0);
            } else if d.abs() > 10.0 {
                self.set_freq(self.freq + d * 0.5);
            }
        } else {
            // Kelime araları kilidi bozmasın; ~3 s tepe yoksa sinyal gitmiştir
            self.quiet_scans += 1;
            if self.quiet_scans as f32 * SCAN_EVERY as f32 * HOP_MS > HOLD_MS {
                self.lock = 0;
                self.good_letters = 0;
            }
        }
    }

    fn edge(&mut self, on: bool) {
        self.run_ms += HOP_MS;
        if on != self.tone {
            let dur = self.run_ms;
            self.tone = on;
            self.run_ms = 0.0;
            #[cfg(test)]
            self.events.push(format!(
                "{}{:.0} dit={:.0} n={} out={}",
                if on { "space " } else { "MARK " },
                dur,
                self.dit_ms,
                self.elems.len(),
                self.out
            ));
            if on {
                self.space_ended(dur);
            } else {
                self.mark_ended(dur);
            }
        } else if !on {
            // Sessizlikte zaman aşımı: harf ve kelime sonu
            let gap = self.run_ms;
            if !self.elems.is_empty() && gap > 2.5 * self.dit_ms {
                self.flush_letter();
            }
            if self.word_open && self.elems.is_empty() && gap > 6.0 * self.dit_ms {
                self.word_open = false;
                self.out.push(' ');
            }
        }
    }

    fn mark_ended(&mut self, ms: f32) {
        // Sönümlenmede noktalar ~yarıya kısalabilir; yalnızca çok kısa darbeler parazit
        if ms < 10.0 || ms < 0.35 * self.dit_ms {
            // parazit: önceki boşlukla sonrakini birleştir
            if let Some(last) = self.elems.last_mut() {
                self.run_ms += last.1 + ms;
                last.1 = 0.0;
            }
            return;
        }
        self.marks[self.mark_n % self.marks.len()] = ms;
        self.mark_n += 1;
        self.adapt();
        if ms < 2.0 * self.dit_ms {
            self.dit_ms += (ms - self.dit_ms) * 0.2;
        } else {
            self.dit_ms += (ms / 3.0 - self.dit_ms) * 0.2;
        }
        self.dit_ms = self.dit_ms.clamp(20.0, 240.0);
        // Sınıflandırma harf bitince yapılır (o ana kadar hız tahmini oturur)
        self.elems.push((ms, 0.0));
        if self.elems.len() > 40 {
            self.flush_letter();
        }
    }

    fn space_ended(&mut self, ms: f32) {
        if let Some(last) = self.elems.last_mut() {
            last.1 = ms;
        }
        if ms > LETTER_GAP * self.dit_ms && !self.elems.is_empty() {
            self.flush_letter();
        }
        if ms > 5.0 * self.dit_ms && self.word_open {
            self.word_open = false;
            self.out.push(' ');
        }
        // İşaretler arası boşluk ~1 nokta: hız tahminini yumuşakça düzelt
        if ms < 1.6 * self.dit_ms && ms > 0.5 * self.dit_ms {
            self.dit_ms += (ms - self.dit_ms) * 0.05;
        }
    }

    /// Son işaret sürelerinde iki küme (nokta/çizgi) arayıp hızı yeniden kestir.
    /// İlk tahmin çok yanlışsa (ör. hepsi "nokta" sanılıyorsa) bunu düzeltir.
    fn adapt(&mut self) {
        let n = self.mark_n.min(self.marks.len());
        if n < 3 {
            return;
        }
        let mut v: Vec<f32> = self.marks[..n].to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        let mut best = (1.0f32, 0usize);
        for i in 1..n {
            let r = v[i] / v[i - 1].max(1.0);
            if r > best.0 {
                best = (r, i);
            }
        }
        // Az örnekle yalnızca belirgin ayrımda (çizgi ≈ 3 nokta) güncelle
        if best.0 > if n < 6 { 2.2 } else { 1.8 } {
            let short: f32 = v[..best.1].iter().sum::<f32>() / best.1 as f32;
            let long: f32 = v[best.1..].iter().sum::<f32>() / (n - best.1) as f32;
            let est = (short + long / 3.0) / 2.0;
            self.dit_ms += (est - self.dit_ms) * 0.5;
        }
    }

    /// Biriken işaretleri güncel hız tahminiyle nokta/çizgiye çevir; içeride harf
    /// arası kadar uzun boşluk varsa (hız sonradan oturduysa) oradan böl.
    fn flush_letter(&mut self) {
        let dit = self.dit_ms;
        let elems = std::mem::take(&mut self.elems);
        let last = elems.len().saturating_sub(1);
        for (i, &(mark, gap)) in elems.iter().enumerate() {
            self.pattern.push(if mark < 2.0 * dit { '.' } else { '-' });
            // Harf arası 3, işaret arası 1 nokta; eşik biraz yukarıda (sönümde
            // işaret arası boşluklar uzayabiliyor)
            if i == last || gap > LETTER_GAP * dit {
                let s = if self.pattern.len() > 9 { "*" } else { morse::decode(&self.pattern) };
                // Çizgi içeren geçerli harf: parazit değil gerçek CW (yalnız kısa
                // darbeler E/I/S/H üretir)
                if s != "*" && self.pattern.contains('-') {
                    self.good_letters = (self.good_letters + 1).min(100);
                }
                self.out.push_str(s);
                self.pattern.clear();
                if i != last && gap > 5.0 * dit {
                    self.out.push(' ');
                }
            }
        }
        self.word_open = true;
    }
}

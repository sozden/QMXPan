//! QMX I/Q'sundan (48 kHz karmaşık, LO ortada) gerçek ses üretir.
//!
//! Zincir: istenen frekansı 0 Hz'e kaydır (NCO) → karmaşık alçak geçiren FIR ile
//! seyrelt → istenen ses frekansına kaydır → gerçek kısmı al.
//! - CW: ayar frekansındaki sinyal 700 Hz tona, 6 kHz örnekle
//! - FT8: USB geçiş bandı (ayar + 0..3 kHz), 12 kHz örnekle

use std::f32::consts::PI;

const IN_RATE: f32 = 48_000.0;

/// Döner fazör: her örnekte e^{jω} ile çarpılır, ara sıra boyu 1'e düzeltilir
struct Nco {
    re: f32,
    im: f32,
    step_re: f32,
    step_im: f32,
    n: u32,
}

impl Nco {
    fn new(hz: f32, rate: f32) -> Self {
        let w = 2.0 * PI * hz / rate;
        Self { re: 1.0, im: 0.0, step_re: w.cos(), step_im: w.sin(), n: 0 }
    }

    /// (i + jq) · e^{jφ}
    #[inline]
    fn mix(&mut self, i: f32, q: f32) -> (f32, f32) {
        let out = (i * self.re - q * self.im, i * self.im + q * self.re);
        let re = self.re * self.step_re - self.im * self.step_im;
        let im = self.re * self.step_im + self.im * self.step_re;
        self.re = re;
        self.im = im;
        self.n += 1;
        if self.n & 1023 == 0 {
            let m = (self.re * self.re + self.im * self.im).sqrt();
            self.re /= m;
            self.im /= m;
        }
        out
    }
}

/// Karmaşık ses üreteci: 48 kHz I/Q → `rate` Hz gerçek ses
pub struct Demod {
    pre: Nco,
    post: Nco,
    taps: Vec<f32>,
    hi: Vec<f32>,
    hq: Vec<f32>,
    pos: usize,
    decim: usize,
    phase: usize,
    /// Otomatik kazanç: çıkışın RMS'ini `target` civarında tutar
    /// Gürültü tabanı (dB, alt zarf), hedef gürültü RMS'i ve o anki kazanç
    floor_db: f32,
    target: f32,
    gain: f32,
    out_rate: f32,
    blk_pow: f32,
    blk_n: u32,
    /// Açılışta taban hızlı otursun diye blok sayacı
    warm: u32,
    /// Test/tanı için sabit kazanç (AGC'yi devre dışı bırakır)
    pub fixed_gain: Option<f32>,
}

impl Demod {
    /// `center`: LO'ya göre 0 Hz'e indirilecek frekans (Hz),
    /// `cutoff`: tek yanlı bant genişliği (Hz), `decim`: seyreltme,
    /// `tone`: çıkışta `center`ın düşeceği ses frekansı (Hz)
    pub fn new(center: f32, cutoff: f32, decim: usize, tone: f32, target: f32) -> Self {
        let ntaps = 96;
        let fc = cutoff / IN_RATE;
        let m = (ntaps - 1) as f32;
        let mut taps: Vec<f32> = (0..ntaps)
            .map(|n| {
                let x = n as f32 - m / 2.0;
                let sinc = if x == 0.0 { 2.0 * fc } else { (2.0 * PI * fc * x).sin() / (PI * x) };
                // Blackman penceresi
                let w = 0.42 - 0.5 * (2.0 * PI * n as f32 / m).cos() + 0.08 * (4.0 * PI * n as f32 / m).cos();
                sinc * w
            })
            .collect();
        let sum: f32 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Self {
            pre: Nco::new(-center, IN_RATE),
            post: Nco::new(tone, IN_RATE / decim as f32),
            hi: vec![0.0; ntaps],
            hq: vec![0.0; ntaps],
            taps,
            pos: 0,
            decim,
            phase: 0,
            floor_db: -200.0,
            target,
            gain: 1.0,
            out_rate: IN_RATE / decim as f32,
            blk_pow: 0.0,
            blk_n: 0,
            warm: 0,
            fixed_gain: None,
        }
    }

    /// Bir I/Q örneği ver; seyreltme anında bir ses örneği döner.
    #[inline]
    pub fn push(&mut self, i: f32, q: f32) -> Option<f32> {
        let (bi, bq) = self.pre.mix(i, q);
        self.hi[self.pos] = bi;
        self.hq[self.pos] = bq;
        self.pos = (self.pos + 1) % self.taps.len();
        self.phase += 1;
        if self.phase < self.decim {
            return None;
        }
        self.phase = 0;
        // Yalnız çıkış örneklerinde süzgeç hesaplanır (çok fazlı seyreltme)
        let n = self.taps.len();
        let (mut si, mut sq) = (0.0f32, 0.0f32);
        let mut k = self.pos;
        for &t in &self.taps {
            si += self.hi[k] * t;
            sq += self.hq[k] * t;
            k += 1;
            if k == n {
                k = 0;
            }
        }
        let (re, _) = self.post.mix(si, sq);
        // Kazanç gürültü tabanına göre: 64 örneklik blok gücünün alt zarfı
        // (hızlı iner, çok yavaş çıkar). Sinyal çalarken kazanç sabit kalır;
        // toplam güce göre AGC, güçlü istasyon başlayınca kazancı çökertip
        // çözücünün eşiklerini şaşırtıyordu.
        // Taban dB cinsinden izlenir: aşağı hızlı, yukarı en fazla ~2 dB/s.
        // (Güç üzerinden doğrusal izlemek, 40 dB'lik güçlü bir çizgi sırasında
        // tabanı birkaç blokta onlarca kat yükseltip kazancı çökertiyordu.)
        self.blk_pow += re * re;
        self.blk_n += 1;
        if self.blk_n == 64 {
            let p_db = 10.0 * (self.blk_pow / 64.0).max(1e-30).log10();
            self.blk_pow = 0.0;
            self.blk_n = 0;
            let blk_s = 64.0 / self.out_rate;
            if self.warm < 20 {
                self.warm += 1;
                self.floor_db = if self.warm == 1 { p_db } else { self.floor_db.min(p_db) };
            } else if p_db < self.floor_db {
                self.floor_db += (p_db - self.floor_db) * 0.2;
            } else {
                self.floor_db += (p_db - self.floor_db).min(2.0 * blk_s);
            }
            self.gain = self.target / 10f32.powf(self.floor_db / 20.0).max(1e-15);
        }
        let gain = self.fixed_gain.unwrap_or(self.gain);
        Some(re * gain)
    }
}

//! I/Q örneklerinden güç spektrumu: 1024 noktalı karmaşık FFT.
//!
//! Büyütme ×1: 48 kHz'de 1024 nokta → 46.9 Hz/bin, saniyede ~47 FFT; dizinin
//! ortası LO frekansıdır. Büyütmede (×2/×4/×8) giriş, ayar frekansı 0 Hz'e
//! indirilip seyreltilmiş akıştır (zoom-FFT): bin genişliği Z kat daralır,
//! dizinin ortası ayar frekansıdır. FFT'ler örtüşmeli alınır, böylece her
//! büyütmede saniyede ~47 FFT (iki FFT ortalaması → ~23 kare/s) korunur.

use std::f32::consts::PI;
use std::sync::atomic::AtomicU32;
use std::sync::Mutex;

/// İstenen büyütme (1, 2, 4, 8, 16): ana döngü yazar, I/Q okuyucu uygular
pub static ZOOM: AtomicU32 = AtomicU32::new(1);
/// Büyütmede ekran ortasının ayar frekansına uzaklığı (Hz, FT8/FT4'te +1500)
pub static ZOOM_OFS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// Ayar frekansının LO'ya göre konumu (Hz): 12000; CW'de 12000 + CW offset.
/// Ana döngü yazar; büyütme, CW ve FT8 sesi bunu merkez alır.
pub static IF_HZ: AtomicU32 = AtomicU32::new(12_000);

pub const N: usize = 1024;
/// QMX I/Q kanallarının yönü ters gelirse (spektrum aynalanırsa) true yap
pub const SWAP_IQ: bool = false;
const AVG: u32 = 2;

pub struct Spectrum {
    /// Her yeni sonuçta bir artar
    pub seq: u32,
    /// dBFS cinsinden N değer
    pub db: Vec<f32>,
    /// Bu spektrumun büyütmesi (1, 2, 4, 8)
    pub zoom: u32,
    /// Dizinin ortasının (N/2) LO'ya göre frekansı (Hz)
    pub center_hz: f32,
    /// Bin genişliği (Hz)
    pub bin_hz: f32,
}

pub static SPECTRUM: Mutex<Spectrum> = Mutex::new(Spectrum {
    seq: 0,
    db: Vec::new(),
    zoom: 1,
    center_hz: 0.0,
    bin_hz: 48_000.0 / N as f32,
});

/// LO'ya göre [lo_hz, hi_hz] aralığındaki en güçlü bin'in dBFS değeri.
pub fn peak_in(db: &[f32], center_hz: f32, bin_hz: f32, lo_hz: i64, hi_hz: i64) -> f32 {
    let k = |hz: i64| {
        ((N as f32 / 2.0 + (hz as f32 - center_hz) / bin_hz).round() as isize).clamp(0, N as isize - 1) as usize
    };
    let (a, b) = (k(lo_hz.min(hi_hz)), k(lo_hz.max(hi_hz)));
    db[a..=b].iter().cloned().fold(f32::MIN, f32::max)
}

pub struct Analyzer {
    win: Vec<f32>,
    norm: f32,
    tw_re: Vec<f32>,
    tw_im: Vec<f32>,
    rev: Vec<u16>,
    /// Son N örneğin halka tamponu
    ri: Vec<f32>,
    rq: Vec<f32>,
    pos: usize,
    /// Son FFT'den beri gelen örnek; `hop`a ulaşınca yeni FFT
    since: usize,
    hop: usize,
    filled: usize,
    re: Vec<f32>,
    im: Vec<f32>,
    acc: Vec<f32>,
    nacc: u32,
    zoom: u32,
    center_hz: f32,
    bin_hz: f32,
}

impl Analyzer {
    pub fn new() -> Self {
        // Blackman-Harris penceresi (yan loblar -92 dB)
        let win: Vec<f32> = (0..N)
            .map(|n| {
                let x = 2.0 * PI * n as f32 / (N - 1) as f32;
                0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos()
                    - 0.01168 * (3.0 * x).cos()
            })
            .collect();
        let sum: f32 = win.iter().sum();
        let bits = N.trailing_zeros();
        let rev = (0..N)
            .map(|k| ((k as u32).reverse_bits() >> (32 - bits)) as u16)
            .collect();
        let mut a = Self {
            norm: 1.0 / (sum * sum),
            win,
            tw_re: (0..N / 2).map(|k| (-2.0 * PI * k as f32 / N as f32).cos()).collect(),
            tw_im: (0..N / 2).map(|k| (-2.0 * PI * k as f32 / N as f32).sin()).collect(),
            rev,
            ri: vec![0.0; N],
            rq: vec![0.0; N],
            pos: 0,
            since: 0,
            hop: N,
            filled: 0,
            re: vec![0.0; N],
            im: vec![0.0; N],
            acc: vec![0.0; N],
            nacc: 0,
            zoom: 1,
            center_hz: 0.0,
            bin_hz: 0.0,
        };
        a.set_zoom(1, 0.0);
        a
    }

    /// Büyütmeyi ayarlar. `center_hz`: girişin 0 Hz'inin LO'ya göre frekansı.
    pub fn set_zoom(&mut self, zoom: u32, center_hz: f32) {
        let rate = 48_000.0 / zoom as f32;
        self.zoom = zoom;
        self.center_hz = center_hz;
        self.bin_hz = rate / N as f32;
        // Saniyede ~47 FFT: ×1'de örtüşmesiz, büyütmede örtüşmeli
        self.hop = ((rate / 47.0) as usize).clamp(64, N);
        self.since = 0;
        self.filled = 0;
        self.nacc = 0;
        self.acc.fill(0.0);
    }

    #[inline]
    pub fn push(&mut self, i: f32, q: f32) {
        let (i, q) = if SWAP_IQ { (q, i) } else { (i, q) };
        self.ri[self.pos] = i;
        self.rq[self.pos] = q;
        self.pos = (self.pos + 1) % N;
        self.filled = (self.filled + 1).min(N);
        self.since += 1;
        if self.since >= self.hop && self.filled == N {
            self.since = 0;
            self.process();
        }
    }

    fn process(&mut self) {
        // Halkadaki örnekler, eskiden yeniye. ×1'de DC kayması atılır (LO tepesi);
        // büyütmede 0 Hz ayar frekansıdır, oradaki sinyal silinmemeli
        let (mi, mq) = if self.zoom == 1 {
            (self.ri.iter().sum::<f32>() / N as f32, self.rq.iter().sum::<f32>() / N as f32)
        } else {
            (0.0, 0.0)
        };
        for k in 0..N {
            let s = (self.pos + k) % N;
            let r = self.rev[k] as usize;
            self.re[r] = (self.ri[s] - mi) * self.win[k];
            self.im[r] = (self.rq[s] - mq) * self.win[k];
        }
        // Yerinde, iteratif radix-2 FFT
        let mut len = 2;
        while len <= N {
            let half = len / 2;
            let step = N / len;
            for start in (0..N).step_by(len) {
                for j in 0..half {
                    let (wr, wi) = (self.tw_re[j * step], self.tw_im[j * step]);
                    let a = start + j;
                    let b = a + half;
                    let tr = self.re[b] * wr - self.im[b] * wi;
                    let ti = self.re[b] * wi + self.im[b] * wr;
                    self.re[b] = self.re[a] - tr;
                    self.im[b] = self.im[a] - ti;
                    self.re[a] += tr;
                    self.im[a] += ti;
                }
            }
            len *= 2;
        }
        // fftshift: negatif frekanslar sola
        for k in 0..N {
            let src = (k + N / 2) % N;
            self.acc[k] += self.re[src] * self.re[src] + self.im[src] * self.im[src];
        }
        self.nacc += 1;
        if self.nacc == AVG {
            let scale = self.norm / AVG as f32;
            let mut s = SPECTRUM.lock().unwrap();
            s.db.resize(N, 0.0);
            for k in 0..N {
                s.db[k] = 10.0 * (self.acc[k] * scale).max(1e-14).log10();
                self.acc[k] = 0.0;
            }
            s.zoom = self.zoom;
            s.center_hz = self.center_hz;
            s.bin_hz = self.bin_hz;
            s.seq = s.seq.wrapping_add(1);
            self.nacc = 0;
        }
    }
}

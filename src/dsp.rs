//! I/Q örneklerinden güç spektrumu: 1024 noktalı karmaşık FFT.
//!
//! 48 kHz'de 1024 nokta → 46.9 Hz/bin, saniyede ~47 FFT. İki FFT'nin
//! ortalaması yayımlanır (~23 kare/s). Dizinin ortası (N/2) LO frekansıdır;
//! sol yarı LO'nun altı, sağ yarı üstü.

use std::f32::consts::PI;
use std::sync::Mutex;

pub const N: usize = 1024;
/// QMX I/Q kanallarının yönü ters gelirse (spektrum aynalanırsa) true yap
pub const SWAP_IQ: bool = false;
const AVG: u32 = 2;

pub struct Spectrum {
    /// Her yeni sonuçta bir artar
    pub seq: u32,
    /// dBFS cinsinden N değer, LO ortada
    pub db: Vec<f32>,
}

pub static SPECTRUM: Mutex<Spectrum> = Mutex::new(Spectrum {
    seq: 0,
    db: Vec::new(),
});

/// LO'ya göre [lo_hz, hi_hz] aralığındaki en güçlü bin'in dBFS değeri.
pub fn peak_in(db: &[f32], lo_hz: i64, hi_hz: i64) -> f32 {
    let bin_hz = 48_000.0 / N as f32;
    let k = |hz: i64| ((N as f32 / 2.0 + hz as f32 / bin_hz).round() as isize).clamp(0, N as isize - 1) as usize;
    let (a, b) = (k(lo_hz.min(hi_hz)), k(lo_hz.max(hi_hz)));
    db[a..=b].iter().cloned().fold(f32::MIN, f32::max)
}

pub struct Analyzer {
    win: Vec<f32>,
    norm: f32,
    tw_re: Vec<f32>,
    tw_im: Vec<f32>,
    rev: Vec<u16>,
    i: Vec<f32>,
    q: Vec<f32>,
    pos: usize,
    re: Vec<f32>,
    im: Vec<f32>,
    acc: Vec<f32>,
    nacc: u32,
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
        Self {
            norm: 1.0 / (sum * sum),
            win,
            tw_re: (0..N / 2).map(|k| (-2.0 * PI * k as f32 / N as f32).cos()).collect(),
            tw_im: (0..N / 2).map(|k| (-2.0 * PI * k as f32 / N as f32).sin()).collect(),
            rev,
            i: vec![0.0; N],
            q: vec![0.0; N],
            pos: 0,
            re: vec![0.0; N],
            im: vec![0.0; N],
            acc: vec![0.0; N],
            nacc: 0,
        }
    }

    #[inline]
    pub fn push(&mut self, i: f32, q: f32) {
        let (i, q) = if SWAP_IQ { (q, i) } else { (i, q) };
        self.i[self.pos] = i;
        self.q[self.pos] = q;
        self.pos += 1;
        if self.pos == N {
            self.pos = 0;
            self.process();
        }
    }

    fn process(&mut self) {
        // DC kaymasını at (LO'daki sabit tepe küçülsün)
        let mi = self.i.iter().sum::<f32>() / N as f32;
        let mq = self.q.iter().sum::<f32>() / N as f32;
        for k in 0..N {
            let r = self.rev[k] as usize;
            self.re[r] = (self.i[k] - mi) * self.win[k];
            self.im[r] = (self.q[k] - mq) * self.win[k];
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
            s.seq = s.seq.wrapping_add(1);
            self.nacc = 0;
        }
    }
}

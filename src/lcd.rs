//! 800x480 RGB565 paralel ekran (ST7262) ve PSRAM'deki ekran tamponu.

use core::convert::Infallible;
use core::ffi::c_void;
use core::ptr::null_mut;

use embedded_graphics::pixelcolor::{IntoStorage, Rgb565};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use esp_idf_svc::sys::{esp, extra as x};

pub const W: usize = 800;
pub const H: usize = 480;

/// Ekran tamponunun adresi: yalnız ekran görüntüsü almak için okunur
pub static FB_PTR: core::sync::atomic::AtomicPtr<u16> = core::sync::atomic::AtomicPtr::new(null_mut());

/// Waveshare 4.3" ekranının pinleri ve zamanlaması.
pub fn init() -> anyhow::Result<Framebuffer> {
    unsafe {
        let mut cfg: x::esp_lcd_rgb_panel_config_t = core::mem::zeroed();
        cfg.clk_src = x::soc_periph_lcd_clk_src_t_LCD_CLK_SRC_DEFAULT;
        cfg.timings.pclk_hz = 16_000_000;
        cfg.timings.h_res = W as u32;
        cfg.timings.v_res = H as u32;
        cfg.timings.hsync_pulse_width = 4;
        cfg.timings.hsync_back_porch = 8;
        cfg.timings.hsync_front_porch = 8;
        cfg.timings.vsync_pulse_width = 4;
        cfg.timings.vsync_back_porch = 8;
        cfg.timings.vsync_front_porch = 8;
        cfg.timings.flags.set_pclk_active_neg(1);
        cfg.data_width = 16;
        cfg.bits_per_pixel = 16;
        cfg.num_fbs = 1;
        // Ekran tamponu PSRAM'de; DMA iç RAM'deki küçük ara tamponlardan okur
        // (PSRAM meşgulken ekran kaymasın diye)
        cfg.bounce_buffer_size_px = (W * 10) as _;
        cfg.hsync_gpio_num = 46;
        cfg.vsync_gpio_num = 3;
        cfg.de_gpio_num = 5;
        cfg.pclk_gpio_num = 7;
        cfg.disp_gpio_num = -1;
        // D0-4 = mavi, D5-10 = yeşil, D11-15 = kırmızı
        cfg.data_gpio_nums = [14, 38, 18, 17, 10, 39, 0, 45, 48, 47, 21, 1, 2, 42, 41, 40];
        cfg.flags.set_fb_in_psram(1);

        let mut panel: x::esp_lcd_panel_handle_t = null_mut();
        esp!(x::esp_lcd_new_rgb_panel(&cfg, &mut panel))?;
        esp!(x::esp_lcd_panel_reset(panel))?;
        esp!(x::esp_lcd_panel_init(panel))?;

        let mut fb: *mut c_void = null_mut();
        esp!(x::esp_lcd_rgb_panel_get_frame_buffer(panel, 1, &mut fb as *mut *mut c_void))?;
        FB_PTR.store(fb as *mut u16, core::sync::atomic::Ordering::SeqCst);
        Ok(Framebuffer {
            buf: core::slice::from_raw_parts_mut(fb as *mut u16, W * H),
            h: H,
        })
    }
}

/// Doğrudan ekranın gösterdiği bellek: buraya yazılan anında görünür.
/// Aynı yapı, ekran dışı çizim katmanları için de (genişlik W, yükseklik h) kullanılır.
pub struct Framebuffer {
    pub buf: &'static mut [u16],
    pub h: usize,
}

impl Framebuffer {
    /// Ekran dışı bir katman: W × h piksel, ömür boyu ayrılır
    pub fn layer(h: usize, fill: u16) -> Self {
        Self {
            buf: Box::leak(vec![fill; W * h].into_boxed_slice()),
            h,
        }
    }

    #[inline]
    pub fn put(&mut self, x: usize, y: usize, c: Rgb565) {
        if x < W && y < self.h {
            self.buf[y * W + x] = c.into_storage();
        }
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(W as u32, self.h as u32)
    }
}

impl DrawTarget for Framebuffer {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 {
                self.put(p.x as usize, p.y as usize, c);
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let a = area.intersection(&self.bounding_box());
        if let Some(br) = a.bottom_right() {
            let v = color.into_storage();
            let (x0, x1) = (a.top_left.x as usize, br.x as usize);
            for y in a.top_left.y as usize..=br.y as usize {
                self.buf[y * W + x0..=y * W + x1].fill(v);
            }
        }
        Ok(())
    }
}

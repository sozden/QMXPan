//! Waveshare ESP32-S3-Touch-LCD-4.3 kartına özgü bağlantılar.
//!
//! I2C (SDA=GPIO8, SCL=GPIO9) hattında iki çip var:
//! - CH422G G/Ç genişletici: EXIO1=dokunmatik reset, EXIO2=arka ışık,
//!   EXIO3=LCD reset, EXIO4=SD CS, EXIO5=USB_SEL (0=USB portu, 1=CAN).
//! - GT911 dokunmatik denetleyici (INT=GPIO4).

use esp_idf_svc::hal::delay::{FreeRtos, BLOCK};
use esp_idf_svc::hal::i2c::I2cDriver;

/// CH422G'nin "adresleri" aslında komut kodlarıdır.
const CH422G_SET_MODE: u8 = 0x24;
const CH422G_WRITE_IO: u8 = 0x38;

pub const EXIO_TP_RST: u8 = 1 << 1;
pub const EXIO_BACKLIGHT: u8 = 1 << 2;
pub const EXIO_LCD_RST: u8 = 1 << 3;
pub const EXIO_SD_CS: u8 = 1 << 4;
/// Bu bit HER ZAMAN 0 kalmalı: 1 olursa ESP32'nin USB pinleri CAN çipine gider
/// ve QMX ile bağlantı kopar.
#[allow(dead_code)]
pub const EXIO_USB_SEL_CAN: u8 = 1 << 5;

pub struct Expander {
    out: u8,
}

impl Expander {
    /// IO0-7'yi çıkış yapar; ekranı ve dokunmatiği resetten çıkarır, arka ışığı yakar.
    pub fn init(i2c: &mut I2cDriver) -> anyhow::Result<Self> {
        i2c.write(CH422G_SET_MODE, &[0x01], BLOCK)?;
        let mut e = Self {
            out: EXIO_TP_RST | EXIO_BACKLIGHT | EXIO_LCD_RST | EXIO_SD_CS,
        };
        e.flush(i2c)?;
        Ok(e)
    }

    pub fn set(&mut self, i2c: &mut I2cDriver, bit: u8, on: bool) -> anyhow::Result<()> {
        if on {
            self.out |= bit;
        } else {
            self.out &= !bit;
        }
        self.flush(i2c)
    }

    fn flush(&mut self, i2c: &mut I2cDriver) -> anyhow::Result<()> {
        // USB_SEL'i ne olursa olsun USB'de tut
        self.out &= !EXIO_USB_SEL_CAN;
        i2c.write(CH422G_WRITE_IO, &[self.out], BLOCK)?;
        Ok(())
    }
}

/// GT911 adresini seçerek dokunmatiği resetler (INT düşük → 0x5D).
pub fn reset_touch(
    i2c: &mut I2cDriver,
    exp: &mut Expander,
    int_pin: esp_idf_svc::hal::gpio::AnyIOPin,
) -> anyhow::Result<()> {
    use esp_idf_svc::hal::gpio::PinDriver;
    let mut int = PinDriver::output(int_pin)?;
    int.set_low()?;
    exp.set(i2c, EXIO_TP_RST, false)?;
    FreeRtos::delay_ms(10);
    exp.set(i2c, EXIO_TP_RST, true)?;
    FreeRtos::delay_ms(10);
    // INT'i serbest bırak (GT911 artık kendisi sürer)
    let _ = int.try_into_input(esp_idf_svc::hal::gpio::Pull::Floating)?;
    FreeRtos::delay_ms(60);
    Ok(())
}

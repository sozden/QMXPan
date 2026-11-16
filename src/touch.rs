//! GT911 kapasitif dokunmatik için en sade sürücü (tek nokta).

use esp_idf_svc::hal::delay::BLOCK;
use esp_idf_svc::hal::i2c::I2cDriver;

const REG_PRODUCT_ID: u16 = 0x8140;
const REG_STATUS: u16 = 0x814E;
const REG_POINT1: u16 = 0x814F;

pub struct Gt911 {
    addr: u8,
}

fn reg(r: u16) -> [u8; 2] {
    r.to_be_bytes()
}

impl Gt911 {
    /// İki olası adresi dener, ürün kimliğini kayda yazar.
    pub fn probe(i2c: &mut I2cDriver) -> Option<Self> {
        for addr in [0x5D, 0x14] {
            let mut id = [0u8; 4];
            if i2c.write_read(addr, &reg(REG_PRODUCT_ID), &mut id, BLOCK).is_ok() {
                log::info!(
                    "Dokunmatik bulundu: adres 0x{addr:02X}, kimlik \"{}\"",
                    String::from_utf8_lossy(&id).trim_end_matches('\0')
                );
                return Some(Self { addr });
            }
        }
        log::warn!("GT911 dokunmatik bulunamadı");
        None
    }

    /// Yeni ölçüm yoksa `None`; varsa `Some(nokta)` — parmak kalktıysa `Some(None)`.
    pub fn poll(&mut self, i2c: &mut I2cDriver) -> Option<Option<(u16, u16)>> {
        let mut st = [0u8; 1];
        i2c.write_read(self.addr, &reg(REG_STATUS), &mut st, BLOCK).ok()?;
        if st[0] & 0x80 == 0 {
            return None; // yeni veri yok
        }
        let n = st[0] & 0x0F;
        let mut pt = None;
        if n > 0 {
            let mut b = [0u8; 8];
            if i2c.write_read(self.addr, &reg(REG_POINT1), &mut b, BLOCK).is_ok() {
                let x = u16::from_le_bytes([b[1], b[2]]);
                let y = u16::from_le_bytes([b[3], b[4]]);
                pt = Some((x, y));
            }
        }
        // Durum kaydını temizle ki bir sonraki ölçüm gelsin
        let r = reg(REG_STATUS);
        let _ = i2c.write(self.addr, &[r[0], r[1], 0], BLOCK);
        Some(pt)
    }
}

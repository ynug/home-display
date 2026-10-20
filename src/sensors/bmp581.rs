//! Bosch BMP581: pressure and temperature. One forced conversion per read.

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;

use super::{SensorError, Values, bus_err};

const REG_CHIP_ID: u8 = 0x01;
const REG_TEMP_XLSB: u8 = 0x1D;
const REG_INT_STATUS: u8 = 0x27;
const REG_STATUS: u8 = 0x28;
const REG_OSR_CONFIG: u8 = 0x36;
const REG_ODR_CONFIG: u8 = 0x37;
const REG_CMD: u8 = 0x7E;

const CHIP_ID: u8 = 0x50;
const CMD_SOFT_RESET: u8 = 0xB6;
const STATUS_NVM_RDY: u8 = 0x02;
const STATUS_NVM_ERR: u8 = 0x04;
/// ODR_CONFIG: bit 7 `deep_dis` (1 = no deep standby), bits 1:0 power mode.
const ODR_DEEP_DIS: u8 = 0x80;
const MODE_MASK: u8 = 0x03;
const MODE_STANDBY: u8 = 0x00;
const MODE_FORCED: u8 = 0x02;
/// OSR_CONFIG: `press_en` | pressure ×16 (bits 5:3 = 4) | temperature ×2 (bits 2:0 = 1).
const OSR_PRESS_EN_P16_T2: u8 = 0x40 | (4 << 3) | 1;
/// A forced conversion at these settings takes well under 50 ms.
const POLL_MS: u32 = 5;
const POLL_TRIES: u32 = 20;

pub struct Bmp581 {
    addr: u8,
}

impl Bmp581 {
    pub fn new(addr: u8) -> Self {
        Bmp581 { addr }
    }

    fn reg<I: I2c>(&self, i2c: &mut I, reg: u8) -> Result<u8, SensorError> {
        let mut b = [0u8];
        i2c.write_read(self.addr, &[reg], &mut b).map_err(bus_err)?;
        Ok(b[0])
    }

    fn set<I: I2c>(&self, i2c: &mut I, reg: u8, value: u8) -> Result<(), SensorError> {
        i2c.write(self.addr, &[reg, value]).map_err(bus_err)
    }

    pub fn init<I: I2c, D: DelayNs>(&mut self, i2c: &mut I, delay: &mut D) -> Result<(), SensorError> {
        self.set(i2c, REG_CMD, CMD_SOFT_RESET)?;
        delay.delay_ms(2);
        let id = self.reg(i2c, REG_CHIP_ID)?;
        if id != CHIP_ID {
            return Err(SensorError::ChipId(id));
        }
        let status = self.reg(i2c, REG_STATUS)?;
        if status & STATUS_NVM_RDY == 0 || status & STATUS_NVM_ERR != 0 {
            return Err(SensorError::NotReady);
        }
        self.reg(i2c, REG_INT_STATUS)?; // clears the power-on-reset flag
        // OSR may only be changed in standby.
        self.set(i2c, REG_ODR_CONFIG, ODR_DEEP_DIS | MODE_STANDBY)?;
        delay.delay_ms(5);
        self.set(i2c, REG_OSR_CONFIG, OSR_PRESS_EN_P16_T2)?;
        Ok(())
    }

    pub fn read<I: I2c, D: DelayNs>(&mut self, i2c: &mut I, delay: &mut D) -> Result<Values, SensorError> {
        self.set(i2c, REG_ODR_CONFIG, ODR_DEEP_DIS | MODE_FORCED)?;
        // The sensor returns to standby when the conversion is done.
        let mut done = false;
        for _ in 0..POLL_TRIES {
            delay.delay_ms(POLL_MS);
            if self.reg(i2c, REG_ODR_CONFIG)? & MODE_MASK == MODE_STANDBY {
                done = true;
                break;
            }
        }
        if !done {
            return Err(SensorError::Timeout);
        }
        let mut b = [0u8; 6];
        i2c.write_read(self.addr, &[REG_TEMP_XLSB], &mut b).map_err(bus_err)?;
        Ok(convert(b))
    }
}

/// `[t_xlsb, t_lsb, t_msb, p_xlsb, p_lsb, p_msb]` → °C (signed 24-bit / 2^16)
/// and hPa (unsigned 24-bit / 2^6 Pa).
fn convert(b: [u8; 6]) -> Values {
    let t_raw = i32::from_le_bytes([b[0], b[1], b[2], 0]) << 8 >> 8;
    let p_raw = u32::from_le_bytes([b[3], b[4], b[5], 0]);
    Values {
        temperature: Some(t_raw as f32 / 65536.0),
        pressure: Some(p_raw as f32 / 64.0 / 100.0),
        ..Values::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::testing::{FakeBus, NoDelay};

    fn le24(v: u32) -> [u8; 3] {
        let [a, b, c, _] = v.to_le_bytes();
        [a, b, c]
    }

    #[test]
    fn converts_raw_values() {
        // 23.5 °C, 1013.25 hPa.
        let t = le24((23.5 * 65536.0) as u32);
        let p = le24((101_325.0 * 64.0) as u32);
        let v = convert([t[0], t[1], t[2], p[0], p[1], p[2]]);
        assert!((v.temperature.unwrap() - 23.5).abs() < 0.001);
        assert!((v.pressure.unwrap() - 1013.25).abs() < 0.001);
        // Negative temperature (two's complement in 24 bits).
        let t = le24(((-5.25f32 * 65536.0) as i32 as u32) & 0xFF_FFFF);
        let v = convert([t[0], t[1], t[2], 0, 0, 0]);
        assert!((v.temperature.unwrap() + 5.25).abs() < 0.001);
    }

    #[test]
    fn init_checks_chip_id() {
        let mut bus = FakeBus::new(0x47);
        bus.reply(&[0x51]);
        assert_eq!(
            Bmp581::new(0x47).init(&mut bus, &mut NoDelay),
            Err(SensorError::ChipId(0x51))
        );
    }

    #[test]
    fn init_and_forced_read() {
        let mut bus = FakeBus::new(0x47);
        for r in [CHIP_ID, STATUS_NVM_RDY, 0x10] {
            bus.reply(&[r]);
        }
        let mut s = Bmp581::new(0x47);
        s.init(&mut bus, &mut NoDelay).unwrap();
        // Still converting once, then back in standby, then the data.
        bus.reply(&[ODR_DEEP_DIS | MODE_FORCED]);
        bus.reply(&[ODR_DEEP_DIS | MODE_STANDBY]);
        let t = le24(20 * 65536);
        let p = le24(100_000 * 64);
        bus.reply(&[t[0], t[1], t[2], p[0], p[1], p[2]]);
        let v = s.read(&mut bus, &mut NoDelay).unwrap();
        assert_eq!(v.pressure, Some(1000.0));
        assert_eq!(v.temperature, Some(20.0));
        assert!(bus.writes().contains(&vec![REG_OSR_CONFIG, 0x61]));
    }

    #[test]
    fn conversion_timeout() {
        let mut bus = FakeBus::new(0x47);
        for _ in 0..POLL_TRIES {
            bus.reply(&[ODR_DEEP_DIS | MODE_FORCED]);
        }
        assert_eq!(
            Bmp581::new(0x47).read(&mut bus, &mut NoDelay),
            Err(SensorError::Timeout)
        );
    }
}

//! Sensirion SHT3x (SHT30/31/35): temperature and humidity.
//! Single-shot, high repeatability, no clock stretching.

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;

use super::{SensorError, Values, bus_err, crc};

const SOFT_RESET: [u8; 2] = [0x30, 0xA2];
const MEASURE_HIGH: [u8; 2] = [0x24, 0x00];
/// Max. 15.5 ms for a high-repeatability measurement.
const MEASURE_MS: u32 = 16;

pub struct Sht3x {
    addr: u8,
}

impl Sht3x {
    pub fn new(addr: u8) -> Self {
        Sht3x { addr }
    }

    pub fn init<I: I2c, D: DelayNs>(&mut self, i2c: &mut I, delay: &mut D) -> Result<(), SensorError> {
        i2c.write(self.addr, &SOFT_RESET).map_err(bus_err)?;
        delay.delay_ms(2);
        Ok(())
    }

    pub fn read<I: I2c, D: DelayNs>(&mut self, i2c: &mut I, delay: &mut D) -> Result<Values, SensorError> {
        i2c.write(self.addr, &MEASURE_HIGH).map_err(bus_err)?;
        delay.delay_ms(MEASURE_MS);
        let mut buf = [0u8; 6];
        i2c.read(self.addr, &mut buf).map_err(bus_err)?;
        let [t, rh] = crc::words::<2>(&buf)?;
        Ok(Values {
            temperature: Some(temperature(t)),
            humidity: Some(humidity(rh)),
            ..Values::default()
        })
    }
}

/// Shared by SHT3x and SCD4x: −45 + 175 × raw / 65535.
pub fn temperature(raw: u16) -> f32 {
    -45.0 + 175.0 * raw as f32 / 65535.0
}

/// Shared by SHT3x and SCD4x: 100 × raw / 65535.
pub fn humidity(raw: u16) -> f32 {
    100.0 * raw as f32 / 65535.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::testing::{FakeBus, NoDelay};

    #[test]
    fn converts_a_measurement() {
        let mut bus = FakeBus::new(0x44);
        // 25.0 °C → raw 26214 (0x6666); 50 %RH → raw 32768 (0x8000).
        bus.reply(&[crc::with_crc(0x6666), crc::with_crc(0x8000)].concat());
        let mut s = Sht3x::new(0x44);
        s.init(&mut bus, &mut NoDelay).unwrap();
        let v = s.read(&mut bus, &mut NoDelay).unwrap();
        assert!((v.temperature.unwrap() - 25.0).abs() < 0.01, "{v:?}");
        assert!((v.humidity.unwrap() - 50.0).abs() < 0.01, "{v:?}");
        assert_eq!(bus.writes(), vec![SOFT_RESET.to_vec(), MEASURE_HIGH.to_vec()]);
    }

    #[test]
    fn rejects_a_bad_crc() {
        let mut bus = FakeBus::new(0x44);
        let mut reply = [crc::with_crc(0x6666), crc::with_crc(0x8000)].concat();
        reply[2] ^= 0xFF;
        bus.reply(&reply);
        assert_eq!(Sht3x::new(0x44).read(&mut bus, &mut NoDelay), Err(SensorError::Crc));
    }

    #[test]
    fn missing_device_is_a_bus_error() {
        let mut bus = FakeBus::new(0x45);
        let err = Sht3x::new(0x44).read(&mut bus, &mut NoDelay).unwrap_err();
        assert!(matches!(err, SensorError::Bus(_)), "{err:?}");
    }
}

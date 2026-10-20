//! Sensirion SCD4x (SCD40/41): CO2, temperature, humidity.
//! Periodic measurement: a new value every 5 s.

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;

use super::sht3x::{humidity, temperature};
use super::{SensorError, Values, bus_err, crc};

const START_PERIODIC: u16 = 0x21B1;
const STOP_PERIODIC: u16 = 0x3F86;
const READ_MEASUREMENT: u16 = 0xEC05;
const DATA_READY: u16 = 0xE4B8;
const SET_AMBIENT_PRESSURE: u16 = 0xE000;
const SET_ASC_ENABLED: u16 = 0x2416;
const SERIAL_NUMBER: u16 = 0x3682;
/// The sensor ignores commands for 500 ms after `stop_periodic_measurement`.
const STOP_MS: u32 = 500;
/// `set_ambient_pressure` accepts 700–1200 hPa.
const PRESSURE_RANGE: std::ops::RangeInclusive<f32> = 700.0..=1200.0;

pub struct Scd4x {
    addr: u8,
    auto_calibration: Option<bool>,
    /// Last pressure sent (hPa), so it is only re-sent when it changes.
    pressure_sent: Option<u16>,
}

impl Scd4x {
    pub fn new(addr: u8, auto_calibration: Option<bool>) -> Self {
        Scd4x {
            addr,
            auto_calibration,
            pressure_sent: None,
        }
    }

    fn command<I: I2c>(&self, i2c: &mut I, cmd: u16) -> Result<(), SensorError> {
        i2c.write(self.addr, &cmd.to_be_bytes()).map_err(bus_err)
    }

    fn command_with<I: I2c>(&self, i2c: &mut I, cmd: u16, arg: u16) -> Result<(), SensorError> {
        let [c0, c1] = cmd.to_be_bytes();
        let [a0, a1, crc] = crc::with_crc(arg);
        i2c.write(self.addr, &[c0, c1, a0, a1, crc]).map_err(bus_err)
    }

    fn query<I: I2c, D: DelayNs, const N: usize, const B: usize>(
        &self,
        i2c: &mut I,
        delay: &mut D,
        cmd: u16,
    ) -> Result<[u16; N], SensorError> {
        self.command(i2c, cmd)?;
        delay.delay_ms(1);
        let mut buf = [0u8; B];
        i2c.read(self.addr, &mut buf).map_err(bus_err)?;
        crc::words::<N>(&buf)
    }

    /// The sensor keeps measuring across our restarts (it stays powered), so
    /// always stop it first; settings can only be changed while idle.
    pub fn init<I: I2c, D: DelayNs>(&mut self, i2c: &mut I, delay: &mut D) -> Result<(), SensorError> {
        // An idle sensor accepts the stop command as well; a missing one is
        // reported by the serial-number read below.
        let _ = self.command(i2c, STOP_PERIODIC);
        delay.delay_ms(STOP_MS);
        self.query::<I, D, 3, 9>(i2c, delay, SERIAL_NUMBER)?;
        if let Some(on) = self.auto_calibration {
            // RAM only: never `persist_settings` (EEPROM wear).
            self.command_with(i2c, SET_ASC_ENABLED, on as u16)?;
            delay.delay_ms(1);
        }
        self.command(i2c, START_PERIODIC)?;
        self.pressure_sent = None;
        Ok(())
    }

    /// `Ok(None)` until the next 5-second measurement is ready.
    pub fn read<I: I2c, D: DelayNs>(
        &mut self,
        i2c: &mut I,
        delay: &mut D,
        pressure_hpa: Option<f32>,
    ) -> Result<Option<Values>, SensorError> {
        if let Some(p) = pressure_hpa.filter(|p| PRESSURE_RANGE.contains(p)) {
            let hpa = p.round() as u16;
            if self.pressure_sent != Some(hpa) {
                self.command_with(i2c, SET_AMBIENT_PRESSURE, hpa)?;
                delay.delay_ms(1);
                self.pressure_sent = Some(hpa);
            }
        }
        let [status] = self.query::<I, D, 1, 3>(i2c, delay, DATA_READY)?;
        if status & 0x07FF == 0 {
            return Ok(None);
        }
        let [co2, t, rh] = self.query::<I, D, 3, 9>(i2c, delay, READ_MEASUREMENT)?;
        if co2 == 0 {
            return Ok(None); // not a valid sample (seen right after start)
        }
        Ok(Some(Values {
            co2: Some(co2 as f32),
            temperature: Some(temperature(t)),
            humidity: Some(humidity(rh)),
            ..Values::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::testing::{FakeBus, NoDelay};

    fn reply_words(bus: &mut FakeBus, words: &[u16]) {
        bus.reply(&words.iter().flat_map(|w| crc::with_crc(*w)).collect::<Vec<_>>());
    }

    #[test]
    fn init_stops_configures_and_starts() {
        let mut bus = FakeBus::new(0x62);
        reply_words(&mut bus, &[0x1234, 0x5678, 0x9ABC]);
        let mut s = Scd4x::new(0x62, Some(false));
        s.init(&mut bus, &mut NoDelay).unwrap();
        assert_eq!(
            bus.writes(),
            vec![
                vec![0x3F, 0x86],
                vec![0x36, 0x82],
                [vec![0x24, 0x16], crc::with_crc(0).to_vec()].concat(),
                vec![0x21, 0xB1],
            ]
        );
    }

    #[test]
    fn reads_when_ready_and_sends_pressure_once() {
        let mut bus = FakeBus::new(0x62);
        let mut s = Scd4x::new(0x62, None);
        // Not ready yet.
        reply_words(&mut bus, &[0x8000]);
        assert_eq!(s.read(&mut bus, &mut NoDelay, Some(1012.4)).unwrap(), None);
        // Ready: 812 ppm, 25 °C, 50 %RH.
        reply_words(&mut bus, &[0x8006]);
        reply_words(&mut bus, &[812, 0x6666, 0x8000]);
        let v = s.read(&mut bus, &mut NoDelay, Some(1012.2)).unwrap().unwrap();
        assert_eq!(v.co2, Some(812.0));
        assert!((v.temperature.unwrap() - 25.0).abs() < 0.01);
        let pressure_writes: Vec<_> = bus.writes().into_iter().filter(|w| w[..2] == [0xE0, 0x00]).collect();
        assert_eq!(
            pressure_writes,
            vec![[vec![0xE0, 0x00], crc::with_crc(1012).to_vec()].concat()]
        );
    }

    #[test]
    fn zero_co2_is_not_a_sample() {
        let mut bus = FakeBus::new(0x62);
        reply_words(&mut bus, &[0x0001]);
        reply_words(&mut bus, &[0, 0x6666, 0x8000]);
        assert_eq!(Scd4x::new(0x62, None).read(&mut bus, &mut NoDelay, None), Ok(None));
    }

    #[test]
    fn missing_sensor_fails_init() {
        let mut bus = FakeBus::new(0x44);
        let err = Scd4x::new(0x62, None).init(&mut bus, &mut NoDelay).unwrap_err();
        assert!(matches!(err, SensorError::Bus(_)), "{err:?}");
    }
}

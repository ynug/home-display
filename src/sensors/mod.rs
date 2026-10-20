//! Indoor sensors read directly over I2C (docs/sensors.md).
//!
//! One plain thread per bus polls every device each `interval_seconds` and
//! writes the latest values into [`Shared`]; the UI thread reads that once a
//! second, like the radar download progress. The [`Repository`] only hears
//! about health changes (and a heartbeat every 30 s), so per-second readings
//! never trigger a full snapshot refresh.

mod bmp581;
mod bus;
mod crc;
mod scd4x;
mod sht3x;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::{self, I2c, NoAcknowledgeSource};

use crate::clock;
use crate::config::{Quantity, SensorDevice, SensorModel, SensorsConfig};
use crate::repository::Repository;
use crate::status::{ErrorKind, SourceError, SourceId};

pub use bus::{Bus, FTDI};

/// One reading; a field is `None` when the sensor does not measure it.
/// Units: °C, %RH, ppm, hPa.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Values {
    pub temperature: Option<f32>,
    pub humidity: Option<f32>,
    pub co2: Option<f32>,
    pub pressure: Option<f32>,
}

impl Values {
    pub fn get(&self, q: Quantity) -> Option<f32> {
        match q {
            Quantity::Temperature => self.temperature,
            Quantity::Humidity => self.humidity,
            Quantity::Co2 => self.co2,
            Quantity::Pressure => self.pressure,
        }
    }

    /// Physically implausible values mean a broken read, not weather.
    fn check(self) -> Result<Values, SensorError> {
        let limits = [
            (Quantity::Temperature, -40.0, 125.0),
            (Quantity::Humidity, 0.0, 100.0),
            (Quantity::Co2, 0.0, 40_000.0),
            (Quantity::Pressure, 300.0, 1250.0),
        ];
        for (q, lo, hi) in limits {
            if let Some(v) = self.get(q)
                && !(lo..=hi).contains(&v)
            {
                return Err(SensorError::OutOfRange(format!("{} {v:.1}{}", q.label(), q.unit())));
            }
        }
        Ok(self)
    }

    /// "温度 24.12℃ · 湿度 45.3%" (for logs and `--read-sensors`).
    pub fn describe(&self) -> String {
        Quantity::ALL
            .iter()
            .filter_map(|q| {
                self.get(*q).map(|v| {
                    let digits = match q {
                        Quantity::Temperature | Quantity::Pressure => 2,
                        Quantity::Humidity => 1,
                        Quantity::Co2 => 0,
                    };
                    format!("{} {v:.digits$}{}", q.label(), q.unit())
                })
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SensorError {
    #[error("I2C バスを開けません: {0}")]
    Open(String),
    #[error("{0}")]
    Bus(String),
    #[error("CRC が一致しません（ノイズ・配線を確認）")]
    Crc,
    #[error("チップ ID が違います (0x{0:02X})。型番・アドレスを確認してください")]
    ChipId(u8),
    #[error("センサーの準備ができていません")]
    NotReady,
    #[error("計測が時間内に終わりません")]
    Timeout,
    #[error("値が範囲外です ({0})")]
    OutOfRange(String),
}

fn bus_err<E: i2c::Error>(e: E) -> SensorError {
    use i2c::ErrorKind as K;
    SensorError::Bus(
        match e.kind() {
            K::NoAcknowledge(NoAcknowledgeSource::Data) => "データに応答がありません",
            K::NoAcknowledge(_) => "応答がありません（配線・アドレスを確認）",
            K::ArbitrationLoss => "バスの競合が起きました",
            K::Bus => "バスエラー（配線・プルアップを確認）",
            K::Overrun => "オーバーランが起きました",
            _ => "I2C 通信エラー",
        }
        .into(),
    )
}

/// Blocking delay; the drivers run on the sensor thread only.
pub struct StdDelay;

impl DelayNs for StdDelay {
    fn delay_ns(&mut self, ns: u32) {
        std::thread::sleep(Duration::from_nanos(ns as u64));
    }
}

enum Driver {
    Sht3x(sht3x::Sht3x),
    Scd4x(scd4x::Scd4x),
    Bmp581(bmp581::Bmp581),
}

impl Driver {
    fn new(d: &SensorDevice) -> Driver {
        match d.model {
            SensorModel::Sht35 => Driver::Sht3x(sht3x::Sht3x::new(d.address)),
            SensorModel::Scd41 => Driver::Scd4x(scd4x::Scd4x::new(d.address, d.auto_calibration)),
            SensorModel::Bmp581 => Driver::Bmp581(bmp581::Bmp581::new(d.address)),
        }
    }

    fn init<I: I2c, D: DelayNs>(&mut self, i2c: &mut I, delay: &mut D) -> Result<(), SensorError> {
        match self {
            Driver::Sht3x(s) => s.init(i2c, delay),
            Driver::Scd4x(s) => s.init(i2c, delay),
            Driver::Bmp581(s) => s.init(i2c, delay),
        }
    }

    /// `Ok(None)`: the sensor answered but has no new value yet (SCD41).
    fn read<I: I2c, D: DelayNs>(
        &mut self,
        i2c: &mut I,
        delay: &mut D,
        pressure_hpa: Option<f32>,
    ) -> Result<Option<Values>, SensorError> {
        let v = match self {
            Driver::Sht3x(s) => Some(s.read(i2c, delay)?),
            Driver::Scd4x(s) => s.read(i2c, delay, pressure_hpa)?,
            Driver::Bmp581(s) => Some(s.read(i2c, delay)?),
        };
        v.map(Values::check).transpose()
    }
}

/// Result of one device in one polling cycle.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Value(Values),
    /// Answered, but no new value yet.
    Pending,
    Failed(SensorError),
    /// Backing off after failures; the last error still applies.
    Waiting,
}

struct Device {
    cfg: SensorDevice,
    driver: Driver,
    ready: bool,
    failures: u32,
    retry_at: Option<Instant>,
    last_error: Option<SensorError>,
    /// Latest value and when it was read (feeds `pressure_from`).
    latest: Option<(Values, Instant)>,
}

/// Read failures in a row before a device is re-initialised.
const REINIT_AFTER: u32 = 3;
/// Cycles in which every device failed before the bus is reopened
/// (e.g. the USB adapter was unplugged and plugged in again).
const REOPEN_AFTER: u32 = 5;
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A `pressure_from` value older than this is not used for compensation.
const PRESSURE_MAX_AGE: Duration = Duration::from_secs(60);

fn backoff(failures: u32) -> Duration {
    Duration::from_secs(1u64 << failures.saturating_sub(1).min(6)).min(MAX_BACKOFF)
}

/// All devices on one bus. Generic over the bus so tests can use a fake one.
pub struct Poller<B> {
    open: Box<dyn FnMut() -> Result<B, SensorError> + Send>,
    bus: Option<B>,
    bus_failures: u32,
    bus_retry_at: Option<Instant>,
    all_failed_cycles: u32,
    devices: Vec<Device>,
    /// Pressure sources first, so compensation uses this cycle's value.
    order: Vec<usize>,
}

impl Poller<Bus> {
    pub fn for_config(cfg: &SensorsConfig) -> Self {
        let spec = cfg.bus.clone();
        Poller::new(cfg, Box::new(move || Bus::open(&spec)))
    }
}

impl<B: I2c> Poller<B> {
    pub fn new(cfg: &SensorsConfig, open: Box<dyn FnMut() -> Result<B, SensorError> + Send>) -> Self {
        let devices: Vec<Device> = cfg
            .device
            .iter()
            .map(|d| Device {
                cfg: d.clone(),
                driver: Driver::new(d),
                ready: false,
                failures: 0,
                retry_at: None,
                last_error: None,
                latest: None,
            })
            .collect();
        let mut order: Vec<usize> = (0..devices.len()).collect();
        order.sort_by_key(|&i| devices[i].cfg.pressure_from.is_some());
        Poller {
            open,
            bus: None,
            bus_failures: 0,
            bus_retry_at: None,
            all_failed_cycles: 0,
            devices,
            order,
        }
    }

    pub fn devices(&self) -> impl Iterator<Item = &SensorDevice> {
        self.devices.iter().map(|d| &d.cfg)
    }

    /// The last error of each device (`None` = healthy).
    pub fn errors(&self) -> impl Iterator<Item = (&SensorDevice, Option<&SensorError>)> {
        self.devices.iter().map(|d| (&d.cfg, d.last_error.as_ref()))
    }

    /// Poll every device once. Results are in configuration order.
    pub fn cycle<D: DelayNs>(&mut self, now: Instant, delay: &mut D) -> Vec<Outcome> {
        let mut out = vec![Outcome::Waiting; self.devices.len()];
        if self.bus.is_none() {
            if self.bus_retry_at.is_some_and(|t| now < t) {
                return out;
            }
            match (self.open)() {
                Ok(b) => {
                    self.bus = Some(b);
                    self.bus_failures = 0;
                    self.bus_retry_at = None;
                    for d in &mut self.devices {
                        d.ready = false;
                        d.retry_at = None;
                    }
                }
                Err(e) => {
                    self.bus_failures += 1;
                    self.bus_retry_at = Some(now + backoff(self.bus_failures));
                    for (d, o) in self.devices.iter_mut().zip(&mut out) {
                        d.last_error = Some(e.clone());
                        *o = Outcome::Failed(e.clone());
                    }
                    return out;
                }
            }
        }
        let bus = self.bus.as_mut().expect("opened above");

        for &i in &self.order {
            let pressure = self.devices[i]
                .cfg
                .pressure_from
                .as_ref()
                .and_then(|src| self.devices.iter().find(|d| &d.cfg.id == src))
                .and_then(|d| d.latest)
                .filter(|(_, at)| now.saturating_duration_since(*at) <= PRESSURE_MAX_AGE)
                .and_then(|(v, _)| v.pressure);
            let d = &mut self.devices[i];
            if d.retry_at.is_some_and(|t| now < t) {
                continue; // Outcome::Waiting
            }
            let result = if d.ready {
                d.driver.read(bus, delay, pressure)
            } else {
                d.driver.init(bus, delay).and_then(|()| {
                    d.ready = true;
                    d.driver.read(bus, delay, pressure)
                })
            };
            out[i] = match result {
                Ok(v) => {
                    d.failures = 0;
                    d.retry_at = None;
                    d.last_error = None;
                    match v {
                        Some(v) => {
                            d.latest = Some((v, now));
                            Outcome::Value(v)
                        }
                        None => Outcome::Pending,
                    }
                }
                Err(e) => {
                    d.failures += 1;
                    if !d.ready || d.failures >= REINIT_AFTER {
                        d.ready = false;
                        d.retry_at = Some(now + backoff(d.failures));
                    }
                    d.last_error = Some(e.clone());
                    Outcome::Failed(e)
                }
            };
        }

        let attempted = out.iter().filter(|o| !matches!(o, Outcome::Waiting)).count();
        let failed = out.iter().filter(|o| matches!(o, Outcome::Failed(_))).count();
        if attempted > 0 && failed == attempted {
            self.all_failed_cycles += 1;
            if self.all_failed_cycles >= REOPEN_AFTER {
                self.bus = None;
                self.all_failed_cycles = 0;
            }
        } else if attempted > 0 {
            self.all_failed_cycles = 0;
        }
        out
    }
}

/// Latest state of one device, as shown on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceState {
    pub id: String,
    pub model: SensorModel,
    pub address: u8,
    pub values: Values,
    pub measured_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub devices: Vec<DeviceState>,
    /// Bumped on every change, so the UI can skip unchanged ticks.
    pub revision: u64,
}

impl State {
    pub fn device(&self, id: &str) -> Option<&DeviceState> {
        self.devices.iter().find(|d| d.id == id)
    }
}

/// Written by the sensor thread, read by the UI thread.
#[derive(Debug, Default)]
pub struct Shared {
    state: Mutex<State>,
}

impl Shared {
    pub fn new(cfg: Option<&SensorsConfig>) -> Arc<Shared> {
        let devices = cfg
            .map(|c| {
                c.device
                    .iter()
                    .map(|d| DeviceState {
                        id: d.id.clone(),
                        model: d.model,
                        address: d.address,
                        values: Values::default(),
                        measured_at: None,
                        error: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Arc::new(Shared {
            state: Mutex::new(State { devices, revision: 0 }),
        })
    }

    pub fn snapshot(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    pub fn revision(&self) -> u64 {
        self.state.lock().unwrap().revision
    }

    /// Set values directly (fixtures).
    pub fn set(&self, id: &str, values: Values, at: DateTime<Utc>) {
        let mut s = self.state.lock().unwrap();
        if let Some(d) = s.devices.iter_mut().find(|d| d.id == id) {
            d.values = values;
            d.measured_at = Some(at);
            d.error = None;
            s.revision += 1;
        }
    }

    fn apply(&self, outcomes: &[Outcome], now: DateTime<Utc>) {
        let mut s = self.state.lock().unwrap();
        let mut changed = false;
        for (d, o) in s.devices.iter_mut().zip(outcomes) {
            match o {
                Outcome::Value(v) => {
                    d.values = *v;
                    d.measured_at = Some(now);
                    d.error = None;
                    changed = true;
                }
                Outcome::Failed(e) => {
                    let msg = e.to_string();
                    if d.error.as_ref() != Some(&msg) {
                        d.error = Some(msg);
                        changed = true;
                    }
                }
                Outcome::Pending | Outcome::Waiting => {}
            }
        }
        if changed {
            s.revision += 1;
        }
    }
}

/// Overall health reported to the repository.
#[derive(Debug, Clone, PartialEq)]
enum Health {
    AllOk,
    /// Some devices fail: shown as "retrying" with the error.
    Partial(String),
    AllFailed(String),
}

const HEARTBEAT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Reporter {
    last: Option<(Health, Instant)>,
}

impl Reporter {
    fn report<B: I2c>(&mut self, poller: &Poller<B>, repo: &Repository, now: Instant) {
        let failing: Vec<String> = poller
            .errors()
            .filter_map(|(d, e)| e.map(|e| format!("{}: {e}", d.id)))
            .collect();
        let total = poller.devices.len();
        let health = match failing.len() {
            0 => Health::AllOk,
            n if n == total => Health::AllFailed(failing.join(" / ")),
            _ => Health::Partial(failing.join(" / ")),
        };
        let changed = self.last.as_ref().is_none_or(|(h, _)| *h != health);
        let due = self
            .last
            .as_ref()
            .is_none_or(|(_, t)| now.duration_since(*t) >= HEARTBEAT);
        if !changed && !due {
            return;
        }
        if changed {
            match &health {
                Health::AllOk => tracing::info!("sensors: all {total} readable"),
                Health::Partial(m) | Health::AllFailed(m) => tracing::warn!("sensors: {m}"),
            }
        }
        let t = clock::now_utc();
        repo.update_status(SourceId::Sensors, |s| match &health {
            Health::AllOk => s.record_success(t),
            Health::Partial(m) => {
                s.record_success(t);
                s.record_failure(t, SourceError::new(ErrorKind::Device, m.clone()));
            }
            Health::AllFailed(m) => s.record_failure(t, SourceError::new(ErrorKind::Device, m.clone())),
        });
        self.last = Some((health, now));
    }
}

/// Start the polling thread for `[sensors]`.
pub fn spawn(cfg: SensorsConfig, shared: Arc<Shared>, repo: Arc<Repository>) -> std::io::Result<()> {
    std::thread::Builder::new().name("hd-sensors".into()).spawn(move || {
        let interval = Duration::from_secs(cfg.interval_seconds);
        let mut poller = Poller::for_config(&cfg);
        let mut reporter = Reporter::default();
        loop {
            let started = Instant::now();
            let outcomes = poller.cycle(started, &mut StdDelay);
            shared.apply(&outcomes, clock::now_utc());
            reporter.report(&poller, &repo, started);
            std::thread::sleep(interval.saturating_sub(started.elapsed()));
        }
    })?;
    Ok(())
}

/// Addresses that acknowledge a one-byte read (like `i2cdetect -r`).
/// An SHT3x with no pending measurement does not acknowledge reads, so it
/// may be missing from the list even when connected.
pub fn scan<B: I2c>(bus: &mut B) -> Vec<u8> {
    let mut b = [0u8];
    crate::config::I2C_ADDRESS_RANGE
        .filter(|&a| bus.read(a, &mut b).is_ok())
        .collect()
}

/// `--read-sensors`: list responding addresses, then poll for `duration`,
/// printing every value and error. Returns true when every device produced
/// at least one value.
pub fn probe(cfg: &SensorsConfig, duration: Duration, mut print: impl FnMut(String)) -> bool {
    match Bus::open(&cfg.bus) {
        Ok(mut bus) => {
            let found: Vec<String> = scan(&mut bus).iter().map(|a| format!("0x{a:02X}")).collect();
            print(format!(
                "応答したアドレス: {}",
                if found.is_empty() {
                    "なし".into()
                } else {
                    found.join(" ")
                }
            ));
        }
        Err(e) => {
            print(format!("{e}"));
            return false;
        }
    }
    let mut poller = Poller::for_config(cfg);
    let names: Vec<String> = poller
        .devices()
        .map(|d| format!("{} ({} 0x{:02X})", d.id, d.model.name(), d.address))
        .collect();
    let mut got = vec![false; names.len()];
    let interval = Duration::from_secs(cfg.interval_seconds);
    let start = Instant::now();
    while start.elapsed() < duration {
        let t = Instant::now();
        let secs = start.elapsed().as_secs_f32();
        for (i, o) in poller.cycle(t, &mut StdDelay).into_iter().enumerate() {
            match o {
                Outcome::Value(v) => {
                    got[i] = true;
                    print(format!("{secs:5.1}s  {}: {}", names[i], v.describe()));
                }
                Outcome::Failed(e) => print(format!("{secs:5.1}s  {}: エラー: {e}", names[i])),
                Outcome::Pending | Outcome::Waiting => {}
            }
        }
        std::thread::sleep(interval.saturating_sub(t.elapsed()));
    }
    for (name, ok) in names.iter().zip(&got) {
        print(format!(
            "{name}: {}",
            if *ok {
                "OK"
            } else {
                "値を取得できませんでした"
            }
        ));
    }
    got.iter().all(|g| *g)
}

#[cfg(test)]
pub(crate) mod testing {
    //! A scripted I2C bus for driver tests.

    use std::collections::VecDeque;

    use embedded_hal::delay::DelayNs;
    use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};

    pub struct NoDelay;

    impl DelayNs for NoDelay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    /// One device at `addr`; reads are answered from a queue, writes recorded.
    pub struct FakeBus {
        addr: u8,
        writes: Vec<Vec<u8>>,
        replies: VecDeque<Vec<u8>>,
        pub fail: bool,
    }

    impl FakeBus {
        pub fn new(addr: u8) -> Self {
            FakeBus {
                addr,
                writes: Vec::new(),
                replies: VecDeque::new(),
                fail: false,
            }
        }

        pub fn reply(&mut self, bytes: &[u8]) {
            self.replies.push_back(bytes.to_vec());
        }

        pub fn writes(&self) -> Vec<Vec<u8>> {
            self.writes.clone()
        }
    }

    impl ErrorType for FakeBus {
        type Error = ErrorKind;
    }

    impl I2c for FakeBus {
        fn transaction(&mut self, address: u8, ops: &mut [Operation<'_>]) -> Result<(), ErrorKind> {
            if address != self.addr || self.fail {
                return Err(ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address));
            }
            for op in ops {
                match op {
                    Operation::Write(b) => self.writes.push(b.to_vec()),
                    Operation::Read(buf) => {
                        let r = self.replies.pop_front().ok_or(ErrorKind::Other)?;
                        assert_eq!(r.len(), buf.len(), "scripted reply length");
                        buf.copy_from_slice(&r);
                    }
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{FakeBus, NoDelay};
    use super::*;

    fn cfg() -> SensorsConfig {
        SensorsConfig {
            device: vec![SensorDevice {
                id: "room".into(),
                model: SensorModel::Sht35,
                address: 0x44,
                pressure_from: None,
                auto_calibration: None,
            }],
            ..SensorsConfig::default()
        }
    }

    fn sht_reply(bus: &mut FakeBus) {
        bus.reply(&[crc::with_crc(0x6666), crc::with_crc(0x8000)].concat());
    }

    #[test]
    fn range_check_rejects_nonsense() {
        let v = Values {
            humidity: Some(130.0),
            ..Values::default()
        };
        assert!(matches!(v.check(), Err(SensorError::OutOfRange(_))));
        let v = Values {
            pressure: Some(1013.0),
            ..Values::default()
        };
        assert_eq!(v.check(), Ok(v));
        assert_eq!(v.describe(), "気圧 1013.00hPa");
    }

    #[test]
    fn poller_reads_backs_off_and_recovers() {
        let shared = Arc::new(Mutex::new(FakeBus::new(0x44)));
        let s2 = shared.clone();
        // The poller owns its bus; hand it a proxy to the shared fake.
        struct Proxy(Arc<Mutex<FakeBus>>);
        impl i2c::ErrorType for Proxy {
            type Error = i2c::ErrorKind;
        }
        impl I2c for Proxy {
            fn transaction(&mut self, a: u8, ops: &mut [i2c::Operation<'_>]) -> Result<(), i2c::ErrorKind> {
                self.0.lock().unwrap().transaction(a, ops)
            }
        }
        let mut p = Poller::new(&cfg(), Box::new(move || Ok(Proxy(s2.clone()))));
        let t0 = Instant::now();

        sht_reply(&mut shared.lock().unwrap());
        let out = p.cycle(t0, &mut NoDelay);
        assert!(matches!(out[0], Outcome::Value(_)), "{out:?}");

        // Unplugged: read failures, then re-init with growing backoff.
        shared.lock().unwrap().fail = true;
        for i in 1..=REINIT_AFTER {
            let out = p.cycle(t0 + Duration::from_secs(i as u64), &mut NoDelay);
            assert!(matches!(out[0], Outcome::Failed(SensorError::Bus(_))), "{out:?}");
        }
        let at = t0 + Duration::from_secs(REINIT_AFTER as u64);
        assert_eq!(p.cycle(at, &mut NoDelay), vec![Outcome::Waiting]);
        assert!(p.errors().next().unwrap().1.is_some());

        // Back: re-initialised after the backoff and read again.
        shared.lock().unwrap().fail = false;
        sht_reply(&mut shared.lock().unwrap());
        let out = p.cycle(at + Duration::from_secs(10), &mut NoDelay);
        assert!(matches!(out[0], Outcome::Value(_)), "{out:?}");
        assert!(p.errors().next().unwrap().1.is_none());
    }

    #[test]
    fn scan_lists_acknowledging_addresses() {
        let mut bus = FakeBus::new(0x62);
        bus.reply(&[0]);
        assert_eq!(scan(&mut bus), vec![0x62]);
    }

    #[test]
    fn bus_open_failure_reports_every_device() {
        let mut p: Poller<FakeBus> = Poller::new(&cfg(), Box::new(|| Err(SensorError::Open("no bus".into()))));
        let t0 = Instant::now();
        assert_eq!(
            p.cycle(t0, &mut NoDelay),
            vec![Outcome::Failed(SensorError::Open("no bus".into()))]
        );
        assert_eq!(p.cycle(t0, &mut NoDelay), vec![Outcome::Waiting], "backing off");
    }

    #[test]
    fn shared_state_tracks_values_and_errors() {
        let c = cfg();
        let shared = Shared::new(Some(&c));
        let now = clock::now_utc();
        let v = Values {
            temperature: Some(24.0),
            ..Values::default()
        };
        shared.apply(&[Outcome::Value(v)], now);
        assert_eq!(shared.revision(), 1);
        shared.apply(&[Outcome::Pending], now);
        assert_eq!(shared.revision(), 1, "no change, no revision");
        shared.apply(&[Outcome::Failed(SensorError::Crc)], now);
        let s = shared.snapshot();
        assert_eq!(s.device("room").unwrap().values, v, "last value kept on error");
        assert!(s.device("room").unwrap().error.is_some());
    }
}

//! The I2C bus named by `[sensors] bus`: `/dev/i2c-N` on Linux, or `"ftdi"`
//! (FT232H over USB, `usb-i2c` feature). Both are wrapped in one type so the
//! drivers and the polling loop are not generic over the platform.

use embedded_hal::i2c::{self, ErrorKind, ErrorType, I2c, Operation, SevenBitAddress};

use super::SensorError;

/// `bus` value selecting the FT232H adapter.
pub const FTDI: &str = "ftdi";

pub enum Bus {
    #[cfg(target_os = "linux")]
    Linux(linux_embedded_hal::I2cdev),
    #[cfg(feature = "usb-i2c")]
    Ftdi(ftdi_embedded_hal::I2c<ftdi_embedded_hal::ftdi::Device>),
}

impl Bus {
    pub fn open(spec: &str) -> Result<Bus, SensorError> {
        if spec == FTDI {
            return open_ftdi();
        }
        open_dev(spec)
    }
}

#[cfg(target_os = "linux")]
fn open_dev(path: &str) -> Result<Bus, SensorError> {
    linux_embedded_hal::I2cdev::new(path)
        .map(Bus::Linux)
        .map_err(|e| SensorError::Open(format!("{path}: {e}")))
}

#[cfg(not(target_os = "linux"))]
fn open_dev(path: &str) -> Result<Bus, SensorError> {
    Err(SensorError::Open(format!(
        "{path}: この OS では /dev/i2c を使えません（開発用には bus = \"{FTDI}\" と usb-i2c 機能）"
    )))
}

#[cfg(feature = "usb-i2c")]
fn open_ftdi() -> Result<Bus, SensorError> {
    use ftdi_embedded_hal::{FtHal, ftdi};
    // 0403:6014 = FT232H. 100 kHz (standard mode) suits every supported sensor.
    let device = ftdi::find_by_vid_pid(0x0403, 0x6014)
        .interface(ftdi::Interface::A)
        .open()
        .map_err(|e| SensorError::Open(format!("FT232H: {e}")))?;
    let hal = FtHal::init_default(device).map_err(|e| SensorError::Open(format!("FT232H: {e}")))?;
    hal.i2c()
        .map(Bus::Ftdi)
        .map_err(|e| SensorError::Open(format!("FT232H I2C: {e}")))
}

#[cfg(not(feature = "usb-i2c"))]
fn open_ftdi() -> Result<Bus, SensorError> {
    Err(SensorError::Open(
        "FT232H を使うには --features usb-i2c でビルドしてください".into(),
    ))
}

/// Platform error reduced to its embedded-hal kind (all the drivers need).
#[derive(Debug)]
pub struct BusError(ErrorKind);

impl i2c::Error for BusError {
    fn kind(&self) -> ErrorKind {
        self.0
    }
}

impl ErrorType for Bus {
    type Error = BusError;
}

impl I2c<SevenBitAddress> for Bus {
    // Without a platform bus the enum is empty and the parameters are unused.
    #[allow(unused_variables)]
    fn transaction(&mut self, address: u8, operations: &mut [Operation<'_>]) -> Result<(), BusError> {
        match *self {
            #[cfg(target_os = "linux")]
            Bus::Linux(ref mut b) => b
                .transaction(address, operations)
                .map_err(|e| BusError(i2c::Error::kind(&e))),
            #[cfg(feature = "usb-i2c")]
            Bus::Ftdi(ref mut b) => b
                .transaction(address, operations)
                .map_err(|e| BusError(i2c::Error::kind(&e))),
        }
    }
}

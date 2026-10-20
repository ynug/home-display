//! Sensirion CRC-8 (SHT3x, SCD4x): polynomial 0x31, init 0xFF, no reflection.
//! Every 16-bit word on the wire is followed by its CRC byte.

use super::SensorError;

pub fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0xFF;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x31 } else { crc << 1 };
        }
    }
    crc
}

/// `[hi, lo, crc]` for a command argument.
pub fn with_crc(word: u16) -> [u8; 3] {
    let [hi, lo] = word.to_be_bytes();
    [hi, lo, crc8(&[hi, lo])]
}

/// Split `N` words of `[hi, lo, crc]` and verify every CRC.
pub fn words<const N: usize>(buf: &[u8]) -> Result<[u16; N], SensorError> {
    debug_assert_eq!(buf.len(), N * 3);
    let mut out = [0u16; N];
    for (w, &[hi, lo, crc]) in out.iter_mut().zip(buf.as_chunks::<3>().0) {
        if crc8(&[hi, lo]) != crc {
            return Err(SensorError::Crc);
        }
        *w = u16::from_be_bytes([hi, lo]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datasheet_example() {
        // SHT3x / SCD4x datasheets: CRC(0xBEEF) = 0x92.
        assert_eq!(crc8(&[0xBE, 0xEF]), 0x92);
        assert_eq!(with_crc(0xBEEF), [0xBE, 0xEF, 0x92]);
    }

    #[test]
    fn words_check_every_crc() {
        let mut buf = [0u8; 6];
        buf[..3].copy_from_slice(&with_crc(0x1234));
        buf[3..].copy_from_slice(&with_crc(0xABCD));
        assert_eq!(words::<2>(&buf), Ok([0x1234, 0xABCD]));
        buf[5] ^= 1;
        assert_eq!(words::<2>(&buf), Err(SensorError::Crc));
    }
}

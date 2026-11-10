//! Byte-order helpers shared by the binary parsers.
//!
//! GameStream mixes both orders: the control channel and the pairing blobs it
//! hands around are big-endian, while video RTP headers are network order too,
//! but some length prefixes and the host's own structures are little-endian.
//! Making the order an explicit argument is cheap and stops a class of bug that
//! is otherwise invisible until packets fail to decode on a real TV.

/// Byte order of a multi-byte field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endian {
    /// Least-significant byte first (`x86_64`, most host structs).
    Little,
    /// Most-significant byte first (network order, most GameStream wire fields).
    Big,
}

/// Number of bytes a field of `size` occupies; a helper for offset arithmetic.
#[inline]
pub const fn size_of(size: usize) -> usize {
    size
}

macro_rules! endian_fns {
    ($ty:ty, $read_be:ident, $read_le:ident, $write_be:ident, $write_le:ident, $size:expr) => {
        #[doc = concat!("Read a `", stringify!($ty), "` in big-endian order from `bytes`.")]
        ///
        /// # Errors
        ///
        /// Returns `None` when `bytes` is shorter than the field.
        #[inline]
        pub fn $read_be(bytes: &[u8]) -> Option<$ty> {
            let raw: [u8; $size] = bytes.get(..$size)?.try_into().ok()?;
            Some(<$ty>::from_be_bytes(raw))
        }

        #[doc = concat!("Read a `", stringify!($ty), "` in little-endian order from `bytes`.")]
        ///
        /// # Errors
        ///
        /// Returns `None` when `bytes` is shorter than the field.
        #[inline]
        pub fn $read_le(bytes: &[u8]) -> Option<$ty> {
            let raw: [u8; $size] = bytes.get(..$size)?.try_into().ok()?;
            Some(<$ty>::from_le_bytes(raw))
        }

        #[doc = concat!("Write a `", stringify!($ty), "` in big-endian order into `out`.")]
        ///
        /// # Errors
        ///
        /// Returns `false` when `out` is too small.
        #[inline]
        pub fn $write_be(value: $ty, out: &mut [u8]) -> bool {
            let Some(slot) = out.get_mut(..$size) else {
                return false;
            };
            slot.copy_from_slice(&value.to_be_bytes());
            true
        }

        #[doc = concat!("Write a `", stringify!($ty), "` in little-endian order into `out`.")]
        ///
        /// # Errors
        ///
        /// Returns `false` when `out` is too small.
        #[inline]
        pub fn $write_le(value: $ty, out: &mut [u8]) -> bool {
            let Some(slot) = out.get_mut(..$size) else {
                return false;
            };
            slot.copy_from_slice(&value.to_le_bytes());
            true
        }
    };
}

endian_fns!(u16, read_u16_be, read_u16_le, write_u16_be, write_u16_le, 2);
endian_fns!(u32, read_u32_be, read_u32_le, write_u32_be, write_u32_le, 4);
endian_fns!(u64, read_u64_be, read_u64_le, write_u64_be, write_u64_le, 8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_and_little_disagree_for_multi_byte_fields() {
        let bytes = [0x01, 0x02, 0x03, 0x04];
        assert_eq!(read_u32_be(&bytes), Some(0x0102_0304));
        assert_eq!(read_u32_le(&bytes), Some(0x0403_0201));
    }

    #[test]
    fn short_slices_are_rejected_without_panicking() {
        assert_eq!(read_u16_be(&[0x01]), None);
        assert_eq!(read_u32_le(&[]), None);
        assert_eq!(read_u64_be(&[0; 7]), None);
    }

    #[test]
    fn writes_round_trip_both_orders() {
        let mut buf = [0u8; 8];
        assert!(write_u32_le(0xDEAD_BEEF, &mut buf[..4]));
        assert_eq!(read_u32_le(&buf[..4]), Some(0xDEAD_BEEF));
        assert!(write_u32_be(0xDEAD_BEEF, &mut buf[..4]));
        assert_eq!(read_u32_be(&buf[..4]), Some(0xDEAD_BEEF));
    }

    #[test]
    fn writes_report_small_buffers() {
        let mut buf = [0u8; 2];
        assert!(!write_u32_be(1, &mut buf));
        assert!(!write_u64_le(1, &mut buf));
    }
}

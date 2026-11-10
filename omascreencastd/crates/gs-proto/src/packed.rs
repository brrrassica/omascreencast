//! Fixed-layout, endian-explicit structs.
//!
//! GameStream's binary control packets are fixed-layout: every field sits at a
//! known offset and the layout is part of the protocol, not of the host's ABI.
//! [`packed_struct!`] generates a plain Rust struct plus `SIZE`, `parse`,
//! `serialize`, and `to_bytes`, reading each field with an explicit [`Endian`]
//! and refusing short buffers instead of panicking.
//!
//! The generated code never takes a raw pointer, so the workspace-wide
//! `unsafe_code = "forbid"` lint still holds.
//!
//! ```
//! use gs_proto::{packed_struct, Endian};
//!
//! packed_struct! {
//!     #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//!     pub struct Sample: Endian::Big {
//!         pub kind: u8,
//!         pub sequence: u16,
//!         pub timestamp: u32,
//!     }
//! }
//!
//! let packet = Sample { kind: 7, sequence: 0x0102, timestamp: 0x0A0B_0C0D };
//! let bytes = packet.to_bytes();
//! assert_eq!(bytes, [7, 0x01, 0x02, 0x0A, 0x0B, 0x0C, 0x0D]);
//! assert_eq!(Sample::parse(&bytes).unwrap(), packet);
//! ```

use crate::endian::Endian;

/// Failure while reading or writing a fixed-layout struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PackedError {
    /// The input ended before every field could be read.
    #[error("buffer truncated: needed {needed} bytes, had {available}")]
    Truncated {
        /// Total number of bytes the struct requires.
        needed: usize,
        /// Number of bytes actually supplied.
        available: usize,
    },
    /// The output buffer was smaller than the struct.
    #[error("output buffer too small: needed {needed} bytes, had {available}")]
    BufferTooSmall {
        /// Total number of bytes the struct requires.
        needed: usize,
        /// Number of bytes actually available in the slice.
        available: usize,
    },
}

/// A field that can be read from and written to a byte slice at a known offset.
///
/// Implemented for the unsigned integers and for byte arrays, which covers
/// every fixed-layout field in this protocol.
pub trait PackedField: Sized {
    /// Number of bytes the field occupies.
    const SIZE: usize;

    /// Read the field at `offset`, interpreting multi-byte values per `endian`.
    ///
    /// # Errors
    ///
    /// Returns [`PackedError::Truncated`] if the slice is too short.
    fn read(bytes: &[u8], offset: usize, endian: Endian) -> Result<Self, PackedError>;

    /// Write the field at `offset`, interpreting multi-byte values per `endian`.
    ///
    /// # Errors
    ///
    /// Returns [`PackedError::BufferTooSmall`] if the slice is too short.
    fn write(&self, out: &mut [u8], offset: usize, endian: Endian) -> Result<(), PackedError>;
}

fn check_read(len: usize, offset: usize, size: usize) -> Result<(), PackedError> {
    let needed = offset.saturating_add(size);
    if needed <= len {
        Ok(())
    } else {
        Err(PackedError::Truncated {
            needed,
            available: len,
        })
    }
}

macro_rules! impl_int_field {
    ($ty:ty, $size:expr, $read_be:path, $read_le:path) => {
        impl PackedField for $ty {
            const SIZE: usize = $size;

            fn read(bytes: &[u8], offset: usize, endian: Endian) -> Result<Self, PackedError> {
                check_read(bytes.len(), offset, Self::SIZE)?;
                let slice = &bytes[offset..offset + Self::SIZE];
                Ok(match endian {
                    Endian::Big => $read_be(slice),
                    Endian::Little => $read_le(slice),
                }
                .expect("length checked above"))
            }

            fn write(
                &self,
                out: &mut [u8],
                offset: usize,
                endian: Endian,
            ) -> Result<(), PackedError> {
                let end = offset
                    .checked_add(Self::SIZE)
                    .ok_or(PackedError::BufferTooSmall {
                        needed: Self::SIZE,
                        available: out.len(),
                    })?;
                let Some(slot) = out.get_mut(offset..end) else {
                    return Err(PackedError::BufferTooSmall {
                        needed: end,
                        available: out.len(),
                    });
                };
                let encoded = match endian {
                    Endian::Big => self.to_be_bytes(),
                    Endian::Little => self.to_le_bytes(),
                };
                slot.copy_from_slice(&encoded);
                Ok(())
            }
        }
    };
}

impl_int_field!(
    u16,
    2,
    crate::endian::read_u16_be,
    crate::endian::read_u16_le
);
impl_int_field!(
    u32,
    4,
    crate::endian::read_u32_be,
    crate::endian::read_u32_le
);
impl_int_field!(
    u64,
    8,
    crate::endian::read_u64_be,
    crate::endian::read_u64_le
);

impl PackedField for u8 {
    const SIZE: usize = 1;

    fn read(bytes: &[u8], offset: usize, _endian: Endian) -> Result<Self, PackedError> {
        bytes.get(offset).copied().ok_or(PackedError::Truncated {
            needed: offset.saturating_add(1),
            available: bytes.len(),
        })
    }

    fn write(&self, out: &mut [u8], offset: usize, _endian: Endian) -> Result<(), PackedError> {
        let Some(slot) = out.get_mut(offset) else {
            return Err(PackedError::BufferTooSmall {
                needed: offset.saturating_add(1),
                available: out.len(),
            });
        };
        *slot = *self;
        Ok(())
    }
}

impl<const N: usize> PackedField for [u8; N] {
    const SIZE: usize = N;

    fn read(bytes: &[u8], offset: usize, _endian: Endian) -> Result<Self, PackedError> {
        check_read(bytes.len(), offset, N)?;
        let mut value = [0u8; N];
        value.copy_from_slice(&bytes[offset..offset + N]);
        Ok(value)
    }

    fn write(&self, out: &mut [u8], offset: usize, _endian: Endian) -> Result<(), PackedError> {
        let Some(slot) = out.get_mut(offset..offset + N) else {
            return Err(PackedError::BufferTooSmall {
                needed: offset.saturating_add(N),
                available: out.len(),
            });
        };
        slot.copy_from_slice(self);
        Ok(())
    }
}

/// Define a fixed-layout struct with generated `SIZE`, `parse`, `serialize`, and
/// `to_bytes`.
///
/// Syntax:
///
/// ```text
/// packed_struct! {
///     #[derive(...)]
///     pub struct Name: Endian::Big {
///         pub field: u8,
///         pub other: [u8; 16],
///     }
/// }
/// ```
///
/// The endian token is `Endian::Big` or `Endian::Little` and applies to every
/// multi-byte field in the struct.
#[macro_export]
macro_rules! packed_struct {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident : $endian:ident::$order:ident {
            $(
                $(#[$field_meta:meta])*
                $fvis:vis $field:ident : $ty:ty
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            $(
                $(#[$field_meta])*
                $fvis $field : $ty,
            )*
        }

        impl $name {
            /// Total encoded size in bytes.
            pub const SIZE: usize = 0 $(+ <$ty as $crate::PackedField>::SIZE)*;

            /// Decode the struct from the start of `bytes`.
            ///
            /// # Errors
            ///
            /// Returns [`$crate::PackedError::Truncated`] when `bytes` is
            /// shorter than [`Self::SIZE`].
            pub fn parse(bytes: &[u8]) -> ::core::result::Result<Self, $crate::PackedError> {
                let endian = $crate::Endian::$order;
                let mut offset: usize = 0;
                $(
                    let $field = <$ty as $crate::PackedField>::read(bytes, offset, endian)?;
                    offset += <$ty as $crate::PackedField>::SIZE;
                )*
                let _ = offset;
                ::core::result::Result::Ok(Self { $($field),* })
            }

            /// Encode the struct into the start of `out`.
            ///
            /// # Errors
            ///
            /// Returns [`$crate::PackedError::BufferTooSmall`] when `out` is
            /// shorter than [`Self::SIZE`].
            pub fn serialize(&self, out: &mut [u8]) -> ::core::result::Result<(), $crate::PackedError> {
                let endian = $crate::Endian::$order;
                let mut offset: usize = 0;
                $(
                    $crate::PackedField::write(&self.$field, out, offset, endian)?;
                    offset += <$ty as $crate::PackedField>::SIZE;
                )*
                // The final increment is never read; consume it so the macro
                // does not emit `unused_assignments` for structs whose last
                // field has a non-zero size.
                let _ = offset;
                ::core::result::Result::Ok(())
            }

            /// Encode the struct into a freshly allocated buffer of [`Self::SIZE`].
            ///
            /// Takes `&self` rather than `self` on purpose: the macro must work
            /// for non-`Copy` structs too, and callers usually encode a borrowed
            /// value they still need.
            #[allow(clippy::wrong_self_convention)]
            pub fn to_bytes(&self) -> ::std::vec::Vec<u8> {
                let mut out = ::std::vec![0u8; Self::SIZE];
                self.serialize(&mut out)
                    .expect("buffer is exactly Self::SIZE bytes");
                out
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    packed_struct! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct BigSample: Endian::Big {
            pub kind: u8,
            pub sequence: u16,
            pub timestamp: u32,
            pub opaque: [u8; 3],
        }
    }

    packed_struct! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct LittleSample: Endian::Little {
            pub first: u16,
            pub second: u32,
            pub last: u64,
        }
    }

    #[test]
    fn size_is_the_sum_of_field_sizes() {
        assert_eq!(BigSample::SIZE, 1 + 2 + 4 + 3);
        assert_eq!(LittleSample::SIZE, 2 + 4 + 8);
    }

    #[test]
    fn big_endian_layout_is_exact() {
        let sample = BigSample {
            kind: 0x11,
            sequence: 0x2233,
            timestamp: 0x4455_6677,
            opaque: [0x88, 0x99, 0xAA],
        };
        assert_eq!(
            sample.to_bytes(),
            [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA]
        );
    }

    #[test]
    fn little_endian_layout_is_exact() {
        let sample = LittleSample {
            first: 0x0102,
            second: 0x0304_0506,
            last: 0x0708_090A_0B0C_0D0E,
        };
        let wire = sample.to_bytes();
        assert_eq!(
            wire,
            [
                0x02, 0x01, 0x06, 0x05, 0x04, 0x03, 0x0E, 0x0D, 0x0C, 0x0B, 0x0A, 0x09, 0x08, 0x07
            ]
        );
        assert_eq!(LittleSample::parse(&wire).unwrap(), sample);
    }

    #[test]
    fn parse_round_trips_extra_trailing_bytes_are_ignored() {
        let sample = BigSample {
            kind: 1,
            sequence: 2,
            timestamp: 3,
            opaque: [4, 5, 6],
        };
        let mut wire = sample.to_bytes();
        wire.extend_from_slice(&[0xFF; 4]);
        assert_eq!(BigSample::parse(&wire).unwrap(), sample);
    }

    #[test]
    fn truncated_input_is_an_error_not_a_panic() {
        let sample = BigSample {
            kind: 1,
            sequence: 2,
            timestamp: 3,
            opaque: [4, 5, 6],
        };
        let wire = sample.to_bytes();
        for cut in 0..wire.len() {
            assert!(matches!(
                BigSample::parse(&wire[..cut]),
                Err(PackedError::Truncated { .. })
            ));
        }
    }

    #[test]
    fn short_output_is_an_error_not_a_panic() {
        let sample = LittleSample {
            first: 1,
            second: 2,
            last: 3,
        };
        let mut out = [0u8; LittleSample::SIZE - 1];
        assert!(matches!(
            sample.serialize(&mut out),
            Err(PackedError::BufferTooSmall { .. })
        ));
    }
}

//! Encrypted RTSP (`rtspenc`) framing (M2-T2).
//!
//! When a client offers encrypted RTSP, GameStream wraps each RTSP message in
//! AES-GCM. The key material arrives on `/launch` as `rikey`/`rikeyid`; this
//! module turns that into a keyed, sequence-numbered cipher and frames messages
//! as `ciphertext || tag`.
//!
//! # Provisional, and honest about it
//!
//! The **byte-exact** key and IV derivation is a capture fact that M0-T5 pins,
//! exactly as the pairing KDF is pinned by the same run. Until then this module
//! implements a self-consistent construction behind a small interface:
//!
//! - the key is the 16-byte `rikey`;
//! - the nonce is the 32-bit sequence number big-endian in the first four bytes
//!   of a 12-byte block, the rest zero;
//! - the tag is the 16-byte GCM tag appended to the ciphertext.
//!
//! It round-trips and rejects tampering, which is what the golden-trace harness
//! needs to assert framing. It is **not** claimed to interoperate with a real
//! client until the capture lands. Nothing here is transcribed from Sunshine or
//! `moonlight-common-c`; see `THIRD_PARTY_NOTICES.md`.

use openssl::symm::Cipher;

/// Length of the AES-128 key, in bytes.
pub const KEY_LEN: usize = 16;

/// Length of the GCM authentication tag, in bytes.
pub const TAG_LEN: usize = 16;

/// Why an `rtspenc` operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RtspCryptoError {
    /// The key was not [`KEY_LEN`] bytes.
    #[error("the rtspenc key must be {KEY_LEN} bytes, got {0}")]
    KeyLength(usize),
    /// The key was not valid hexadecimal.
    #[error("the rtspenc key is not valid hex: {0}")]
    KeyHex(String),
    /// The ciphertext was shorter than the authentication tag.
    #[error("rtspenc ciphertext is shorter than the {TAG_LEN}-byte tag")]
    Truncated,
    /// The tag did not verify: the key is wrong or the message was tampered
    /// with.
    #[error("rtspenc authentication failed")]
    Authentication,
    /// The underlying cipher reported an error.
    #[error("rtspenc cipher error: {0}")]
    Cipher(String),
}

/// A 16-byte AES-128-GCM key for `rtspenc`.
///
/// The key is never printed: [`Debug`] redacts it so a log line cannot leak it.
#[derive(Clone, PartialEq, Eq)]
pub struct RtspKey {
    key: [u8; KEY_LEN],
}

impl RtspKey {
    /// Build a key from raw bytes, which must be exactly [`KEY_LEN`] long.
    ///
    /// # Errors
    ///
    /// [`RtspCryptoError::KeyLength`] when the slice is the wrong size.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RtspCryptoError> {
        let key: [u8; KEY_LEN] = bytes
            .try_into()
            .map_err(|_| RtspCryptoError::KeyLength(bytes.len()))?;
        Ok(Self { key })
    }

    /// Build a key from a hexadecimal string.
    ///
    /// # Errors
    ///
    /// [`RtspCryptoError::KeyHex`] when the string is not hex, or
    /// [`RtspCryptoError::KeyLength`] when it does not decode to [`KEY_LEN`]
    /// bytes.
    pub fn from_hex(hex: &str) -> Result<Self, RtspCryptoError> {
        let bytes = decode_hex(hex).ok_or_else(|| RtspCryptoError::KeyHex(hex.to_owned()))?;
        Self::from_bytes(&bytes)
    }

    fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.key
    }
}

impl std::fmt::Debug for RtspKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RtspKey(<redacted>)")
    }
}

/// One direction of an `rtspenc` channel.
///
/// Each direction has its own sequence space, so a bidirectional session uses
/// two ciphers: one for messages the host sends, one for messages it receives.
#[derive(Debug, Clone)]
pub struct RtspCipher {
    key: RtspKey,
    next_sequence: u32,
}

impl RtspCipher {
    /// Build a cipher starting at sequence zero.
    #[must_use]
    pub fn new(key: RtspKey) -> Self {
        Self {
            key,
            next_sequence: 0,
        }
    }

    /// The sequence number the next operation will use.
    #[must_use]
    pub fn sequence(&self) -> u32 {
        self.next_sequence
    }

    /// Override the sequence number, for a channel that starts mid-stream.
    pub fn set_sequence(&mut self, sequence: u32) {
        self.next_sequence = sequence;
    }

    /// Encrypt one message, advancing the sequence number.
    ///
    /// # Errors
    ///
    /// [`RtspCryptoError::Cipher`] when the cipher fails.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, RtspCryptoError> {
        let iv = derive_iv(self.next_sequence);
        let framed = aes_gcm_encrypt(self.key.as_bytes(), &iv, plaintext)?;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        Ok(framed)
    }

    /// Decrypt one message, advancing the sequence number.
    ///
    /// # Errors
    ///
    /// [`RtspCryptoError::Truncated`] when the input is too short, or
    /// [`RtspCryptoError::Authentication`] when the tag does not verify.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, RtspCryptoError> {
        let iv = derive_iv(self.next_sequence);
        let plaintext = aes_gcm_decrypt(self.key.as_bytes(), &iv, ciphertext)?;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        Ok(plaintext)
    }
}

/// Derive the 12-byte GCM nonce for a sequence number.
///
/// Twelve bytes is the standard GCM nonce length and the only length OpenSSL 3
/// accepts through the high-level `Crypter` API. See the module note: this is
/// provisional until M0-T5 pins the real derivation.
fn derive_iv(sequence: u32) -> [u8; 12] {
    let mut iv = [0u8; 12];
    iv[..4].copy_from_slice(&sequence.to_be_bytes());
    iv
}

/// Encrypt `plaintext`, returning `ciphertext || tag`.
///
/// Uses the high-level AEAD helper rather than a raw `Crypter`: OpenSSL 3 only
/// accepts a GCM nonce through the AEAD path, and the raw `Crypter` API fails
/// to set the IV length.
fn aes_gcm_encrypt(
    key: &[u8; KEY_LEN],
    iv: &[u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>, RtspCryptoError> {
    let mut tag = [0u8; TAG_LEN];
    let mut ciphertext = openssl::symm::encrypt_aead(
        Cipher::aes_128_gcm(),
        key,
        Some(iv),
        &[],
        plaintext,
        &mut tag,
    )
    .map_err(|error| RtspCryptoError::Cipher(error.to_string()))?;
    ciphertext.extend_from_slice(&tag);
    Ok(ciphertext)
}

/// Decrypt `ciphertext || tag`, verifying the tag.
fn aes_gcm_decrypt(
    key: &[u8; KEY_LEN],
    iv: &[u8; 12],
    ciphertext: &[u8],
) -> Result<Vec<u8>, RtspCryptoError> {
    if ciphertext.len() < TAG_LEN {
        return Err(RtspCryptoError::Truncated);
    }
    let (data, tag) = ciphertext.split_at(ciphertext.len() - TAG_LEN);
    openssl::symm::decrypt_aead(Cipher::aes_128_gcm(), key, Some(iv), &[], data, tag)
        .map_err(|_| RtspCryptoError::Authentication)
}

/// Decode a hexadecimal string, returning `None` on any non-hex character or an
/// odd length.
fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let chars: Vec<char> = hex.chars().collect();
    for pair in chars.chunks(2) {
        let high = pair[0].to_digit(16)?;
        let low = pair[1].to_digit(16)?;
        bytes.push(((high << 4) | low) as u8);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> RtspKey {
        RtspKey::from_bytes(&[0x11; KEY_LEN]).unwrap()
    }

    #[test]
    fn a_message_round_trips_through_encrypt_and_decrypt() {
        let mut sender = RtspCipher::new(key());
        let mut receiver = RtspCipher::new(key());
        let plaintext = b"DESCRIBE rtsp://host RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let framed = sender.encrypt(plaintext).unwrap();
        assert_ne!(framed, plaintext);
        assert_eq!(framed.len(), plaintext.len() + TAG_LEN);
        assert_eq!(receiver.decrypt(&framed).unwrap(), plaintext);
    }

    #[test]
    fn the_sequence_number_advances_per_message() {
        let mut cipher = RtspCipher::new(key());
        assert_eq!(cipher.sequence(), 0);
        cipher.encrypt(b"one").unwrap();
        assert_eq!(cipher.sequence(), 1);
        cipher.encrypt(b"two").unwrap();
        assert_eq!(cipher.sequence(), 2);
    }

    #[test]
    fn the_iv_is_deterministic_for_a_sequence_number() {
        // Same key, same sequence, same plaintext: identical framing. This is
        // what makes a golden-trace comparison meaningful.
        let mut first = RtspCipher::new(key());
        let mut second = RtspCipher::new(key());
        assert_eq!(
            first.encrypt(b"same").unwrap(),
            second.encrypt(b"same").unwrap()
        );
    }

    #[test]
    fn a_different_sequence_produces_different_framing() {
        let mut cipher = RtspCipher::new(key());
        let at_zero = cipher.encrypt(b"same").unwrap();
        let at_one = cipher.encrypt(b"same").unwrap();
        assert_ne!(at_zero, at_one);
    }

    #[test]
    fn a_wrong_key_fails_authentication() {
        let mut sender = RtspCipher::new(key());
        let mut receiver = RtspCipher::new(RtspKey::from_bytes(&[0x22; KEY_LEN]).unwrap());
        let framed = sender.encrypt(b"secret").unwrap();
        assert_eq!(
            receiver.decrypt(&framed),
            Err(RtspCryptoError::Authentication)
        );
    }

    #[test]
    fn tampering_with_the_ciphertext_fails_authentication() {
        let mut sender = RtspCipher::new(key());
        let mut receiver = RtspCipher::new(key());
        let mut framed = sender.encrypt(b"secret").unwrap();
        framed[0] ^= 0xff;
        assert_eq!(
            receiver.decrypt(&framed),
            Err(RtspCryptoError::Authentication)
        );
    }

    #[test]
    fn a_short_ciphertext_is_truncated() {
        let mut cipher = RtspCipher::new(key());
        assert_eq!(cipher.decrypt(&[0u8; 4]), Err(RtspCryptoError::Truncated));
    }

    #[test]
    fn key_length_is_validated() {
        assert_eq!(
            RtspKey::from_bytes(&[0u8; 8]),
            Err(RtspCryptoError::KeyLength(8))
        );
    }

    #[test]
    fn keys_parse_from_hex() {
        let key = RtspKey::from_hex("00112233445566778899aabbccddeeff").unwrap();
        assert_eq!(
            key.as_bytes(),
            &[
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
        assert!(matches!(
            RtspKey::from_hex("zz"),
            Err(RtspCryptoError::KeyHex(_))
        ));
        assert!(matches!(
            RtspKey::from_hex("abc"),
            Err(RtspCryptoError::KeyHex(_))
        ));
    }

    #[test]
    fn debug_redacts_the_key() {
        let rendered = format!("{:?}", key());
        assert!(rendered.contains("redacted"));
        assert!(!rendered.contains("11"));
    }
}

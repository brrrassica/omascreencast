//! The pairing XML documents, built on [`gs_proto::xml::Element`].
//!
//! Each document here is a **shape**, not a captured byte sequence. The element
//! and attribute names follow the documented GameStream exchange; the exact
//! bytes are pinned by the M0 trace corpus (M0-T5) and this module is where a
//! disagreement will be corrected. Building through the shared element type
//! keeps escaping correct and makes the shape testable.
//!
//! # Clean-room note
//!
//! Names come from public protocol documentation and captured traces, not from
//! a GPL implementation. See `THIRD_PARTY_NOTICES.md`.

use gs_proto::xml::{Element, XmlError};

/// Root element name for every pairing document.
pub const ROOT: &str = "root";

/// A hex-encoded byte string, as the protocol carries challenges.
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Decode a hex string, tolerating uppercase.
///
/// # Errors
///
/// Returns [`XmlError::Malformed`] for an odd length or a non-hex character.
pub fn hex_decode(value: &str) -> Result<Vec<u8>, XmlError> {
    let value = value.trim();
    if value.len() % 2 != 0 {
        return Err(XmlError::Malformed);
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16).map_err(|_| XmlError::Malformed)
        })
        .collect()
}

/// The host's certificate, served to an unpaired client.
#[must_use]
pub fn server_certificate(pem: &str) -> Element {
    Element::new(ROOT).child(Element::with_text("cert", pem))
}

/// The result of a pairing phase that carries no further data.
#[must_use]
pub fn status(ok: bool) -> Element {
    Element::new(ROOT).child(Element::with_text("paired", if ok { "1" } else { "0" }))
}

/// The client's challenge, if the client sent one in `client-challenge`.
#[must_use]
pub fn client_challenge(document: &Element) -> Option<Vec<u8>> {
    let challenge = document
        .find("client-challenge")
        .and_then(|element| element.find("challenge"))
        .or_else(|| document.find("challenge"))?;
    hex_decode(&challenge.text).ok()
}

/// The response to `/clientchallenge` and `/serverchallengeresp`.
///
/// Both phases carry a server challenge and the host's proof of the client's
/// challenge; a client polls the same response until pairing completes.
#[must_use]
pub fn challenge_response(challenge: &[u8], proof: &[u8]) -> Element {
    Element::new(ROOT).child(
        Element::new("challengeresp")
            .child(Element::with_text("challenge", hex_encode(challenge)))
            .child(Element::with_text("response", hex_encode(proof))),
    )
}

/// The client's proof of the server's challenge.
#[must_use]
pub fn client_pairing_secret(document: &Element) -> Option<Vec<u8>> {
    let secret = document
        .find("clientpairingsecret")
        .and_then(|element| element.find("secret"))
        .or_else(|| document.find("secret"))
        .or_else(|| document.find("response"))?;
    hex_decode(&secret.text).ok()
}

/// The client's certificate, if it sent one during pairing.
///
/// Some clients send the certificate in `/clientpairingsecret` rather than
/// presenting one on the TLS connection. The host accepts either source; the
/// certificate must still be a real one before a fingerprint can be trusted.
#[must_use]
pub fn client_certificate(document: &Element) -> Option<&str> {
    document
        .find("clientpairingsecret")
        .and_then(|element| element.find("cert"))
        .or_else(|| document.find("cert"))
        .map(|element| element.text.as_str())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = vec![0x00, 0x0f, 0xab, 0xff];
        let encoded = hex_encode(&bytes);
        assert_eq!(encoded, "000fabff");
        assert_eq!(hex_decode(&encoded).unwrap(), bytes);
        assert_eq!(hex_decode("000FABFF").unwrap(), bytes);
    }

    #[test]
    fn malformed_hex_is_rejected() {
        assert_eq!(hex_decode("abc"), Err(XmlError::Malformed));
        assert_eq!(hex_decode("zz"), Err(XmlError::Malformed));
        assert_eq!(hex_decode(""), Ok(Vec::new()));
    }

    #[test]
    fn server_certificate_carries_the_pem() {
        let document = server_certificate("-----BEGIN CERTIFICATE-----\nMIIB\n");
        let xml = document.to_xml().unwrap();
        let parsed = Element::parse(&xml).unwrap();
        assert!(
            parsed
                .child_text("cert")
                .unwrap()
                .contains("BEGIN CERTIFICATE")
        );
    }

    #[test]
    fn challenge_response_round_trips_through_xml() {
        let document = challenge_response(&[1, 2, 3], &[4, 5, 6]);
        let parsed = Element::parse(&document.to_xml().unwrap()).unwrap();
        let response = parsed.find("challengeresp").unwrap();
        assert_eq!(response.child_text("challenge"), Some("010203"));
        assert_eq!(response.child_text("response"), Some("040506"));
    }

    #[test]
    fn client_challenge_accepts_nested_and_flat_shapes() {
        let nested = Element::parse(
            "<root><client-challenge><challenge>0a0b</challenge></client-challenge></root>",
        )
        .unwrap();
        assert_eq!(client_challenge(&nested), Some(vec![0x0a, 0x0b]));

        let flat = Element::parse("<root><challenge>0c0d</challenge></root>").unwrap();
        assert_eq!(client_challenge(&flat), Some(vec![0x0c, 0x0d]));

        assert_eq!(client_challenge(&Element::new(ROOT)), None);
    }

    #[test]
    fn client_secret_and_certificate_are_optional() {
        let with = Element::parse(
            "<root><clientpairingsecret><secret>0f</secret><cert>PEM</cert></clientpairingsecret></root>",
        )
        .unwrap();
        assert_eq!(client_pairing_secret(&with), Some(vec![0x0f]));
        assert_eq!(client_certificate(&with), Some("PEM"));

        let empty = Element::new(ROOT);
        assert_eq!(client_pairing_secret(&empty), None);
        assert_eq!(client_certificate(&empty), None);
    }

    #[test]
    fn status_reports_paired_as_a_flag() {
        assert_eq!(
            status(true).to_xml().unwrap(),
            "<root><paired>1</paired></root>"
        );
        assert_eq!(
            status(false).to_xml().unwrap(),
            "<root><paired>0</paired></root>"
        );
    }
}

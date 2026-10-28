//! Lowercase hex for digests. sha2 0.11's output has no `LowerHex`, and
//! persisted digests and plan revisions must keep their exact form.

use sha2::{Digest, Sha256};

/// Lowercase hex, two digits per byte; mirrors `konnect_sexp`'s crate-internal
/// encoder, which writes lock names and journal paths.
pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_the_fips_180_2_vector() {
        // SHA-256("abc") from FIPS 180-2, appendix B.1; its 0x01, 0x03 and
        // 0x00 bytes need the leading zero.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

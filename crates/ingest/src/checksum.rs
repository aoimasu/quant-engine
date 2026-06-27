//! SHA-256 verification against Binance `.CHECKSUM` sidecars.
//!
//! Each dump file `X.zip` is published alongside `X.zip.CHECKSUM` whose content is
//! `"<sha256-hex>  <filename>"`. We recompute the digest of the downloaded bytes and compare.

use sha2::{Digest, Sha256};

/// Lowercase-hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Extract the expected digest from a `.CHECKSUM` file's text (the first whitespace-delimited
/// token, lowercased). Returns `None` if no 64-char hex token is present.
#[must_use]
pub fn parse_checksum_file(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?.to_ascii_lowercase();
    if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(token)
    } else {
        None
    }
}

/// Whether `bytes` matches the digest recorded in `checksum_text`.
#[must_use]
pub fn verify(bytes: &[u8], checksum_text: &str) -> bool {
    parse_checksum_file(checksum_text).is_some_and(|expected| expected == sha256_hex(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_answer() {
        // SHA-256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Empty input.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn parses_binance_checksum_format() {
        let digest = sha256_hex(b"payload");
        let line = format!("{digest}  BTCUSDT-5m-2020-01-07.zip\n");
        assert_eq!(parse_checksum_file(&line).as_deref(), Some(digest.as_str()));
        // Uppercase is normalised.
        assert_eq!(
            parse_checksum_file(&digest.to_ascii_uppercase()).as_deref(),
            Some(digest.as_str())
        );
        // Garbage → None.
        assert_eq!(parse_checksum_file("not-a-digest file"), None);
        assert_eq!(parse_checksum_file(""), None);
    }

    #[test]
    fn verify_accepts_match_and_rejects_mismatch() {
        let bytes = b"the actual file bytes";
        let good = format!("{}  file.zip", sha256_hex(bytes));
        assert!(verify(bytes, &good));
        assert!(!verify(b"different bytes", &good));
        assert!(!verify(bytes, "garbage"));
    }
}

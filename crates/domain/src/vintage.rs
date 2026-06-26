//! Vintage hash — the audit key tying an artefact to its reproducible inputs.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::DomainError;

/// A 64-character lowercase-hex SHA-256 digest.
///
/// This is the shape of `qe_config::Config::content_hash` and `qe_determinism::Lineage::id`; one
/// type gives the information firewall a single, validated audit key. Deserialisation re-validates
/// (via [`TryFrom<String>`]), so a malformed digest cannot enter the audit trail through serde.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct VintageHash(String);

impl TryFrom<String> for VintageHash {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        VintageHash::new(value)
    }
}

impl VintageHash {
    /// Validate a 64-char lowercase-hex digest.
    ///
    /// # Errors
    /// [`DomainError::InvalidVintageHash`] if the input is not exactly 64 characters of lowercase
    /// hexadecimal.
    pub fn new(hex: impl Into<String>) -> Result<Self, DomainError> {
        let hex = hex.into();
        if hex.len() != 64 {
            return Err(DomainError::InvalidVintageHash("must be 64 characters"));
        }
        if !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(DomainError::InvalidVintageHash(
                "must be lowercase hexadecimal",
            ));
        }
        Ok(VintageHash(hex))
    }

    /// The digest string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VintageHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn accepts_valid_digest() {
        assert_eq!(VintageHash::new(VALID).unwrap().as_str(), VALID);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(matches!(
            VintageHash::new("abc"),
            Err(DomainError::InvalidVintageHash("must be 64 characters"))
        ));
    }

    #[test]
    fn rejects_uppercase_and_non_hex() {
        let upper = VALID.to_ascii_uppercase();
        assert!(VintageHash::new(upper).is_err());
        let non_hex = "g".repeat(64);
        assert!(VintageHash::new(non_hex).is_err());
    }

    #[test]
    fn deserialize_rejects_malformed_digest() {
        assert!(serde_json::from_str::<VintageHash>("\"xyz\"").is_err());
        let valid_json = format!("\"{VALID}\"");
        assert_eq!(
            serde_json::from_str::<VintageHash>(&valid_json)
                .unwrap()
                .as_str(),
            VALID
        );
    }
}

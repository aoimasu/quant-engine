//! Signed session cookie (QE-256): an HMAC-SHA256-signed, tamper-evident token carrying the
//! authenticated email + an expiry.
//!
//! Format: `b64url(payload) "." b64url(HMAC-SHA256(payload_b64))`, where
//! `payload = "v1|<email>|<exp_epoch_secs>"`. The MAC is keyed by `QE_SESSION_SECRET`.
//!
//! **Verification uses [`hmac::Mac::verify_slice`], a constant-time comparison** — never `==` on the
//! MAC — so a forged cookie cannot be distinguished by timing. `mint_session_cookie` is `pub` so the
//! integration tests can authenticate through the *same* signing code the login handler uses.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Name of the signed session cookie.
pub const SESSION_COOKIE_NAME: &str = "qe_session";

/// Payload schema version — lets us evolve the cookie format without silently accepting old shapes.
const PAYLOAD_VERSION: &str = "v1";

/// A decoded, signature- and expiry-valid session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Authenticated (allowlisted) email.
    pub email: String,
    /// Expiry, epoch seconds.
    pub exp: u64,
}

/// Mint a signed session **cookie value** (the token, not the full `Set-Cookie` header) for `email`
/// expiring at `exp_secs` (epoch seconds), signed with `secret`.
///
/// `pub` so integration tests can mint a valid session via the exact production signing path rather
/// than a parallel implementation that could drift.
pub fn mint_session_cookie(secret: &[u8], email: &str, exp_secs: u64) -> String {
    let payload = format!("{PAYLOAD_VERSION}|{email}|{exp_secs}");
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(sign(secret, payload_b64.as_bytes()));
    format!("{payload_b64}.{sig_b64}")
}

/// Verify a session cookie `token` against `secret` at wall-clock `now` (epoch seconds).
///
/// Returns the decoded [`Session`] only if the MAC verifies (constant-time) **and** the token is not
/// yet expired; any structural, signature, or expiry failure yields `None` (⇒ `401` upstream).
pub fn verify_session_cookie(secret: &[u8], token: &str, now: u64) -> Option<Session> {
    let (payload_b64, sig_b64) = token.split_once('.')?;

    // Signature first, constant-time — reject before trusting any payload bytes.
    let sig = URL_SAFE_NO_PAD.decode(sig_b64).ok()?;
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts a key of any length");
    mac.update(payload_b64.as_bytes());
    mac.verify_slice(&sig).ok()?;

    let payload = String::from_utf8(URL_SAFE_NO_PAD.decode(payload_b64).ok()?).ok()?;
    // `rsplit_once` on the final `|` isolates `exp`, so an email containing `|` cannot corrupt parsing.
    let rest = payload.strip_prefix(&format!("{PAYLOAD_VERSION}|"))?;
    let (email, exp_str) = rest.rsplit_once('|')?;
    let exp: u64 = exp_str.parse().ok()?;
    if email.is_empty() || now >= exp {
        return None;
    }
    Some(Session {
        email: email.to_owned(),
        exp,
    })
}

/// HMAC-SHA256 of `msg` under `secret`.
fn sign(secret: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts a key of any length");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-session-secret-0123456789";

    #[test]
    fn sign_then_verify_round_trips() {
        let token = mint_session_cookie(SECRET, "admin@example.com", 2_000);
        let s = verify_session_cookie(SECRET, &token, 1_000).expect("valid");
        assert_eq!(s.email, "admin@example.com");
        assert_eq!(s.exp, 2_000);
    }

    #[test]
    fn expired_token_is_rejected() {
        let token = mint_session_cookie(SECRET, "admin@example.com", 1_000);
        // now == exp and now > exp both reject.
        assert!(verify_session_cookie(SECRET, &token, 1_000).is_none());
        assert!(verify_session_cookie(SECRET, &token, 1_001).is_none());
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let token = mint_session_cookie(SECRET, "admin@example.com", 2_000);
        assert!(verify_session_cookie(b"a-different-secret", &token, 1_000).is_none());
    }

    #[test]
    fn tampered_payload_is_rejected() {
        // Forge a cookie for a different email but keep the original signature.
        let token = mint_session_cookie(SECRET, "admin@example.com", 2_000);
        let sig = token.split_once('.').unwrap().1;
        let forged_payload = URL_SAFE_NO_PAD.encode(b"v1|attacker@evil.com|2000");
        let forged = format!("{forged_payload}.{sig}");
        assert!(verify_session_cookie(SECRET, &forged, 1_000).is_none());
    }

    #[test]
    fn tampered_mac_is_rejected() {
        let token = mint_session_cookie(SECRET, "admin@example.com", 2_000);
        let (payload, _) = token.split_once('.').unwrap();
        let bad = format!("{payload}.{}", URL_SAFE_NO_PAD.encode(b"not-a-real-mac"));
        assert!(verify_session_cookie(SECRET, &bad, 1_000).is_none());
    }

    #[test]
    fn structurally_broken_tokens_are_rejected() {
        for bad in ["", "no-dot", "a.b.c", ".", "abc.", ".abc"] {
            assert!(
                verify_session_cookie(SECRET, bad, 1_000).is_none(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn email_containing_pipe_does_not_corrupt_parsing() {
        // Not a real Google email, but proves `rsplit_once` isolates exp robustly.
        let token = mint_session_cookie(SECRET, "weird|name@example.com", 2_000);
        let s = verify_session_cookie(SECRET, &token, 1_000).expect("valid");
        assert_eq!(s.email, "weird|name@example.com");
        assert_eq!(s.exp, 2_000);
    }
}

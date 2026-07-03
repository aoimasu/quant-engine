//! Real Google ID-token verifier (QE-256), behind the default-off `http` feature.
//!
//! Two blocking `ureq` calls (native-tls / system trust store, matching QE-253's dependency choice —
//! it avoids `ring`'s non-allowlisted licence and the `rsa` crate's RUSTSEC-2023-0071 Marvin
//! advisory):
//!  1. **token exchange** — POST the token endpoint (`code` → `id_token`);
//!  2. **validation** — GET `tokeninfo?id_token=…`, which performs the **signature + expiry**
//!     verification server-side and returns the decoded claims.
//!
//! Delegating signature verification to `tokeninfo` (rather than a local JWKS/RS256 verify) is a
//! deliberate, documented trade-off forced by the dependency policy (no `ring`, no `rsa`); a local
//! JWKS verify is a follow-up. The claim *policy* (`aud`/`iss`/`exp`/`email_verified`) is enforced by
//! [`super::check_claims`] regardless of which verifier produced the claims.

use serde::Deserialize;

use super::{GoogleClaims, IdTokenVerifier, VerifyError};

/// A verifier that talks to Google over the network.
pub struct GoogleOidcVerifier {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    token_endpoint: String,
    tokeninfo_endpoint: String,
}

impl GoogleOidcVerifier {
    /// Build from the resolved [`super::AuthConfig`] fields.
    pub fn new(config: &super::AuthConfig) -> Self {
        Self {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            redirect_uri: config.redirect_uri.clone(),
            token_endpoint: config.token_endpoint.clone(),
            tokeninfo_endpoint: config.tokeninfo_endpoint.clone(),
        }
    }
}

/// The token-endpoint response (we only need the ID token).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

/// The tokeninfo response. Google returns `email_verified` and `exp` as **strings**.
#[derive(Debug, Deserialize)]
struct TokenInfo {
    email: Option<String>,
    email_verified: Option<String>,
    aud: Option<String>,
    iss: Option<String>,
    exp: Option<String>,
}

impl IdTokenVerifier for GoogleOidcVerifier {
    fn verify(&self, code: &str) -> Result<GoogleClaims, VerifyError> {
        // 1) Exchange the authorization code for tokens.
        let token_resp = ureq::post(&self.token_endpoint)
            .send_form(&[
                ("code", code),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("redirect_uri", &self.redirect_uri),
                ("grant_type", "authorization_code"),
            ])
            .map_err(|e| VerifyError::Upstream(format!("token exchange: {e}")))?
            .into_string()
            .map_err(|e| VerifyError::Upstream(format!("token body: {e}")))?;
        let token: TokenResponse = serde_json::from_str(&token_resp)
            .map_err(|e| VerifyError::Malformed(format!("token json: {e}")))?;
        let id_token = token
            .id_token
            .ok_or_else(|| VerifyError::Malformed("no id_token in token response".to_owned()))?;

        // 2) Validate the ID token via tokeninfo (signature + expiry checked server-side). A 4xx here
        //    surfaces as an `Upstream` error ⇒ the caller treats it as "not signed in".
        let info_resp = ureq::get(&self.tokeninfo_endpoint)
            .query("id_token", &id_token)
            .call()
            .map_err(|e| VerifyError::Upstream(format!("tokeninfo: {e}")))?
            .into_string()
            .map_err(|e| VerifyError::Upstream(format!("tokeninfo body: {e}")))?;
        let info: TokenInfo = serde_json::from_str(&info_resp)
            .map_err(|e| VerifyError::Malformed(format!("tokeninfo json: {e}")))?;

        let email = info
            .email
            .ok_or_else(|| VerifyError::Malformed("no email in tokeninfo".to_owned()))?;
        let email_verified = info
            .email_verified
            .as_deref()
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let aud = info
            .aud
            .ok_or_else(|| VerifyError::Malformed("no aud in tokeninfo".to_owned()))?;
        let iss = info
            .iss
            .ok_or_else(|| VerifyError::Malformed("no iss in tokeninfo".to_owned()))?;
        let exp = info
            .exp
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| VerifyError::Malformed("no/!parseable exp in tokeninfo".to_owned()))?;

        Ok(GoogleClaims {
            email,
            email_verified,
            aud,
            iss,
            exp,
        })
    }
}

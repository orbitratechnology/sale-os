//! Compact JWTs signed with Ed25519 (ADR-013: EdDSA). Built on `ed25519-dalek` directly:
//! the header must say `EdDSA` exactly (no algorithm is taken from the token), the signature
//! is verified strictly, and the issuer, kind (`typ` claim) and expiry are checked.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

const HEADER: &str = r#"{"alg":"EdDSA","typ":"JWT"}"#;
/// Seconds of clock difference tolerated for `iat`.
const SKEW: i64 = 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The account (user tokens) or the device (device tokens).
    pub sub: String,
    /// `user` or `device`: a token of one kind is never accepted as the other.
    pub kind: String,
    pub iss: String,
    pub iat: i64,
    pub exp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub register_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("malformed token")]
    Malformed,
    #[error("unsupported algorithm")]
    Algorithm,
    #[error("bad signature")]
    Signature,
    #[error("wrong issuer or kind")]
    WrongKind,
    #[error("expired")]
    Expired,
}

pub struct TokenSigner {
    key: SigningKey,
    verifying: VerifyingKey,
    issuer: String,
}

impl TokenSigner {
    /// From the 32-byte private seed (`POS_JWT_SIGNING_KEY`).
    pub fn from_seed(seed: &[u8; 32], issuer: &str) -> Self {
        let key = SigningKey::from_bytes(seed);
        Self {
            verifying: key.verifying_key(),
            key,
            issuer: issuer.to_owned(),
        }
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn sign(&self, claims: &Claims) -> String {
        let payload = serde_json::to_vec(claims).expect("claims serialize");
        let signing_input = format!("{}.{}", B64.encode(HEADER), B64.encode(payload));
        let sig = self.key.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", B64.encode(sig.to_bytes()))
    }

    /// The claims of a valid, unexpired token of `kind` issued by this service.
    pub fn verify(&self, token: &str, kind: &str, now: i64) -> Result<Claims, TokenError> {
        let mut parts = token.split('.');
        let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(TokenError::Malformed);
        };
        let header: serde_json::Value =
            serde_json::from_slice(&B64.decode(h).map_err(|_| TokenError::Malformed)?)
                .map_err(|_| TokenError::Malformed)?;
        if header.get("alg").and_then(|a| a.as_str()) != Some("EdDSA") {
            return Err(TokenError::Algorithm);
        }
        let sig: [u8; 64] = B64
            .decode(s)
            .map_err(|_| TokenError::Malformed)?
            .try_into()
            .map_err(|_| TokenError::Malformed)?;
        let signing_input = &token[..h.len() + 1 + p.len()];
        self.verifying
            .verify_strict(signing_input.as_bytes(), &Signature::from_bytes(&sig))
            .map_err(|_| TokenError::Signature)?;
        let claims: Claims = serde_json::from_slice(&B64.decode(p).map_err(|_| TokenError::Malformed)?)
            .map_err(|_| TokenError::Malformed)?;
        if claims.iss != self.issuer || claims.kind != kind {
            return Err(TokenError::WrongKind);
        }
        if claims.exp <= now || claims.iat > now + SKEW {
            return Err(TokenError::Expired);
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> TokenSigner {
        TokenSigner::from_seed(&[7; 32], "sale-os-test")
    }

    fn claims(kind: &str, exp: i64) -> Claims {
        Claims {
            sub: "acct".into(),
            kind: kind.into(),
            iss: "sale-os-test".into(),
            iat: 1_000,
            exp,
            business_id: None,
            register_id: None,
            device_id: None,
        }
    }

    #[test]
    fn signs_and_verifies() {
        let s = signer();
        let t = s.sign(&claims("user", 2_000));
        assert_eq!(s.verify(&t, "user", 1_500).unwrap().sub, "acct");
        assert_eq!(s.verify(&t, "user", 2_000), Err(TokenError::Expired));
        assert_eq!(s.verify(&t, "device", 1_500), Err(TokenError::WrongKind));
    }

    #[test]
    fn rejects_tampering_other_keys_and_other_algorithms() {
        let s = signer();
        let t = s.sign(&claims("user", 2_000));
        // Another key's signature.
        let other = TokenSigner::from_seed(&[8; 32], "sale-os-test");
        assert_eq!(other.verify(&t, "user", 1_500), Err(TokenError::Signature));
        // A changed payload (another subject) keeps the old signature: refused.
        let parts: Vec<&str> = t.split('.').collect();
        let mut forged = claims("user", 2_000);
        forged.sub = "someone-else".into();
        let tampered = format!(
            "{}.{}.{}",
            parts[0],
            B64.encode(serde_json::to_vec(&forged).unwrap()),
            parts[2]
        );
        assert_eq!(s.verify(&tampered, "user", 1_500), Err(TokenError::Signature));
        // "alg: none" and HS256 headers are refused before anything else.
        for alg in ["none", "HS256"] {
            let h = B64.encode(format!(r#"{{"alg":"{alg}","typ":"JWT"}}"#));
            let t2 = format!("{h}.{}.{}", parts[1], parts[2]);
            assert_eq!(s.verify(&t2, "user", 1_500), Err(TokenError::Algorithm));
        }
        assert_eq!(s.verify("a.b", "user", 1_500), Err(TokenError::Malformed));
    }
}

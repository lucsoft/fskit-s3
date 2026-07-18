//! Minimal AWS Signature Version 4 for S3 GET/HEAD requests.
//!
//! Only what a read-only filesystem needs: sign a request that has no body
//! (payload hash is the SHA-256 of the empty string). Kept dependency-light —
//! just RustCrypto's `sha2`/`hmac` — so there's no async or SDK weight in the
//! extension. The signing math is service-agnostic; the S3-specific parts are
//! only the canonical URI and query building in `s3.rs`.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// SHA-256 of the empty string — the payload hash for any body-less request.
pub const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Static credentials. A session token is included as `x-amz-security-token`
/// when present (STS / assumed-role credentials).
#[derive(Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// Hex-encoded SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Derive the SigV4 signing key: HMAC chain over date → region → service →
/// `aws4_request`, seeded with `"AWS4" + secret`.
pub fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    hmac(&k_service, b"aws4_request")
}

/// The pieces of a request that SigV4 signs. Headers must already be the exact
/// set sent on the wire (lowercased names are produced here).
pub struct CanonicalRequest<'a> {
    pub method: &'a str,
    /// URI-encoded path, `/`-separated, e.g. `/photos/a%20b.jpg`.
    pub canonical_uri: &'a str,
    /// Already-canonical (sorted, encoded) query string, or empty.
    pub canonical_query: &'a str,
    /// `(name, value)` pairs; names lowercased, sorted by the caller.
    pub headers: &'a [(String, String)],
    pub payload_sha256: &'a str,
}

impl CanonicalRequest<'_> {
    fn signed_headers(&self) -> String {
        self.headers
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(";")
    }

    fn canonical(&self) -> String {
        let headers = self
            .headers
            .iter()
            .map(|(n, v)| format!("{n}:{}\n", v.trim()))
            .collect::<String>();
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            self.method,
            self.canonical_uri,
            self.canonical_query,
            headers,
            self.signed_headers(),
            self.payload_sha256,
        )
    }
}

/// Compute the `Authorization` header value for a request.
///
/// `amz_date` is `YYYYMMDDTHHMMSSZ`; `date` is its `YYYYMMDD` prefix.
pub fn authorization_header(
    creds: &Credentials,
    req: &CanonicalRequest,
    region: &str,
    service: &str,
    amz_date: &str,
    date: &str,
) -> String {
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(req.canonical().as_bytes())
    );
    let key = signing_key(&creds.secret_access_key, date, region, service);
    let signature = hex::encode(hmac(&key, string_to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={}, Signature={signature}",
        creds.access_key_id,
        req.signed_headers(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_payload_hash_is_known_constant() {
        assert_eq!(sha256_hex(b""), EMPTY_PAYLOAD_SHA256);
    }

    // RFC 4231 HMAC-SHA256 Test Case 1 — pins the HMAC wiring to a published
    // vector so a wrong key/order would fail here, not silently against S3.
    #[test]
    fn hmac_matches_rfc4231() {
        let key = [0x0bu8; 20];
        let got = hex::encode(hmac(&key, b"Hi There"));
        assert_eq!(
            got,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    // The signing key is deterministic; this locks the HMAC chain order
    // (date→region→service→aws4_request) against AWS's documented example
    // credentials so a reordering regression is caught offline.
    #[test]
    fn signing_key_is_stable() {
        let key = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "iam",
        );
        assert_eq!(
            hex::encode(&key),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }
}

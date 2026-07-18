//! S3 client + [`StorageBackend`] implementation.
//!
//! Read-only for now: `ListObjectsV2` (with `delimiter=/` so the flat key space
//! comes back already grouped into files + common prefixes) and ranged
//! `GetObject`. Works against real S3 and S3-compatible endpoints (MinIO,
//! LocalStack, R2) via `path_style` + a custom `endpoint`.

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use fskit_s3_core::{path, Entry, StorageBackend, StorageError};

use crate::sigv4::{self, CanonicalRequest, Credentials, EMPTY_PAYLOAD_SHA256};

const SERVICE: &str = "s3";

/// Where the bucket lives and how to address it.
#[derive(Clone)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    /// Host[:port] of the endpoint, e.g. `s3.us-east-1.amazonaws.com` or
    /// `localhost:9000`.
    pub endpoint: String,
    /// `https` (default) or `http` (local test endpoints).
    pub scheme: String,
    /// Path-style addressing (`/{bucket}/{key}`) instead of virtual-hosted
    /// (`{bucket}.{endpoint}`). Required by MinIO/LocalStack.
    pub path_style: bool,
    pub creds: Credentials,
}

impl S3Config {
    /// AWS defaults for a bucket in `region`: `https`, virtual-hosted,
    /// `s3.{region}.amazonaws.com`.
    pub fn aws(bucket: impl Into<String>, region: impl Into<String>, creds: Credentials) -> Self {
        let region = region.into();
        S3Config {
            endpoint: format!("s3.{region}.amazonaws.com"),
            bucket: bucket.into(),
            region,
            scheme: "https".into(),
            path_style: false,
            creds,
        }
    }
}

/// A read-only S3-backed store.
pub struct S3Backend {
    cfg: S3Config,
    agent: ureq::Agent,
}

impl S3Backend {
    pub fn new(cfg: S3Config) -> Self {
        S3Backend { cfg, agent: ureq::Agent::new() }
    }

    fn host(&self) -> String {
        if self.cfg.path_style {
            self.cfg.endpoint.clone()
        } else {
            format!("{}.{}", self.cfg.bucket, self.cfg.endpoint)
        }
    }

    /// Canonical (URI-encoded) request path for a key. Path-style prepends the
    /// bucket; virtual-hosted does not (the bucket is in the host).
    fn canonical_uri(&self, key: &str) -> String {
        let encoded = encode_path(key);
        if self.cfg.path_style {
            format!("/{}/{}", self.cfg.bucket, encoded.trim_start_matches('/'))
        } else {
            format!("/{}", encoded.trim_start_matches('/'))
        }
    }

    fn url(&self, canonical_uri: &str, query: &str) -> String {
        let q = if query.is_empty() { String::new() } else { format!("?{query}") };
        format!("{}://{}{canonical_uri}{q}", self.cfg.scheme, self.host())
    }

    /// Issue a signed GET/HEAD and return `(status, body)`. `range` signs
    /// nothing extra (Range is left unsigned, which S3 accepts).
    fn send(
        &self,
        method: &str,
        canonical_uri: &str,
        query: &str,
        range: Option<(u64, u64)>,
    ) -> Result<(u16, Vec<u8>), StorageError> {
        let (amz_date, date) = now_amz();
        let host = self.host();

        let mut signed: Vec<(String, String)> = vec![
            ("host".into(), host.clone()),
            ("x-amz-content-sha256".into(), EMPTY_PAYLOAD_SHA256.into()),
            ("x-amz-date".into(), amz_date.clone()),
        ];
        if let Some(tok) = &self.cfg.creds.session_token {
            signed.push(("x-amz-security-token".into(), tok.clone()));
        }
        signed.sort_by(|a, b| a.0.cmp(&b.0));

        let creq = CanonicalRequest {
            method,
            canonical_uri,
            canonical_query: query,
            headers: &signed,
            payload_sha256: EMPTY_PAYLOAD_SHA256,
        };
        let auth = sigv4::authorization_header(
            &self.cfg.creds,
            &creq,
            &self.cfg.region,
            SERVICE,
            &amz_date,
            &date,
        );

        let mut req = self
            .agent
            .request(method, &self.url(canonical_uri, query))
            .set("host", &host)
            .set("x-amz-content-sha256", EMPTY_PAYLOAD_SHA256)
            .set("x-amz-date", &amz_date)
            .set("authorization", &auth);
        if let Some(tok) = &self.cfg.creds.session_token {
            req = req.set("x-amz-security-token", tok);
        }
        if let Some((start, end)) = range {
            req = req.set("range", &format!("bytes={start}-{end}"));
        }

        match req.call() {
            Ok(resp) => {
                let status = resp.status();
                let mut body = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut body)
                    .map_err(|e| StorageError::Backend(format!("read body: {e}")))?;
                Ok((status, body))
            }
            Err(ureq::Error::Status(code, _)) => Ok((code, Vec::new())),
            Err(e) => Err(StorageError::Backend(format!("http: {e}"))),
        }
    }

    fn list_objects_v2(&self, prefix: &str) -> Result<Listing, StorageError> {
        // delimiter=/ groups the flat key space into files (Contents) and
        // subdirectories (CommonPrefixes) at this level.
        let query = format!(
            "delimiter={}&list-type=2&prefix={}",
            encode_component("/"),
            encode_component(prefix),
        );
        let base = if self.cfg.path_style {
            format!("/{}", self.cfg.bucket)
        } else {
            "/".to_string()
        };
        let (status, body) = self.send("GET", &base, &query, None)?;
        if status == 404 {
            return Err(StorageError::NotFound);
        }
        if !(200..300).contains(&status) {
            return Err(StorageError::Backend(format!("ListObjectsV2 status {status}")));
        }
        Ok(parse_listing(&String::from_utf8_lossy(&body), prefix))
    }
}

/// Files and subdirectories at one level of the bucket.
struct Listing {
    files: Vec<(String, u64)>, // (basename, size)
    dirs: Vec<String>,         // basenames
}

impl StorageBackend for S3Backend {
    fn list(&self, dir: &str) -> Result<Vec<Entry>, StorageError> {
        let dir = path::normalize(dir);
        let prefix = path::to_key(&dir, true);
        let listing = self.list_objects_v2(&prefix)?;

        // An empty listing at a non-root path means the prefix doesn't exist.
        if prefix.is_empty() == false && listing.files.is_empty() && listing.dirs.is_empty() {
            return Err(StorageError::NotFound);
        }

        let mut out: Vec<Entry> = listing.dirs.into_iter().map(Entry::dir).collect();
        out.extend(listing.files.into_iter().map(|(n, s)| Entry::file(n, s)));
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn stat(&self, p: &str) -> Result<Entry, StorageError> {
        let norm = path::normalize(p);
        if norm == "/" {
            return Ok(Entry::dir(""));
        }
        let name = path::basename(&norm).to_string();

        // HEAD the exact key: present ⇒ file.
        let (status, _) = self.send("HEAD", &self.canonical_uri(&path::to_key(&norm, false)), "", None)?;
        if (200..300).contains(&status) {
            // HEAD gives Content-Length, but ureq drops the body/headers here;
            // fetch size via a 1-object listing of the parent instead. Cheap
            // enough for stat and keeps this method single-purpose.
            let listing = self.list_objects_v2(&path::to_key(&norm, false))?;
            let size = listing.files.iter().find(|(n, _)| *n == name).map(|(_, s)| *s).unwrap_or(0);
            return Ok(Entry::file(name, size));
        }

        // Otherwise, is it a directory prefix? List with a trailing slash.
        let listing = self.list_objects_v2(&path::to_key(&norm, true))?;
        if !listing.files.is_empty() || !listing.dirs.is_empty() {
            return Ok(Entry::dir(name));
        }
        Err(StorageError::NotFound)
    }

    fn read(&self, p: &str, offset: u64, len: usize) -> Result<Vec<u8>, StorageError> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let norm = path::normalize(p);
        let key = path::to_key(&norm, false);
        let end = offset + len as u64 - 1;
        let (status, body) = self.send("GET", &self.canonical_uri(&key), "", Some((offset, end)))?;
        match status {
            200 | 206 => Ok(body),
            404 => Err(StorageError::NotFound),
            416 => Ok(Vec::new()), // requested range past EOF
            other => Err(StorageError::Backend(format!("GetObject status {other}"))),
        }
    }
}

// ---- ListObjectsV2 XML (minimal, tag-scanning) --------------------------------

/// Extract `<Contents><Key>/<Size>` and `<CommonPrefixes><Prefix>` from a
/// ListObjectsV2 response, returning basenames relative to `prefix`.
fn parse_listing(xml: &str, prefix: &str) -> Listing {
    let mut files = Vec::new();
    let mut dirs = Vec::new();

    for block in between_all(xml, "<Contents>", "</Contents>") {
        let key = inner(block, "<Key>", "</Key>").unwrap_or_default();
        if key == prefix || key.ends_with('/') {
            continue; // the prefix placeholder or an explicit dir marker
        }
        if let Some(rest) = key.strip_prefix(prefix) {
            if !rest.is_empty() && !rest.contains('/') {
                let size = inner(block, "<Size>", "</Size>")
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(0);
                files.push((rest.to_string(), size));
            }
        }
    }

    for block in between_all(xml, "<CommonPrefixes>", "</CommonPrefixes>") {
        if let Some(cp) = inner(block, "<Prefix>", "</Prefix>") {
            if let Some(rest) = cp.strip_prefix(prefix) {
                let name = rest.trim_end_matches('/');
                if !name.is_empty() {
                    dirs.push(name.to_string());
                }
            }
        }
    }

    Listing { files, dirs }
}

fn inner<'a>(hay: &'a str, open: &str, close: &str) -> Option<String> {
    let start = hay.find(open)? + open.len();
    let end = hay[start..].find(close)? + start;
    Some(xml_unescape(&hay[start..end]))
}

fn between_all<'a>(hay: &'a str, open: &'a str, close: &'a str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = hay;
    while let Some(s) = rest.find(open) {
        let after = &rest[s + open.len()..];
        if let Some(e) = after.find(close) {
            out.push(&after[..e]);
            rest = &after[e + close.len()..];
        } else {
            break;
        }
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

// ---- URI encoding -------------------------------------------------------------

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
}

/// Encode a query-string component (encodes `/` too).
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Encode a path, preserving `/` between segments (S3 canonical URI rules).
fn encode_path(s: &str) -> String {
    s.split('/').map(encode_component).collect::<Vec<_>>().join("/")
}

// ---- date/time (no chrono) ----------------------------------------------------

/// Current time as `(YYYYMMDDTHHMMSSZ, YYYYMMDD)`.
fn now_amz() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    (
        format!("{y:04}{m:02}{d:02}T{h:02}{mi:02}{s:02}Z"),
        format!("{y:04}{m:02}{d:02}"),
    )
}

/// Days since the Unix epoch → `(year, month, day)` (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_epoch_and_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(18_628), (2021, 1, 1));
    }

    #[test]
    fn encoding_rules() {
        assert_eq!(encode_component("a b/c"), "a%20b%2Fc");
        assert_eq!(encode_path("photos/a b.jpg"), "photos/a%20b.jpg");
        assert_eq!(encode_component("tilde~and.dot-_"), "tilde~and.dot-_");
    }

    #[test]
    fn parse_listing_files_and_prefixes() {
        let xml = r#"<?xml version="1.0"?>
        <ListBucketResult>
          <Contents><Key>photos/a.jpg</Key><Size>10</Size></Contents>
          <Contents><Key>photos/b.jpg</Key><Size>20</Size></Contents>
          <Contents><Key>photos/</Key><Size>0</Size></Contents>
          <CommonPrefixes><Prefix>photos/2026/</Prefix></CommonPrefixes>
        </ListBucketResult>"#;
        let listing = parse_listing(xml, "photos/");
        assert_eq!(listing.dirs, vec!["2026"]);
        assert_eq!(
            listing.files,
            vec![("a.jpg".to_string(), 10), ("b.jpg".to_string(), 20)]
        );
    }
}

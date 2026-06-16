//! IPFS **Pinning Services API** client (the standard PSA spec, implemented by
//! Filebase, Pinata, 4everland, and any self-hosted IPFS pinning service). One client
//! speaks to all of them: a service is just an `{endpoint, token}`, and pinning a
//! site's root CID makes it available on that service's always-on nodes — so a
//! published site stays reachable even when this node is offline. Spec:
//! <https://ipfs.github.io/pinning-services-api-spec/>.

use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

/// One pinning service's stored credentials: the PSA base endpoint + bearer token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinService {
    pub endpoint: String,
    pub token: String,
}

/// All configured pinning services (persisted 0600 as `<store>/security/pin.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinCredentials {
    pub filebase: Option<PinService>,
    pub pinata: Option<PinService>,
    pub foureverland: Option<PinService>,
    pub ipfs: Option<PinService>,
}

impl PinCredentials {
    pub fn get(&self, service: &str) -> Option<&PinService> {
        match service {
            "filebase" => self.filebase.as_ref(),
            "pinata" => self.pinata.as_ref(),
            "foureverland" => self.foureverland.as_ref(),
            "ipfs" => self.ipfs.as_ref(),
            _ => None,
        }
    }
}

/// The result of a pin request: the service's request id, current status, and the
/// peer multiaddrs ("delegates") this node should connect to so the service can pull
/// the content via bitswap.
#[derive(Debug, Clone)]
pub struct PinOutcome {
    pub request_id: String,
    pub status: String,
    pub delegates: Vec<String>,
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|e| format!("http client: {e}"))
}

fn pins_url(service: &PinService) -> String {
    format!("{}/pins", service.endpoint.trim_end_matches('/'))
}

/// Verify a service's credentials — the "Test connection" step. Pinata's free plan
/// gates the PSA endpoints, so it's checked via its always-free `testAuthentication`
/// endpoint; every other service is verified by listing pins over the PSA.
pub fn verify(service: &str, svc: &PinService) -> Result<String, String> {
    let url = if service == "pinata" {
        "https://api.pinata.cloud/data/testAuthentication".to_string()
    } else {
        format!("{}?limit=1", pins_url(svc))
    };
    let resp = client()?
        .get(url)
        .bearer_auth(&svc.token)
        .send()
        .map_err(|e| format!("connect: {e}"))?;
    let status = resp.status();
    if status.is_success() {
        Ok(if service == "pinata" {
            "api.pinata.cloud".to_string()
        } else {
            host_label(&svc.endpoint)
        })
    } else {
        Err(format!(
            "HTTP {} — {}",
            status.as_u16(),
            clip(&resp.text().unwrap_or_default())
        ))
    }
}

/// Pin a CID to the service. Returns the request id + status + delegate multiaddrs.
pub fn pin_cid(service: &PinService, cid: &str, name: &str) -> Result<PinOutcome, String> {
    let resp = client()?
        .post(pins_url(service))
        .bearer_auth(&service.token)
        .json(&serde_json::json!({ "cid": cid, "name": name }))
        .send()
        .map_err(|e| format!("pin request: {e}"))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!("HTTP {} — {}", status.as_u16(), clip(&body)));
    }
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("parse pin response: {e}"))?;
    let request_id = value
        .get("requestid")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let pin_status = value
        .get("status")
        .and_then(|x| x.as_str())
        .unwrap_or("queued")
        .to_string();
    let delegates = value
        .get("delegates")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok(PinOutcome {
        request_id,
        status: pin_status,
        delegates,
    })
}

/// Pinata's free plan disallows pin-by-CID (PSA) — it only allows **direct upload**.
/// Upload a whole site folder via `pinFileToIPFS` (multipart): each file is sent under
/// a common `<name>/…` path so Pinata wraps them into one directory and returns that
/// directory's CID, with `index.html` at its root. Returns the directory CID.
pub fn upload_pinata_dir(
    token: &str,
    files: &[(String, Vec<u8>)],
    name: &str,
) -> Result<String, String> {
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(48)
        .collect();
    let wrapper = if safe.is_empty() {
        "site".to_string()
    } else {
        safe
    };
    let mut form = reqwest::blocking::multipart::Form::new()
        .text("pinataOptions", r#"{"cidVersion":1}"#)
        .text(
            "pinataMetadata",
            serde_json::json!({ "name": name }).to_string(),
        );
    for (rel, bytes) in files {
        let part = reqwest::blocking::multipart::Part::bytes(bytes.clone())
            .file_name(format!("{wrapper}/{rel}"));
        form = form.part("file", part);
    }
    let resp = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| format!("http client: {e}"))?
        .post("https://api.pinata.cloud/pinning/pinFileToIPFS")
        .bearer_auth(token)
        .multipart(form)
        .send()
        .map_err(|e| format!("pinata upload: {e}"))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!("HTTP {} — {}", status.as_u16(), clip(&body)));
    }
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("parse pinata response: {e}"))?;
    value
        .get("IpfsHash")
        .and_then(|h| h.as_str())
        .map(String::from)
        .ok_or_else(|| format!("pinata: no IpfsHash in response — {}", clip(&body)))
}

// ── Filebase S3 (CAR import) — the SDK's native, most-reliable path ─────────────
// Filebase's own @filebase/sdk uploads a directory by packing it into a CAR and
// PUTting it to the S3 endpoint with `x-amz-meta-import: car`; Filebase imports the CAR
// and pins its root. This pushes the bytes to Filebase (no need for our node to be
// reachable), unlike PSA pin-by-CID which makes Filebase pull from us.

const FILEBASE_S3_HOST: &str = "s3.filebase.com";
const S3_REGION: &str = "us-east-1";

/// Split a Filebase PSA token (`base64(accessKey:secretKey:bucket)`, as the SDK builds
/// it) back into its parts — reused for the S3 upload's SigV4 signing.
pub fn decode_filebase_token(token: &str) -> Result<(String, String, String), String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(token.trim())
        .map_err(|e| format!("decode filebase token: {e}"))?;
    let text = String::from_utf8(raw).map_err(|e| format!("filebase token not utf-8: {e}"))?;
    let parts: Vec<&str> = text.splitn(3, ':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Err("filebase credentials malformed (expected key:secret:bucket)".to_string());
    }
    Ok((
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    ))
}

/// Ensure the IPFS bucket exists (S3 `CreateBucket`), so the user never has to make one
/// in the dashboard — Filebase buckets on the S3 endpoint are IPFS buckets. Idempotent:
/// "already owned by you" counts as success.
pub fn filebase_s3_ensure_bucket(
    access_key: &str,
    secret_key: &str,
    bucket: &str,
) -> Result<(), String> {
    let path = uri_encode(&format!("/{bucket}"), true);
    let (status, body) = filebase_s3_send("PUT", &path, false, &[], access_key, secret_key)?;
    if (200..300).contains(&status) || body.contains("BucketAlreadyOwnedByYou") {
        Ok(())
    } else if body.contains("BucketAlreadyExists") {
        Err(format!(
            "the bucket name '{bucket}' is already taken by another Filebase account — choose a different name"
        ))
    } else {
        Err(format!("create bucket: HTTP {status} — {}", clip(&body)))
    }
}

/// Upload `car_bytes` (a CAR whose root is `cid`) to Filebase's S3 endpoint as a CAR
/// import, signed with AWS Signature V4. Filebase pins the root DAG, so the content is
/// hosted on Filebase's always-on nodes regardless of this node's reachability.
pub fn filebase_s3_put_car(
    access_key: &str,
    secret_key: &str,
    bucket: &str,
    key: &str,
    car_bytes: &[u8],
) -> Result<(), String> {
    let path = uri_encode(&format!("/{bucket}/{key}"), true);
    let (status, body) = filebase_s3_send("PUT", &path, true, car_bytes, access_key, secret_key)?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!("HTTP {status} — {}", clip(&body)))
    }
}

/// Send one AWS-SigV4-signed request to Filebase's S3 endpoint. `import_car` adds the
/// signed `x-amz-meta-import: car` header (only the headers we send are signed).
/// Returns `(status, body)`.
fn filebase_s3_send(
    method: &str,
    path: &str,
    import_car: bool,
    body: &[u8],
    access_key: &str,
    secret_key: &str,
) -> Result<(u16, String), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("clock: {e}"))?
        .as_secs();
    let (datestamp, amzdate) = amz_date(now);
    let payload_hash = hex(&Sha256::digest(body));

    let (canonical_headers, signed_headers) = if import_car {
        (
            format!("host:{FILEBASE_S3_HOST}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amzdate}\nx-amz-meta-import:car\n"),
            "host;x-amz-content-sha256;x-amz-date;x-amz-meta-import",
        )
    } else {
        (
            format!("host:{FILEBASE_S3_HOST}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amzdate}\n"),
            "host;x-amz-content-sha256;x-amz-date",
        )
    };
    let canonical_request =
        format!("{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");

    let scope = format!("{datestamp}/{S3_REGION}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amzdate}\n{scope}\n{}",
        hex(&Sha256::digest(canonical_request.as_bytes()))
    );
    let signature = sigv4_sign(secret_key, &datestamp, S3_REGION, "s3", &string_to_sign);
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
    );

    let verb =
        reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| format!("bad method: {e}"))?;
    let mut request = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| format!("http client: {e}"))?
        .request(verb, format!("https://{FILEBASE_S3_HOST}{path}"))
        .header("Host", FILEBASE_S3_HOST)
        .header("x-amz-content-sha256", &payload_hash)
        .header("x-amz-date", &amzdate)
        .header("Authorization", authorization)
        .body(body.to_vec());
    if import_car {
        request = request.header("x-amz-meta-import", "car");
    }
    let resp = request
        .send()
        .map_err(|e| format!("filebase request: {e}"))?;
    let status = resp.status().as_u16();
    Ok((status, resp.text().unwrap_or_default()))
}

/// AWS SigV4: derive the signing key (HMAC chain) and sign the string-to-sign.
fn sigv4_sign(
    secret_key: &str,
    datestamp: &str,
    region: &str,
    service: &str,
    string_to_sign: &str,
) -> String {
    let k_date = hmac(format!("AWS4{secret_key}").as_bytes(), datestamp.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    hex(&hmac(&k_signing, string_to_sign.as_bytes()))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap());
    }
    out
}

/// RFC-3986 percent-encoding as AWS SigV4 requires (unreserved chars pass through;
/// `/` is preserved in paths).
fn uri_encode(value: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => {
                out.push('%');
                out.push(
                    char::from_digit((byte >> 4) as u32, 16)
                        .unwrap()
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit((byte & 0x0f) as u32, 16)
                        .unwrap()
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    out
}

/// Format a Unix timestamp as the SigV4 `(YYYYMMDD, YYYYMMDDTHHMMSSZ)` pair (UTC), with
/// no date/time dependency (Howard Hinnant's civil-from-days algorithm).
fn amz_date(unix_secs: u64) -> (String, String) {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (
        format!("{year:04}{month:02}{day:02}"),
        format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z"),
    )
}

fn host_label(endpoint: &str) -> String {
    endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(endpoint)
        .to_string()
}

fn clip(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > 180 {
        format!("{}…", s.chars().take(180).collect::<String>())
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    type Captured = Arc<Mutex<Vec<String>>>;

    /// One-shot mock PSA endpoint: capture the request, return a fixed response.
    fn spawn(status: u16, body: &'static str) -> (String, Captured, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let captured: Captured = Arc::new(Mutex::new(Vec::new()));
        let seen = captured.clone();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(1500)))
                .ok();
            // Read until the full request (incl. any body sent after a 100-continue
            // pause) has arrived: stop once we have Content-Length bytes past the header.
            let mut req = Vec::new();
            let mut buf = [0u8; 2048];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 || req.len() > 16384 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(header_end) = text.find("\r\n\r\n") {
                    let want = text
                        .to_ascii_lowercase()
                        .find("content-length:")
                        .map(|i| {
                            text[i + 15..]
                                .lines()
                                .next()
                                .unwrap_or("")
                                .trim()
                                .parse::<usize>()
                                .unwrap_or(0)
                        })
                        .unwrap_or(0);
                    if req.len() >= header_end + 4 + want {
                        break; // headers + full body in hand
                    }
                }
            }
            seen.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&req).into_owned());
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        (base, captured, handle)
    }

    #[test]
    fn pin_cid_posts_to_pins_with_bearer_and_parses_response() {
        let (base, captured, handle) = spawn(
            200,
            r#"{"requestid":"req-1","status":"pinning","delegates":["/dnsaddr/node.example/p2p/Qm123"]}"#,
        );
        let svc = PinService {
            endpoint: base,
            token: "tok-abc".into(),
        };
        let out = pin_cid(&svc, "bafyCID", "my-site").unwrap();
        assert_eq!(out.request_id, "req-1");
        assert_eq!(out.status, "pinning");
        assert_eq!(out.delegates, vec!["/dnsaddr/node.example/p2p/Qm123"]);
        handle.join().unwrap();
        let req = captured.lock().unwrap()[0].clone();
        assert!(req.starts_with("POST "), "{req}");
        assert!(req.contains("/pins"), "{req}");
        assert!(req.contains("Bearer tok-abc"), "{req}");
        assert!(req.contains("bafyCID"), "{req}");
    }

    #[test]
    fn verify_lists_pins_and_surfaces_http_errors() {
        let (base, captured, handle) = spawn(200, r#"{"count":0,"results":[]}"#);
        let svc = PinService {
            endpoint: base,
            token: "t".into(),
        };
        let label = verify("ipfs", &svc).unwrap();
        assert!(label.contains("127.0.0.1"), "{label}");
        handle.join().unwrap();
        let req = captured.lock().unwrap()[0].clone();
        assert!(req.starts_with("GET "), "{req}");
        assert!(req.contains("/pins?limit=1"), "{req}");

        let (base, _c, handle) = spawn(401, r#"{"error":{"reason":"UNAUTHORIZED"}}"#);
        let bad = PinService {
            endpoint: base,
            token: "bad".into(),
        };
        let err = verify("ipfs", &bad).unwrap_err();
        assert!(err.contains("401"), "{err}");
        handle.join().unwrap();
    }

    #[test]
    fn sigv4_matches_aws_official_test_vector() {
        // AWS SigV4 "GET Object" documented example — secret, scope, and string-to-sign
        // are AWS's; the expected signature is the one AWS publishes.
        let secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let string_to_sign = "AWS4-HMAC-SHA256\n20130524T000000Z\n20130524/us-east-1/s3/aws4_request\n7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972";
        let sig = sigv4_sign(secret, "20130524", "us-east-1", "s3", string_to_sign);
        assert_eq!(
            sig,
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn amz_date_formats_utc() {
        assert_eq!(amz_date(0), ("19700101".into(), "19700101T000000Z".into()));
        // 2013-05-24T00:00:00Z and 2009-02-13T23:31:30Z
        assert_eq!(
            amz_date(1369353600),
            ("20130524".into(), "20130524T000000Z".into())
        );
        assert_eq!(
            amz_date(1234567890),
            ("20090213".into(), "20090213T233130Z".into())
        );
    }

    #[test]
    fn filebase_token_round_trips() {
        let token = base64::engine::general_purpose::STANDARD.encode("KEY:SECRET:my-bucket");
        let (k, s, b) = decode_filebase_token(&token).unwrap();
        assert_eq!(
            (k.as_str(), s.as_str(), b.as_str()),
            ("KEY", "SECRET", "my-bucket")
        );
        assert!(decode_filebase_token("not-base64!!").is_err());
        let bad = base64::engine::general_purpose::STANDARD.encode("only:two");
        assert!(decode_filebase_token(&bad).is_err());
    }

    #[test]
    fn uri_encode_preserves_unreserved_and_slash() {
        assert_eq!(uri_encode("/my-bucket/bafyabc", true), "/my-bucket/bafyabc");
        assert_eq!(uri_encode("a b", false), "a%20b");
    }

    #[test]
    fn credentials_lookup_by_service() {
        let creds = PinCredentials {
            filebase: Some(PinService {
                endpoint: "https://api.filebase.io/v1/ipfs".into(),
                token: "x".into(),
            }),
            ..Default::default()
        };
        assert!(creds.get("filebase").is_some());
        assert!(creds.get("pinata").is_none());
        assert!(creds.get("unknown").is_none());
    }
}

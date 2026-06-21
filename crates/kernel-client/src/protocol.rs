//! The kernel IPC wire protocol: one JSON [`Request`] in, one [`Response`] out,
//! correlated by `id`. The request deliberately mirrors the GUI host's route
//! contract — `path + query + optional body` — so `/api/*` handlers can move into
//! the daemon without growing one-off operation structs for every endpoint.

use serde::{Deserialize, Serialize};

/// The wire protocol version this binary speaks. Bump on any breaking change to the
/// `Request`/`Response` shape or route semantics. The kernel stamps it on every
/// response; a client compares it to its own and replaces a kernel that no longer
/// matches (the "stale daemon left running across an app upgrade" case).
pub const PROTOCOL_VERSION: u32 = 1;

/// A request from a client (GUI host, CLI, or MCP) to the kernel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Correlates the response back to this request.
    pub id: u64,
    /// Existing route path, for example `/api/search`.
    #[serde(default)]
    pub path: String,
    /// Existing URL query string without the leading `?`.
    #[serde(default)]
    pub query: String,
    /// Optional request body, reserved for mutation routing in later phases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// The kernel's reply to a [`Request`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    /// Wire protocol version the responding kernel speaks. Absent on a pre-versioning
    /// kernel, which `#[serde(default)]` reads as 0 — a client treats any value other
    /// than its own [`PROTOCOL_VERSION`] as "stale kernel, replace it".
    #[serde(default)]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub result: serde_json::Value,
    #[serde(default)]
    pub status: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
}

impl Response {
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        let body = result.to_string();
        Self {
            id,
            ok: true,
            version: PROTOCOL_VERSION,
            error: None,
            result,
            status: 200,
            content_type: "application/json; charset=utf-8".to_string(),
            body,
        }
    }

    pub fn err(id: u64, error: impl Into<String>) -> Self {
        let error = error.into();
        let body = serde_json::json!({ "error": error }).to_string();
        Self {
            id,
            ok: false,
            version: PROTOCOL_VERSION,
            error: Some(error),
            result: serde_json::Value::Null,
            status: 500,
            content_type: "application/json; charset=utf-8".to_string(),
            body,
        }
    }

    pub fn api(
        id: u64,
        status: u16,
        content_type: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        let body = body.into();
        let result = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        let ok = status < 400;
        Self {
            id,
            ok,
            version: PROTOCOL_VERSION,
            error: (!ok).then(|| {
                result
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or("kernel request failed")
                    .to_string()
            }),
            result,
            status,
            content_type: content_type.into(),
            body,
        }
    }

    pub fn json(id: u64, status: u16, body: impl Into<String>) -> Self {
        Self::api(id, status, "application/json; charset=utf-8", body)
    }

    pub fn bad_request(id: u64, message: &str) -> Self {
        Self::json(id, 400, serde_json::json!({ "error": message }).to_string())
    }

    pub fn not_found(id: u64, path: &str) -> Self {
        Self::json(
            id,
            404,
            serde_json::json!({ "error": format!("unknown kernel route: {path}") }).to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Request, Response};

    #[test]
    fn request_uses_route_contract() {
        let req = Request {
            id: 7,
            path: "/api/search".to_string(),
            query: "q=kernel".to_string(),
            body: Some("{}".to_string()),
        };
        let text = serde_json::to_string(&req).unwrap();
        assert!(text.contains("\"path\":\"/api/search\""));
        assert!(text.contains("\"query\":\"q=kernel\""));
        assert!(text.contains("\"body\":\"{}\""));
        let decoded: Request = serde_json::from_str(&text).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn api_response_preserves_http_shape_and_result() {
        let resp = Response::json(1, 200, r#"{"items":[]}"#);
        assert!(resp.ok);
        assert_eq!(resp.status, 200);
        assert_eq!(resp.result["items"].as_array().unwrap().len(), 0);
        assert_eq!(resp.body, r#"{"items":[]}"#);
    }

    #[test]
    fn response_stamps_current_version_and_legacy_defaults_to_zero() {
        // A response this binary builds always carries our protocol version…
        assert_eq!(
            super::PROTOCOL_VERSION,
            Response::ok(1, serde_json::json!({})).version
        );
        assert_eq!(
            super::PROTOCOL_VERSION,
            Response::json(1, 200, "{}").version
        );
        // …while a pre-versioning kernel's response (no `version` field) decodes to 0,
        // which a client reads as "stale daemon, replace it".
        let legacy: Response = serde_json::from_str(r#"{"id":1,"ok":true,"status":200}"#).unwrap();
        assert_eq!(legacy.version, 0);
        assert_ne!(legacy.version, super::PROTOCOL_VERSION);
    }
}

//! The "Brain" tab backend (brain-tab.md) — a dashboard over the **connected
//! sovereign LLM** plus Concierge's internal **embedder**.
//!
//! Built in two layers so it works across engines and operating systems without UI
//! rework:
//!
//! 1. **Portable baseline (any engine, any OS)** — everything reachable through the
//!    OpenAI-compatible local API every serious local engine exposes: is it up,
//!    `GET /v1/models`, and the active model. Works for oMLX, PyOMlx, and (later)
//!    Ollama / LM Studio.
//! 2. **Engine-specific metric providers (optional, pluggable)** — rich telemetry only
//!    some engines expose. Each implements [`BrainMetricsProvider`]; the backend picks
//!    one by detected engine. **No provider → baseline only** (never a fabricated bar).
//!
//! The first full provider is [`OmlxProvider`] (macOS / Apple Silicon): it reads oMLX's
//! `GET /admin/api/stats` for cache/activity and [macmon](https://github.com/vladkens/macmon)
//! at `http://127.0.0.1:9090/json` for hardware. Both are loopback-only and degrade to
//! `None` when absent, so the same code compiles and runs on every platform.

use serde::Serialize;

use crate::error::{Error, Result};

/// Default macmon metrics endpoint (Apple-Silicon only; absent elsewhere → degrades).
const MACMON_URL: &str = "http://127.0.0.1:9090/json";

/// The portable, always-available status from the OpenAI-compatible API.
#[derive(Debug, Clone, Serialize)]
pub struct BaselineStatus {
    /// Is the engine answering on its local API?
    pub up: bool,
    /// Detected engine label (`"oMLX"`, `"OpenAI-compatible"`, or `"none"`).
    pub engine: String,
    /// The base URL probed (e.g. `http://localhost:8000`).
    pub base_url: String,
    /// Models the engine reports via `GET /v1/models` (ids/aliases).
    pub models: Vec<String>,
    /// The model Concierge is configured to route to, if set.
    pub active_model: Option<String>,
}

/// Engine-specific rich telemetry. Both fields are upstream JSON passed through
/// verbatim (loopback, no secrets) so the GUI can render whatever the engine exposes
/// without this crate guessing at a schema that drifts between versions.
#[derive(Debug, Clone, Serialize)]
pub struct RichMetrics {
    /// Raw macmon `/json` (CPU/GPU/memory/pressure/swap on Apple Silicon).
    pub macmon: Option<serde_json::Value>,
    /// Raw oMLX `/admin/api/stats` (model weights / hot-KV cache / per-request PP·TG).
    pub omlx_stats: Option<serde_json::Value>,
}

impl RichMetrics {
    fn is_empty(&self) -> bool {
        self.macmon.is_none() && self.omlx_stats.is_none()
    }
}

/// Concierge's internal retrieval engine status (Panel B). Backend + model are read
/// from [`crate::config::LibrarianConfig`]; the live counters need `LibrarianState`
/// instrumentation (brain-tab.md §2) and are `None` until that lands.
#[derive(Debug, Clone, Serialize)]
pub struct EmbedderStatus {
    pub backend: String,
    pub model: String,
    /// `true` if the `http` embedder backend points at the connected engine's
    /// `/v1/embeddings` (same sovereign engine powers generation + retrieval).
    pub shares_engine: bool,
    pub indexed_nodes: Option<u64>,
    pub queue_depth: Option<u64>,
    pub last_latency_ms: Option<u64>,
}

/// The full snapshot the CLI/GUI consume.
#[derive(Debug, Clone, Serialize)]
pub struct BrainMetrics {
    pub baseline: BaselineStatus,
    /// Present only when the detected engine has a metrics provider with data.
    pub rich: Option<RichMetrics>,
    pub embedder: EmbedderStatus,
}

/// A source of Brain metrics. The baseline tier is mandatory; `rich` is optional and
/// defaults to none (the portable case).
pub trait BrainMetricsProvider {
    fn baseline(&self) -> BaselineStatus;
    fn rich(&self) -> Option<RichMetrics> {
        None
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("concierge-plugin")
        // Short timeouts: the GUI polls this; a hung engine must not stall the panel.
        .timeout(std::time::Duration::from_millis(1500))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// Parse model ids from an OpenAI `GET /v1/models` body (`{ "data": [ { "id": … } ] }`).
fn parse_model_ids(body: &serde_json::Value) -> Vec<String> {
    body.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// The portable provider: probes any OpenAI-compatible engine. Used for PyOMlx and as
/// the fallback for an unknown engine.
pub struct OpenAiBaselineProvider {
    base_url: String,
    active_model: Option<String>,
    engine_label: String,
}

impl OpenAiBaselineProvider {
    pub fn new(base_url: impl Into<String>, active_model: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            active_model,
            engine_label: "OpenAI-compatible".to_string(),
        }
    }
}

fn probe_models(base_url: &str) -> Option<Vec<String>> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let resp = client().get(&url).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().ok()?;
    Some(parse_model_ids(&body))
}

impl BrainMetricsProvider for OpenAiBaselineProvider {
    fn baseline(&self) -> BaselineStatus {
        match probe_models(&self.base_url) {
            Some(models) => BaselineStatus {
                up: true,
                engine: self.engine_label.clone(),
                base_url: self.base_url.clone(),
                models,
                active_model: self.active_model.clone(),
            },
            None => BaselineStatus {
                up: false,
                engine: "none".to_string(),
                base_url: self.base_url.clone(),
                models: Vec::new(),
                active_model: self.active_model.clone(),
            },
        }
    }
}

/// The oMLX provider: baseline **plus** rich telemetry from macmon + oMLX admin stats.
/// macmon is Apple-Silicon-only and `/admin/api/stats` needs a live admin session; both
/// degrade to `None` rather than failing, so this is safe to use on any platform.
pub struct OmlxProvider {
    base_url: String,
    active_model: Option<String>,
}

impl OmlxProvider {
    pub fn new(base_url: impl Into<String>, active_model: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            active_model,
        }
    }

    fn fetch_json(url: &str) -> Option<serde_json::Value> {
        let resp = client().get(url).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().ok()
    }
}

impl BrainMetricsProvider for OmlxProvider {
    fn baseline(&self) -> BaselineStatus {
        // Reuse the OpenAI probe but label it "oMLX" when up (it falls back to "none"
        // on failure, which we keep).
        OpenAiBaselineProvider {
            base_url: self.base_url.clone(),
            active_model: self.active_model.clone(),
            engine_label: "oMLX".to_string(),
        }
        .baseline()
    }

    fn rich(&self) -> Option<RichMetrics> {
        let base = self.base_url.trim_end_matches('/');
        let rich = RichMetrics {
            macmon: Self::fetch_json(MACMON_URL),
            omlx_stats: Self::fetch_json(&format!("{base}/admin/api/stats")),
        };
        if rich.is_empty() {
            None
        } else {
            Some(rich)
        }
    }
}

/// Detect the engine at `base_url` and return the right provider. If `/admin/api/stats`
/// or macmon answers it's treated as oMLX (rich); if only `/v1/models` answers it's a
/// generic OpenAI-compatible engine (baseline, e.g. PyOMlx); otherwise baseline-down.
pub fn select_provider(
    base_url: &str,
    active_model: Option<String>,
) -> Box<dyn BrainMetricsProvider> {
    let base = base_url.trim_end_matches('/');
    let omlx_like = OmlxProvider::fetch_json(MACMON_URL).is_some()
        || OmlxProvider::fetch_json(&format!("{base}/admin/api/stats")).is_some();
    if omlx_like {
        Box::new(OmlxProvider::new(base.to_string(), active_model))
    } else {
        Box::new(OpenAiBaselineProvider::new(base.to_string(), active_model))
    }
}

impl crate::binding::MemCli {
    /// Assemble a full Brain snapshot: engine baseline (+ rich if the provider has it)
    /// and the internal embedder status. Always returns — a down engine yields
    /// `baseline.up = false`, not an error.
    pub fn brain_metrics(&self) -> Result<BrainMetrics> {
        let cfg = self.config()?;
        let base_url = cfg.brain.engine_base_url.clone();
        let active_model = cfg.brain.active_model.clone();

        let provider = select_provider(&base_url, active_model);
        let baseline = provider.baseline();
        let rich = provider.rich();

        // Panel B — read what exists from config; live counters await LibrarianState.
        let shares_engine = cfg.librarian.embedder == "http"
            && !cfg.librarian.embedding_url.is_empty()
            && url_host_eq(&cfg.librarian.embedding_url, &base_url);
        let embedder = EmbedderStatus {
            backend: cfg.librarian.embedder.clone(),
            model: cfg.librarian.embedding_model.clone(),
            shares_engine,
            indexed_nodes: None,
            queue_depth: None,
            last_latency_ms: None,
        };

        Ok(BrainMetrics {
            baseline,
            rich,
            embedder,
        })
    }

    /// Persist which model the Brain tab routes requests to (the `model` field). Empty
    /// clears the selection.
    pub fn brain_set_model(&self, model: &str) -> Result<()> {
        let mut cfg = self.config()?;
        cfg.brain.active_model = if model.trim().is_empty() {
            None
        } else {
            Some(model.trim().to_string())
        };
        cfg.save_to_project_root(self.working_dir())
            .map_err(Error::Io)
    }
}

/// Cheap host:port comparison so "embedder points at the connected engine" doesn't
/// false-positive on path differences (`/v1/embeddings` vs `/v1/models`).
fn url_host_eq(a: &str, b: &str) -> bool {
    fn host(s: &str) -> &str {
        let s = s.split("//").nth(1).unwrap_or(s);
        s.split('/').next().unwrap_or(s)
    }
    !a.is_empty() && !b.is_empty() && host(a) == host(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_model_list() {
        let body = serde_json::json!({
            "object": "list",
            "data": [{ "id": "Qwen3-Coder" }, { "id": "Llama-3" }]
        });
        assert_eq!(parse_model_ids(&body), vec!["Qwen3-Coder", "Llama-3"]);
        assert!(parse_model_ids(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn baseline_down_when_engine_absent() {
        // Nothing is listening on this port in tests → up:false, never an error.
        let p = OpenAiBaselineProvider::new("http://127.0.0.1:1", None);
        let b = p.baseline();
        assert!(!b.up);
        assert_eq!(b.engine, "none");
        assert!(b.models.is_empty());
    }

    #[test]
    fn provider_without_rich_returns_none() {
        let p = OpenAiBaselineProvider::new("http://127.0.0.1:1", None);
        assert!(p.rich().is_none());
    }

    #[test]
    fn rich_is_none_when_both_sources_absent() {
        let p = OmlxProvider::new("http://127.0.0.1:1", None);
        assert!(p.rich().is_none());
    }

    #[test]
    fn select_provider_falls_back_to_baseline_when_down() {
        // No oMLX/macmon on this port → generic baseline provider, reports down.
        let p = select_provider("http://127.0.0.1:1", None);
        let b = p.baseline();
        assert!(!b.up);
        assert!(p.rich().is_none());
    }

    #[test]
    fn url_host_comparison() {
        assert!(url_host_eq(
            "http://localhost:8000/v1/embeddings",
            "http://localhost:8000"
        ));
        assert!(!url_host_eq(
            "http://localhost:8000",
            "http://localhost:11434"
        ));
        assert!(!url_host_eq("", "http://localhost:8000"));
    }
}

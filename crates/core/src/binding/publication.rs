use super::*;

impl MemCli {
    /// Append a publish receipt to the local trail beside the store.
    pub(super) fn append_receipt(&self, receipt: &PublishReceipt) -> Result<()> {
        use std::io::Write;
        let path = self
            .working_dir
            .join(self.config()?.store.root.join("publish-receipts.jsonl"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("create receipt dir: {e}")))?;
        }
        let line = serde_json::to_string(receipt).map_err(|e| Error::Io(e.to_string()))?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| Error::Io(format!("open receipt trail: {e}")))?;
        writeln!(file, "{line}").map_err(|e| Error::Io(format!("write receipt: {e}")))?;
        Ok(())
    }

    /// Record a website publish as a real DAG node, filed into the day calendar so
    /// it shows up in Records/Graph like any other node — the store is the single
    /// CID ledger. The published bytes themselves live in Kubo (UnixFS) or the
    /// external host; this `publication` node *references* that root + its IPNS/URL
    /// so the published CID is never invisible to the explorer. The receipt trail
    /// (`publish-receipts.jsonl`) stays as the signed egress log; this is the
    /// in-store, content-addressed counterpart.
    pub(super) fn record_publication(&self, receipt: &PublishReceipt) -> Result<()> {
        let ts = receipt.unix_time;
        // A `memory` node of kind `reference` (a publication *is* a reference to
        // external published content). The mem node-kind enum AND its memory-kind
        // sub-enum are both closed, and the on-disk store is also read by the
        // external `mem` CLI, so we don't add a new variant — the published root
        // CID, IPNS, and URL are encoded in the `text` so the explorer surfaces them.
        let ipns_part = receipt
            .ipns_name
            .as_deref()
            .map(|ipns| format!(" · ipns {ipns}"))
            .unwrap_or_default();
        let text = format!(
            "Published \"{}\" to {} — root {}{} · {}",
            receipt.site_name.as_deref().unwrap_or("site"),
            receipt.backend,
            receipt.root,
            ipns_part,
            receipt.gateway_url,
        );
        let node = Node {
            kind: "memory".to_string(),
            fields_json: serde_json::json!({ "kind": "reference", "text": text }).to_string(),
        };
        let cid = self.put_node(&node)?;
        // One record per publish event (ts is unique per publish; IPFS roots also
        // change each time). Files into today's day so it appears under Records.
        let event_key = format!("publication-{}-{ts}", receipt.backend);
        self.record_event_in_day(&utc_date(ts), &event_key, &cid)?;
        Ok(())
    }

    /// Save the current Studio draft as a checkpoint at any time — no publish, no
    /// egress. The HTML is content-addressed as a blob (a real CID), wrapped in a
    /// genuine `checkpoint` node (retained + egress-locked like any checkpoint), and
    /// that node is filed into the day calendar so it appears in Records. Returns the
    /// snapshot's content CID + the timestamp.
    pub fn save_site_checkpoint(&self, name: &str, html: &str) -> Result<(String, u64)> {
        let ts = now_secs();
        let root = self.put_blob(html.as_bytes(), "text/html")?;
        let checkpoint = self.checkpoint(&format!("studio:{name}"), &root, None)?;
        let event_key = format!("studio-checkpoint-{name}-{ts}");
        // Best-effort calendar filing — the node + blob are already content-addressed
        // even if the day index can't be updated.
        let _ = self.record_event_in_day(&utc_date(ts), &event_key, &checkpoint);
        Ok((root.0, ts))
    }
}

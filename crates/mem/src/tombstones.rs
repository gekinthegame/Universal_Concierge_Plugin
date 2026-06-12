//! The tombstone ledger — a receipt of truth for every block GC deletes.
//!
//! In a content-addressed store a missing block is normally corruption. But a
//! block GC pruned on purpose is a *fact*, not a fault: the ledger records when
//! it died, why, and which surviving record continues its history. So a walk
//! that crosses a pruned link reports the receipt and treats it as a legitimate
//! frontier, while a genuinely missing (untombstoned) block still errors loudly.
//!
//! Like the name index, this is mutable state that lives BESIDE the immutable
//! blocks, never inside the DAG, and is written atomically (temp + rename).

use crate::cid::Cid;
use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;

/// A block's death certificate: when it was pruned, why, and the surviving
/// record that continues its history (the "pointer" you follow forward).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tombstone {
    pub pruned_at: u64,             // unix seconds — time of death
    pub reason: String,             // why it was pruned (e.g. "auto-checkpoint trimmed (keep=10)")
    pub superseded_by: Option<Cid>, // the surviving record this one led to
}

/// On-disk shape: CIDs (key and `superseded_by`) are canonical strings, parsed
/// back explicitly — the same discipline as the name index.
#[derive(Serialize, Deserialize)]
struct WireTombstone {
    pruned_at: u64,
    reason: String,
    superseded_by: Option<String>,
}

/// Persisted as `.concierge/tombstones.json` (pruned CID -> Tombstone). Mutable
/// sidecar; the one other place (besides the name index) mutation is legal.
#[derive(Debug)]
pub struct Tombstones {
    path: PathBuf,
    map: HashMap<Cid, Tombstone>,
}

impl Tombstones {
    /// Load the ledger from `path`, or start empty if the file doesn't exist.
    pub fn load(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();
        let map = if path.try_exists()? {
            let content = std::fs::read_to_string(&path)?;
            let raw: BTreeMap<String, WireTombstone> = serde_json::from_str(&content)
                .map_err(|e| anyhow::anyhow!("tombstone ledger parse: {e}"))?;
            let mut map = HashMap::with_capacity(raw.len());
            for (cid_str, wire) in raw {
                let cid: Cid = cid_str
                    .parse()
                    .map_err(|e| anyhow::anyhow!("tombstone ledger: bad CID {cid_str:?}: {e}"))?;
                let superseded_by = match wire.superseded_by {
                    Some(s) => Some(s.parse().map_err(|e| {
                        anyhow::anyhow!("tombstone ledger: bad superseded_by for {cid_str:?}: {e}")
                    })?),
                    None => None,
                };
                map.insert(
                    cid,
                    Tombstone {
                        pruned_at: wire.pruned_at,
                        reason: wire.reason,
                        superseded_by,
                    },
                );
            }
            map
        } else {
            HashMap::new()
        };
        Ok(Self { path, map })
    }

    /// The death certificate for a CID, if it was pruned.
    pub fn get(&self, cid: &Cid) -> Option<&Tombstone> {
        self.map.get(cid)
    }

    pub fn contains(&self, cid: &Cid) -> bool {
        self.map.contains_key(cid)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// All recorded tombstones, for listing.
    pub fn iter(&self) -> impl Iterator<Item = (&Cid, &Tombstone)> {
        self.map.iter()
    }

    /// Record a batch of deaths and persist atomically. Batched so one GC pass
    /// writes the ledger once, not once per block.
    pub fn record(
        &mut self,
        entries: impl IntoIterator<Item = (Cid, Tombstone)>,
    ) -> anyhow::Result<()> {
        let mut next = self.map.clone();
        for (cid, tombstone) in entries {
            next.insert(cid, tombstone);
        }
        self.save(&next)?;
        self.map = next;
        Ok(())
    }

    fn save(&self, map: &HashMap<Cid, Tombstone>) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        // BTreeMap → deterministic key order, so the file is stable across saves.
        let serializable: BTreeMap<String, WireTombstone> = map
            .iter()
            .map(|(cid, t)| {
                (
                    cid.to_string(),
                    WireTombstone {
                        pruned_at: t.pruned_at,
                        reason: t.reason.clone(),
                        superseded_by: t.superseded_by.map(|c| c.to_string()),
                    },
                )
            })
            .collect();
        let json = serde_json::to_string_pretty(&serializable)?;
        let mut f = AtomicWriteFile::open(&self.path)?;
        f.write_all(json.as_bytes())?;
        f.commit()?; // temp + atomic rename — a partial ledger is never observed
        Ok(())
    }
}

/// Render a unix timestamp as a UTC ISO-8601 string (`1970-01-01T00:00:00Z`),
/// so a tombstone shows a human time of death without pulling in a date crate.
/// Uses Howard Hinnant's `civil_from_days` algorithm (days-since-epoch → date).
pub fn iso8601(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = year + i64::from(month <= 2);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cid(b: &[u8]) -> Cid {
        crate::cid::compute(b)
    }

    fn fresh() -> (TempDir, Tombstones) {
        let dir = TempDir::new().unwrap();
        let t = Tombstones::load(dir.path().join(".concierge/tombstones.json")).unwrap();
        (dir, t)
    }

    #[test]
    fn record_then_get() {
        let (_d, mut t) = fresh();
        let dead = cid(b"old checkpoint");
        let tip = cid(b"surviving tip");
        t.record([(
            dead,
            Tombstone {
                pruned_at: 1_700_000_000,
                reason: "auto-checkpoint trimmed (keep=10)".into(),
                superseded_by: Some(tip),
            },
        )])
        .unwrap();

        let got = t.get(&dead).unwrap();
        assert_eq!(got.pruned_at, 1_700_000_000);
        assert_eq!(got.superseded_by, Some(tip));
        assert!(t.contains(&dead));
        assert!(t.get(&cid(b"never pruned")).is_none());
    }

    #[test]
    fn survives_reload_with_cids_intact() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".concierge/tombstones.json");
        let dead = cid(b"dead");
        let tip = cid(b"tip");
        {
            let mut t = Tombstones::load(path.clone()).unwrap();
            t.record([(
                dead,
                Tombstone {
                    pruned_at: 42,
                    reason: "orphan".into(),
                    superseded_by: None,
                },
            )])
            .unwrap();
        }
        let reloaded = Tombstones::load(path).unwrap();
        let got = reloaded.get(&dead).unwrap();
        assert_eq!(got.reason, "orphan");
        assert_eq!(got.superseded_by, None);
        assert!(reloaded.get(&tip).is_none());
    }

    #[test]
    fn record_is_cumulative_across_passes() {
        let (_d, mut t) = fresh();
        t.record([(
            cid(b"a"),
            Tombstone {
                pruned_at: 1,
                reason: "orphan".into(),
                superseded_by: None,
            },
        )])
        .unwrap();
        t.record([(
            cid(b"b"),
            Tombstone {
                pruned_at: 2,
                reason: "orphan".into(),
                superseded_by: None,
            },
        )])
        .unwrap();
        assert_eq!(t.len(), 2);
        assert!(t.contains(&cid(b"a")) && t.contains(&cid(b"b")));
    }

    #[test]
    fn malformed_ledger_errors_explicitly() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tombstones.json");
        std::fs::write(&path, "{").unwrap();
        let err = Tombstones::load(path).unwrap_err().to_string();
        assert!(err.contains("tombstone ledger parse"), "got: {err}");
    }

    #[test]
    fn iso8601_formats_known_epochs() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso8601(86_400), "1970-01-02T00:00:00Z");
    }
}

//! Durable idempotency receipts contain counts/IDs and status, never tool arguments or screen text.
use crate::{digest, nonce, private_dir, private_file};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Receipt {
    pub run_id: String,
    pub request_id: String,
    pub console: String,
    pub kind: String,
    pub status: String,
    pub cursor: u64,
}

pub struct Runs {
    dir: PathBuf,
    records: BTreeMap<String, Receipt>,
}
impl Runs {
    pub fn open(dir: &Path) -> Result<Self, String> {
        private_dir(dir)?;
        let mut this = Self {
            dir: dir.into(),
            records: BTreeMap::new(),
        };
        for file in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let path = file.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let mut receipt: Receipt = serde_json::from_slice(
                &std::fs::read(&path).map_err(|e| e.to_string())?,
            )
            .map_err(|_| "Unreadable run receipt; refusing automatic restart".to_string())?;
            if matches!(
                receipt.status.as_str(),
                "starting" | "running" | "needs_attention"
            ) {
                receipt.status = "interrupted".into();
                receipt.cursor += 1;
                this.save(&receipt)?;
            }
            this.records.insert(receipt.run_id.clone(), receipt);
        }
        Ok(this)
    }
    pub fn previous(&self, request: &str) -> Option<&Receipt> {
        self.records.values().find(|r| r.request_id == request)
    }
    pub fn get(&self, id: &str) -> Option<&Receipt> {
        self.records.get(id)
    }
    pub fn list(&self) -> Vec<&Receipt> {
        self.records.values().collect()
    }
    pub fn begin(&mut self, request: &str, console: &str, kind: &str) -> Result<Receipt, String> {
        if request.is_empty()
            || request.len() > 128
            || !request
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err("request_id must be 1-128 ASCII letters/digits, dash, dot or underscore; never put credentials in it".into());
        }
        if let Some(old) = self.previous(request) {
            return Ok(old.clone());
        }
        let receipt = Receipt {
            run_id: nonce()?,
            request_id: request.into(),
            console: console.into(),
            kind: kind.into(),
            status: "starting".into(),
            cursor: 1,
        };
        self.save(&receipt)?;
        self.records.insert(receipt.run_id.clone(), receipt.clone());
        Ok(receipt)
    }
    pub fn update(&mut self, id: &str, status: &str) -> Result<(), String> {
        self.advance(id, status, false)
    }
    /// Every externally visible progress change has a durable monotonic cursor.
    pub fn advance(
        &mut self,
        id: &str,
        status: &str,
        progress_changed: bool,
    ) -> Result<(), String> {
        let mut receipt = self.get(id).ok_or("Unknown run")?.clone();
        if receipt.status == status && !progress_changed {
            return Ok(());
        }
        receipt.status = status.into();
        receipt.cursor += 1;
        self.save(&receipt)?;
        self.records.insert(id.into(), receipt);
        Ok(())
    }
    fn save(&self, receipt: &Receipt) -> Result<(), String> {
        let path = self
            .dir
            .join(format!("{}.json", digest(receipt.run_id.as_bytes())));
        let tmp = self.dir.join(format!("{}.tmp", nonce()?));
        let result = (|| {
            let mut file = private_file(&tmp).map_err(|e| e.to_string())?;
            file.write_all(&serde_json::to_vec(receipt).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                std::fs::File::open(&self.dir)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(tmp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_marks_uncertain_effects_and_deduplicates_requests() {
        let dir = std::env::temp_dir().join(nonce().unwrap());
        let mut runs = Runs::open(&dir).unwrap();
        let first = runs.begin("once", "console-1", "workflow").unwrap();
        assert_eq!(first, runs.begin("once", "console-1", "workflow").unwrap());
        runs.update(&first.run_id, "running").unwrap();
        drop(runs);
        let mut runs = Runs::open(&dir).unwrap();
        assert_eq!(
            runs.begin("once", "console-1", "workflow").unwrap().status,
            "interrupted"
        );
        assert_eq!(runs.list().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn progress_cursors_remain_monotonic_across_restart() {
        let dir = std::env::temp_dir().join(nonce().unwrap());
        let mut runs = Runs::open(&dir).unwrap();
        let receipt = runs.begin("progress", "one", "workflow").unwrap();
        for _ in 0..20 {
            runs.advance(&receipt.run_id, "running", true).unwrap();
        }
        let before = runs.get(&receipt.run_id).unwrap().cursor;
        drop(runs);
        let runs = Runs::open(&dir).unwrap();
        assert!(runs.get(&receipt.run_id).unwrap().cursor > before);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

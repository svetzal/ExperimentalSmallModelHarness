use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug)]
pub struct TraceRecorder {
    path: PathBuf,
    file: Mutex<File>,
}

impl TraceRecorder {
    pub fn create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating trace directory {}", dir.display()))?;
        let filename = format!("run-{}.jsonl", Utc::now().format("%Y%m%dT%H%M%SZ"));
        let path = dir.join(filename);
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("creating trace file {}", path.display()))?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn event(&self, kind: &str, payload: impl Serialize) -> Result<()> {
        let payload = serde_json::to_value(payload)?;
        self.value_event(kind, payload)
    }

    pub fn value_event(&self, kind: &str, mut payload: Value) -> Result<()> {
        let credential_redacted =
            crate::recovery::redact_value(&mut payload, &crate::recovery::credentials());
        let record = TraceEvent {
            timestamp: Utc::now(),
            kind,
            payload,
            credential_redacted: credential_redacted.then_some(true),
            exact: credential_redacted.then_some(false),
        };
        let mut file = self.file.lock().expect("trace mutex poisoned");
        serde_json::to_writer(&mut *file, &record)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct TraceEvent<'a> {
    timestamp: DateTime<Utc>,
    kind: &'a str,
    payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_redacted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exact: Option<bool>,
}

pub fn error_payload(error: &anyhow::Error) -> Value {
    json!({ "error": error.to_string() })
}

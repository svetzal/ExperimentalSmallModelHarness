//! User-facing defaults only; run policy remains in the existing runtime.
use crate::agent::Provider;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema_version: String,
    pub provider: Provider,
    pub model: String,
    /// None preserves the provider's model default, rather than inventing effort.
    pub reasoning_effort: Option<String>,
    /// Advanced: all standing context as a fraction of the window, recorded
    /// against in traces. Default 0.15.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standing_context_limit: Option<f64>,
    /// Advanced: the guidance budget as a fraction of the window, between
    /// 500 and 8,000 tokens. Default 0.125.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance_fraction: Option<f64>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: "microgentic_config.v1".into(),
            provider: Provider::default(),
            model: Provider::default().default_model().into(),
            reasoning_effort: None,
            standing_context_limit: None,
            guidance_fraction: None,
        }
    }
}

impl Preferences {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != "microgentic_config.v1" {
            bail!(
                "unsupported configuration version {:?}",
                self.schema_version
            );
        }
        if !valid_model_id(&self.model) {
            bail!("default model must be a nonempty model ID without control characters");
        }
        if let Some(value) = &self.reasoning_effort {
            crate::cli::parse_reasoning_effort(value).map_err(anyhow::Error::msg)?;
        }
        for (key, value) in [
            ("standing_context_limit", self.standing_context_limit),
            ("guidance_fraction", self.guidance_fraction),
        ] {
            if let Some(value) = value {
                crate::residency::validate_fraction(key, value).map_err(anyhow::Error::msg)?;
            }
        }
        Ok(())
    }
}

/// A model ID is non-empty and has no control characters, including newlines.
pub fn valid_model_id(model: &str) -> bool {
    !model.trim().is_empty() && !model.chars().any(char::is_control)
}

pub fn config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(base) if !base.is_empty() => PathBuf::from(base),
        _ => PathBuf::from(
            std::env::var_os("HOME").context("HOME is unavailable; use --config PATH")?,
        )
        .join(".config"),
    };
    Ok(base.join("microgentic/config.json"))
}

pub fn load(path: &Path) -> Result<Option<Preferences>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let config: Preferences = serde_json::from_str(&text).with_context(|| {
        format!(
            "invalid configuration {}; repair this JSON file, then retry microgentic setup",
            path.display()
        )
    })?;
    config
        .validate()
        .with_context(|| format!("invalid configuration {}", path.display()))?;
    Ok(Some(config))
}

pub fn save(path: &Path, preferences: &Preferences) -> Result<()> {
    preferences.validate()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, preferences)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)
        .with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

pub fn state_root() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(base) if !base.is_empty() => PathBuf::from(base),
        _ => PathBuf::from(
            std::env::var_os("HOME").context("HOME is unavailable; set XDG_STATE_HOME")?,
        )
        .join(".local/state"),
    };
    Ok(base.join("microgentic/runs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_is_reusable_and_model_default_effort_stays_unset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert!(load(&path).unwrap().is_none());
        save(&path, &Preferences::default()).unwrap();
        let mut prefs = load(&path).unwrap().unwrap();
        assert!(prefs.reasoning_effort.is_none());
        prefs.provider = Provider::Ollama;
        prefs.model = "my-model".into();
        prefs.reasoning_effort = Some("low".into());
        save(&path, &prefs).unwrap();
        assert_eq!(load(&path).unwrap().unwrap().model, "my-model");
        prefs.reasoning_effort = Some("unsupported".into());
        assert!(save(&path, &prefs).is_err());
        assert_eq!(
            load(&path).unwrap().unwrap().reasoning_effort.as_deref(),
            Some("low")
        );
    }
}

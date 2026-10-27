//! Named presets: a set of (feature, value) assignments per display.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::config::{config_dir, write_atomic};
use crate::vcp;

pub fn presets_path() -> PathBuf {
    config_dir().join("presets.json")
}

pub fn validate_spec(spec: &BTreeMap<String, Value>) -> Result<()> {
    for (feature, value) in spec {
        let code = vcp::feature_code(feature).map_err(anyhow::Error::msg)?;
        vcp::parse_value(code, value).map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

pub struct Presets {
    pub path: PathBuf,
    pub presets: BTreeMap<String, BTreeMap<String, Value>>,
}

impl Presets {
    pub fn new() -> Result<Self> {
        Self::with_path(presets_path())
    }

    /// Load the store. A missing file means no presets. A file that exists but
    /// cannot be parsed is an error, so a later save cannot silently replace it.
    pub fn with_path(path: PathBuf) -> Result<Self> {
        let presets: BTreeMap<String, BTreeMap<String, Value>> =
            match std::fs::read_to_string(&path) {
                Ok(text) => serde_json::from_str(&text).with_context(|| {
                    format!(
                        "{} is not valid preset JSON; fix or move it aside",
                        path.display()
                    )
                })?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(err) => {
                    return Err(err).with_context(|| format!("could not read {}", path.display()))
                }
            };
        Ok(Presets { path, presets })
    }

    pub fn save(&self) -> Result<()> {
        write_atomic(&self.path, &serde_json::to_string_pretty(&self.presets)?)
    }

    pub fn names(&self) -> Vec<String> {
        self.presets.keys().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<&BTreeMap<String, Value>> {
        self.presets.get(name)
    }

    pub fn set(&mut self, name: &str, spec: BTreeMap<String, Value>) -> Result<()> {
        validate_spec(&spec)?;
        self.presets.insert(name.to_string(), spec);
        self.save()
    }

    pub fn delete(&mut self, name: &str) -> Result<()> {
        if self.presets.remove(name).is_none() {
            bail!("no preset named {}", vcp::py_repr(name));
        }
        self.save()
    }
}

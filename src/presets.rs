//! Named presets: a set of (feature, value) assignments per display.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::config::config_dir;
use crate::vcp;

pub fn presets_path() -> PathBuf {
    config_dir().join("presets.json")
}

pub fn validate_spec(spec: &BTreeMap<String, Value>) -> Result<()> {
    // Raise for unknown feature/value tokens in spec dict.
    for (feature, value) in spec {
        let code = vcp::feature_code(feature).map_err(|e| anyhow!(e))?;
        vcp::parse_value(code, value).map_err(|e| anyhow!(e))?;
    }
    Ok(())
}

pub struct Presets {
    pub path: PathBuf,
    pub presets: BTreeMap<String, BTreeMap<String, Value>>,
}

impl Default for Presets {
    fn default() -> Self {
        Self::new()
    }
}

impl Presets {
    pub fn new() -> Self {
        Self::with_path(presets_path())
    }

    pub fn with_path(path: PathBuf) -> Self {
        let mut p = Presets {
            path,
            presets: BTreeMap::new(),
        };
        p.load();
        p
    }

    pub fn load(&mut self) {
        let Ok(data) = std::fs::read_to_string(&self.path) else {
            return;
        };
        match serde_json::from_str(&data) {
            Ok(presets) => self.presets = presets,
            Err(_) => self.presets = BTreeMap::new(),
        }
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(&self.presets)?)?;
        Ok(())
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
            anyhow::bail!("no preset named {}", vcp::py_repr(name));
        }
        self.save()
    }
}

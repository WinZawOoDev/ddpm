//! Application configuration stored in ~/.config/ddpm/config.json.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};

pub const DEFAULT_KVM_PORT: &str = "1-1";
pub const DEFAULT_DRIVER: &str = "auto";

pub fn config_dir() -> PathBuf {
    match std::env::var_os("DDPM_CONFIG_DIR") {
        Some(v) => PathBuf::from(v),
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".config/ddpm")
        }
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub struct Config {
    pub configfile: PathBuf,
    pub config: Map<String, Value>,
}

impl Config {
    pub fn new() -> Result<Self> {
        Self::with_path(config_path())
    }

    pub fn with_path(path: PathBuf) -> Result<Self> {
        let mut config = Map::new();
        config.insert("kvm_port".into(), Value::String(DEFAULT_KVM_PORT.into()));
        config.insert("driver".into(), Value::String(DEFAULT_DRIVER.into()));
        config.insert("monitors".into(), Value::Object(Map::new()));
        config.insert("map".into(), Value::Object(Map::new()));
        let mut cfg = Config {
            configfile: path,
            config,
        };
        cfg.load()?;
        Ok(cfg)
    }

    pub fn load(&mut self) -> Result<()> {
        if !self.configfile.exists() {
            return Ok(());
        }
        let data: Value =
            serde_json::from_str(&std::fs::read_to_string(&self.configfile).map_err(|e| {
                anyhow!(
                    "Could not read config file {}: {}",
                    self.configfile.display(),
                    e
                )
            })?)
            .map_err(|e| {
                anyhow!(
                    "Invalid JSON in config file {}: {}",
                    self.configfile.display(),
                    e
                )
            })?;
        if let Value::Object(map) = data {
            for (k, v) in map {
                self.config.insert(k, v);
            }
        }
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.configfile.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Could not write config file {}", self.configfile.display())
            })?;
        }
        std::fs::write(
            &self.configfile,
            serde_json::to_string_pretty(&self.config).with_context(|| {
                format!("Could not write config file {}", self.configfile.display())
            })?,
        )
        .map_err(|e| {
            anyhow!(
                "Could not write config file {}: {}",
                self.configfile.display(),
                e
            )
        })?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.config.get(key)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.config.insert(key.to_string(), value);
    }

    pub fn unset(&mut self, key: &str) -> Result<()> {
        if self.config.remove(key).is_none() {
            anyhow::bail!("Key {} not in config", crate::vcp::py_repr(key));
        }
        Ok(())
    }

    pub fn get_map(&self) -> Map<String, Value> {
        match self.config.get("map") {
            Some(Value::Object(map)) => map.clone(),
            _ => Map::new(),
        }
    }

    pub fn mono_attr(&self, mono: &str, key: &str) -> Option<&Value> {
        self.config.get("monitors")?.get(mono)?.get(key)
    }
}

/// Convenience alias matching the Python module's shape (kept BTreeMap-based
/// elsewhere); JSON objects stay ordered like Python dicts here.
pub type JsonMap = BTreeMap<String, Value>;

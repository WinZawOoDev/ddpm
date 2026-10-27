//! Application configuration stored in ~/.config/ddpm/config.json.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{Map, Value};

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

/// Write `data` to `path` through a temporary file and a rename, so an
/// interrupted write cannot leave a truncated file behind.
pub fn write_atomic(path: &Path, data: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data).with_context(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("could not replace {}", path.display()))
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
        let text = std::fs::read_to_string(&self.configfile).map_err(|e| {
            anyhow!(
                "Could not read config file {}: {}",
                self.configfile.display(),
                e
            )
        })?;
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            anyhow!(
                "Invalid JSON in config file {}: {}",
                self.configfile.display(),
                e
            )
        })?;
        match data {
            Value::Object(map) => {
                for (k, v) in map {
                    self.config.insert(k, v);
                }
                Ok(())
            }
            _ => bail!(
                "config file {} must contain a JSON object",
                self.configfile.display()
            ),
        }
    }

    pub fn save(&self) -> Result<()> {
        write_atomic(
            &self.configfile,
            &serde_json::to_string_pretty(&self.config)?,
        )
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.config.get(key)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.config.insert(key.to_string(), value);
    }

    pub fn unset(&mut self, key: &str) -> Result<()> {
        if self.config.remove(key).is_none() {
            bail!("Key {} not in config", crate::vcp::py_repr(key));
        }
        Ok(())
    }

    pub fn get_map(&self) -> Map<String, Value> {
        match self.config.get("map") {
            Some(Value::Object(map)) => map.clone(),
            _ => Map::new(),
        }
    }
}

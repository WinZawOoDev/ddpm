//! ddpm command line interface.

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::Value;

use crate::config::Config;
use crate::ddcci;
use crate::driver::{monitors, py_round, Monitor, SCALER_CODES};
use crate::presets::Presets;
use crate::vcp;

#[derive(ValueEnum, Clone, Debug)]
pub enum BackendSel {
    Auto,
    Ddcutil,
    Ddcci,
    Mock,
}

impl BackendSel {
    fn as_str(&self) -> &'static str {
        match self {
            BackendSel::Auto => "auto",
            BackendSel::Ddcutil => "ddcutil",
            BackendSel::Ddcci => "ddcci",
            BackendSel::Mock => "mock",
        }
    }
}

#[derive(Subcommand, Clone, Debug, ValueEnum)]
enum PresetAction {
    /// snapshot current settings (or use FEATURE=VALUE pairs)
    Save,
    /// apply a saved preset
    Apply,
    /// list saved presets
    List,
    /// remove a preset
    Rm,
}

#[derive(Subcommand, Clone, Debug, ValueEnum)]
enum ConfigAction {
    /// print the config file path
    Path,
    /// print the config as JSON
    Get,
    /// set KEY to VALUE
    Set,
    /// remove a key
    Unset,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// enumerate displays
    List {
        #[arg(short, long)]
        verbose: bool,
    },
    /// scan i2c buses directly for displays
    Detect,
    /// read feature values
    Get {
        display: String,
        #[arg(required = true)]
        features: Vec<String>,
    },
    /// set features: set DISPLAY FEATURE=VALUE ...
    Set {
        display: String,
        #[arg(required = true)]
        pairs: Vec<String>,
    },
    /// save/apply/list/rm display presets
    Preset {
        #[arg(value_enum)]
        action: PresetAction,
        name: Option<String>,
        #[arg(value_name = "FEATURE=VALUE")]
        pairs: Vec<String>,
        /// display for save/apply
        #[arg(short, long)]
        display: Option<String>,
    },
    /// config path/get/set/unset
    Config {
        #[arg(value_enum)]
        action: ConfigAction,
        key: Option<String>,
        value: Option<String>,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "ddpm",
    about = "Dell Display and Peripheral Manager clone for Ubuntu - controls monitors over DDC/CI (brightness, contrast, input source, power, volume, presets...)"
)]
pub struct Cli {
    /// backend driver: auto (default), ddcutil, direct i2c (ddcci), mock
    #[arg(long, value_enum, ignore_case = true)]
    backend: Option<BackendSel>,

    #[command(subcommand)]
    command: Command,
}

fn die(msg: &str) -> ! {
    eprintln!("Error: {}", msg);
    std::process::exit(1);
}

fn backend_selector(cli: &Cli) -> String {
    cli.backend
        .as_ref()
        .map(|b| b.as_str().to_string())
        .or_else(|| {
            Config::new()
                .ok()
                .and_then(|c| c.get("driver").and_then(|v| v.as_str()).map(String::from))
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "auto".to_string())
}

fn mons_of(cli: &Cli) -> Vec<Monitor> {
    match monitors(&backend_selector(cli)) {
        Ok((_, mons)) => mons,
        Err(e) => die(&e.to_string()),
    }
}

fn pick_monitor(ident: &str, mons: &[Monitor], cfg: &Config) -> Monitor {
    let mapping = cfg.get_map();
    if let Some(target) = mapping.get(ident) {
        let target = target.as_str().unwrap_or("");
        for m in mons {
            if m.uuid() == target || m.serial() == Some(target) {
                return m.clone();
            }
        }
        die(&format!(
            "{} maps to {}, which is not connected",
            vcp::py_repr(ident),
            vcp::py_repr(target)
        ));
    }
    for m in mons {
        // match uuid (== id, the bus), serial, or model name
        if ident == m.uuid() || m.serial() == Some(ident) || ident == m.name() {
            return m.clone();
        }
    }
    if let Ok(num) = ident.parse::<usize>() {
        if (1..=mons.len()).contains(&num) {
            return mons[num - 1].clone();
        }
    }
    if !mons.is_empty() {
        let names: Vec<&str> = mons.iter().map(|m| m.name()).collect();
        eprintln!(
            "Error: display {} not found. Known displays: {}",
            vcp::py_repr(ident),
            names.join(", ")
        );
        std::process::exit(2);
    }
    die("No displays found. Try 'ddpm detect'.\nTip: sudo apt install ddcutil, and/or run 'sudo modprobe i2c-dev'.");
}

pub fn fmt_value(code: u8, value: i64) -> String {
    if let Some(mapping) = vcp::value_map(code) {
        for (k, name) in mapping {
            if *k as i64 == value {
                return format!("{} (0x{:02X})", name, value);
            }
        }
    }
    value.to_string()
}

/// ['brightness=60', 'input=dp1'] -> [(code, raw value token), ...]
fn parse_pairs(pairs: &[String]) -> Result<Vec<(u8, String)>> {
    let mut out = Vec::new();
    for tok in pairs {
        let Some((f, val)) = tok.split_once('=') else {
            return Err(anyhow!("expected FEATURE=VALUE, got {}", vcp::py_repr(tok)));
        };
        let code = vcp::feature_code(f).map_err(|e| anyhow!(e))?;
        out.push((code, val.to_string()));
    }
    Ok(out)
}

fn pairs_to_spec(pairs: &[String]) -> Result<BTreeMap<String, Value>> {
    let mut spec = BTreeMap::new();
    for (code, raw) in parse_pairs(pairs)? {
        let val = vcp::parse_value(code, &Value::String(raw)).map_err(|e| anyhow!(e))?;
        spec.insert(vcp::feature_name(code), Value::from(val));
    }
    Ok(spec)
}

/// Read all standard features of `mon` into a preset spec dict.
pub fn snapshot(mon: &Monitor) -> BTreeMap<String, Value> {
    let mut spec = BTreeMap::new();
    for &code in vcp::VALUE_MAP_CODES {
        let token = vcp::feature_name(code);
        let g = match mon.get_code(code) {
            Ok(g) => g,
            Err(_) => continue,
        };
        let Some(value) = g.value else { continue };
        let val = if SCALER_CODES.contains(&code) {
            let mx = g.max.filter(|m| *m != 0).unwrap_or(100);
            Value::from(py_round(value as f64 * 100.0 / mx as f64))
        } else {
            match vcp::value_map(code).and_then(|m| m.iter().find(|(k, _)| *k == value)) {
                Some((_, name)) => Value::String(name.to_string()),
                None => Value::from(value),
            }
        };
        spec.insert(token, val);
    }
    spec
}

fn cmd_list(cli: &Cli, verbose: bool) -> i32 {
    let mons = mons_of(cli);
    if mons.is_empty() {
        println!("No displays found.");
        println!("Try: sudo apt install ddcutil ; sudo modprobe i2c-dev");
        return 1;
    }
    let sel = backend_selector(cli);
    println!("Displays: {} (backend: {})", mons.len(), sel);
    for (i, m) in mons.iter().enumerate() {
        let serial = match m.serial() {
            Some(s) => format!(" serial={}", s),
            None => String::new(),
        };
        println!(
            "  {}. {}  bus={}{}",
            i + 1,
            m.name(),
            m.bus().unwrap_or(""),
            serial
        );
        if verbose {
            match m.caps().first() {
                Some(_) => {
                    let caps: Vec<String> =
                        m.caps().iter().map(|&c| vcp::feature_name(c)).collect();
                    println!("     features: {}", caps.join(", "));
                }
                None => println!("     features:"),
            }
        }
    }
    0
}

fn cmd_detect(cli: &Cli) -> i32 {
    if backend_selector(cli) == "ddcutil" {
        return cmd_list(cli, false);
    }
    let mut found = false;
    for ident in ddcci::scan_buses(ddcci::DEFAULT_GLOB) {
        found = true;
        let info = &ident.info;
        println!(
            "  {:<11} mfr={} prod=0x{:04X} name={} serial={} ddcci={}",
            info.bus.as_deref().unwrap_or(""),
            info.mfr.as_deref().unwrap_or(""),
            info.prod.unwrap_or(0),
            vcp::py_repr(&info.name),
            match &info.serial {
                Some(s) => vcp::py_repr(s),
                None => "None".to_string(),
            },
            if ident.ddcci { "True" } else { "False" },
        );
    }
    if !found {
        println!("No displays detected on /dev/i2c-*.");
        println!("Check: ls /dev/i2c-*   and   sudo modprobe i2c-dev");
        return 1;
    }
    0
}

fn cmd_get(cli: &Cli, display: &str, features: &[String]) -> i32 {
    let cfg = match Config::new() {
        Ok(c) => c,
        Err(e) => die(&e.to_string()),
    };
    let mons = mons_of(cli);
    let mon = pick_monitor(display, &mons, &cfg);
    let mut rc = 0;
    for token in features {
        let code = match vcp::feature_code(token) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("{}: {}", token, e);
                rc = 1;
                continue;
            }
        };
        let g = match mon.get_code(code) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("{}: read failed: {}", token, e);
                rc = 1;
                continue;
            }
        };
        let Some(value) = g.value else {
            eprintln!("{}: not supported", token);
            rc = 1;
            continue;
        };
        if SCALER_CODES.contains(&code) {
            let mx = g.max.filter(|m| *m != 0).unwrap_or(100);
            let val = py_round(value as f64 * 100.0 / mx as f64);
            println!(
                "{:<14} {}/100",
                format!("{}:", vcp::feature_name(code)),
                fmt_value(code, val)
            );
        } else {
            println!(
                "{:<14} {}",
                format!("{}:", vcp::feature_name(code)),
                fmt_value(code, value as i64)
            );
        }
    }
    rc
}

fn cmd_set(cli: &Cli, display: &str, pairs: &[String]) -> i32 {
    let cfg = match Config::new() {
        Ok(c) => c,
        Err(e) => die(&e.to_string()),
    };
    let mons = mons_of(cli);
    let mon = pick_monitor(display, &mons, &cfg);
    let spec = match pairs_to_spec(pairs) {
        Ok(spec) => spec,
        Err(e) => die(&e.to_string()),
    };
    let mut rc = 0;
    for (token, val) in &spec {
        if let Err(e) = mon.set(token, val) {
            eprintln!("{}={} failed: {}", token, val, e);
            rc = 1;
        }
    }
    rc
}

/// Compact, Python-json-style rendering: {"a": 1, "b": "x"}
fn py_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let items: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        serde_json::to_string(k).unwrap_or_default(),
                        py_json(v)
                    )
                })
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        other => other.to_string(),
    }
}

fn cmd_preset(
    cli: &Cli,
    action: &PresetAction,
    name: &Option<String>,
    pairs: &[String],
    display: &Option<String>,
) -> i32 {
    let cfg = match Config::new() {
        Ok(c) => c,
        Err(e) => die(&e.to_string()),
    };
    let mut ps = match Presets::new() {
        Ok(ps) => ps,
        Err(e) => die(&e.to_string()),
    };
    match action {
        PresetAction::List => {
            if ps.names().is_empty() {
                println!("no presets saved; try: ddpm preset save work");
                return 0;
            }
            for name in ps.names() {
                let spec = &ps.presets[&name];
                let spec_json = py_json(&Value::Object(
                    spec.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                ));
                println!("{:<16} {}", name, spec_json);
            }
            0
        }
        PresetAction::Rm => {
            let Some(name) = name else {
                die("preset rm requires a name");
            };
            if let Err(e) = ps.delete(name) {
                die(&e.to_string());
            }
            println!("removed preset {}", vcp::py_repr(name));
            0
        }
        PresetAction::Save => {
            let Some(name) = name else {
                die("preset save requires a name");
            };
            let mons = mons_of(cli);
            let mon = pick_monitor(display.as_deref().unwrap_or("1"), &mons, &cfg);
            let spec = if !pairs.is_empty() {
                match pairs_to_spec(pairs) {
                    Ok(spec) => spec,
                    Err(e) => die(&e.to_string()),
                }
            } else {
                snapshot(&mon)
            };
            if let Err(e) = ps.set(name, spec.clone()) {
                die(&e.to_string());
            }
            let spec_json = py_json(&Value::Object(
                spec.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            ));
            println!("saved preset {}: {}", vcp::py_repr(name), spec_json);
            0
        }
        PresetAction::Apply => {
            let Some(name) = name else {
                die("preset apply requires a name");
            };
            let Some(spec) = ps.get(name).cloned() else {
                die(&format!("no preset named {}", vcp::py_repr(name)));
            };
            let mons = mons_of(cli);
            let mon = pick_monitor(display.as_deref().unwrap_or("1"), &mons, &cfg);
            let mut rc = 0;
            for (f, val) in &spec {
                if let Err(e) = mon.set(f, val) {
                    eprintln!("{}={} failed: {}", f, val, e);
                    rc = 1;
                }
            }
            rc
        }
    }
}

fn cmd_config(action: &ConfigAction, key: &Option<String>, value: &Option<String>) -> i32 {
    let mut cfg = match Config::new() {
        Ok(c) => c,
        Err(e) => die(&e.to_string()),
    };
    match action {
        ConfigAction::Path => {
            println!("{}", cfg.configfile.display());
            0
        }
        ConfigAction::Get => {
            let pretty =
                serde_json::to_string_pretty(&cfg.config).unwrap_or_else(|_| "{}".to_string());
            println!("{}", pretty);
            0
        }
        ConfigAction::Set => {
            let Some(key) = key else {
                die("config set requires KEY VALUE");
            };
            cfg.set(key, Value::String(value.clone().unwrap_or_default()));
            if let Err(e) = cfg.save() {
                die(&e.to_string());
            }
            println!("set {}={}", key, value.clone().unwrap_or_default());
            0
        }
        ConfigAction::Unset => {
            let Some(key) = key else {
                die("config unset requires KEY");
            };
            if let Err(e) = cfg.unset(key) {
                die(&e.to_string());
            }
            if let Err(e) = cfg.save() {
                die(&e.to_string());
            }
            0
        }
    }
}

pub fn main() -> i32 {
    let cli = Cli::parse();
    match &cli.command {
        Command::List { verbose } => cmd_list(&cli, *verbose),
        Command::Detect => cmd_detect(&cli),
        Command::Get { display, features } => cmd_get(&cli, display, features),
        Command::Set { display, pairs } => cmd_set(&cli, display, pairs),
        Command::Preset {
            action,
            name,
            pairs,
            display,
        } => cmd_preset(&cli, action, name, pairs, display),
        Command::Config { action, key, value } => cmd_config(action, key, value),
    }
}

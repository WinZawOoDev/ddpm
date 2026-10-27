//! Backend selection and the high-level Monitor abstraction.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use wait_timeout::ChildExt;

use crate::ddcci::{self, DDCCIDriver};
use crate::vcp;

pub const SCALER_CODES: &[u8] = &[vcp::CODE_BRIGHTNESS, vcp::CODE_CONTRAST, vcp::CODE_VOLUME];

/// Identity of one display, as reported by a backend.
#[derive(Debug, Clone, Default)]
pub struct DisplayInfo {
    pub bus: Option<String>,
    pub name: String,
    pub mfr: Option<String>,
    pub serial: Option<String>,
    pub prod: Option<u16>,
}

/// Result of reading one VCP feature from a display.
#[derive(Debug, Clone)]
pub struct VcpFrame {
    pub result: u8,
    pub code: u8,
    pub max: Option<u16>,
    pub value: Option<u16>,
}

pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    fn list_displays(&self) -> Result<Vec<DisplayInfo>>;
    fn capabilities(&self, display: &DisplayInfo) -> Option<Vec<u8>>;
    fn get(&self, display: &DisplayInfo, code: u8) -> Result<VcpFrame>;
    fn set(&self, display: &DisplayInfo, code: u8, value: u16) -> Result<()>;
}

/// Python's round(): ties round half to even.
pub fn py_round(x: f64) -> i64 {
    let f = x.floor();
    let diff = x - f;
    if diff > 0.5 || (diff == 0.5 && (f as i64) % 2 != 0) {
        (f + 1.0) as i64
    } else {
        f as i64
    }
}

/// High-level view of one display, backed by a shared backend.
#[derive(Clone)]
pub struct Monitor {
    backend: Arc<dyn Backend>,
    pub info: DisplayInfo,
}

impl Monitor {
    pub fn new(backend: Arc<dyn Backend>, info: DisplayInfo) -> Self {
        Monitor { backend, info }
    }

    pub fn id(&self) -> String {
        self.info
            .bus
            .clone()
            .unwrap_or_else(|| self.info.name.clone())
    }

    pub fn name(&self) -> &str {
        &self.info.name
    }

    pub fn serial(&self) -> Option<&str> {
        self.info.serial.as_deref()
    }

    pub fn bus(&self) -> Option<&str> {
        self.info.bus.as_deref()
    }

    pub fn label(&self) -> String {
        match &self.info.serial {
            Some(serial) => format!("{} ({})", self.info.name, serial),
            None => self.info.name.clone(),
        }
    }

    pub fn uuid(&self) -> String {
        self.id()
    }

    pub fn caps(&self) -> Vec<u8> {
        if let Some(caps) = self.backend.capabilities(&self.info) {
            if !caps.is_empty() {
                return caps;
            }
        }
        let mut fallback = vec![
            vcp::CODE_BRIGHTNESS,
            vcp::CODE_CONTRAST,
            vcp::CODE_INPUT_SOURCE,
            vcp::CODE_POWER,
            vcp::CODE_VOLUME,
            vcp::CODE_MUTE,
            vcp::CODE_OSD,
            vcp::CODE_COLOR_PRESET,
        ];
        fallback.sort();
        fallback
    }

    /// Read a feature by VCP code.
    pub fn get_code(&self, code: u8) -> Result<VcpFrame> {
        self.backend.get(&self.info, code)
    }

    /// Read a feature by token (name or numeric/hex string).
    pub fn get(&self, feature: &str) -> Result<VcpFrame> {
        let code = vcp::feature_code(feature).map_err(anyhow::Error::msg)?;
        self.get_code(code)
    }

    pub fn max_of(&self, code: u8) -> Result<u16> {
        let g = self.get_code(code)?;
        Ok(match g.max {
            Some(max) if max != 0 => max,
            _ => vcp::max_default(code).unwrap_or(0xFFFF),
        })
    }

    /// Set a feature by token; `value` may be a number or a symbolic name.
    pub fn set(&self, feature: &str, value: &Value) -> Result<()> {
        let code = vcp::feature_code(feature).map_err(anyhow::Error::msg)?;
        let mut val = vcp::parse_value(code, value).map_err(anyhow::Error::msg)?;
        if SCALER_CODES.contains(&code) {
            val = py_round(val as f64 * self.max_of(code)? as f64 / 100.0);
        }
        if !(0..=u16::MAX as i64).contains(&val) {
            bail!("value {} out of range for 0x{:02X}", val, code);
        }
        self.backend.set(&self.info, code, val as u16)
    }

    /// Return (value, max) scaled to 0-100 percent for scaler features.
    pub fn get_scaled(&self, feature: &str) -> (Option<u16>, Option<u16>) {
        let code = match vcp::feature_code(feature) {
            Ok(code) => code,
            Err(_) => return (None, None),
        };
        let g = match self.get_code(code) {
            Ok(g) => g,
            Err(_) => return (None, None),
        };
        let Some(value) = g.value else {
            return (None, None);
        };
        let mx = match g.max {
            Some(max) if max != 0 => max,
            _ => vcp::max_default(code).unwrap_or(100),
        };
        if SCALER_CODES.contains(&code) {
            (
                Some(py_round(value as f64 * 100.0 / mx as f64) as u16),
                Some(100),
            )
        } else {
            (Some(value), Some(mx))
        }
    }

    pub fn set_scaled(&self, feature: &str, percent: i64) -> Result<()> {
        self.set(feature, &Value::from(percent))
    }

    /// Read (or set, when `value` is given) the brightness percentage.
    pub fn brightness(&self, value: Option<i64>) -> Option<u16> {
        match value {
            None => self.get_scaled("brightness").0,
            Some(v) => {
                self.set("brightness", &Value::from(v)).ok()?;
                None
            }
        }
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(bin);
        if p.is_file()
            && std::fs::metadata(&p)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        {
            return Some(p);
        }
    }
    None
}

/// Run a helper command with a timeout and return its stdout. A non-zero exit
/// becomes an error that quotes stderr (or stdout, when stderr is empty), so a
/// failed command is never mistaken for a successful one.
fn run(cmd: &mut Command, timeout: Duration) -> Result<String> {
    use std::io::Read;

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start {:?}", cmd))?;
    // Drain both pipes on their own threads, so a child that writes a lot
    // cannot block on a full pipe while we wait for it to exit.
    let mut stdout = child.stdout.take().context("stdout pipe missing")?;
    let mut stderr = child.stderr.take().context("stderr pipe missing")?;
    let out_reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let err_reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            bail!("command timed out after {}s: {:?}", timeout.as_secs(), cmd);
        }
    };
    let out = out_reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    if !status.success() {
        let detail = if err.trim().is_empty() {
            out.trim()
        } else {
            err.trim()
        };
        bail!(
            "{} exited with {}: {}",
            cmd.get_program().to_string_lossy(),
            status,
            detail
        );
    }
    Ok(out)
}

fn busno(display: &DisplayInfo) -> Option<u64> {
    let bus = display.bus.as_deref().unwrap_or("");
    bus.rsplit('-').next().and_then(|s| s.parse::<u64>().ok())
}

pub struct DdcutilBackend {
    bin: PathBuf,
    lock: Mutex<()>,
}

impl DdcutilBackend {
    pub fn new() -> Self {
        DdcutilBackend {
            bin: which("ddcutil").unwrap_or_else(|| PathBuf::from("ddcutil")),
            lock: Mutex::new(()),
        }
    }

    pub fn available() -> bool {
        which("ddcutil").is_some()
    }
}

impl Default for DdcutilBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for DdcutilBackend {
    fn name(&self) -> &'static str {
        "ddcutil"
    }

    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        let out = run(
            Command::new(&self.bin).arg("detect").arg("--terse"),
            Duration::from_secs(15),
        )?;
        let mut displays: Vec<DisplayInfo> = Vec::new();
        let mut cur: Option<DisplayInfo> = None;
        for ln in out.lines() {
            let trimmed = ln.trim();
            // Section headers: "Display:", "Display 1", "Invalid display".
            if trimmed.starts_with("Display:")
                || trimmed.starts_with("Display ")
                || trimmed.starts_with("Invalid display")
            {
                if let Some(prev) = cur.take() {
                    displays.push(prev);
                }
                cur = Some(DisplayInfo {
                    name: "Display".to_string(),
                    ..Default::default()
                });
                continue;
            }
            let Some(info) = cur.as_mut() else { continue };
            if let Some(val) = trimmed.strip_prefix("I2C bus:") {
                info.bus = Some(val.trim().to_string());
            } else if let Some(val) = trimmed.strip_prefix("Monitor:") {
                // "Monitor: MFR:MODEL:SERIAL" (ddcutil >= 2.0 --terse)
                let parts: Vec<&str> = val.trim().split(':').collect();
                if parts.len() == 3 {
                    if info.mfr.is_none() && !parts[0].is_empty() {
                        info.mfr = Some(parts[0].to_string());
                    }
                    if !parts[1].is_empty() && info.name == "Display" {
                        info.name = parts[1].to_string();
                    }
                    if info.serial.is_none() && !parts[2].is_empty() {
                        info.serial = Some(parts[2].to_string());
                    }
                }
            } else if let Some(val) = trimmed.strip_prefix("Model:") {
                info.name = val.trim().to_string();
            } else if let Some(val) = trimmed.strip_prefix("Serial number:") {
                info.serial = Some(val.trim().to_string());
            } else if let Some(val) = trimmed.strip_prefix("Manufacturer:") {
                info.mfr = Some(val.trim().to_string());
            }
        }
        if let Some(prev) = cur {
            displays.push(prev);
        }
        Ok(displays.into_iter().filter(|d| d.bus.is_some()).collect())
    }

    fn capabilities(&self, display: &DisplayInfo) -> Option<Vec<u8>> {
        let out = run(
            Command::new(&self.bin)
                .arg("--bus")
                .arg(busno(display).map(|b| b.to_string()).unwrap_or_default())
                .arg("capabilities"),
            Duration::from_secs(15),
        )
        .ok()?;
        let caps = if out.contains("vcp(") {
            ddcci::parse_caps_string(&out)
        } else {
            ddcci::parse_feature_lines(&out)
        };
        if caps.is_empty() {
            None
        } else {
            Some(caps)
        }
    }

    fn get(&self, display: &DisplayInfo, code: u8) -> Result<VcpFrame> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let out = run(
            Command::new(&self.bin)
                .arg("--bus")
                .arg(busno(display).map(|b| b.to_string()).unwrap_or_default())
                .arg("getvcp")
                .arg(format!("0x{:02x}", code)),
            Duration::from_secs(15),
        )?;
        let cur_re = regex::Regex::new(r"(?i)current(?:\s+value)?\s*=\s*0*(\d+)").unwrap();
        let max_re = regex::Regex::new(r"(?i)max(?:\s+value)?\s*=\s*0*(\d+)").unwrap();
        let m = cur_re.captures(&out);
        let mx = max_re.captures(&out);
        let Some(value) = m.and_then(|c| c[1].parse::<u16>().ok()) else {
            bail!(
                "could not parse ddcutil output for 0x{:02X}: {}",
                code,
                out.trim()
            );
        };
        Ok(VcpFrame {
            result: ddcci::RESULT_OK,
            code,
            value: Some(value),
            max: mx.and_then(|c| c[1].parse::<u16>().ok()),
        })
    }

    fn set(&self, display: &DisplayInfo, code: u8, value: u16) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        run(
            Command::new(&self.bin)
                .arg("--bus")
                .arg(busno(display).map(|b| b.to_string()).unwrap_or_default())
                .arg("setvcp")
                .arg(format!("0x{:02x}", code))
                .arg(value.to_string()),
            Duration::from_secs(30),
        )?;
        Ok(())
    }
}

struct MockDisplay {
    info: DisplayInfo,
    state: BTreeMap<u8, (u16, u16)>,
}

/// Deterministic in-memory display state for tests and CI without hardware.
/// Shared process-wide (like the Python class attribute).
fn mock_displays() -> &'static Mutex<Vec<MockDisplay>> {
    static MOCK: OnceLock<Mutex<Vec<MockDisplay>>> = OnceLock::new();
    MOCK.get_or_init(|| Mutex::new(default_mock_displays()))
}

fn default_mock_displays() -> Vec<MockDisplay> {
    vec![MockDisplay {
        info: DisplayInfo {
            bus: Some("mock-i2c-7".to_string()),
            name: "U2723QE".to_string(),
            serial: Some("FAKE-12345".to_string()),
            mfr: Some("DEL".to_string()),
            prod: Some(0xA0F1),
        },
        state: BTreeMap::from([
            (vcp::CODE_BRIGHTNESS, (45u16, 100u16)),
            (vcp::CODE_CONTRAST, (50, 100)),
            (vcp::CODE_INPUT_SOURCE, (0x0F, 0xFFFF)),
            (vcp::CODE_POWER, (0x01, 0xFFFF)),
            (vcp::CODE_VOLUME, (30, 100)),
            (vcp::CODE_MUTE, (0x01, 0xFFFF)),
            (vcp::CODE_OSD, (0x01, 0xFFFF)),
            (vcp::CODE_COLOR_PRESET, (0x02, 0xFFFF)),
        ]),
    }]
}

pub struct MockBackend;

impl MockBackend {
    pub fn new() -> Self {
        MockBackend
    }

    /// Restore the deterministic initial mock state.
    pub fn reset(&self) {
        let mut store = mock_displays().lock().unwrap_or_else(|p| p.into_inner());
        *store = default_mock_displays();
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        let store = mock_displays().lock().unwrap_or_else(|p| p.into_inner());
        Ok(store.iter().map(|d| d.info.clone()).collect())
    }

    fn capabilities(&self, display: &DisplayInfo) -> Option<Vec<u8>> {
        let store = mock_displays().lock().unwrap_or_else(|p| p.into_inner());
        store
            .iter()
            .find(|d| d.info.bus == display.bus)
            .map(|d| d.state.keys().copied().collect())
    }

    fn get(&self, display: &DisplayInfo, code: u8) -> Result<VcpFrame> {
        let store = mock_displays().lock().unwrap_or_else(|p| p.into_inner());
        let Some(d) = store.iter().find(|d| d.info.bus == display.bus) else {
            bail!("display {:?} not found", display.bus);
        };
        match d.state.get(&code) {
            None => Ok(VcpFrame {
                result: ddcci::RESULT_UNSUPPORTED,
                code,
                value: None,
                max: None,
            }),
            Some(&(cur, max)) => Ok(VcpFrame {
                result: 0,
                code,
                value: Some(cur),
                max: Some(max),
            }),
        }
    }

    fn set(&self, display: &DisplayInfo, code: u8, value: u16) -> Result<()> {
        let mut store = mock_displays().lock().unwrap_or_else(|p| p.into_inner());
        let Some(d) = store.iter_mut().find(|d| d.info.bus == display.bus) else {
            bail!("display {:?} not found", display.bus);
        };
        let Some(&(_, max)) = d.state.get(&code) else {
            bail!("VCP 0x{:02X} not supported", code);
        };
        d.state.insert(code, (value.min(max), max));
        Ok(())
    }
}

/// Pick a backend: explicit selector, or ddcutil if installed, else ddcci.
pub fn based_on_config(selector: &str) -> Result<Arc<dyn Backend>> {
    let selector = selector.trim().to_lowercase();
    if selector == "mock" || selector == "test" {
        return Ok(Arc::new(MockBackend::new()));
    }
    if selector == "ddcci" || selector == "i2c" {
        return Ok(Arc::new(DDCCIDriver::new()));
    }
    if selector == "ddcutil" {
        if !DdcutilBackend::available() {
            bail!("ddcutil binary not found on PATH");
        }
        return Ok(Arc::new(DdcutilBackend::new()));
    }
    if DdcutilBackend::available() {
        Ok(Arc::new(DdcutilBackend::new()))
    } else {
        Ok(Arc::new(DDCCIDriver::new()))
    }
}

pub fn monitors(selector: &str) -> Result<(Arc<dyn Backend>, Vec<Monitor>)> {
    let backend = based_on_config(selector)?;
    let mons = backend
        .list_displays()?
        .into_iter()
        .map(|info| Monitor::new(backend.clone(), info))
        .collect();
    Ok((backend, mons))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_stdout_on_success() {
        let out = run(
            Command::new("sh").arg("-c").arg("echo hi"),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.trim(), "hi");
    }

    #[test]
    fn run_turns_failure_into_error_with_stderr() {
        let err = run(
            Command::new("sh").arg("-c").arg("echo boom >&2; exit 3"),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn run_enforces_timeout() {
        let err = run(Command::new("sleep").arg("5"), Duration::from_millis(200)).unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
    }
}

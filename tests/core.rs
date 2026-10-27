//! Port of tests/test_core.py (pytest suite, no hardware required).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use ddpm::ddcci::{self, Bus, OP_GET_VCP};
use ddpm::driver::{based_on_config, MockBackend, Monitor};
use ddpm::presets::Presets;
use ddpm::vcp;
use serde_json::{json, Value};

/// Serialize tests that share the mock backend's global state (cargo runs
/// test threads in parallel; pytest ran sequentially).
fn mock_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let lock = LOCK.get_or_init(|| Mutex::new(()));
    lock.lock().unwrap_or_else(|p| p.into_inner())
}

fn tmpfile(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("ddpm-rs-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let n = N.fetch_add(1, Ordering::SeqCst);
    dir.join(format!(
        "{}-{}-{}.json",
        tag,
        n,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn mon() -> Monitor {
    MockBackend::new().reset();
    let backend = based_on_config("mock").unwrap();
    let info = backend.list_displays().unwrap().remove(0);
    Monitor::new(backend, info)
}

fn spec_of(value: Value) -> BTreeMap<String, Value> {
    serde_json::from_value(value).unwrap()
}

// --- helpers ported from the Python test suite ---

fn synth_edid(name: &str, serial: Option<&str>) -> Vec<u8> {
    let mut edid = vec![0u8; 128];
    edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x10]);
    let mfr: u16 = (4 << 10) | (5 << 5) | 12; // D, E, L
    edid[8..10].copy_from_slice(&mfr.to_be_bytes());
    edid[10] = 0xF1;
    edid[11] = 0xE0;
    edid[18] = 0x33; // year 2031
    let mut desc = vec![0, 0, 0, 0xFC, 0x00];
    let mut nb = name.as_bytes().to_vec();
    nb.truncate(13);
    nb.resize(13, b' ');
    desc.extend(nb);
    edid[54..72].copy_from_slice(&desc);
    if let Some(serial) = serial {
        let mut desc = vec![0, 0, 0, 0xFF, 0x00];
        let mut sb = serial.as_bytes().to_vec();
        sb.truncate(13);
        sb.resize(13, b' ');
        desc.extend(sb);
        edid[72..90].copy_from_slice(&desc);
    }
    let sum: u64 = edid[..127].iter().map(|&b| b as u64).sum();
    edid[127] = ((256 - (sum % 256)) % 256) as u8;
    edid
}

// --- tests ---

#[test]
fn test_vcp_feature_codes() {
    assert_eq!(vcp::feature_code("brightness").unwrap(), 0x10);
    assert_eq!(vcp::feature_code("BRIGHTNESS").unwrap(), 0x10);
    assert_eq!(vcp::feature_code("0x60").unwrap(), 0x60);
    assert_eq!(vcp::feature_code("input").unwrap(), 0x60);
    assert_eq!(vcp::feature_name(0x10), "brightness");
    assert!(vcp::feature_code("nope").is_err());
}

#[test]
fn test_vcp_parse_values() {
    assert_eq!(vcp::parse_value(0x10, &json!("60")).unwrap(), 60);
    assert_eq!(vcp::parse_value(0x60, &json!("dp1")).unwrap(), 0x0F);
    assert_eq!(vcp::parse_value(0x60, &json!("DP1")).unwrap(), 0x0F);
    assert_eq!(vcp::parse_value(0xD6, &json!("on")).unwrap(), 0x01);
    assert_eq!(vcp::parse_value(0xD6, &json!(4)).unwrap(), 4);
    assert!(vcp::parse_value(0x60, &json!("banana")).is_err());
}

#[test]
fn test_checksum_roundtrip() {
    let data = vec![0x51u8, 0x82, 0x01, 0x10];
    let rev: Vec<u8> = data.iter().rev().copied().collect();
    assert_eq!(ddcci::checksum(&data), ddcci::checksum(&rev[..4]));
}

#[test]
fn test_mock_monitor_set_get_scaled() {
    let _guard = mock_guard();
    let mon = mon();
    mon.set("brightness", &json!(80)).unwrap();
    let (val, mx) = mon.get_scaled("brightness");
    assert_eq!(val, Some(80));
    assert_eq!(mx, Some(100));
    mon.set("input", &json!("dp1")).unwrap();
    assert_eq!(mon.get("input").unwrap().value, Some(0x0F));
    let (val, _) = mon.get_scaled("input");
    assert_eq!(val, Some(0x0F));
}

#[test]
fn test_mock_monitor_power_and_preset_values() {
    let _guard = mock_guard();
    let mon = mon();
    mon.set("power", &json!("standby")).unwrap();
    assert_eq!(mon.get("power-state").unwrap().value, Some(0x05));
    mon.set("color-temp", &json!("6500k")).unwrap();
    assert_eq!(mon.get("color-temp").unwrap().value, Some(0x02));
}

#[test]
fn test_monitor_set_unsupported_feature_raises() {
    let _guard = mock_guard();
    let mon = mon();
    assert!(mon.set("0x77", &json!(3)).is_err());
}

#[test]
fn test_presets_roundtrip() {
    let _guard = mock_guard();
    let path = tmpfile("presets");
    let mut p = Presets::with_path(path.clone());
    p.set(
        "work",
        spec_of(json!({"brightness": 70, "input": "dp1", "power": "on", "0x62": 33})),
    )
    .unwrap();
    assert_eq!(p.names(), vec!["work"]);
    let spec = p.get("work").unwrap().clone();
    assert_eq!(spec["brightness"], 70);
    assert_eq!(spec["input"], "dp1");
    assert_eq!(spec["power"], "on");
    assert_eq!(spec["0x62"], 33);
    let mut p2 = Presets::with_path(path.clone());
    assert_eq!(p2.get("work").unwrap(), &spec);
    let mon = mon();
    mon.set("brightness", &json!(70)).unwrap();
    mon.set("input", &spec["input"]).unwrap();
    mon.set("power", &spec["power"]).unwrap();
    assert_eq!(mon.get("input").unwrap().value, Some(0x0F));
    assert_eq!(mon.get("power-state").unwrap().value, Some(0x01));
    p2.delete("work").unwrap();
    assert!(p2.delete("work").is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_presets_invalid_feature_rejected() {
    let _guard = mock_guard();
    let path = tmpfile("p");
    let mut p = Presets::with_path(path);
    assert!(p.set("bad", spec_of(json!({"banana": 1}))).is_err());
    assert!(p.set("bad2", spec_of(json!({"input": "hdmi9"}))).is_err());
}

#[test]
fn test_presets_snapshot_matches_mock_state() {
    let _guard = mock_guard();
    let mon = mon();
    mon.set("brightness", &json!(22)).unwrap();
    let spec = ddpm::cli::snapshot(&mon);
    assert_eq!(spec["brightness"], 22);
    assert_eq!(spec["input-source"], "dp1");
    let path = tmpfile("p");
    let mut p = Presets::with_path(path);
    p.set("snap", spec.clone()).unwrap();
    mon.set("brightness", &json!(88)).unwrap();
    for (f, v) in &spec {
        mon.set(f, v).unwrap();
    }
    assert_eq!(mon.brightness(None), Some(22));
}

#[test]
fn test_edid_helpers() {
    let edid = synth_edid("U2723QE", Some("FAKE-12345"));
    assert!(ddcci::valid_edid(&edid));
    let ident = ddcci::edid_identity(&edid);
    assert_eq!(ident.mfr, "DEL");
    assert_eq!(ident.name, "U2723QE");
    assert_eq!(ident.serial.as_deref(), Some("FAKE-12345"));
}

#[test]
fn test_frame_builder_validator() {
    let mut raw = vec![
        0x6Eu8, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x2D, 0x00,
    ];
    let ck = ddcci::xor(&[&[0x50u8], &raw[..10]].concat());
    *raw.last_mut().unwrap() = ck;
    let mut buf = raw.clone();
    buf.extend_from_slice(&[0x00, 0x00]);
    let frame = ddcci::find_frame(&buf, 0x10).unwrap();
    assert_eq!(frame.result, 0);
    assert_eq!(frame.value, 0x2D);
    assert_eq!(frame.max, 0x64);
}

#[test]
fn test_based_on_config_mock() {
    let b = based_on_config("mock").unwrap();
    assert_eq!(b.name(), "mock");
}

#[test]
fn test_getvcp_probe_bytes_xor() {
    let probe = [0x51u8, 0x02 | 0x80, OP_GET_VCP, 0x10];
    // Python asserted the checksum is an int; assert the concrete value.
    let ck = ddcci::checksum(&[&[0x6Eu8], &probe[..]].concat());
    assert_eq!(ck, ddcci::xor(&[0x6Eu8, 0x6E, 0x51, 0x82, 0x01, 0x10]));
}

#[test]
fn test_bus_open_error_message() {
    assert!(Bus::open("/nonexistent/ddpm-no-such-bus").is_err());
}

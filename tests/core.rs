//! Integration tests for the ddpm library. No hardware is required.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use ddpm::ddcci::{self, Bus};
use ddpm::driver::{based_on_config, MockBackend, Monitor};
use ddpm::presets::Presets;
use ddpm::vcp;
use serde_json::{json, Value};

/// Serialize tests that share the mock backend's global state (cargo runs
/// test threads in parallel).
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
fn test_negative_and_out_of_range_values_rejected() {
    let _guard = mock_guard();
    assert!(vcp::parse_value(0x10, &json!("-5")).is_err());
    assert!(vcp::parse_value(0x10, &json!(-5)).is_err());
    assert!(vcp::parse_value(0x10, &json!(150)).is_err());
    assert!(vcp::parse_value(0x10, &json!("1_0")).is_err());
    assert_eq!(vcp::parse_value(0x10, &json!("100")).unwrap(), 100);
    assert_eq!(vcp::parse_value(0x60, &json!("0x1F")).unwrap(), 0x1F);
    let mon = mon();
    assert!(mon.set("brightness", &json!(-5)).is_err());
    assert_eq!(mon.get_scaled("brightness").0, Some(45));
}

#[test]
fn test_color_presets_follow_mccs() {
    assert_eq!(vcp::parse_value(0x14, &json!("srgb")).unwrap(), 0x01);
    assert_eq!(vcp::parse_value(0x14, &json!("native")).unwrap(), 0x02);
    assert_eq!(vcp::parse_value(0x14, &json!("5000k")).unwrap(), 0x04);
    assert_eq!(vcp::parse_value(0x14, &json!("6500k")).unwrap(), 0x05);
    assert_eq!(vcp::parse_value(0x14, &json!("9300k")).unwrap(), 0x08);
    let name = vcp::value_map(0x14)
        .unwrap()
        .iter()
        .find(|(v, _)| *v == 0x05)
        .unwrap()
        .1;
    assert_eq!(name, "6500k");
}

#[test]
fn test_osd_and_power_values_follow_mccs() {
    assert_eq!(vcp::parse_value(0xCA, &json!("lock")).unwrap(), 0x01);
    assert_eq!(vcp::parse_value(0xCA, &json!("unlock")).unwrap(), 0x02);
    assert_eq!(vcp::parse_value(0xD6, &json!("standby")).unwrap(), 0x02);
    assert_eq!(vcp::parse_value(0xD6, &json!("off")).unwrap(), 0x04);
    assert_eq!(vcp::parse_value(0xD6, &json!("hard-off")).unwrap(), 0x05);
}

#[test]
fn test_checksum_known_vectors() {
    // GET_VCP brightness request: 51 82 01 10 AC
    assert_eq!(ddcci::checksum(&[0x51, 0x82, 0x01, 0x10]), 0xAC);
    // SET_VCP brightness = 0x55: 51 84 03 10 00 55 FD
    assert_eq!(ddcci::checksum(&[0x51, 0x84, 0x03, 0x10, 0x00, 0x55]), 0xFD);
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
    assert_eq!(mon.get("power-state").unwrap().value, Some(0x02));
    mon.set("power", &json!("hard-off")).unwrap();
    assert_eq!(mon.get("power-state").unwrap().value, Some(0x05));
    mon.set("color-temp", &json!("6500k")).unwrap();
    assert_eq!(mon.get("color-temp").unwrap().value, Some(0x05));
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
    let mut p = Presets::with_path(path.clone()).unwrap();
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
    let mut p2 = Presets::with_path(path.clone()).unwrap();
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
    let mut p = Presets::with_path(path).unwrap();
    assert!(p.set("bad", spec_of(json!({"banana": 1}))).is_err());
    assert!(p.set("bad2", spec_of(json!({"input": "hdmi9"}))).is_err());
    assert!(p.set("bad3", spec_of(json!({"brightness": 150}))).is_err());
}

#[test]
fn test_corrupt_presets_file_is_an_error_and_is_kept() {
    let path = tmpfile("corrupt");
    let garbage = "{\"broken\": ";
    std::fs::write(&path, garbage).unwrap();
    assert!(Presets::with_path(path.clone()).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), garbage);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_missing_presets_file_is_empty() {
    let path = tmpfile("missing");
    let _ = std::fs::remove_file(&path);
    let p = Presets::with_path(path).unwrap();
    assert!(p.names().is_empty());
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
    let mut p = Presets::with_path(path).unwrap();
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
fn test_caps_string_ignores_value_lists() {
    let caps = "(prot(monitor)type(lcd)model(U3011)cmds(01 02 03 07 0C E3 F3)vcp(02 04 05 06 08 10 12 14(01 05 08 0B 0C) 16 18 1A 52 60(01 03 04 0C 0F 11 12) AC AE B2 B6 C6 C8 C9 D6(01 04 05) DC(00 02 03 04 05) DF FD)mccs_ver(2.1)mswhql(1))";
    assert_eq!(
        ddcci::parse_caps_string(caps),
        vec![
            0x02, 0x04, 0x05, 0x06, 0x08, 0x10, 0x12, 0x14, 0x16, 0x18, 0x1A, 0x52, 0x60, 0xAC,
            0xAE, 0xB2, 0xB6, 0xC6, 0xC8, 0xC9, 0xD6, 0xDC, 0xDF, 0xFD
        ]
    );
}

#[test]
fn test_feature_lines_from_ddcutil_report() {
    let report = "VCP Features:\n   Feature: 02 (New control value)\n   Feature: 10 (Brightness)\n   Feature: 14 (Select color preset)\n      Values:\n         05: 6500 K\n   Feature: 60 (Input Source)\n   Feature: D6 (Power mode)\n";
    assert_eq!(
        ddcci::parse_feature_lines(report),
        vec![0x02, 0x10, 0x14, 0x60, 0xD6]
    );
}

#[test]
fn test_based_on_config_mock() {
    let b = based_on_config("mock").unwrap();
    assert_eq!(b.name(), "mock");
}

#[test]
fn test_bus_open_error_message() {
    assert!(Bus::open("/nonexistent/ddpm-no-such-bus").is_err());
}

/// A capabilities reply frame as a display would send it, checksum included.
fn caps_reply(offset: u16, fragment: &[u8]) -> Vec<u8> {
    let mut frame = vec![
        0x6Eu8,
        0x80 | (3 + fragment.len()) as u8,
        ddcci::OP_CAPS_REPLY,
        (offset >> 8) as u8,
        offset as u8,
    ];
    frame.extend_from_slice(fragment);
    let ck = ddcci::xor(&[&[0x50u8], &frame[..]].concat());
    frame.push(ck);
    frame
}

#[test]
fn test_caps_reply_is_matched_by_offset_and_checksum() {
    let frame = caps_reply(0, b"vcp(10 12)");
    assert_eq!(
        ddcci::find_caps_fragment(&frame, 0),
        Some(b"vcp(10 12)".to_vec())
    );
    assert_eq!(ddcci::find_caps_fragment(&frame, 4), None);
    let mut bad = frame.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    assert_eq!(ddcci::find_caps_fragment(&bad, 0), None);
}

#[test]
fn test_collect_fragments_reassembles_capabilities() {
    let text = b"(prot(monitor)vcp(10 12 14(05 08))mccs_ver(2.1))".to_vec();
    let served = text.clone();
    let got = ddcci::collect_fragments(|offset| {
        let start = offset as usize;
        let end = (start + 7).min(served.len());
        Ok(served.get(start..end).unwrap_or(&[]).to_vec())
    })
    .unwrap();
    assert_eq!(got, text);
}

#[test]
fn test_collect_fragments_is_bounded() {
    let err = ddcci::collect_fragments(|_| Ok(vec![b'x'; 100])).unwrap_err();
    assert!(err.to_string().contains("longer than"), "{err}");
}

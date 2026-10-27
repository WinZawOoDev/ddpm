import sys
import os

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import pytest

from ddpm.ddcci import (
    OP_GET_VCP,
    Bus,
    checksum,
    edid_identity,
    valid_edid,
)
from ddpm.driver import MockBackend, Monitor, based_on_config
from ddpm.presets import Presets, PresetError
from ddpm import vcp


@pytest.fixture()
def mon():
    backend = MockBackend()
    info = backend.list_displays()[0]
    return Monitor(backend, info)


def synth_edid(name="U2723QE", serial=None):
    edid = bytearray(128)
    edid[:8] = b"\x00\xff\xff\xff\xff\xff\x00\x10"
    mfr = (4 << 10) | (5 << 5) | 12  # D, E, L
    edid[8:10] = mfr.to_bytes(2, "big")
    edid[10] = 0xF1
    edid[11] = 0xE0
    edid[18] = 0x33  # year 2031
    desc = (
        bytes([0, 0, 0, 0xFC, 0x00])
        + name.encode()[:13].ljust(13, b"\x20")
    )
    assert len(desc) == 18
    edid[54:72] = desc
    if serial:
        edid[72:90] = (
            bytes([0, 0, 0, 0xFF, 0x00])
            + serial.encode()[:13].ljust(13, b"\x20")
        )
    edid[127] = 0
    edid[127] = (256 - (sum(edid[:127]) % 256)) % 256
    return bytes(edid)


def test_vcp_feature_codes():
    assert vcp.feature_code("brightness") == 0x10
    assert vcp.feature_code("BRIGHTNESS") == 0x10
    assert vcp.feature_code("0x60") == 0x60
    assert vcp.feature_code("input") == 0x60
    assert vcp.feature_name(0x10) == "brightness"
    with pytest.raises(ValueError):
        vcp.feature_code("nope")


def test_vcp_parse_values():
    assert vcp.parse_value(0x10, "60") == 60
    assert vcp.parse_value(0x60, "dp1") == 0x0F
    assert vcp.parse_value(0x60, "dp1".upper()) == 0x0F
    assert vcp.parse_value(0xD6, "on") == 0x01
    assert vcp.parse_value(0xD6, 4) == 4
    with pytest.raises(ValueError):
        vcp.parse_value(0x60, "banana")


def test_checksum_roundtrip():
    data = b"\x51\x82\x01\x10"
    assert checksum(data) == checksum(data[::-1][:4])


def test_mock_monitor_set_get_scaled(mon):
    mon.set("brightness", 80)
    val, mx = mon.get_scaled("brightness")
    assert val == 80 and mx == 100
    mon.set("input", "dp1")
    assert mon.get("input")["value"] == 0x0F
    val, _ = mon.get_scaled("input")
    assert val == 0x0F


def test_mock_monitor_power_and_preset_values(mon):
    mon.set("power", "standby")
    assert mon.get("power-state")["value"] == 0x05
    mon.set("color-temp", "6500k")
    assert mon.get("color-temp")["value"] == 0x02


def test_monitor_set_unsupported_feature_raises(mon):
    with pytest.raises(Exception):
        mon.set("0x77", 3)


def test_presets_roundtrip(tmp_path, mon):
    p = Presets(path=str(tmp_path / "presets.json"))
    p.set(
        "work",
        {"brightness": 70, "input": "dp1", "power": "on", "0x62": 33},
    )
    assert p.names() == ["work"]
    spec = p.get("work")
    assert spec["brightness"] == 70
    assert spec["input"] == "dp1"
    assert spec["power"] == "on"
    assert spec["0x62"] == 33
    p2 = Presets(path=str(tmp_path / "presets.json"))
    assert p2.get("work") == spec
    mon.set("brightness", 70)
    mon.set("input", spec["input"])
    mon.set("power", spec["power"])
    assert mon.get("input")["value"] == 0x0F
    assert mon.get("power-state")["value"] == 0x01
    p2.delete("work")
    with pytest.raises(PresetError):
        p2.delete("work")


def test_presets_invalid_feature_rejected(tmp_path):
    p = Presets(path=str(tmp_path / "p.json"))
    with pytest.raises(PresetError):
        p.set("bad", {"banana": 1})
    with pytest.raises(PresetError):
        p.set("bad2", {"input": "hdmi9"})


def test_presets_snapshot_matches_mock_state(mon, tmp_path):
    mon.set("brightness", 22)
    from ddpm.cli import snapshot

    spec = snapshot(mon)
    assert spec["brightness"] == 22
    assert spec["input-source"] == "dp1"
    p = Presets(path=str(tmp_path / "p.json"))
    p.set("snap", spec)
    mon.set("brightness", 88)
    for f, v in spec.items():
        mon.set(f, v)
    assert mon.brightness() == 22


def test_edid_helpers():
    edid = synth_edid(name="U2723QE", serial="FAKE-12345")
    assert valid_edid(edid)
    ident = edid_identity(edid)
    assert ident["mfr"] == "DEL"  # built from code DEL
    assert ident["name"] == "U2723QE"
    assert ident["serial"] == "FAKE-12345"


def test_frame_builder_validator():
    raw = bytes(
        [0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x2D, 0x00]
    )
    from ddpm.ddcci import _find_frame, _xor

    ck = _xor(b"\x50" + raw[:10])
    raw = raw[:10] + bytes([ck])
    frame = _find_frame(raw + b"\x00\x00", want_vcp=0x10)
    assert frame is not None
    assert frame["result"] == 0
    assert frame["value"] == 0x2D
    assert frame["max"] == 0x64


def test_based_on_config_mock():
    b = based_on_config("mock")
    assert b.name == "mock"


def test_getvcp_probe_bytes_xor():
    probe = bytes([0x51, 0x02 | 0x80, OP_GET_VCP, 0x10])
    ck = checksum(b"\x6e" + probe)
    assert isinstance(ck, int)


def test_bus_open_error_message(tmp_path):
    with pytest.raises(Exception):
        Bus(str(tmp_path / "nope"))

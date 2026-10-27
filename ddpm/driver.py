"""Backend selection and the high-level Monitor abstraction."""
import re
import shutil
import subprocess
import threading

from . import ddcci, vcp

SCALER_CODES = (vcp.CODE_BRIGHTNESS, vcp.CODE_CONTRAST, vcp.CODE_VOLUME)


def _run(cmd, timeout=15):
    return subprocess.run(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=timeout,
        check=False,
    ).stdout


def _busno(display):
    bus = display.get("bus") or ""
    try:
        return int(bus.rsplit("-", 1)[-1])
    except ValueError:
        return None


class Monitor:
    """High-level view of one display, backed by a driver dict."""

    def __init__(self, driver, info):
        self._driver = driver
        self.info = info
        self.id = info.get("bus") or info.get("name")
        self.name = info.get("name", "Display")
        self.serial = info.get("serial")
        self.mfr = info.get("mfr")
        self.bus = info.get("bus")

    def __repr__(self):
        return "<Monitor %s bus=%s>" % (self.name, self.bus)

    @property
    def label(self):
        if self.serial:
            return "%s (%s)" % (self.name, self.serial)
        return self.name

    @property
    def uuid(self):
        return self.id

    def caps(self):
        caps = self._driver.capabilities(self.info)
        if caps:
            return caps
        return sorted(
            {
                vcp.CODE_BRIGHTNESS,
                vcp.CODE_CONTRAST,
                vcp.CODE_INPUT_SOURCE,
                vcp.CODE_POWER,
                vcp.CODE_VOLUME,
                vcp.CODE_MUTE,
                vcp.CODE_OSD,
                vcp.CODE_COLOR_PRESET,
            }
        )

    def get(self, feature):
        code = vcp.feature_code(feature)
        return self._driver.get(self.info, code)

    def max_of(self, code):
        g = self.get("0x%02x" % code)
        if g["max"]:
            return g["max"]
        return vcp.MAX_DEFAULT.get(code, 0xFFFF)

    def set(self, feature, value):
        code = vcp.feature_code(feature)
        val = vcp.parse_value(code, value)
        if code in SCALER_CODES:
            val = round(val * self.max_of(code) / 100.0)
        self._driver.set(self.info, code, val)

    def get_scaled(self, feature):
        """Return (value, max) scaled to 0-100 percent for scaler features."""
        code = vcp.feature_code(feature)
        g = self._driver.get(self.info, code)
        if g["value"] is None:
            return None, None
        mx = g["max"] or vcp.MAX_DEFAULT.get(code, 100)
        if code in SCALER_CODES:
            return round(g["value"] * 100.0 / mx), 100
        return g["value"], mx

    def set_scaled(self, feature, percent):
        self.set(feature, round(percent))

    def brightness(self, value=None):
        if value is None:
            return self.get_scaled("brightness")[0]
        self.set("brightness", value)


class DdcutilBackend:
    name = "ddcutil"

    def __init__(self, binary=None):
        self.bin = binary or shutil.which("ddcutil")
        self._lock = threading.Lock()

    @classmethod
    def available(cls):
        return shutil.which("ddcutil") is not None

    def list_displays(self):
        out = _run([self.bin, "detect", "--terse"])
        displays, cur = [], None
        for ln in out.splitlines():
            if ln.startswith("Display:"):
                if cur:
                    displays.append(cur)
                    cur = None
                cur = {"name": "Display"}
            elif cur is None:
                continue
            else:
                for key, dest in (
                    ("I2C bus:", "bus"),
                    ("Model:", "name"),
                    ("Serial number:", "serial"),
                    ("Manufacturer:", "mfr"),
                ):
                    if ln.strip().startswith(key):
                        cur[dest] = ln.split(":", 1)[1].strip()
        if cur:
            displays.append(cur)
        return [d for d in displays if d.get("bus")]

    def capabilities(self, display):
        try:
            out = _run([self.bin, "--bus", str(_busno(display) or ""), "capabilities"])
            return ddcci.parse_caps_string(out) or None
        except Exception:
            return None

    _CUR_RE = re.compile(r"current(?:\s+value)?\s*=\s*0*(\d+)", re.I)
    _MAX_RE = re.compile(r"max(?:\s+value)?\s*=\s*0*(\d+)", re.I)

    def get(self, display, code):
        with self._lock:
            out = _run(
                [
                    self.bin,
                    "--bus",
                    str(_busno(display)),
                    "getvcp",
                    "0x%02x" % code,
                ]
            )
        m = self._CUR_RE.search(out)
        mx = self._MAX_RE.search(out)
        return {
            "result": ddcci.RESULT_OK if m else ddcci.RESULT_BROKEN,
            "code": code,
            "value": int(m.group(1)) if m else None,
            "max": int(mx.group(1)) if mx else None,
        }

    def set(self, display, code, value):
        with self._lock:
            _run(
                [
                    self.bin,
                    "--bus",
                    str(_busno(display)),
                    "setvcp",
                    "0x%02x" % code,
                    str(value),
                ],
                timeout=30,
            )


class MockBackend:
    """Deterministic in-memory display for tests and CI without hardware."""

    name = "mock"
    displays = []

    def __init__(self):
        if not type(self).displays:
            type(self).displays = [
                {
                    "bus": "mock-i2c-7",
                    "name": "U2723QE",
                    "serial": "FAKE-12345",
                    "mfr": "DEL",
                    "prod": 0xA0F1,
                    "state": {
                        vcp.CODE_BRIGHTNESS: [45, 100],
                        vcp.CODE_CONTRAST: [50, 100],
                        vcp.CODE_INPUT_SOURCE: [0x0F, 0xFFFF],
                        vcp.CODE_POWER: [0x01, 0xFFFF],
                        vcp.CODE_VOLUME: [30, 100],
                        vcp.CODE_MUTE: [0x01, 0xFFFF],
                        vcp.CODE_OSD: [0x01, 0xFFFF],
                        vcp.CODE_COLOR_PRESET: [0x02, 0xFFFF],
                    },
                }
            ]

    def list_displays(self):
        return list(self.displays)

    def capabilities(self, display):
        return sorted(display["state"].keys())

    def get(self, display, code):
        st = display["state"].get(code)
        if st is None:
            return {
                "result": ddcci.RESULT_UNSUPPORTED,
                "code": code,
                "value": None,
                "max": None,
            }
        cur, mx = st
        return {"result": 0, "code": code, "value": cur, "max": mx}

    def set(self, display, code, value):
        if code not in display["state"]:
            raise ddcci.DDCCIError("VCP 0x%02X not supported" % code)
        cur, mx = display["state"][code]
        display["state"][code] = [min(value, mx), mx]


BACKENDS = {
    "ddcutil": DdcutilBackend,
    "ddcci": ddcci.DDCCIDriver,
    "mock": MockBackend,
}


def based_on_config(selector="auto"):
    """Pick a backend: explicit selector, or ddcutil if installed, else ddcci."""
    selector = (selector or "auto").strip().lower()
    if selector in ("mock", "test"):
        return MockBackend()
    if selector in ("ddcci", "i2c"):
        return ddcci.DDCCIDriver()
    if selector == "ddcutil":
        if not DdcutilBackend.available():
            raise RuntimeError("ddcutil binary not found on PATH")
        return DdcutilBackend()
    if DdcutilBackend.available():
        return DdcutilBackend()
    return ddcci.DDCCIDriver()


def monitors(selector="auto"):
    backend = based_on_config(selector)
    return backend, [Monitor(backend, info) for info in backend.list_displays()]

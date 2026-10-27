"""Named feature and value tables (VCP codes), plus parsing helpers."""

CODE_BRIGHTNESS = 0x10
CODE_CONTRAST = 0x12
CODE_COLOR_PRESET = 0x14
CODE_INPUT_SOURCE = 0x60
CODE_VOLUME = 0x62
CODE_MUTE = 0x8D
CODE_OSD = 0xCA
CODE_POWER = 0xD6

FEATURE_CODES = {
    "brightness": CODE_BRIGHTNESS,
    "contrast": CODE_CONTRAST,
    "color-temp": CODE_COLOR_PRESET,
    "input-source": CODE_INPUT_SOURCE,
    "input": CODE_INPUT_SOURCE,
    "volume": CODE_VOLUME,
    "mute": CODE_MUTE,
    "osd": CODE_OSD,
    "power-state": CODE_POWER,
    "power": CODE_POWER,
}

POWER_VALUES = {0x01: "on", 0x04: "off", 0x05: "standby"}
COLOR_PRESET_VALUES = {
    0x00: "auto",
    0x01: "srgb",
    0x02: "6500k",
    0x03: "7500k",
    0x04: "8200k",
    0x05: "9300k",
    0x06: "10000k",
    0x07: "user1",
    0x08: "user2",
    0x09: "user3",
    0x13: "5000k",
    0x14: "4000k",
    0xA0: "factory",
}
INPUT_VALUES = {
    0x01: "vga1",
    0x02: "vga2",
    0x03: "dvi1",
    0x04: "dvi2",
    0x05: "composite",
    0x06: "s-video",
    0x07: "tuner",
    0x08: "bnc",
    0x0F: "dp1",
    0x10: "dp2",
    0x11: "hdmi1",
    0x12: "hdmi2",
    0x17: "mdp1",
    0x18: "dp3",
    0x19: "usbc",
}
MUTE_VALUES = {0x01: "unmute", 0x02: "mute"}
OSD_VALUES = {0x01: "unlock", 0x02: "lock"}

VALUE_MAPS = {
    CODE_COLOR_PRESET: COLOR_PRESET_VALUES,
    CODE_INPUT_SOURCE: INPUT_VALUES,
    CODE_MUTE: MUTE_VALUES,
    CODE_OSD: OSD_VALUES,
    CODE_POWER: POWER_VALUES,
    CODE_BRIGHTNESS: None,
    CODE_CONTRAST: None,
    CODE_VOLUME: None,
}

MAX_DEFAULT = {CODE_BRIGHTNESS: 100, CODE_CONTRAST: 100, CODE_VOLUME: 100}


def normalize(s):
    return (
        s.strip().lower().replace("_", "").replace("-", "").replace(" ", "")
    )


def feature_code(token):
    """Resolve a feature token (name or numeric/hex string) to a VCP code."""
    s = normalize(str(token))
    for form in (str(token).strip().lower(), s):
        if form in FEATURE_CODES:
            code = FEATURE_CODES[form]
            break
    else:
        if s.startswith("0x"):
            code = int(s, 16)
        elif s.isdigit():
            code = int(s)
        else:
            raise ValueError("unknown feature %r" % token)
    if not 0 <= code <= 0xFF:
        raise ValueError("invalid VCP code 0x%X" % code)
    return code


def feature_name(code):
    for name, c in FEATURE_CODES.items():
        if c == code:
            return name
    return "0x%02x" % code


def parse_value(code, token):
    """Resolve a value token (symbolic name, decimal or hex string) to int."""
    if isinstance(token, int):
        return token
    s = normalize(str(token))
    if s.startswith("0x"):
        return int(s, 16)
    if s.isdigit():
        return int(s)
    mapping = VALUE_MAPS.get(code)
    if mapping:
        for k, name in mapping.items():
            if normalize(str(name)) == s:
                return k
    raise ValueError(
        "unknown value %r for feature 0x%02X (%s)"
        % (token, code, feature_name(code))
    )

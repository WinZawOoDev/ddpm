//! Named feature and value tables (VCP codes), plus parsing helpers.

use serde_json::Value;

pub const CODE_BRIGHTNESS: u8 = 0x10;
pub const CODE_CONTRAST: u8 = 0x12;
pub const CODE_COLOR_PRESET: u8 = 0x14;
pub const CODE_INPUT_SOURCE: u8 = 0x60;
pub const CODE_VOLUME: u8 = 0x62;
pub const CODE_MUTE: u8 = 0x8D;
pub const CODE_OSD: u8 = 0xCA;
pub const CODE_POWER: u8 = 0xD6;

/// (feature name, VCP code), same order as the Python FEATURE_CODES dict.
pub const FEATURE_CODES: &[(&str, u8)] = &[
    ("brightness", CODE_BRIGHTNESS),
    ("contrast", CODE_CONTRAST),
    ("color-temp", CODE_COLOR_PRESET),
    ("input-source", CODE_INPUT_SOURCE),
    ("input", CODE_INPUT_SOURCE),
    ("volume", CODE_VOLUME),
    ("mute", CODE_MUTE),
    ("osd", CODE_OSD),
    ("power-state", CODE_POWER),
    ("power", CODE_POWER),
];

/// MCCS 0xD6 (power mode). 0x05 is write-only: it switches the display off
/// the way the power button does, and reads never report it, so it is named
/// "hard-off".
pub const POWER_VALUES: &[(u16, &str)] = &[
    (0x01, "on"),
    (0x02, "standby"),
    (0x03, "suspend"),
    (0x04, "off"),
    (0x05, "hard-off"),
];

/// MCCS 0x14 (select color preset).
pub const COLOR_PRESET_VALUES: &[(u16, &str)] = &[
    (0x01, "srgb"),
    (0x02, "native"),
    (0x03, "4000k"),
    (0x04, "5000k"),
    (0x05, "6500k"),
    (0x06, "7500k"),
    (0x07, "8200k"),
    (0x08, "9300k"),
    (0x09, "10000k"),
    (0x0A, "11500k"),
    (0x0B, "user1"),
    (0x0C, "user2"),
    (0x0D, "user3"),
];

pub const INPUT_VALUES: &[(u16, &str)] = &[
    (0x01, "vga1"),
    (0x02, "vga2"),
    (0x03, "dvi1"),
    (0x04, "dvi2"),
    (0x05, "composite"),
    (0x06, "s-video"),
    (0x07, "tuner"),
    (0x08, "bnc"),
    (0x0F, "dp1"),
    (0x10, "dp2"),
    (0x11, "hdmi1"),
    (0x12, "hdmi2"),
    (0x17, "mdp1"),
    (0x18, "dp3"),
    (0x1B, "usbc"),
];

pub const MUTE_VALUES: &[(u16, &str)] = &[(0x01, "unmute"), (0x02, "mute")];

/// MCCS 0xCA (OSD / button control): 0x01 disables the OSD, which is "lock".
pub const OSD_VALUES: &[(u16, &str)] = &[(0x01, "lock"), (0x02, "unlock")];

/// VCP codes iterated when snapshotting a display (keys of the Python
/// VALUE_MAPS dict, in order). Mapped codes carry a value table; the scaler
/// codes (brightness/contrast/volume) do not.
pub const VALUE_MAP_CODES: &[u8] = &[
    CODE_COLOR_PRESET,
    CODE_INPUT_SOURCE,
    CODE_MUTE,
    CODE_OSD,
    CODE_POWER,
    CODE_BRIGHTNESS,
    CODE_CONTRAST,
    CODE_VOLUME,
];

/// Value table for a code, if it has symbolic names.
pub fn value_map(code: u8) -> Option<&'static [(u16, &'static str)]> {
    match code {
        CODE_COLOR_PRESET => Some(COLOR_PRESET_VALUES),
        CODE_INPUT_SOURCE => Some(INPUT_VALUES),
        CODE_MUTE => Some(MUTE_VALUES),
        CODE_OSD => Some(OSD_VALUES),
        CODE_POWER => Some(POWER_VALUES),
        _ => None,
    }
}

/// Default maximum for scaler features (used when the display does not
/// report one).
pub fn max_default(code: u8) -> Option<u16> {
    match code {
        CODE_BRIGHTNESS | CODE_CONTRAST | CODE_VOLUME => Some(100),
        _ => None,
    }
}

/// Scaler features take percentages (0-100) on the command line and in presets.
pub fn is_percent(code: u8) -> bool {
    matches!(code, CODE_BRIGHTNESS | CODE_CONTRAST | CODE_VOLUME)
}

pub fn normalize(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .filter(|c| *c != '_' && *c != '-' && *c != ' ')
        .collect()
}

fn feature_code_error(token: &str) -> String {
    format!("unknown feature {}", py_repr(token))
}

/// Resolve a feature token (name or numeric/hex string) to a VCP code.
pub fn feature_code(token: &str) -> Result<u8, String> {
    let lower = token.trim().to_lowercase();
    let norm = normalize(token);
    let mut found = None;
    for form in [&lower, &norm] {
        if let Some(&(_, code)) = FEATURE_CODES.iter().find(|(name, _)| *name == form) {
            found = Some(code);
            break;
        }
    }
    let code: u32 = match found {
        Some(code) => code as u32,
        None => {
            if let Some(hex) = norm.strip_prefix("0x") {
                u32::from_str_radix(hex, 16).map_err(|_| feature_code_error(token))?
            } else if !norm.is_empty() && norm.chars().all(|c| c.is_ascii_digit()) {
                norm.parse::<u32>().map_err(|_| feature_code_error(token))?
            } else {
                return Err(feature_code_error(token));
            }
        }
    };
    if code > 0xFF {
        return Err(format!("invalid VCP code 0x{:X}", code));
    }
    Ok(code as u8)
}

/// Canonical name for a code (first FEATURE_CODES entry), else raw hex form.
pub fn feature_name(code: u8) -> String {
    for (name, c) in FEATURE_CODES {
        if *c == code {
            return (*name).to_string();
        }
    }
    format!("0x{:02x}", code)
}

fn parse_value_error(token: &Value, code: u8) -> String {
    let shown = match token {
        Value::String(s) => py_repr(s),
        other => other.to_string(),
    };
    format!(
        "unknown value {} for feature 0x{:02X} ({})",
        shown,
        code,
        feature_name(code)
    )
}

/// Strict integer: an optional '-', then decimal digits or 0x-prefixed hex.
/// Unlike `normalize`, this never drops characters, so "-5" stays negative.
fn parse_int(s: &str) -> Option<i64> {
    let (negative, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let magnitude = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        Some(hex) if !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
            i64::from_str_radix(hex, 16).ok()?
        }
        Some(_) => return None,
        None if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) => {
            body.parse::<i64>().ok()?
        }
        None => return None,
    };
    Some(if negative { -magnitude } else { magnitude })
}

/// Resolve a value token to a numeric VCP value: an integer (decimal, or hex
/// with 0x), or a name from the feature's table. Percent features must be 0-100.
pub fn parse_value(code: u8, token: &Value) -> Result<i64, String> {
    let number = match token {
        Value::Number(_) => token.as_i64(),
        Value::String(raw) => {
            let raw = raw.trim();
            parse_int(raw).or_else(|| {
                let wanted = normalize(raw);
                value_map(code)?
                    .iter()
                    .find(|(_, name)| normalize(name) == wanted)
                    .map(|(value, _)| i64::from(*value))
            })
        }
        _ => None,
    };
    let Some(n) = number else {
        return Err(parse_value_error(token, code));
    };
    let max = if is_percent(code) {
        100
    } else {
        i64::from(u16::MAX)
    };
    if (0..=max).contains(&n) {
        Ok(n)
    } else {
        Err(format!(
            "value {} out of range for {} (0-{})",
            n,
            feature_name(code),
            max
        ))
    }
}

/// Python-style repr for error messages (single quotes).
pub fn py_repr(s: &str) -> String {
    format!("'{}'", s.replace('\'', "\\'"))
}

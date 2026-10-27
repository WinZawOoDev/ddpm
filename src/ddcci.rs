//! Pure-Rust DDC/CI over /dev/i2c-* (no ddcutil dependency).
//!
//! Host messages go to slave 0x37 as [0x51, len|0x80, body..., checksum], where
//! checksum = XOR(0x6E, all previous bytes) and len counts the body bytes.
//!
//! Display replies are [0x6E, len|0x80, payload..., checksum], where
//! checksum = XOR(0x50, all previous bytes) and len counts the payload bytes.
//!
//! - GET_VCP:      body 01 vcp;          reply payload 02 result vcp type MH ML SH SL.
//! - SET_VCP:      body 03 vcp SH SL;    no reply.
//! - Capabilities: body F3 offH offL;    reply payload E3 offH offL fragment.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use crate::driver::{bus_lock, Backend, DisplayInfo, VcpFrame};
use crate::vcp;

const I2C_SLAVE: libc::c_ulong = 0x0703;
pub const DDC_ADDR: u16 = 0x37;
pub const EDID_ADDR: u16 = 0x50;
pub const OP_GET_VCP: u8 = 0x01;
pub const OP_REPLY: u8 = 0x02;
pub const OP_SET_VCP: u8 = 0x03;
pub const OP_CAPS_REQUEST: u8 = 0xF3;
pub const OP_CAPS_REPLY: u8 = 0xE3;

pub const RESULT_OK: u8 = 0x00;
pub const RESULT_UNSUPPORTED: u8 = 0x01;
pub const RESULT_RETRY: u8 = 0x02;
pub const RESULT_BROKEN: u8 = 0x03;

pub const DEFAULT_GLOB: &str = "/dev/i2c-*";

/// Attempts per DDC/CI operation before giving up.
const ATTEMPTS: usize = 3;
/// Pause after a command and between attempts. DDC/CI wants about 40 ms
/// between messages.
const GAP: Duration = Duration::from_millis(50);
/// How long to wait for the reply to one request.
const REPLY_TIMEOUT: Duration = Duration::from_millis(1500);
/// Upper bound on the capabilities string, so a misbehaving display cannot
/// keep us reading forever.
const MAX_CAPS_LEN: usize = 16 * 1024;

pub fn xor(data: &[u8]) -> u8 {
    let mut c = 0u8;
    for b in data {
        c ^= b;
    }
    c
}

/// DDC/CI write-path checksum: XOR with the virtual start byte 0x6E.
pub fn checksum(data: &[u8]) -> u8 {
    xor(&[&[0x6Eu8], data].concat())
}

/// A parsed GET_VCP reply frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub result: u8,
    pub code: u8,
    pub vcp_type: u8,
    pub max: u16,
    pub value: u16,
}

pub struct Bus {
    path: String,
    file: File,
}

impl Bus {
    pub fn open(path: &str) -> Result<Bus> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| anyhow!("{}: {}", path, e))?;
        Ok(Bus {
            path: path.to_string(),
            file,
        })
    }

    pub fn select(&self, addr: u16) -> Result<()> {
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), I2C_SLAVE, addr as libc::c_ulong) };
        if rc < 0 {
            bail!(
                "{}: ioctl I2C_SLAVE failed: {}",
                self.path,
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }

    fn write_bytes(&self, data: &[u8]) -> Result<()> {
        (&self.file).write_all(data)?;
        Ok(())
    }

    pub fn read_edid(&self) -> Result<[u8; 128]> {
        let mut edid = [0u8; 128];
        for (i, &off) in [0u8, 32, 64, 96].iter().enumerate() {
            self.select(EDID_ADDR)?;
            self.write_bytes(&[off])?;
            sleep(Duration::from_millis(5));
            let n = (&self.file).read(&mut edid[i * 32..(i + 1) * 32])?;
            if n != 32 {
                bail!("short EDID read at offset {}", off);
            }
        }
        Ok(edid)
    }

    /// Send a host command body and do not wait for a reply (SET_VCP has none).
    pub fn write_command(&self, body: &[u8]) -> Result<()> {
        let mut payload = vec![0x51u8, ((body.len() as u8) & 0x7F) | 0x80];
        payload.extend_from_slice(body);
        payload.push(checksum(&payload));
        self.select(DDC_ADDR)?;
        self.write_bytes(&payload)
    }

    /// Send a command, then poll the bus until `find` recognizes the reply.
    fn request<T>(&self, body: &[u8], find: impl Fn(&[u8]) -> Option<T>) -> Result<T> {
        self.write_command(body)?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        let mut buf = [0u8; 256];
        while Instant::now() < deadline {
            sleep(Duration::from_millis(30));
            match (&self.file).read(&mut buf) {
                Ok(n) if n > 0 => {
                    if let Some(found) = find(&buf[..n]) {
                        return Ok(found);
                    }
                }
                _ => {}
            }
        }
        bail!("no valid DDC/CI reply from {}", self.path);
    }

    /// GET_VCP round trip: the reply frame for `want_vcp`.
    pub fn transaction(&self, data: &[u8], want_vcp: u8) -> Result<Frame> {
        self.request(data, |raw| find_frame(raw, want_vcp))
    }

    /// Fetch one capabilities fragment starting at `offset`.
    fn capabilities_fragment(&self, offset: u16) -> Result<Vec<u8>> {
        let body = [OP_CAPS_REQUEST, (offset >> 8) as u8, offset as u8];
        self.request(&body, |raw| find_caps_fragment(raw, offset))
    }

    /// Read the whole capabilities string, e.g. `(prot(monitor)vcp(10 12)...)`.
    pub fn read_capabilities(&self) -> Result<String> {
        let bytes = collect_fragments(|offset| self.capabilities_fragment(offset))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Reassemble a capabilities string. `fetch(offset)` returns the bytes at that
/// offset, and an empty fragment marks the end.
pub fn collect_fragments(mut fetch: impl FnMut(u16) -> Result<Vec<u8>>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let offset = u16::try_from(out.len()).map_err(|_| anyhow!("capabilities too long"))?;
        let fragment = fetch(offset)?;
        if fragment.is_empty() {
            return Ok(out);
        }
        out.extend_from_slice(&fragment);
        if out.len() > MAX_CAPS_LEN {
            bail!("capabilities string longer than {} bytes", MAX_CAPS_LEN);
        }
    }
}

/// Find a capabilities reply for `offset` in a raw read buffer and return its
/// fragment bytes.
pub fn find_caps_fragment(raw: &[u8], offset: u16) -> Option<Vec<u8>> {
    for (idx, &byte) in raw.iter().enumerate() {
        if byte != 0x6E {
            continue;
        }
        let buf = &raw[idx..];
        if buf.len() < 2 {
            continue;
        }
        let len = (buf[1] & 0x7F) as usize;
        let end = 2 + len; // index of the checksum byte
        if len < 3 || buf.len() <= end {
            continue;
        }
        if xor(&[&[0x50u8], &buf[..end]].concat()) != buf[end] {
            continue;
        }
        if buf[2] != OP_CAPS_REPLY || u16::from_be_bytes([buf[3], buf[4]]) != offset {
            continue;
        }
        return Some(buf[5..end].to_vec());
    }
    None
}

/// Scan a read buffer for a valid GET_VCP reply frame.
pub fn find_frame(raw: &[u8], want_vcp: u8) -> Option<Frame> {
    for (idx, &byte) in raw.iter().enumerate() {
        if byte != 0x6E {
            continue;
        }
        let buf = &raw[idx..];
        if buf.len() >= 3 && (buf[1] & 0x7F) == 0 && buf[2] == 0xBE {
            continue; // null response
        }
        if buf.len() < 11 {
            continue;
        }
        if xor(&[&[0x50u8], &buf[..10]].concat()) != buf[10] {
            continue;
        }
        if buf[2] != OP_REPLY {
            continue;
        }
        if buf[4] != want_vcp {
            continue;
        }
        return Some(Frame {
            result: buf[3],
            code: buf[4],
            vcp_type: buf[5],
            max: ((buf[6] as u16) << 8) | buf[7] as u16,
            value: ((buf[8] as u16) << 8) | buf[9] as u16,
        });
    }
    None
}

pub fn valid_edid(edid: &[u8]) -> bool {
    edid.len() == 128
        && edid[..6] == [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
        && edid.iter().map(|&b| b as u64).sum::<u64>() % 256 == 0
}

pub struct Identity {
    pub mfr: String,
    pub prod: u16,
    pub name: String,
    pub serial: Option<String>,
}

pub fn edid_identity(edid: &[u8]) -> Identity {
    let letters = |reg: usize| -> String {
        let x = u16::from_be_bytes([edid[reg], edid[reg + 1]]);
        [10u32, 5, 0]
            .iter()
            .map(|&sh| char::from(64 + ((x >> sh) & 0x1F) as u8))
            .collect()
    };

    let clean = |bytes: &[u8]| -> String {
        let s: String = bytes.iter().map(|&b| b as char).collect();
        s.trim_matches(|c| c == ' ' || c == '\x00' || c == '\n')
            .to_string()
    };

    let mut name = None;
    let mut serial = None;
    let mut off = 54;
    while off < 120 {
        let blk = &edid[off..off + 18];
        if blk[0] == 0 && blk[1] == 0 && blk[2] == 0 {
            if blk[3] == 0xFC && name.is_none() {
                name = Some(clean(&blk[5..18]));
            } else if blk[3] == 0xFF && serial.is_none() {
                serial = Some(clean(&blk[5..18]));
            }
        }
        off += 18;
    }
    Identity {
        mfr: letters(8),
        prod: u16::from_le_bytes([edid[10], edid[11]]),
        name: name.unwrap_or_else(|| "Unknown".to_string()),
        serial,
    }
}

/// Probe result for one i2c bus: display identity plus whether it answers
/// DDC/CI.
#[derive(Debug, Clone)]
pub struct Probe {
    pub info: DisplayInfo,
    pub ddcci: bool,
}

/// Return a probe result if `path` is a monitor bus, else None.
pub fn probe_display(path: &str) -> Option<Probe> {
    let bus = match Bus::open(path) {
        Ok(b) => b,
        Err(_) => return None,
    };
    let edid = match bus.read_edid() {
        Ok(e) => e,
        Err(_) => return None,
    };
    if !valid_edid(&edid) {
        return None;
    }
    let id = edid_identity(&edid);
    let ddcci = bus.write_command(&[OP_GET_VCP, 0x10]).is_ok();
    Some(Probe {
        info: DisplayInfo {
            bus: Some(path.to_string()),
            name: id.name,
            mfr: Some(id.mfr),
            serial: id.serial,
            prod: Some(id.prod),
        },
        ddcci,
    })
}

/// Identities for each i2c bus that answers as a display.
pub fn scan_buses(bus_glob: &str) -> Vec<Probe> {
    let mut paths: Vec<String> = match glob::glob(bus_glob) {
        Ok(paths) => paths
            .filter_map(|p| p.ok())
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        Err(_) => return Vec::new(),
    };
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        // Bus 0 is skipped: on most machines it is the chipset SMBus, not a
        // monitor's DDC/CI bus.
        match path.rsplit('-').next().and_then(|s| s.parse::<i64>().ok()) {
            Some(0) | None => continue,
            _ => {}
        }
        if let Some(probe) = probe_display(&path) {
            out.push(probe);
        }
    }
    out
}

/// Displays backed directly by /dev/i2c-N DDC/CI.
pub struct DDCCIDriver {
    pub bus_glob: String,
}

impl Default for DDCCIDriver {
    fn default() -> Self {
        DDCCIDriver {
            bus_glob: DEFAULT_GLOB.to_string(),
        }
    }
}

impl DDCCIDriver {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Value of a reply as the feature defines it. Non-continuous features with a
/// value table keep their value in the low byte only. Some displays also fill
/// the high byte: the Dell S2722QC reports input source 0x1B as 0x1B1B.
pub fn value_from_reply(code: u8, raw: u16) -> u16 {
    if vcp::value_map(code).is_some() {
        raw & 0xFF
    } else {
        raw
    }
}

fn bus_path(display: &DisplayInfo) -> Result<&str> {
    display
        .bus
        .as_deref()
        .ok_or_else(|| anyhow!("display has no i2c bus"))
}

impl Backend for DDCCIDriver {
    fn name(&self) -> &'static str {
        "ddcci"
    }

    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        let _guard = bus_lock();
        Ok(scan_buses(&self.bus_glob)
            .into_iter()
            .filter(|p| p.ddcci)
            .map(|p| p.info)
            .collect())
    }

    fn capabilities(&self, display: &DisplayInfo) -> Option<Vec<u8>> {
        let _guard = bus_lock();
        let bus = Bus::open(display.bus.as_deref()?).ok()?;
        let text = bus.read_capabilities().ok()?;
        let codes = parse_caps_string(&text);
        if codes.is_empty() {
            None
        } else {
            Some(codes)
        }
    }

    fn get(&self, display: &DisplayInfo, code: u8) -> Result<VcpFrame> {
        let _guard = bus_lock();
        let bus = Bus::open(bus_path(display)?)?;
        let mut last_error = None;
        for _ in 0..ATTEMPTS {
            match bus.transaction(&[OP_GET_VCP, code], code) {
                Ok(frame) if frame.result == RESULT_OK => {
                    sleep(GAP);
                    return Ok(VcpFrame {
                        result: frame.result,
                        code: frame.code,
                        max: Some(frame.max),
                        value: Some(value_from_reply(code, frame.value)),
                    });
                }
                // The display answered "unsupported": retrying cannot help.
                Ok(frame) if frame.result == RESULT_UNSUPPORTED => {
                    bail!("VCP 0x{:02X} not supported by display", code);
                }
                Ok(frame) => {
                    last_error = Some(anyhow!("VCP 0x{:02X} result code {}", code, frame.result));
                }
                Err(err) => last_error = Some(err),
            }
            sleep(GAP);
        }
        Err(last_error.unwrap_or_else(|| anyhow!("no reply for VCP 0x{:02X}", code)))
    }

    fn set(&self, display: &DisplayInfo, code: u8, value: u16) -> Result<()> {
        let _guard = bus_lock();
        let bus = Bus::open(bus_path(display)?)?;
        let body = [OP_SET_VCP, code, (value >> 8) as u8, value as u8];
        let mut last_error = None;
        for _ in 0..ATTEMPTS {
            match bus.write_command(&body) {
                Ok(()) => {
                    // SET_VCP has no reply; give the display time to act.
                    sleep(Duration::from_millis(250));
                    return Ok(());
                }
                Err(err) => last_error = Some(err),
            }
            sleep(GAP);
        }
        Err(last_error.unwrap_or_else(|| anyhow!("write of VCP 0x{:02X} failed", code)))
    }
}

/// Extract the feature codes from a capabilities string such as
/// `(prot(monitor)type(lcd)vcp(10 12 14(05 06) 60(0F 11) D6(01 04))mccs_ver(2.1))`.
/// Only codes directly inside `vcp(...)` count; the value lists in parentheses
/// after a code are skipped.
pub fn parse_caps_string(caps: &str) -> Vec<u8> {
    let Some(start) = caps.to_ascii_lowercase().find("vcp(") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut token = String::new();
    let mut depth = 1usize; // inside "vcp("
    for ch in caps[start + 4..].chars() {
        match ch {
            '(' => {
                if depth == 1 {
                    push_code(&mut out, &token);
                }
                token.clear();
                depth += 1;
            }
            ')' if depth == 1 => {
                push_code(&mut out, &token);
                token.clear();
                break;
            }
            ')' => {
                token.clear();
                depth -= 1;
            }
            c if c.is_whitespace() => {
                if depth == 1 {
                    push_code(&mut out, &token);
                }
                token.clear();
            }
            c => {
                if depth == 1 {
                    token.push(c);
                }
            }
        }
    }
    push_code(&mut out, &token);
    out.sort_unstable();
    out.dedup();
    out
}

/// Extract feature codes from ddcutil's `capabilities` report, which lists one
/// feature per line, e.g. `Feature: 10 (Brightness)`.
pub fn parse_feature_lines(report: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for line in report.lines() {
        let Some(rest) = line.trim_start().strip_prefix("Feature:") else {
            continue;
        };
        let token = rest
            .trim_start()
            .split(|c: char| !c.is_ascii_hexdigit())
            .next()
            .unwrap_or("");
        push_code(&mut out, token);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Append `token` as a feature code if it is exactly two hex digits.
fn push_code(out: &mut Vec<u8>, token: &str) {
    if token.len() == 2 {
        if let Ok(code) = u8::from_str_radix(token, 16) {
            out.push(code);
        }
    }
}

//! Pure-Rust DDC/CI over /dev/i2c-* (no ddcutil dependency).
//!
//! Packet layout (data bytes on the wire after selecting slave 0x37):
//!
//! Write: [0x51, len|0x80, opcode, <args>, checksum]
//!   where len counts opcode+args, checksum = XOR(0x6E, all previous bytes).
//!
//! Reply: [0x6E, len|0x80, 0x02, result, vcp, type, MH, ML, SH, SL, checksum]
//!   validated by XOR(all bytes) == 0x50, i.e. ck = XOR(0x50, previous bytes).
//!
//! GET_VCP reply: result, vcp, type, MH, ML, SH, SL  -> len byte = 0x88.
//! SET_VCP write: opcode 0x03, vcp, SH, SL           -> len byte = 0x84.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use crate::driver::{DisplayInfo, VcpFrame};

const I2C_SLAVE: libc::c_ulong = 0x0703;
pub const DDC_ADDR: u16 = 0x37;
pub const EDID_ADDR: u16 = 0x50;
pub const OP_GET_VCP: u8 = 0x01;
pub const OP_REPLY: u8 = 0x02;
pub const OP_SET_VCP: u8 = 0x03;

pub const RESULT_OK: u8 = 0x00;
pub const RESULT_UNSUPPORTED: u8 = 0x01;
pub const RESULT_RETRY: u8 = 0x02;
pub const RESULT_BROKEN: u8 = 0x03;

pub const DEFAULT_GLOB: &str = "/dev/i2c-*";

pub fn xor(data: &[u8]) -> u8 {
    let mut c = 0u8;
    for b in data {
        c ^= b;
    }
    c
}

/// DDC/CI write-path checksum: XOR with virtual start byte 0x6E.
pub fn checksum(data: &[u8]) -> u8 {
    xor(&[&[0x6Eu8], data].concat())
}

/// raw starts at 0x6E and ends with the checksum byte.
pub fn valid_reply_checksum(raw: &[u8]) -> bool {
    if raw.is_empty() {
        return false;
    }
    xor(&[&[0x50u8], &raw[..raw.len() - 1]].concat()) == raw[raw.len() - 1]
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

    /// Send a DDC/CI command and scan the reply stream for a valid frame.
    pub fn transaction(&self, data: &[u8], want_vcp: u8) -> Result<Frame> {
        let mut payload = vec![0x51u8, ((data.len() as u8) & 0x7F) | 0x80];
        payload.extend_from_slice(data);
        payload.push(checksum(&payload));
        self.select(DDC_ADDR)?;
        self.write_bytes(&payload)?;
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut buf = [0u8; 128];
        while Instant::now() < deadline {
            sleep(Duration::from_millis(30));
            match (&self.file).read(&mut buf) {
                Err(_) => continue,
                Ok(0) => continue,
                Ok(_) => {
                    if let Some(frame) = find_frame(&buf, want_vcp) {
                        return Ok(frame);
                    }
                }
            }
        }
        bail!("no valid DDC/CI reply from {}", self.path);
    }
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
    let mut ddcci = false;
    if bus.select(DDC_ADDR).is_ok() {
        let probe = [0x51u8, 0x02 | 0x80, OP_GET_VCP, 0x10];
        let mut payload = probe.to_vec();
        payload.push(checksum(&probe));
        if bus.write_bytes(&payload).is_ok() {
            ddcci = true;
        }
    }
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

/// Yield identities for each i2c bus that answers as a display.
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

impl crate::driver::Backend for DDCCIDriver {
    fn name(&self) -> &'static str {
        "ddcci"
    }

    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        Ok(scan_buses(&self.bus_glob)
            .into_iter()
            .filter(|p| p.ddcci)
            .map(|p| p.info)
            .collect())
    }

    fn capabilities(&self, _display: &DisplayInfo) -> Option<Vec<u8>> {
        None
    }

    fn get(&self, display: &DisplayInfo, code: u8) -> Result<VcpFrame> {
        let path = display
            .bus
            .as_deref()
            .ok_or_else(|| anyhow!("display has no i2c bus"))?;
        let bus = Bus::open(path)?;
        bus.select(DDC_ADDR)?;
        let frame = bus.transaction(&[OP_GET_VCP, code], code)?;
        sleep(Duration::from_millis(50));
        if frame.result != RESULT_OK {
            if frame.result == RESULT_UNSUPPORTED {
                bail!("VCP 0x{:02X} not supported by display", code);
            }
            bail!("VCP 0x{:02X} result code {}", code, frame.result);
        }
        Ok(VcpFrame {
            result: frame.result,
            code: frame.code,
            max: Some(frame.max),
            value: Some(frame.value),
        })
    }

    fn set(&self, display: &DisplayInfo, code: u8, value: u16) -> Result<()> {
        let path = display
            .bus
            .as_deref()
            .ok_or_else(|| anyhow!("display has no i2c bus"))?;
        // Monitors do not send a useful reply for SET_VCP; write and wait.
        let bus = Bus::open(path)?;
        bus.select(DDC_ADDR)?;
        let data = [OP_SET_VCP, code, (value >> 8) as u8, (value & 0xFF) as u8];
        let mut payload = vec![0x51u8, ((data.len() as u8) & 0x7F) | 0x80];
        payload.extend_from_slice(&data);
        payload.push(checksum(&payload));
        bus.select(DDC_ADDR)?;
        bus.write_bytes(&payload)?;
        drop(bus);
        sleep(Duration::from_millis(250));
        Ok(())
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

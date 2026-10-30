# ddpm — Dell Display and Peripheral Manager clone for Ubuntu

`ddpm` replicates the core of Dell's proprietary **Display and Peripheral
Manager (DDPM/DDM)** on Linux. It talks to monitors directly over
**DDC/CI** (MCCS) on `/dev/i2c-*`, and also works through `ddcutil` when it
is installed. Dell's DDPM itself is Windows/macOS-only; this project is an
independent implementation of its most-used feature set.

This is the **Rust** implementation. Two binaries are built: `ddpm` (CLI) and
`ddpm-gui` (eframe/egui window).

## Features

| Feature | CLI | GUI |
|---|---|---|
| Enumerate displays (model, serial, bus, features) | `list`, `detect` | yes |
| Brightness / contrast / volume (0-100) | `get`, `set` | sliders |
| Input source (vga, dvi, dp, hdmi, usb-c...) | `get`, `set` | dropdown |
| Power state (on / standby / suspend / off / hard-off), mute; OSD lock/unlock (CLI) | `get`, `set` | power/mute dropdowns |
| Color temperature presets | `get`, `set` | dropdown |
| Raw VCP codes | `get`/`set 0x62` | - |
| Named presets (save snapshot, apply later) | `preset` | buttons |
| Backends: `ddcutil`, direct i2c (`ddcci`), `mock` for tests | `--backend` | same |

## Install

Build with cargo (Rust 1.80+; install the toolchain with
[rustup](https://rustup.rs) if needed):

```bash
cargo install --path .                        # installs ddpm and ddpm-gui to ~/.cargo/bin
cargo install --path . --no-default-features  # CLI only, without the GUI (eframe)
```

System setup (same as before; needed for the direct-i2c driver and
recommended for ddcutil):

```bash
sudo apt install ddcutil i2c-tools
sudo modprobe i2c-dev
sudo groupadd -f i2c && sudo usermod -aG i2c $USER    # relogin after
```

Without `ddcutil` installed the built-in Rust DDC/CI driver is used
instead — it needs read/write access to `/dev/i2c-N` (the `i2c` group or a
udev rule provides that):

```
# /etc/udev/rules.d/45-i2c.rules
KERNEL=="i2c-[0-9]*", GROUP="i2c", MODE="0660"
```

## CLI usage

```bash
ddpm list -v                          # enumerate displays + supported features
ddpm detect                           # raw i2c bus scan (ddcci backend)
ddpm get 1 brightness contrast input-source power-state
ddpm set 1 brightness=75              # 0-100
ddpm set 1 input-source=hdmi1         # also: dp1, dp3, usbc, dvi1, vga1...
ddpm set 1 power=on osd=lock volume=40
ddpm set 1 0x62=30                    # raw VCP feature codes too
ddpm preset save work                 # snapshot current settings of display 1
ddpm preset save night -d 1 brightness=10 color-temp=4000k
ddpm preset list / rm night / apply work -d 1
ddpm --backend mock list              # demo/CI mode, no hardware needed
ddpm config get / path / set driver ddcci
```

Display ids: `1`, `2`, ... (order from `list`), the display model name, its
serial, or a short-name mapped with `ddpm set` in config.

## GUI

```bash
ddpm-gui                          # or: ddpm-gui --backend mock
```

A tabbed window (eframe/egui): one tab per display, sliders for
brightness/contrast/volume, dropdowns for input/color temp/power/mute,
plus a preset bar (apply/save). Slow DDC/CI traffic happens on background
threads so the UI stays responsive.

## Presets & config

- `~/.config/ddpm/presets.json` — feature/value maps per preset name.
  Values may be integers (`75`), symbolic names (`hdmi1`, `on`, `6500k`)
  or raw codes (`"0x62": 30`).
- `~/.config/ddpm/config.json` — `driver` (auto|ddcutil|ddcci), monitor
  nicknames, etc.
- Override the config directory with `DDPM_CONFIG_DIR`.
- A malformed `presets.json` or `config.json` is reported as an error and is
  never overwritten. Writes go through a temporary file, so an interrupted
  save cannot truncate them.

## Architecture

```
src/
  vcp.rs       VCP feature tables and token parsing
  ddcci.rs     pure-Rust DDC/CI frames over /dev/i2c-N (probe, get, set)
  driver.rs    backend selection + Monitor abstraction (+ ddcutil, mock)
  presets.rs   named preset store (JSON)
  config.rs    user config (JSON)
  cli.rs       clap CLI (bin: ddpm)
  bin/gui.rs   eframe/egui app (bin: ddpm-gui)
tests/
  core.rs      integration tests (no hardware required)
```

All backends speak the same `Monitor` API (the `Backend` trait), so the CLI
and GUI are backend agnostic. `ddcutil` is preferred when present (it
handles multi-transport quirks); the `ddcci` driver needs no external tools
at all.

## Verify without a monitor

```bash
cargo test
ddpm --backend mock list
```

## Notes / limits

- Monitors must expose DDC/CI (Dell displays: enable in the OSD if needed).
- `ddcutil` caps/quirks can be slow; the GUI runs ops in the background.
- The `ddcutil` backend parses both classic and modern (>= 2.0) `--terse`
  `detect` output.
- USB-KVM matrix switching (per-input USB routing) is vendor firmware
  (Dell uses nonstandard VCP/hub codes) and is not implemented here —
  input switching still works as DDPM does on the video side.
- If nothing is detected: `ls /dev/i2c-*`, `sudo modprobe i2c-dev`, and
  check the OSD's DDC/CI setting. NVIDIA fully supports i2c-dev these days.
- `power=standby` sends MCCS standby (0x02). Some monitors do not accept it
  and report "not supported". `power=hard-off` sends the write-only 0x05
  command, which switches the display off the way its power button does.

## License

Not licensed. No license is granted to use, copy, modify, or distribute this
software.

# ddpm — Dell Display and Peripheral Manager clone for Ubuntu

`ddpm` replicates the core of Dell's proprietary **Display and Peripheral
Manager (DDPM/DDM)** on Linux. It talks to monitors directly over
**DDC/CI** (MCCS) on `/dev/i2c-*`, and also works through `ddcutil` when it
is installed. Dell's DDPM itself is Windows/macOS-only; this project is an
independent implementation of its most-used feature set.

## Features

| Feature | CLI | GUI |
|---|---|---|
| Enumerate displays (model, serial, bus, features) | `list`, `detect` | yes |
| Brightness / contrast / volume (0-100) | `get`, `set` | sliders |
| Input source (vga, dvi, dp, hdmi, usb-c...) | `get`, `set` | dropdown |
| Power state (on / standby / off), OSD lock, mute | `get`, `set` | dropdown |
| Color temperature presets | `get`, `set` | dropdown |
| Raw VCP codes | `get`/`set 0x62` | - |
| Named presets (save snapshot, apply later) | `preset` | buttons |
| Backends: `ddcutil`, direct i2c (`ddcci`), `mock` for tests | `--backend` | same |

## Install

```bash
cd ddpm-project            # this directory
sudo ./install.sh          # installs ddcutil + i2c-dev rules, pip installs ./
```

or manually:

```bash
sudo apt install ddcutil python3-pip i2c-tools
sudo modprobe i2c-dev
sudo groupadd -f i2c && sudo usermod -aG i2c $USER    # relogin after
pip3 install .
```

Without `ddcutil` installed the built-in pure-Python DDC/CI driver is used
instead — it needs read/write access to `/dev/i2c-N` (the `i2c` group or the
udev rule above provides that).

## CLI usage

```bash
ddpm list -v                          # enumerate displays + supported features
ddpm detect                           # raw i2c bus scan (ddcci backend)
ddpm get 1 brightness contrast input-source power-state
ddpm set 1 brightness=75              # 0-100
ddpm set 1 input-source=hdmi1         # also: dp1, dp3 (USB-C), dvi1, vga1...
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
ddpm-gui
```

A tabbed window: one tab per display, sliders for brightness/contrast/volume,
dropdowns for input/color temp/power/OSD/mute, plus a preset bar (apply/save).
Slow DDC/CI traffic happens on background threads so the UI stays responsive.

## Presets & config

- `~/.config/ddpm/presets.json` — feature/value maps per preset name.
  Values may be integers (`75`), symbolic names (`hdmi1`, `on`, `6500k`)
  or raw codes (`"0x62": 30`).
- `~/.config/ddpm/config.json` — `driver` (auto|ddcutil|ddcci), monitor
  nicknames, etc.

## Architecture

```
ddpm/
  ddcci.py     pure-Python DDC/CI frames over /dev/i2c-N (probe, get, set)
  driver.py    backend selection + Monitor abstraction (+ ddcutil, mock)
  vcp.py       VCP feature tables and token parsing
  presets.py   named preset store (JSON)
  config.py    user config (JSON)
  cli.py       argparse CLI
  gui.py       tkinter app
tests/         pytest suite (no hardware required)
```

Both backends speak the same `Monitor` API, so the CLI and GUI are backend
agnostic. `ddcutil` is preferred when present (it handles multi-transport
quirks); the `ddcci` driver needs no external tools at all.

## Verify without a monitor

```bash
python3 -m pytest tests -q
ddpm --backend mock list
```

## Notes / limits

- Monitors must expose DDC/CI (Dell displays: enable in the OSD if needed).
- `ddcutil` caps/quirks can be slow; the GUI runs ops in the background.
- USB-KVM matrix switching (per-input USB routing) is vendor firmware
  (Dell uses nonstandard VCP/hub codes) and is not implemented here —
  input switching still works as DDPM does on the video side.
- If nothing is detected: `ls /dev/i2c-*`, `sudo modprobe i2c-dev`, and
  check the OSD's DDC/CI setting. NVIDIA fully supports i2c-dev these days.

"""ddpm command line interface."""
import argparse
import json
import sys

from . import vcp
from .config import Config
from .driver import monitors
from .presets import PresetError, Presets


def _die(msg, code=1):
    print("Error: %s" % msg, file=sys.stderr)
    sys.exit(code)


def _backend_selector(args):
    sel = getattr(args, "backend", None) or Config().get("driver") or "auto"
    return sel


def _mons(args):
    return monitors(_backend_selector(args))[1]


def _pick_monitor(cli, ident, mons, cfg):
    mapping = cfg.get_map()
    if ident in mapping:
        target = mapping[ident]
        for m in mons:
            if m.uuid == target or m.serial == target:
                return m
        _die("%r maps to %r, which is not connected" % (ident, target))
    for m in mons:
        if ident in (m.uuid, m.id, m.serial, m.name):
            return m
    try:
        num = int(ident)
        if 1 <= num <= len(mons):
            return mons[num - 1]
    except ValueError:
        pass
    if mons:
        cli.error(
            "display %r not found. Known displays: %s"
            % (ident, ", ".join(m.name for m in mons))
        )
    _die(
        "No displays found. Try 'ddpm detect'.\n"
        "Tip: sudo apt install ddcutil, and/or run 'sudo modprobe i2c-dev'."
    )


def fmt_value(code, value):
    mapping = vcp.VALUE_MAPS.get(code) or {}
    name = mapping.get(value)
    if name:
        return "%s (0x%02X)" % (name, value)
    return str(value)


def _feature_token(code):
    for name, c in vcp.FEATURE_CODES.items():
        if c == code:
            return name
    return "0x%02x" % code


def _parse_pairs(pairs):
    """['brightness=60', 'input=dp1'] -> [(code, raw_value_token), ...]"""
    out = []
    for tok in pairs:
        if "=" not in tok:
            raise ValueError("expected FEATURE=VALUE, got %r" % tok)
        f, val = tok.split("=", 1)
        code = vcp.feature_code(f)
        out.append((code, val))
    return out


def _pairs_to_spec(pairs):
    spec = {}
    for code, raw in _parse_pairs(pairs):
        spec[_feature_token(code)] = vcp.parse_value(code, raw)
    return spec


def snapshot(mon):
    """Read all standard features of `mon` into a preset spec dict."""
    spec = {}
    for code, mapping in vcp.VALUE_MAPS.items():
        token = _feature_token(code)
        try:
            g = mon.get("0x%02x" % code)
        except Exception:
            continue
        if g["value"] is None:
            continue
        val = g["value"]
        if code in (vcp.CODE_BRIGHTNESS, vcp.CODE_CONTRAST, vcp.CODE_VOLUME):
            mx = g["max"] or 100
            val = round(val * 100.0 / mx)
        elif mapping:
            val = mapping.get(val, val)
        spec[token] = val
    return spec


def _cmd_list(args, cli):
    mons = _mons(args)
    if not mons:
        print("No displays found.")
        print("Try: sudo apt install ddcutil ; sudo modprobe i2c-dev")
        return 1
    sel = _backend_selector(args)
    print("Displays: %d (backend: %s)" % (len(mons), sel))
    for i, m in enumerate(mons, 1):
        serial = (" serial=%s" % m.serial) if m.serial else ""
        print("  %d. %s  bus=%s%s" % (i, m.name, m.bus, serial))
        if args.verbose:
            try:
                caps = ", ".join(vcp.feature_name(c) for c in m.caps())
                print("     features: %s" % caps)
            except Exception as e:
                print("     (caps unavailable: %s)" % e)
    return 0


def _cmd_detect(args, cli):
    if (getattr(args, "backend", None) or "auto") == "ddcutil":
        return _cmd_list(args, cli)
    from .ddcci import scan_buses

    found = False
    for ident in scan_buses():
        found = True
        print(
            "  %-11s mfr=%s prod=0x%04X name=%r serial=%r ddcci=%s"
            % (
                ident["bus"],
                ident["mfr"],
                ident["prod"],
                ident["name"],
                ident["serial"],
                ident["ddcci"],
            )
        )
    if not found:
        print("No displays detected on /dev/i2c-*.")
        print("Check: ls /dev/i2c-*   and   sudo modprobe i2c-dev")
        return 1
    return 0


def _cmd_get(args, cli):
    cfg = Config()
    mon = _pick_monitor(cli, args.display, _mons(args), cfg)
    rc = 0
    for token in args.features:
        try:
            code = vcp.feature_code(token)
        except ValueError as e:
            print("%s: %s" % (token, e), file=sys.stderr)
            rc = 1
            continue
        try:
            g = mon.get("0x%02x" % code)
        except Exception as e:
            print("%s: read failed: %s" % (token, e), file=sys.stderr)
            rc = 1
            continue
        if g["value"] is None:
            print("%s: not supported" % token, file=sys.stderr)
            rc = 1
            continue
        val = g["value"]
        if code in (vcp.CODE_BRIGHTNESS, vcp.CODE_CONTRAST, vcp.CODE_VOLUME):
            mx = g["max"] or 100
            val = fmt_value(code, round(val * 100.0 / mx))
            print("%-14s %s/100" % (vcp.feature_name(code) + ":", val))
        else:
            print(
                "%-14s %s"
                % (vcp.feature_name(code) + ":", fmt_value(code, val))
            )
    return rc


def _cmd_set(args, cli):
    cfg = Config()
    mon = _pick_monitor(cli, args.display, _mons(args), cfg)
    try:
        spec = _pairs_to_spec(args.pairs)
    except ValueError as e:
        _die(str(e))
    rc = 0
    for token, val in spec.items():
        try:
            mon.set(token, val)
        except Exception as e:
            print("%s=%s failed: %s" % (token, val, e), file=sys.stderr)
            rc = 1
    return rc


def _cmd_preset(args, cli):
    cfg = Config()
    ps = Presets()
    if args.action == "list":
        if not ps.names():
            print("no presets saved; try: ddpm preset save work")
            return 0
        for name in ps.names():
            print("%-16s %s" % (name, json.dumps(ps.presets[name], sort_keys=True)))
        return 0
    if args.action == "rm":
        try:
            ps.delete(args.name)
        except (PresetError, TypeError) as e:
            _die(str(e))
        print("removed preset %r" % args.name)
        return 0

    if args.action == "save":
        if not args.name:
            _die("preset save requires a name")
        mons = _mons(args)
        mon = _pick_monitor(cli, args.display or "1", mons, cfg)
        if args.pairs:
            try:
                spec = _pairs_to_spec(args.pairs)
            except ValueError as e:
                _die(str(e))
        else:
            spec = snapshot(mon)
        try:
            ps.set(args.name, spec)
        except PresetError as e:
            _die(str(e))
        print("saved preset %r: %s" % (args.name, json.dumps(spec, sort_keys=True)))
        return 0

    spec = ps.get(args.name)
    if spec is None:
        _die("no preset named %r" % args.name)
    mon = _pick_monitor(cli, args.display or "1", _mons(args), cfg)
    rc = 0
    for f, val in spec.items():
        try:
            mon.set(f, val)
        except Exception as e:
            print("%s=%s failed: %s" % (f, val, e), file=sys.stderr)
            rc = 1
    return rc


def _cmd_config(args, cli):
    cfg = Config()
    if args.action == "path":
        print(cfg.configfile)
        return 0
    if args.action == "get":
        print(json.dumps(cfg.config, indent=2, sort_keys=True))
        return 0
    if args.action == "set":
        if not args.key:
            _die("config set requires KEY VALUE")
        cfg.set(args.key, args.value)
        cfg.save()
        print("set %s=%s" % (args.key, args.value))
        return 0
    if args.action == "unset":
        try:
            cfg.unset(args.key)
        except KeyError as e:
            _die(str(e))
        cfg.save()
        return 0
    return 0


def build_parser():
    ap = argparse.ArgumentParser(
        prog="ddpm",
        description=(
            "Dell Display and Peripheral Manager clone for Ubuntu - "
            "controls monitors over DDC/CI (brightness, contrast, input "
            "source, power, volume, presets...)"
        ),
    )
    ap.add_argument(
        "--backend",
        choices=("auto", "ddcutil", "ddcci", "mock"),
        help="backend driver: auto (default), ddcutil, direct i2c (ddcci), mock",
    )
    sub = ap.add_subparsers(dest="command", required=True)

    p = sub.add_parser("list", help="enumerate displays")
    p.add_argument("-v", "--verbose", action="store_true")
    p.set_defaults(func=_cmd_list)

    p = sub.add_parser("detect", help="scan i2c buses directly for displays")
    p.set_defaults(func=_cmd_detect)

    p = sub.add_parser("get", help="read feature values")
    p.add_argument("display")
    p.add_argument("features", nargs="+")
    p.set_defaults(func=_cmd_get)

    p = sub.add_parser("set", help="set features: set DISPLAY FEATURE=VALUE ...")
    p.add_argument("display")
    p.add_argument("pairs", nargs="+", metavar="FEATURE=VALUE")
    p.set_defaults(func=_cmd_set)

    p = sub.add_parser("preset", help="save/apply/list/rm display presets")
    p.add_argument("action", choices=("save", "apply", "list", "rm"))
    p.add_argument("name", nargs="?")
    p.add_argument("pairs", nargs="*", metavar="FEATURE=VALUE")
    p.add_argument("-d", "--display", default=None, help="display for save/apply")
    p.set_defaults(func=_cmd_preset)

    p = sub.add_parser("config", help="config path/get/set/unset")
    p.add_argument("action", choices=("path", "get", "set", "unset"))
    p.add_argument("key", nargs="?")
    p.add_argument("value", nargs="?")
    p.set_defaults(func=_cmd_config)
    return ap


def main(argv=None):
    ap = build_parser()
    args = ap.parse_args(argv)
    try:
        rc = args.func(args, ap)
    except KeyboardInterrupt:
        return 130
    return rc if isinstance(rc, int) else 0


if __name__ == "__main__":
    sys.exit(main())

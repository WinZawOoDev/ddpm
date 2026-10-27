"""Application configuration stored in ~/.config/ddpm/config.json."""
import json
import os

CONFIG_DIR = os.environ.get(
    "DDPM_CONFIG_DIR", os.path.expanduser("~/.config/ddpm")
)
CONFIG_PATH = os.path.join(CONFIG_DIR, "config.json")

DEFAULT_KVM_PORT = "1-1"

DEFAULT_DRIVER = "auto"


def _as_bool(o):
    if isinstance(o, str):
        return o.strip().lower() in ("1", "true", "yes", "on")
    return bool(o)


class ConfigLoadError(Exception):
    """Raised when config.json cannot be loaded."""


class ConfigSaveError(Exception):
    """Raised when config.json cannot be written."""


class Config:
    """A tiny setuptools of config keys, loaded from config.json."""

    def __init__(self, path=CONFIG_PATH):
        self.configfile = path
        # defaults
        self.config = {
            "kvm_port": DEFAULT_KVM_PORT,
            "driver": DEFAULT_DRIVER,
            "monitors": {},
            "map": {},
        }
        self.load()

    def load(self):
        if not os.path.exists(self.configfile):
            return
        try:
            with open(self.configfile, "r", encoding="utf-8") as f:
                data = json.load(f)
        except json.JSONDecodeError as e:
            raise ConfigLoadError(
                "Invalid JSON in config file %s: %s" % (self.configfile, e)
            )
        except (PermissionError, OSError) as e:
            raise ConfigLoadError(
                "Could not read config file %s: %s" % (self.configfile, e)
            )
        self.config.update(data)

    def save(self):
        try:
            os.makedirs(os.path.dirname(self.configfile), exist_ok=True)
            with open(self.configfile, "w", encoding="utf-8") as f:
                json.dump(self.config, f, indent=2, sort_keys=True)
        except OSError as e:
            raise ConfigSaveError(
                "Could not write config file %s: %s" % (self.configfile, e)
            )

    def get(self, key, default=None):
        return self.config.get(key, default)

    def set(self, key, value):
        self.config[key] = value

    def unset(self, key):
        if key in self.config:
            del self.config[key]
        else:
            raise KeyError("Key %r not in config" % key)

    def mono_attr(self, mono, key, default=None):
        return self.config.get("monitors", {}).get(mono, {}).get(key, default)

    def set_mono_attr(self, mono, key, value):
        self.config.setdefault("monitors", {}).setdefault(mono, {})[key] = value

    def add_to_map(self, key, uuid):
        self.config.setdefault("map", {})[key] = uuid

    def get_map(self):
        return self.config.get("map", {})

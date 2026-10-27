"""Named presets: a set of (feature, value) assignments per display."""
import json
import os

from . import vcp
from .config import CONFIG_DIR

PRESETS_PATH = os.path.join(CONFIG_DIR, "presets.json")


class PresetError(Exception):
    pass


def validate_spec(spec):
    """Raise PresetError for unknown feature/value tokens in spec dict."""
    if not isinstance(spec, dict):
        raise PresetError("preset must be a mapping of feature -> value")
    for feature, value in spec.items():
        try:
            code = vcp.feature_code(feature)
        except ValueError as e:
            raise PresetError(str(e))
        try:
            vcp.parse_value(code, value)
        except ValueError as e:
            raise PresetError(str(e))


class Presets:
    def __init__(self, path=PRESETS_PATH):
        self.path = path
        self.presets = {}
        self.load()

    def load(self):
        if os.path.exists(self.path):
            try:
                with open(self.path, "r", encoding="utf-8") as f:
                    self.presets = json.load(f)
            except (OSError, json.JSONDecodeError):
                self.presets = {}

    def save(self):
        d = os.path.dirname(self.path)
        if d:
            os.makedirs(d, exist_ok=True)
        with open(self.path, "w", encoding="utf-8") as f:
            json.dump(self.presets, f, indent=2, sort_keys=True)

    def names(self):
        return sorted(self.presets.keys())

    def get(self, name):
        return self.presets.get(name)

    def set(self, name, spec):
        validate_spec(spec)
        self.presets[name] = {str(k): v for k, v in spec.items()}
        self.save()

    def delete(self, name):
        if name not in self.presets:
            raise PresetError("no preset named %r" % name)
        del self.presets[name]
        self.save()

    def dump(self):
        return dict(self.presets)

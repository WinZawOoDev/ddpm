"""ddpm GUI: tkinter window with per-display controls (DDPM-style)."""
import queue
import threading
import tkinter as tk
from tkinter import messagebox, simpledialog, ttk

from .config import Config
from .driver import monitors
from .presets import PresetError, Presets
from . import vcp

FEATURE_LABELS = {
    "brightness": "Brightness",
    "contrast": "Contrast",
    "volume": "Volume",
}
MENUS = {
    "input": ("Input", vcp.INPUT_VALUES),
    "color-temp": ("Color temp", vcp.COLOR_PRESET_VALUES),
    "power": ("Power", vcp.POWER_VALUES),
    "osd": ("OSD", vcp.OSD_VALUES),
    "mute": ("Mute", vcp.MUTE_VALUES),
}


class AsyncRunner:
    def __init__(self, root):
        self.root = root
        self.q = queue.Queue()
        root.after(80, self._drain)

    def submit(self, fn, *args, on_done=None, on_error=None):
        def work():
            try:
                self.q.put(("ok", on_done, fn(*args)))
            except Exception as e:
                self.q.put(("err", on_error, e))

        threading.Thread(target=work, daemon=True).start()

    def _drain(self):
        try:
            while True:
                kind, cb, payload = self.q.get_nowait()
                if cb is not None:
                    cb(payload)
        except queue.Empty:
            pass
        self.root.after(80, self._drain)


class DisplayPanel(ttk.LabelFrame):
    def __init__(self, master, runner, mon):
        super().__init__(master, text=mon.label, padding=8)
        self.runner = runner
        self.mon = mon
        self.grid_columnconfigure(1, weight=1)
        row = 0
        self.sliders = {}
        for key, label in FEATURE_LABELS.items():
            lab = ttk.Label(self, text=label)
            lab.grid(row=row, column=0, sticky="w")
            var = tk.IntVar(value=50)
            scale = ttk.Scale(
                self, from_=0, to=100, variable=var, orient="horizontal"
            )
            scale.grid(row=row, column=1, sticky="ew", padx=6)
            val_lab = ttk.Label(self, text="", width=5)
            val_lab.grid(row=row, column=2, sticky="e")
            scale.bind(
                "<ButtonRelease-1>",
                lambda e, k=key, v=var, lv=val_lab: self._apply_slider(k, v, lv),
            )
            self.sliders[key] = (var, val_lab)
            row += 1
        self.combos = {}
        self.combo_vars = {}
        for key, (label, values) in MENUS.items():
            lab = ttk.Label(self, text=label)
            lab.grid(row=row, column=0, sticky="w")
            names = [v for _, v in sorted(values.items())]
            var = tk.StringVar()
            combo = ttk.Combobox(
                self, textvariable=var, values=names, state="readonly", width=12
            )
            combo.grid(row=row, column=1, sticky="ew", padx=6)
            combo.bind(
                "<<ComboboxSelected>>",
                lambda e, k=key, c=combo: self._apply_menu(k, c),
            )
            self.combos[key] = combo
            self.combo_vars[key] = var
            row += 1
        self.refresh()

    def name_from_value(self, code, value):
        mapping = vcp.VALUE_MAPS.get(code) or {}
        for c, n in mapping.items():
            if c == value:
                return n
        return None

    def refresh(self):
        def work():
            state = {}
            for key in FEATURE_LABELS:
                state[key] = self.mon.get_scaled(key)
            menus = {}
            for key in MENUS:
                code = vcp.feature_code(key)
                g = self.mon.get("0x%02x" % code)
                menus[key] = self.name_from_value(code, g["value"]) if g["value"] is not None else None
            return state, menus

        def done(payload):
            state, menus = payload
            for key, (var, val_lab) in self.sliders.items():
                val, _ = state[key]
                if val is None:
                    var.set(0)
                    val_lab.config(text="n/a")
                else:
                    var.set(val)
                    val_lab.config(text="%d" % val)
            for key, combo in self.combos.items():
                self.combo_vars[key].set(menus.get(key) or "")

        self.runner.submit(work, on_done=done)

    def _apply_slider(self, key, var, val_lab):
        val = int(var.get())
        val_lab.config(text="%d" % val)
        self.runner.submit(
            lambda: self.mon.set(key, val),
            on_error=lambda e: messagebox.showerror("ddpm", str(e)),
        )

    def _apply_menu(self, key, combo):
        name = combo.get()
        if not name:
            self.refresh()
            return
        self.runner.submit(
            lambda: self.mon.set(key, name),
            on_error=lambda e: messagebox.showerror("ddpm", str(e)),
        )


class App(tk.Tk):
    def __init__(self, backend=None):
        super().__init__()
        self.title("DDPM - Dell Display and Peripheral Manager (clone)")
        self.minsize(720, 320)
        self.runner = AsyncRunner(self)
        sel = backend or Config().get("driver") or "auto"
        self.mons = []
        self.panels = []
        self.presets = Presets()

        top = ttk.Frame(self, padding=6)
        top.pack(fill="both", expand=True)
        self.note = ttk.Notebook(top)
        self.note.pack(fill="both", expand=True)

        bar = ttk.Frame(self, padding=6)
        bar.pack(fill="x")
        ttk.Button(bar, text="Refresh", command=self.reload).pack(side="left")
        ttk.Label(bar, text="  Preset:").pack(side="left")
        self.preset_sel = ttk.Combobox(bar, width=16, state="readonly")
        self.preset_sel.pack(side="left", padx=4)
        ttk.Button(bar, text="Apply", command=self.apply_preset).pack(side="left")
        ttk.Button(bar, text="Save as...", command=self.save_preset).pack(side="left", padx=4)
        ttk.Button(bar, text="Delete", command=self.del_preset).pack(side="left")
        ttk.Label(bar, text="  backend=%s" % sel, foreground="#666").pack(
            side="right"
        )
        self.reload()

    def reload(self):
        def work():
            return monitors(Config().get("driver") or "auto")[1]

        def done(payload):
            self.mons = payload
            for w in self.note.winfo_children():
                w.destroy()
            self.panels = []
            if not self.mons:
                f = ttk.Frame(self.note, padding=20)
                f.pack(fill="both", expand=True)
                self.note.add(f, text="No displays")
                ttk.Label(
                    f,
                    text=(
                        "No DDC/CI displays found.\n\n"
                        "Try:\n"
                        "  sudo modprobe i2c-dev\n"
                        "  sudo apt install ddcutil\n"
                        "  sudo usermod -aG i2c $USER"
                    ),
                    justify="left",
                ).pack()
            for i, mon in enumerate(self.mons):
                panel = DisplayPanel(self.note, self.runner, mon)
                self.note.add(panel, text="%s" % (mon.name or "Display %d" % (i + 1)))
                self.panels.append(panel)
            self.refresh_presets()

        self.runner.submit(work, on_done=done)

    def refresh_presets(self):
        self.preset_sel["values"] = self.presets.names()
        if self.presets.names() and not self.preset_sel.get():
            self.preset_sel.current(0)

    def _current_mon(self):
        idx = self.note.index(self.note.select()) if self.note.tabs() else 0
        if idx < len(self.mons):
            return self.mons[idx]
        return None

    def apply_preset(self):
        name = self.preset_sel.get()
        if not name:
            return
        mon = self._current_mon()
        if mon is None:
            return
        spec = self.presets.get(name) or {}
        self.runner.submit(
            lambda: [mon.set(f, v) for f, v in spec.items()],
            on_done=lambda _: self.reload(),
            on_error=lambda e: messagebox.showerror("ddpm", str(e)),
        )

    def save_preset(self):
        mon = self._current_mon()
        if mon is None:
            messagebox.showinfo("ddpm", "No display selected.")
            return
        name = simpledialog.askstring(
            "Save preset", "Preset name:", parent=self
        )
        if not name:
            return

        def work():
            from .cli import snapshot

            return snapshot(mon)

        def done(spec):
            try:
                self.presets.set(name, spec)
            except PresetError as e:
                messagebox.showerror("ddpm", str(e))
                return
            self.refresh_presets()

        self.runner.submit(work, on_done=done)

    def del_preset(self):
        name = self.preset_sel.get()
        if not name:
            return
        try:
            self.presets.delete(name)
        except PresetError as e:
            messagebox.showerror("ddpm", str(e))
        self.refresh_presets()


def main(backend=None):
    App(backend).mainloop()


if __name__ == "__main__":
    main()

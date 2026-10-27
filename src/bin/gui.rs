//! ddpm GUI: eframe/egui window with per-display controls (DDPM-style).
//!
//! Port of the tkinter GUI: one tab per display, sliders for
//! brightness/contrast/volume, dropdowns for input/color temp/power/OSD/mute,
//! plus a preset bar (apply/save). Slow DDC/CI traffic happens on background
//! threads so the UI stays responsive.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use eframe::egui;
use serde_json::Value;

use ddpm::config::Config;
use ddpm::driver::{monitors, Monitor};
use ddpm::presets::Presets;
use ddpm::vcp;

const FEATURE_LABELS: [(&str, &str); 3] = [
    ("brightness", "Brightness"),
    ("contrast", "Contrast"),
    ("volume", "Volume"),
];

const MENUS: [(&str, &str, ValueTable); 5] = [
    ("input", "Input", vcp::INPUT_VALUES),
    ("color-temp", "Color temp", vcp::COLOR_PRESET_VALUES),
    ("power", "Power", vcp::POWER_VALUES),
    ("osd", "OSD", vcp::OSD_VALUES),
    ("mute", "Mute", vcp::MUTE_VALUES),
];

const NO_DISPLAYS_TEXT: &str = "No DDC/CI displays found.\n\nTry:\n  sudo modprobe i2c-dev\n  sudo apt install ddcutil\n  sudo usermod -aG i2c $USER";

type Spec = BTreeMap<String, Value>;
type ValueTable = &'static [(u16, &'static str)];

enum GuiMsg {
    /// reload finished: fresh list of monitors (or error)
    Reload(Result<Vec<Monitor>, String>),
    /// per-display state read finished (keyed by monitor uuid)
    Panel(String, PanelData),
    /// a plain set finished: error -> popup
    SetDone(Result<(), String>),
    /// a preset apply finished: ok -> reload, error -> popup
    Applied(Result<(), String>),
    /// snapshot finished for a "Save as..." with the chosen name
    Snapshot(String, Result<Spec, String>),
}

struct PanelData {
    /// (value, 100) per slider feature, None when unsupported
    sliders: [(Option<u16>, Option<u16>); 3],
    /// selected symbolic name per menu, None when unsupported/unknown
    menus: [Option<String>; 5],
}

struct PanelUi {
    uuid: String,
    sliders: [f32; 3],
    slider_texts: [String; 3],
    menus: [String; 5],
}

impl PanelUi {
    fn new(uuid: String) -> Self {
        PanelUi {
            uuid,
            sliders: [50.0; 3],
            slider_texts: std::array::from_fn(|_| String::new()),
            menus: std::array::from_fn(|_| String::new()),
        }
    }
}

fn name_from_value(code: u8, value: u16) -> Option<String> {
    vcp::value_map(code)?
        .iter()
        .find(|(k, _)| *k == value)
        .map(|(_, name)| name.to_string())
}

fn configured_driver() -> String {
    Config::new()
        .ok()
        .and_then(|c| c.get("driver").and_then(|v| v.as_str()).map(String::from))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "auto".to_string())
}

struct App {
    backend_label: String,
    tx: Sender<GuiMsg>,
    rx: Receiver<GuiMsg>,
    mons: Vec<Monitor>,
    panels: Vec<PanelUi>,
    tab: usize,
    presets: Presets,
    preset_names: Vec<String>,
    preset_sel: String,
    save_open: bool,
    save_name: String,
    error: Option<String>,
    info: Option<String>,
}

impl App {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        // sel = backend arg or Config().get("driver") or "auto" (label only)
        let backend_label = std::env::args()
            .nth(1)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(configured_driver);
        let mut app = App {
            backend_label,
            tx,
            rx,
            mons: Vec::new(),
            panels: Vec::new(),
            tab: 0,
            presets: Presets::new(),
            preset_names: Vec::new(),
            preset_sel: String::new(),
            save_open: false,
            save_name: String::new(),
            error: None,
            info: None,
        };
        app.refresh_presets();
        app.reload();
        app
    }

    fn submit<T, F>(&self, f: F, done: impl FnOnce(Result<T, String>) -> GuiMsg + Send + 'static)
    where
        F: FnOnce() -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(done(f()));
        });
    }

    fn reload(&mut self) {
        let sel = configured_driver();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let msg = match monitors(&sel) {
                Ok((_, mons)) => GuiMsg::Reload(Ok(mons)),
                Err(e) => GuiMsg::Reload(Err(e.to_string())),
            };
            let _ = tx.send(msg);
        });
    }

    fn refresh_panel(&self, mon: &Monitor) {
        let uuid = mon.uuid();
        let mon = mon.clone();
        self.submit(
            move || {
                let mut sliders = [(None, None); 3];
                for (i, (key, _)) in FEATURE_LABELS.iter().enumerate() {
                    sliders[i] = mon.get_scaled(key);
                }
                let mut menus: [Option<String>; 5] = std::array::from_fn(|_| None);
                for (i, (key, _, _)) in MENUS.iter().enumerate() {
                    if let Ok(code) = vcp::feature_code(key) {
                        if let Ok(g) = mon.get_code(code) {
                            menus[i] = g.value.and_then(|v| name_from_value(code, v));
                        }
                    }
                }
                Ok(PanelData { sliders, menus })
            },
            move |res| match res {
                Ok(data) => GuiMsg::Panel(uuid, data),
                Err(e) => GuiMsg::SetDone(Err(e)),
            },
        );
    }

    fn refresh_presets(&mut self) {
        self.preset_names = self.presets.names();
        if !self.preset_names.is_empty() && self.preset_sel.is_empty() {
            self.preset_sel = self.preset_names[0].clone();
        }
    }

    fn current_mon(&self) -> Option<&Monitor> {
        self.mons.get(self.tab)
    }

    fn handle(&mut self, msg: GuiMsg) {
        match msg {
            GuiMsg::Reload(Ok(mons)) => {
                self.mons = mons;
                self.panels.clear();
                self.tab = 0;
                for m in &self.mons {
                    self.panels.push(PanelUi::new(m.uuid()));
                    self.refresh_panel(m);
                }
                self.refresh_presets();
            }
            GuiMsg::Reload(Err(e)) => self.error = Some(e),
            GuiMsg::Panel(uuid, data) => {
                if let Some(panel) = self.panels.iter_mut().find(|p| p.uuid == uuid) {
                    for (i, &(value, _max)) in data.sliders.iter().enumerate() {
                        match value {
                            None => {
                                panel.sliders[i] = 0.0;
                                panel.slider_texts[i] = "n/a".to_string();
                            }
                            Some(v) => {
                                panel.sliders[i] = v as f32;
                                panel.slider_texts[i] = format!("{}", v);
                            }
                        }
                    }
                    for (i, name) in data.menus.into_iter().enumerate() {
                        panel.menus[i] = name.unwrap_or_default();
                    }
                }
            }
            GuiMsg::SetDone(Err(e)) => self.error = Some(e),
            GuiMsg::SetDone(Ok(())) => {}
            GuiMsg::Applied(Ok(())) => self.reload(),
            GuiMsg::Applied(Err(e)) => self.error = Some(e),
            GuiMsg::Snapshot(name, res) => match res {
                Ok(spec) => {
                    if let Err(e) = self.presets.set(&name, spec) {
                        self.error = Some(e.to_string());
                    }
                    self.refresh_presets();
                }
                Err(e) => self.error = Some(e),
            },
        }
    }

    fn apply_slider(&mut self, idx: usize) {
        let Some(mon) = self.current_mon().cloned() else {
            return;
        };
        let key = FEATURE_LABELS[idx].0;
        let val = self.panels[self.tab].sliders[idx].round() as i64;
        self.panels[self.tab].slider_texts[idx] = format!("{}", val);
        self.submit(
            move || mon.set(key, &Value::from(val)).map_err(|e| e.to_string()),
            |res| GuiMsg::SetDone(res.map(|_| ())),
        );
    }

    fn apply_menu(&mut self, idx: usize) {
        let name = self.panels[self.tab].menus[idx].clone();
        if name.is_empty() {
            if let Some(mon) = self.current_mon().cloned() {
                self.refresh_panel(&mon);
            }
            return;
        }
        let Some(mon) = self.current_mon().cloned() else {
            return;
        };
        let key = MENUS[idx].0;
        self.submit(
            move || {
                mon.set(key, &Value::String(name))
                    .map_err(|e| e.to_string())
            },
            |res| GuiMsg::SetDone(res.map(|_| ())),
        );
    }

    fn start_save_preset(&mut self, name: String) {
        let Some(mon) = self.current_mon().cloned() else {
            return;
        };
        self.submit(
            move || {
                let spec = ddpm::cli::snapshot(&mon);
                Ok((name, spec))
            },
            |res| match res {
                Ok((name, spec)) => GuiMsg::Snapshot(name, Ok(spec)),
                Err(e) => GuiMsg::Snapshot(String::new(), Err(e)),
            },
        );
    }

    fn apply_preset(&mut self) {
        let name = self.preset_sel.clone();
        if name.is_empty() {
            return;
        }
        let Some(mon) = self.current_mon().cloned() else {
            return;
        };
        let Some(spec) = self.presets.get(&name).cloned() else {
            return;
        };
        self.submit(
            move || {
                for (f, val) in &spec {
                    mon.set(f, val).map_err(|e| e.to_string())?;
                }
                Ok(())
            },
            GuiMsg::Applied,
        );
    }

    fn delete_preset(&mut self) {
        let name = self.preset_sel.clone();
        if name.is_empty() {
            return;
        }
        if let Err(e) = self.presets.delete(&name) {
            self.error = Some(e.to_string());
        }
        self.refresh_presets();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }

        egui::TopBottomPanel::bottom("ddpm_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui.button("Refresh").clicked() {
                    self.reload();
                }
                ui.separator();
                ui.label("  Preset:");
                let names = self.preset_names.clone();
                egui::ComboBox::from_id_salt("preset_sel")
                    .width(140.0)
                    .selected_text(self.preset_sel.clone())
                    .show_ui(ui, |ui| {
                        for name in names {
                            ui.selectable_value(&mut self.preset_sel, name.clone(), &name);
                        }
                    });
                if ui.button("Apply").clicked() {
                    self.apply_preset();
                }
                if ui.button("Save as...").clicked() {
                    if self.current_mon().is_none() {
                        self.info = Some("No display selected.".to_string());
                    } else {
                        self.save_open = true;
                        self.save_name.clear();
                    }
                }
                if ui.button("Delete").clicked() {
                    self.delete_preset();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("backend={}", self.backend_label));
                });
            });
            ui.add_space(2.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // Tab bar: one tab per display (or "No displays").
            ui.horizontal(|ui| {
                if self.mons.is_empty() {
                    let _ = ui.selectable_label(true, "No displays");
                } else {
                    for (i, m) in self.mons.iter().enumerate() {
                        let label = if m.name().is_empty() {
                            format!("Display {}", i + 1)
                        } else {
                            m.name().to_string()
                        };
                        if ui.selectable_label(self.tab == i, label).clicked() {
                            self.tab = i;
                        }
                    }
                }
            });
            ui.separator();

            let tab = self.tab;
            let Some(mon) = self.mons.get(tab).cloned() else {
                ui.add_space(16.0);
                ui.label(NO_DISPLAYS_TEXT);
                return;
            };
            let Some(panel) = self.panels.get_mut(tab) else {
                return;
            };

            let mut pending_slider: Option<usize> = None;
            let mut pending_menu: Option<usize> = None;
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(4.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_width(ui.available_width() - 16.0);
                    egui::Grid::new(("display_panel", mon.uuid()))
                        .num_columns(3)
                        .spacing([8.0, 8.0])
                        .show(ui, |ui| {
                            for (i, (_, label)) in FEATURE_LABELS.iter().enumerate() {
                                ui.label(*label);
                                let resp =
                                    ui.add(egui::Slider::new(&mut panel.sliders[i], 0.0..=100.0));
                                if resp.drag_stopped() {
                                    pending_slider = Some(i);
                                }
                                ui.monospace(format!("{:>4}", panel.slider_texts[i]));
                                ui.end_row();
                            }
                            for (i, (_, label, _)) in MENUS.iter().enumerate() {
                                ui.label(*label);
                                let mut sel = panel.menus[i].clone();
                                egui::ComboBox::from_id_salt(format!("menu-{}-{}", mon.uuid(), i))
                                    .width(140.0)
                                    .selected_text(if sel.is_empty() {
                                        " ".to_string()
                                    } else {
                                        sel.clone()
                                    })
                                    .show_ui(ui, |ui| {
                                        let (_, _, values) = MENUS[i];
                                        for (_, name) in values {
                                            ui.selectable_value(&mut sel, name.to_string(), *name);
                                        }
                                    });
                                if sel != panel.menus[i] {
                                    panel.menus[i] = sel;
                                    pending_menu = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                });
            });
            if let Some(i) = pending_slider {
                self.apply_slider(i);
            }
            if let Some(i) = pending_menu {
                self.apply_menu(i);
            }
        });

        // Save-preset dialog (like simpledialog.askstring).
        if self.save_open {
            let mut open = self.save_open;
            egui::Window::new("Save preset")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Preset name:");
                    let name_edit = ui
                        .add(egui::TextEdit::singleline(&mut self.save_name).desired_width(200.0));
                    if name_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let name = self.save_name.trim().to_string();
                        self.save_open = false;
                        if !name.is_empty() {
                            self.start_save_preset(name);
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            let name = self.save_name.trim().to_string();
                            self.save_open = false;
                            if !name.is_empty() {
                                self.start_save_preset(name);
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.save_open = false;
                        }
                    });
                });
            self.save_open = open;
        }

        // Error popup (like messagebox.showerror).
        if let Some(err) = self.error.clone() {
            let mut open = true;
            egui::Window::new("ddpm — error")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(err);
                    if ui.button("OK").clicked() {
                        self.error = None;
                    }
                });
            if !open {
                self.error = None;
            }
        }

        // Info popup (like messagebox.showinfo).
        if let Some(info) = self.info.clone() {
            let mut open = true;
            egui::Window::new("ddpm")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(info);
                    if ui.button("OK").clicked() {
                        self.info = None;
                    }
                });
            if !open {
                self.info = None;
            }
        }

        // Keep polling background job results while they may be in flight.
        ctx.request_repaint_after(std::time::Duration::from_millis(80));
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 320.0])
            .with_min_inner_size([720.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DDPM - Dell Display and Peripheral Manager (clone)",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}

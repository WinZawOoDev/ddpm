//! ddpm GUI: eframe/egui window with per-display controls (DDPM-style).
//!
//! Port of the tkinter GUI: one tab per display, sliders for
//! brightness/contrast/volume, dropdowns for input/color temp/power/mute,
//! with slow DDC/CI traffic on background threads so the UI stays responsive.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use eframe::egui;
use serde_json::Value;

use clap::Parser;
use ddpm::driver::{monitors, resolve_backend, BackendSel, Monitor};
use ddpm::vcp;

const FEATURE_LABELS: [(&str, &str); 3] = [
    ("brightness", "Brightness"),
    ("contrast", "Contrast"),
    ("volume", "Volume"),
];

const MENUS: [(&str, &str, ValueTable); 4] = [
    ("input", "Input", vcp::INPUT_VALUES),
    ("color-temp", "Color temp", vcp::COLOR_PRESET_VALUES),
    ("power", "Power", vcp::POWER_VALUES),
    ("mute", "Mute", vcp::MUTE_VALUES),
];

/// Command line options: the same backend choice as the CLI.
#[derive(Parser)]
#[command(name = "ddpm-gui", about = "DDC/CI controls for Dell-style displays")]
struct GuiArgs {
    /// backend driver: auto (default), ddcutil, direct i2c (ddcci), mock
    #[arg(long, value_enum, ignore_case = true)]
    backend: Option<BackendSel>,
}

type ValueTable = &'static [(u16, &'static str)];

enum GuiMsg {
    /// reload finished: fresh list of monitors (or error)
    Reload(Result<Vec<Monitor>, String>),
    /// per-display state read finished (keyed by monitor uuid)
    Panel(String, PanelData),
    /// a plain set finished: error -> popup
    SetDone(Result<(), String>),
}

struct PanelData {
    /// (value, 100) per slider feature, None when unsupported
    sliders: [(Option<u16>, Option<u16>); 3],
    /// selected symbolic name per menu, None when unsupported/unknown
    menus: [Option<String>; 4],
}

struct PanelUi {
    uuid: String,
    sliders: [f32; 3],
    slider_texts: [String; 3],
    menus: [String; 4],
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

struct App {
    /// Backend selector used for every reload (from --backend or the config).
    backend_sel: String,
    tx: Sender<GuiMsg>,
    rx: Receiver<GuiMsg>,
    mons: Vec<Monitor>,
    panels: Vec<PanelUi>,
    tab: usize,
    loading: bool,
    error: Option<String>,
}

impl App {
    fn new(explicit: Option<BackendSel>) -> Self {
        let (tx, rx) = mpsc::channel();
        let (backend_sel, backend_error) = match resolve_backend(explicit) {
            Ok(sel) => (sel, None),
            Err(e) => ("auto".to_string(), Some(e.to_string())),
        };
        let mut app = App {
            backend_sel,
            tx,
            rx,
            mons: Vec::new(),
            panels: Vec::new(),
            tab: 0,
            loading: false,
            error: backend_error,
        };
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
        self.loading = true;
        let sel = self.backend_sel.clone();
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
                let mut menus: [Option<String>; 4] = std::array::from_fn(|_| None);
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

    fn current_mon(&self) -> Option<&Monitor> {
        self.mons.get(self.tab)
    }

    fn handle(&mut self, msg: GuiMsg) {
        match msg {
            GuiMsg::Reload(Ok(mons)) => {
                self.loading = false;
                self.mons = mons;
                self.panels.clear();
                self.tab = self.tab.min(self.mons.len().saturating_sub(1));
                for m in &self.mons {
                    self.panels.push(PanelUi::new(m.uuid()));
                    self.refresh_panel(m);
                }
            }
            GuiMsg::Reload(Err(e)) => {
                self.loading = false;
                self.error = Some(e);
            }
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
}

fn display_name(mon: &Monitor, index: usize) -> String {
    if mon.name().is_empty() {
        format!("Display {}", index + 1)
    } else {
        mon.name().to_string()
    }
}

fn draw_display_preview(
    ui: &mut egui::Ui,
    width: f32,
    brightness: Option<u16>,
    model_name: &str,
    input: &str,
    color_temp: &str,
) {
    let size = egui::vec2(width.max(240.0), 250.0);
    let (bounds, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(bounds);
    let bg = egui::Color32::from_rgb(18, 27, 40);
    painter.rect_filled(bounds, 10.0, bg);

    let display_width = (bounds.width() * 0.64)
        .min(560.0)
        .min((bounds.width() - 32.0).max(140.0));
    let display_height = 154.0;
    let outer_size = egui::vec2(display_width + 18.0, display_height + 18.0);
    let outer = egui::Rect::from_min_size(
        egui::pos2(bounds.center().x - outer_size.x / 2.0, bounds.top() + 16.0),
        outer_size,
    );
    painter.rect_filled(outer, 10.0, egui::Color32::from_rgb(48, 61, 78));

    let screen = outer.shrink(9.0);
    let percent = brightness.unwrap_or(50).min(100);
    let light = (24.0 + percent as f32 * 1.68) as u8;
    let screen_fill = if color_temp.contains("4000") || color_temp.contains("5000") {
        egui::Color32::from_rgb(
            light,
            (light as f32 * 0.79) as u8,
            (light as f32 * 0.57) as u8,
        )
    } else if color_temp.contains("9300") || color_temp.contains("10000") {
        egui::Color32::from_rgb(
            (light as f32 * 0.62) as u8,
            (light as f32 * 0.82) as u8,
            light,
        )
    } else {
        egui::Color32::from_rgb(
            (light as f32 * 0.64) as u8,
            (light as f32 * 0.84) as u8,
            light,
        )
    };
    painter.rect_filled(screen, 4.0, screen_fill);

    // A small wallpaper-like scene makes the preview read as a display rather
    // than a plain brightness meter. Its overall luminance follows the monitor.
    let clipped = painter.with_clip_rect(screen);
    let sun = egui::Color32::from_rgb(
        (255.0 * (0.28 + percent as f32 / 140.0)).min(255.0) as u8,
        (177.0 * (0.28 + percent as f32 / 140.0)).min(255.0) as u8,
        (104.0 * (0.28 + percent as f32 / 140.0)).min(255.0) as u8,
    );
    clipped.circle_filled(
        egui::pos2(
            screen.right() - screen.width() * 0.23,
            screen.top() + screen.height() * 0.35,
        ),
        screen.height() * 0.20,
        sun,
    );
    clipped.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(screen.left(), screen.top() + screen.height() * 0.70),
            screen.right_bottom(),
        ),
        0.0,
        egui::Color32::from_rgba_unmultiplied(14, 31, 50, 125),
    );
    clipped.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(screen.left() + 14.0, screen.top() + 14.0),
            egui::vec2((screen.width() * 0.20).min(100.0), 7.0),
        ),
        3.0,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 175),
    );
    clipped.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(screen.left() + 14.0, screen.top() + 27.0),
            egui::vec2((screen.width() * 0.12).min(62.0), 5.0),
        ),
        2.0,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 105),
    );

    let stand_x = bounds.center().x;
    painter.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(stand_x - 7.0, outer.bottom()),
            egui::vec2(14.0, 17.0),
        ),
        2.0,
        egui::Color32::from_rgb(57, 70, 87),
    );
    painter.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(stand_x - 44.0, outer.bottom() + 15.0),
            egui::vec2(88.0, 6.0),
        ),
        3.0,
        egui::Color32::from_rgb(57, 70, 87),
    );

    let input_label = if input.is_empty() {
        "Input not reported"
    } else {
        input
    };
    painter.text(
        egui::pos2(bounds.left() + 16.0, bounds.bottom() - 15.0),
        egui::Align2::LEFT_CENTER,
        format!("{}   ·   Input source: {}", model_name, input_label),
        egui::FontId::proportional(12.0),
        egui::Color32::from_rgb(183, 199, 216),
    );
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }

        let mut pending_slider: Option<usize> = None;
        let mut pending_menu: Option<usize> = None;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .inner_margin(egui::Margin {
                        left: 16.0,
                        right: 16.0,
                        top: 16.0,
                        bottom: 0.0,
                    }),
            )
            .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(8.0);

                    if self.mons.is_empty() {
                        egui::Frame::group(ui.style())
                            .fill(egui::Color32::from_rgb(20, 29, 41))
                            .show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(20.0);
                                    ui.label(
                                        egui::RichText::new(if self.loading {
                                            "Looking for DDC/CI displays…"
                                        } else {
                                            "No displays found"
                                        })
                                        .size(20.0)
                                        .strong(),
                                    );
                                    ui.add_space(5.0);
                                    ui.label(
                                        "Connect a DDC/CI-capable monitor, then scan again. For setup help:",
                                    );
                                    ui.add_space(5.0);
                                    ui.monospace("sudo modprobe i2c-dev  ·  install ddcutil  ·  grant i2c access");
                                    ui.add_space(12.0);
                                    if ui.button("Scan displays").clicked() {
                                        self.reload();
                                    }
                                    ui.add_space(8.0);
                                });
                            });
                    } else {
                        let cards: Vec<(String, String)> = self
                            .mons
                            .iter()
                            .enumerate()
                            .map(|(i, mon)| {
                                (
                                    display_name(mon, i),
                                    mon.bus().unwrap_or("DDC/CI").to_string(),
                                )
                            })
                            .collect();
                        ui.horizontal(|ui| {
                            for (i, (name, bus)) in cards.iter().enumerate() {
                                egui::Frame::group(ui.style())
                                    .fill(if self.tab == i {
                                        egui::Color32::from_rgb(27, 48, 69)
                                    } else {
                                        egui::Color32::from_rgb(19, 28, 40)
                                    })
                                    .show(ui, |ui| {
                                        ui.set_min_width(190.0);
                                        let response = ui.selectable_label(
                                            self.tab == i,
                                            egui::RichText::new(format!(
                                                "Display {} · {}",
                                                i + 1,
                                                name
                                            ))
                                            .strong()
                                            .color(egui::Color32::from_rgb(225, 237, 249)),
                                        );
                                        if response.clicked() {
                                            self.tab = i;
                                        }
                                        ui.label(
                                            egui::RichText::new(bus)
                                                .size(11.0)
                                                .color(egui::Color32::from_rgb(133, 158, 183)),
                                        );
                                    });
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let refresh_label = if self.loading {
                                        "Scanning…"
                                    } else {
                                        "↻  Refresh"
                                    };
                                    if ui
                                        .add_enabled(
                                            !self.loading,
                                            egui::Button::new(refresh_label),
                                        )
                                        .clicked()
                                    {
                                        self.reload();
                                    }
                                },
                            );
                        });

                        let tab = self.tab.min(self.mons.len() - 1);
                        self.tab = tab;
                        let mon = self.mons[tab].clone();
                        ui.add_space(10.0);

                        if let Some(panel) = self.panels.get_mut(tab) {
                            let brightness = if panel.slider_texts[0] == "n/a" {
                                None
                            } else {
                                Some(panel.sliders[0].round() as u16)
                            };
                            let input = panel.menus[0].clone();
                            let color_temp = panel.menus[1].clone();

                            ui.columns(2, |columns| {
                                let (preview_columns, control_columns) = columns.split_at_mut(1);
                                let preview_column = &mut preview_columns[0];
                                let control_column = &mut control_columns[0];

                                egui::Frame::group(preview_column.style())
                                    .fill(egui::Color32::from_rgb(17, 25, 36))
                                    .show(preview_column, |ui| {
                                        draw_display_preview(
                                            ui,
                                            ui.available_width(),
                                            brightness,
                                            &display_name(&mon, tab),
                                            &input,
                                            &color_temp,
                                        );
                                    });

                                control_column.label(
                                    egui::RichText::new("Picture & audio")
                                        .size(17.0)
                                        .strong(),
                                );
                                control_column.add_space(4.0);
                                for i in 0..FEATURE_LABELS.len() {
                                    egui::Frame::group(control_column.style())
                                        .fill(egui::Color32::from_rgb(20, 29, 41))
                                        .show(control_column, |ui| {
                                            let unavailable = panel.slider_texts[i] == "n/a";
                                            let value = if unavailable {
                                                "—".to_string()
                                            } else {
                                                format!("{}%", panel.sliders[i].round() as u16)
                                            };
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    egui::RichText::new(FEATURE_LABELS[i].1)
                                                        .color(egui::Color32::from_rgb(
                                                            167, 188, 210,
                                                        )),
                                                );
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        ui.label(
                                                            egui::RichText::new(value)
                                                                .size(24.0)
                                                                .strong()
                                                                .color(egui::Color32::from_rgb(
                                                                    237, 245, 255,
                                                                )),
                                                        );
                                                    },
                                                );
                                            });
                                            let response = ui
                                                .add_enabled_ui(!unavailable, |ui| {
                                                    ui.spacing_mut().slider_width =
                                                        ui.available_width();
                                                    ui.add(
                                                        egui::Slider::new(
                                                            &mut panel.sliders[i],
                                                            0.0..=100.0,
                                                        )
                                                        .show_value(false),
                                                    )
                                                })
                                                .inner;
                                            if response.drag_stopped() {
                                                pending_slider = Some(i);
                                            }
                                            ui.label(
                                                egui::RichText::new(if unavailable {
                                                    "Not supported by this display"
                                                } else {
                                                    "Drag to adjust"
                                                })
                                                .size(10.0)
                                                .color(egui::Color32::from_rgb(123, 146, 170)),
                                            );
                                        });
                                }
                            });

                            ui.add_space(12.0);
                            ui.label(egui::RichText::new("Display settings").size(17.0).strong());
                            ui.add_space(4.0);
                            egui::Frame::group(ui.style())
                                .fill(egui::Color32::from_rgb(20, 29, 41))
                                .show(ui, |ui| {
                                    egui::Grid::new(("display_settings", mon.uuid()))
                                        .num_columns(2)
                                        .spacing([18.0, 12.0])
                                        .show(ui, |ui| {
                                            for i in 0..MENUS.len() {
                                                ui.vertical(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(match i {
                                                            0 => "Input source",
                                                            1 => "Color temperature",
                                                            2 => "Power state",
                                                            _ => "Audio mute",
                                                        })
                                                        .color(egui::Color32::from_rgb(167, 188, 210)),
                                                    );
                                                    let mut selected = panel.menus[i].clone();
                                                    egui::ComboBox::from_id_salt((
                                                        "setting",
                                                        mon.uuid(),
                                                        i,
                                                    ))
                                                    .width((ui.available_width() - 12.0).max(150.0))
                                                    .selected_text(if selected.is_empty() {
                                                        "Choose setting…".to_string()
                                                    } else {
                                                        selected.clone()
                                                    })
                                                    .show_ui(ui, |ui| {
                                                        let (_, _, values) = MENUS[i];
                                                        for (_, name) in values {
                                                            ui.selectable_value(
                                                                &mut selected,
                                                                name.to_string(),
                                                                *name,
                                                            );
                                                        }
                                                    });
                                                    if selected != panel.menus[i] {
                                                        panel.menus[i] = selected;
                                                        pending_menu = Some(i);
                                                    }
                                                });
                                                if i % 2 == 1 {
                                                    ui.end_row();
                                                }
                                            }
                                        });
                                });
                        }

                    }
                });
            });

        if let Some(i) = pending_slider {
            self.apply_slider(i);
        }
        if let Some(i) = pending_menu {
            self.apply_menu(i);
        }

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

        ctx.request_repaint_after(std::time::Duration::from_millis(80));
    }
}

fn main() -> eframe::Result<()> {
    let args = GuiArgs::parse();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 560.0])
            .with_min_inner_size([820.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DDPM",
        options,
        Box::new(move |cc| {
            let mut visuals = egui::Visuals::dark();
            visuals.panel_fill = egui::Color32::from_rgb(15, 22, 32);
            visuals.window_fill = egui::Color32::from_rgb(22, 31, 44);
            visuals.extreme_bg_color = egui::Color32::from_rgb(11, 17, 26);
            visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(35, 49, 66);
            visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(48, 68, 89);
            visuals.widgets.active.bg_fill = egui::Color32::from_rgb(55, 112, 161);
            visuals.selection.bg_fill = egui::Color32::from_rgb(46, 126, 187);
            cc.egui_ctx.set_visuals(visuals);
            Ok(Box::new(App::new(args.backend)))
        }),
    )
}

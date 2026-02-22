use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::egui::{self, Color32, RichText, Stroke, vec2};
use rfd::FileDialog;

use crate::{
    alsa_backend::AlsaBackend,
    config::AppUserConfig,
    models::{ControlDescriptor, ControlKind, RouteRef, RoutingIndex},
    presets,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    MixRouting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenameTarget {
    Ain(usize),
    Din(usize),
    Out(usize),
}

pub struct MixerApp {
    backend: AlsaBackend,
    controls: Vec<ControlDescriptor>,
    routing_index: RoutingIndex,
    selected_tab: Tab,
    status_line: String,
    user_config: AppUserConfig,
    rename_target: Option<RenameTarget>,
    rename_buffer: String,
    last_auto_refresh: Instant,
    last_full_refresh: Instant,
    alsa_event_rx: Option<Receiver<()>>,
    event_listener_initialized: bool,
    theme_initialized: bool,
}

impl MixerApp {
    const KNOB_CELL_W: f32 = 82.0;
    const KNOB_CELL_H: f32 = 74.0;
    const ROW_LABEL_W: f32 = 150.0;

    pub fn bootstrap(
        card_override: Option<u32>,
        startup_preset: Option<&str>,
    ) -> Result<Self> {
        let backend = AlsaBackend::pick_card(card_override)?;
        let controls = backend.list_controls()?;
        let mut status_line = format!("Ready ({:?} backend)", backend.active_backend());
        let user_config = match AppUserConfig::load_or_default() {
            Ok(cfg) => cfg,
            Err(err) => {
                status_line = format!("Config load warning: {err}");
                AppUserConfig::default()
            }
        };
        let mut app = Self {
            routing_index: AlsaBackend::build_routing_index(&controls),
            backend,
            controls,
            selected_tab: Tab::MixRouting,
            status_line,
            user_config,
            rename_target: None,
            rename_buffer: String::new(),
            last_auto_refresh: Instant::now(),
            last_full_refresh: Instant::now(),
            alsa_event_rx: None,
            event_listener_initialized: false,
            theme_initialized: false,
        };

        if let Some(path) = startup_preset {
            match app.load_preset_from(Path::new(path)) {
                Ok(()) => {
                    app.status_line = format!("Loaded startup preset: {path}");
                }
                Err(err) => {
                    app.status_line = format!("Startup preset load failed: {err}");
                }
            }
        }

        Ok(app)
    }

    fn refresh_controls(&mut self) {
        let _ = self.refresh_controls_with_status(true);
    }

    fn refresh_controls_with_status(&mut self, show_success_status: bool) -> bool {
        let favorite_map: HashMap<u32, bool> =
            self.controls.iter().map(|c| (c.numid, c.favorite)).collect();
        match self.backend.list_controls() {
            Ok(mut controls) => {
                let had_catalog_change = controls.len() != self.controls.len()
                    || controls
                        .iter()
                        .zip(self.controls.iter())
                        .any(|(new_c, old_c)| new_c.numid != old_c.numid || new_c.values != old_c.values);
                for c in &mut controls {
                    c.favorite = favorite_map.get(&c.numid).copied().unwrap_or(false);
                }
                self.routing_index = AlsaBackend::build_routing_index(&controls);
                self.controls = controls;
                if show_success_status {
                    self.status_line = "Control catalog refreshed".to_string();
                }
                self.last_full_refresh = Instant::now();
                had_catalog_change
            }
            Err(err) => {
                self.status_line = format!("Refresh failed: {err}");
                true
            }
        }
    }

    fn apply_values_to_control(&mut self, control_index: usize, values: Vec<String>) {
        let Some(control) = self.controls.get(control_index).cloned() else {
            return;
        };
        if let Err(err) = self.backend.apply_values(control.numid, &values) {
            self.status_line = format!("Write failed for {}: {err}", control.name);
            return;
        }
        match self.backend.reload_control(&control) {
            Ok(mut reloaded) => {
                reloaded.favorite = control.favorite;
                reloaded.grouped_label = control.grouped_label;
                self.controls[control_index] = reloaded;
                self.status_line = format!("Updated {}", control.name);
                self.last_full_refresh = Instant::now();
            }
            Err(err) => {
                self.status_line = format!("Reload failed for {}: {err}", control.name);
            }
        }
    }

    fn refresh_live_values_only(&mut self) -> bool {
        match self.backend.refresh_control_values(&mut self.controls) {
            Ok(updated) => updated > 0,
            Err(err) => {
                self.status_line = format!("Live refresh failed: {err}");
                true
            }
        }
    }

    fn load_preset_from(&mut self, path: &Path) -> Result<()> {
        let preset = presets::load_preset(path)?;
        let by_numid: HashMap<u32, Vec<String>> = preset
            .controls
            .into_iter()
            .map(|v| (v.numid, v.values))
            .collect();

        let mut applied = 0usize;
        for control in self.controls.clone() {
            if let Some(values) = by_numid.get(&control.numid) {
                self.backend.apply_values(control.numid, values)?;
                applied += 1;
            }
        }
        self.refresh_controls();
        self.status_line = format!("Preset applied ({applied} controls)");
        Ok(())
    }

    fn render_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("FTU Mixer").strong().size(15.0));
            ui.separator();
            ui.label(format!(
                "Card: hw:{} ({})",
                self.backend.card_index, self.backend.card_label
            ));
            if ui.button("Refresh").clicked() {
                self.refresh_controls();
            }
            if ui.button("Save preset").clicked() {
                if let Some(path) = FileDialog::new()
                    .set_file_name("fast-track-ultra-preset.json")
                    .save_file()
                {
                    let preset = presets::to_preset(&self.backend.card_label, &self.controls);
                    match presets::save_preset(&path, &preset) {
                        Ok(()) => self.status_line = format!("Preset saved: {}", path.display()),
                        Err(err) => self.status_line = format!("Save failed: {err}"),
                    }
                }
            }
            if ui.button("Load preset").clicked() {
                if let Some(path) = FileDialog::new().pick_file() {
                    match self.load_preset_from(&path) {
                        Ok(()) => {
                            self.status_line = format!("Preset loaded: {}", path.display());
                        }
                        Err(err) => self.status_line = format!("Load failed: {err}"),
                    }
                }
            }
        });
    }

    fn render_quick_actions(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Mute Analog Monitoring").clicked() {
                self.mute_hardware_routes();
            }
            if ui.button("Pass-through Analog Monitoring to Channel 1/2").clicked() {
                self.pass_through_inputs();
            }
            if ui.button("Disable FX").clicked() {
                self.disable_fx_controls();
            }
            if ui.button("Mute most digital routes").clicked() {
                self.mute_most_digital_routes();
            }
            if ui.button("Mute All Monitoring").clicked() {
                self.panic_mute();
            }
            if ui.button("Reset aliases").clicked() {
                self.user_config.ain_aliases.clear();
                self.user_config.din_aliases.clear();
                self.user_config.out_aliases.clear();
                self.rename_target = None;
                self.rename_buffer.clear();
                self.save_user_config();
            }
        });
    }

    fn render_mix_routing_tab(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(Color32::from_rgb(20, 24, 30))
            .stroke(Stroke::new(1.0, Color32::from_rgb(46, 55, 68)))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.label(RichText::new("Actions rapides").strong());
                self.render_quick_actions(ui);
            });

        ui.add_space(6.0);
        ui.columns(2, |cols| {
            egui::Frame::new()
                .fill(Color32::from_rgb(18, 22, 27))
                .stroke(Stroke::new(1.0, Color32::from_rgb(44, 52, 64)))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(&mut cols[0], |ui| {
                    ui.label(RichText::new("Monitoring analogique").strong().size(14.0));
                    ui.small("AIn -> Out");
                    ui.separator();
                    self.render_monitoring_matrix(ui);
                });

            egui::Frame::new()
                .fill(Color32::from_rgb(18, 22, 27))
                .stroke(Stroke::new(1.0, Color32::from_rgb(44, 52, 64)))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(&mut cols[1], |ui| {
                    ui.label(RichText::new("Routage digital").strong().size(14.0));
                    ui.small("DIn -> Out");
                    ui.separator();
                    self.render_route_matrix(ui, false);
                });
        });

        ui.add_space(6.0);
        egui::Frame::new()
            .fill(Color32::from_rgb(18, 22, 27))
            .stroke(Stroke::new(1.0, Color32::from_rgb(44, 52, 64)))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                self.render_effects_section(ui);
            });
    }

    fn render_monitoring_matrix(&mut self, ui: &mut egui::Ui) {
        let refs = &self.routing_index.analog_routes;
        if refs.is_empty() {
            ui.label("No analog monitoring routes found.");
            return;
        }

        let max_input = refs.iter().map(|r| r.input).max().unwrap_or(0);
        let max_output = refs.iter().map(|r| r.output).max().unwrap_or(0);
        let mut by_pair: HashMap<(usize, usize), usize> = HashMap::new();
        for r in refs {
            by_pair.insert((r.input, r.output), r.control_index);
        }
        let ain_send_map = self.find_fx_send_map(false);

        let mut actions: Vec<(usize, Vec<String>)> = Vec::new();
        egui::Grid::new("monitoring_matrix_grid")
            .striped(true)
            .show(ui, |ui| {
                ui.label("Input \\ Output");
                for output in 0..=max_output {
                    ui.allocate_ui_with_layout(
                        vec2(Self::KNOB_CELL_W, 18.0),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            self.render_alias_label(ui, RenameTarget::Out(output), true, Self::KNOB_CELL_W);
                        },
                    );
                }
                ui.end_row();

                for input in 0..=max_input {
                    ui.allocate_ui_with_layout(
                        vec2(Self::ROW_LABEL_W, Self::KNOB_CELL_H),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            self.render_input_row_header(
                                ui,
                                RenameTarget::Ain(input),
                                ain_send_map.get(&input).copied(),
                                &mut actions,
                            );
                        },
                    );
                    for output in 0..=max_output {
                        if let Some(control_idx) = by_pair.get(&(input, output)).copied() {
                            if let Some(control) = self.controls.get(control_idx) {
                                if let Some(values) = Self::render_route_cell(ui, control) {
                                    actions.push((control_idx, values));
                                }
                            }
                        } else {
                            ui.label("-");
                        }
                    }
                    ui.end_row();
                }
            });

        for (idx, values) in actions {
            self.apply_values_to_control(idx, values);
        }
    }

    fn render_effects_section(&mut self, ui: &mut egui::Ui) {
        let fx_indices: Vec<usize> = self
            .controls
            .iter()
            .enumerate()
            .filter_map(|(idx, c)| {
                if self.is_fx_control(c) && !self.is_channel_fx_send(c) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        if fx_indices.is_empty() {
            ui.label(RichText::new("Effets (FX)").strong());
            ui.label("Contrôles FX dédiés de la Fast Track Ultra.");
            ui.label("Aucun contrôle FX détecté sur cette carte.");
            return;
        }

        let mut actions: Vec<(usize, Vec<String>)> = Vec::new();
        let mut used = HashSet::new();
        ui.columns(2, |cols| {
            egui::Frame::new()
                .fill(Color32::from_rgb(20, 24, 30))
                .stroke(Stroke::new(1.0, Color32::from_rgb(44, 52, 64)))
                .inner_margin(egui::Margin::symmetric(6, 6))
                .show(&mut cols[0], |ui| {
                    ui.label(RichText::new("Effets (FX)").strong());
                    ui.small("Contrôles FX dédiés de la Fast Track Ultra.");
                    if ui.button("Disable FX").clicked() {
                        self.disable_fx_controls();
                    }
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        if let Some(idx) = self.find_first_fx_with(&fx_indices, &used, |n| {
                            n.contains("effect program")
                        }) {
                            used.insert(idx);
                            if let Some(values) = self.render_effect_tile(ui, idx) {
                                actions.push((idx, values));
                            }
                        }
                        if let Some(idx) = self.find_first_fx_with(&fx_indices, &used, |n| {
                            n.contains("effect")
                                && !n.contains("program")
                                && !n.contains("duration")
                                && !n.contains("feedback")
                                && !n.contains("return")
                        }) {
                            used.insert(idx);
                            if let Some(values) = self.render_effect_tile(ui, idx) {
                                actions.push((idx, values));
                            }
                        }
                    });
                });

            egui::Frame::new()
                .fill(Color32::from_rgb(20, 24, 30))
                .stroke(Stroke::new(1.0, Color32::from_rgb(44, 52, 64)))
                .inner_margin(egui::Margin::symmetric(6, 6))
                .show(&mut cols[1], |ui| {
                    ui.label(RichText::new("Returns / Duration / Feedback").strong());
                    let return_indices: Vec<usize> = fx_indices
                        .iter()
                        .copied()
                        .filter(|idx| {
                            let name = self.controls[*idx].name.to_lowercase();
                            name.contains("return") && !used.contains(idx)
                        })
                        .collect();
                    let duration_idx =
                        self.find_first_fx_with(&fx_indices, &used, |n| n.contains("duration"));
                    let feedback_idx =
                        self.find_first_fx_with(&fx_indices, &used, |n| n.contains("feedback"));

                    egui::Grid::new("fx_returns_duration_feedback_grid")
                        .num_columns(3)
                        .spacing(vec2(4.0, 4.0))
                        .show(ui, |ui| {
                            let mut ret_iter = return_indices.iter().copied();
                            if let Some(idx) = ret_iter.next() {
                                used.insert(idx);
                                if let Some(values) = self.render_effect_tile(ui, idx) {
                                    actions.push((idx, values));
                                }
                            } else {
                                ui.label("");
                            }
                            if let Some(idx) = ret_iter.next() {
                                used.insert(idx);
                                if let Some(values) = self.render_effect_tile(ui, idx) {
                                    actions.push((idx, values));
                                }
                            } else {
                                ui.label("");
                            }
                            if let Some(idx) = duration_idx {
                                used.insert(idx);
                                if let Some(values) = self.render_effect_tile(ui, idx) {
                                    actions.push((idx, values));
                                }
                            } else {
                                ui.label("");
                            }
                            ui.end_row();

                            if let Some(idx) = ret_iter.next() {
                                used.insert(idx);
                                if let Some(values) = self.render_effect_tile(ui, idx) {
                                    actions.push((idx, values));
                                }
                            } else {
                                ui.label("");
                            }
                            if let Some(idx) = ret_iter.next() {
                                used.insert(idx);
                                if let Some(values) = self.render_effect_tile(ui, idx) {
                                    actions.push((idx, values));
                                }
                            } else {
                                ui.label("");
                            }
                            if let Some(idx) = feedback_idx {
                                used.insert(idx);
                                if let Some(values) = self.render_effect_tile(ui, idx) {
                                    actions.push((idx, values));
                                }
                            } else {
                                ui.label("");
                            }
                            ui.end_row();
                        });
                });
        });

        let remaining: Vec<usize> = fx_indices
            .iter()
            .copied()
            .filter(|idx| !used.contains(idx))
            .collect();
        if !remaining.is_empty() {
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                for idx in remaining {
                    if let Some(values) = self.render_effect_tile(ui, idx) {
                        actions.push((idx, values));
                    }
                }
            });
        }

        for (idx, values) in actions {
            self.apply_values_to_control(idx, values);
        }
    }

    fn render_effect_tile(&self, ui: &mut egui::Ui, idx: usize) -> Option<Vec<String>> {
        let control = self.controls.get(idx)?.clone();
        let mut out = None;
        ui.allocate_ui_with_layout(
            vec2(124.0, 92.0),
            egui::Layout::top_down(egui::Align::Center),
            |ui| {
                let display_name = Self::fx_display_name(&control.name);
                ui.add_sized(
                    vec2(118.0, 28.0),
                    egui::Label::new(RichText::new(display_name).strong())
                        .wrap()
                        .sense(egui::Sense::hover()),
                );
                out = Self::render_effect_control_inline(ui, &control);
            },
        );
        out
    }

    fn find_first_fx_with<F>(
        &self,
        fx_indices: &[usize],
        used: &HashSet<usize>,
        predicate: F,
    ) -> Option<usize>
    where
        F: Fn(&str) -> bool,
    {
        fx_indices.iter().copied().find(|idx| {
            if used.contains(idx) {
                return false;
            }
            let lower = self.controls[*idx].name.to_lowercase();
            predicate(&lower)
        })
    }

    fn render_effect_control_inline(
        ui: &mut egui::Ui,
        control: &ControlDescriptor,
    ) -> Option<Vec<String>> {
        match &control.kind {
            ControlKind::Integer {
                min,
                max,
                channels,
                db_range,
                ..
            } => {
                let mut new_values = control.values.clone();
                let mut changed = false;
                ui.horizontal_wrapped(|ui| {
                    for ch in 0..*channels {
                        let mut v = control
                            .values
                            .get(ch)
                            .and_then(|x| x.parse::<i64>().ok())
                            .unwrap_or(*min);
                        let ch_label = if *channels > 1 {
                            Some(format!("Ch{}", ch + 1))
                        } else {
                            None
                        };
                        changed |= Self::render_knob(
                            ui,
                            &mut v,
                            *min,
                            *max,
                            ch_label,
                            *db_range,
                        );
                        if ch < new_values.len() {
                            new_values[ch] = v.to_string();
                        } else {
                            new_values.push(v.to_string());
                        }
                    }
                });
                if changed {
                    return Some(new_values);
                }
            }
            ControlKind::Boolean { channels } => {
                let mut new_values = control.values.clone();
                let mut changed = false;
                ui.horizontal_wrapped(|ui| {
                    for ch in 0..*channels {
                        let mut on = control
                            .values
                            .get(ch)
                            .map(|v| v.eq_ignore_ascii_case("on") || v == "1")
                            .unwrap_or(false);
                        changed |= ui.checkbox(&mut on, format!("Ch{}", ch + 1)).changed();
                        if ch < new_values.len() {
                            new_values[ch] = if on { "on" } else { "off" }.to_string();
                        } else {
                            new_values.push(if on { "on" } else { "off" }.to_string());
                        }
                    }
                });
                if changed {
                    return Some(new_values);
                }
            }
            ControlKind::Enumerated { items, channels } => {
                let mut new_values = control.values.clone();
                let mut changed = false;
                ui.horizontal_wrapped(|ui| {
                    for ch in 0..*channels {
                        let mut current = control
                            .values
                            .get(ch)
                            .cloned()
                            .unwrap_or_else(|| items.first().cloned().unwrap_or_default());
                        egui::ComboBox::from_label(format!("Ch{}", ch + 1))
                            .selected_text(current.clone())
                            .show_ui(ui, |ui| {
                                for item in items {
                                    if ui.selectable_label(current == *item, item).clicked() {
                                        current = item.clone();
                                        changed = true;
                                    }
                                }
                            });
                        if ch < new_values.len() {
                            new_values[ch] = current;
                        } else {
                            new_values.push(current);
                        }
                    }
                });
                if changed {
                    return Some(new_values);
                }
            }
            ControlKind::Unknown { .. } => {
                return Self::render_control_editor(ui, control);
            }
        }
        None
    }

    fn fx_display_name(name: &str) -> String {
        name.replace(" Capture Volume", "")
            .replace(" Playback Volume", "")
            .replace(" Switch", "")
            .replace(" Volume", "")
    }

    fn render_route_matrix(&mut self, ui: &mut egui::Ui, analog: bool) {
        let refs = if analog {
            &self.routing_index.analog_routes
        } else {
            &self.routing_index.digital_routes
        };
        if refs.is_empty() {
            ui.label("No routes found for this group.");
            return;
        }

        let max_input = refs.iter().map(|r| r.input).max().unwrap_or(0);
        let max_output = refs.iter().map(|r| r.output).max().unwrap_or(0);
        let mut by_pair: HashMap<(usize, usize), usize> = HashMap::new();
        for r in refs {
            if analog {
                by_pair.insert((r.output, r.input), r.control_index);
            } else {
                by_pair.insert((r.input, r.output), r.control_index);
            }
        }

        let mut actions: Vec<(usize, Vec<String>)> = Vec::new();
        egui::Grid::new(if analog { "analog_grid" } else { "digital_grid" })
            .striped(true)
            .show(ui, |ui| {
                if analog {
                    ui.label("Out \\ AIn");
                    for input in 0..=max_input {
                        ui.allocate_ui_with_layout(
                            vec2(Self::KNOB_CELL_W, 18.0),
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                                self.render_alias_label(
                                    ui,
                                    RenameTarget::Ain(input),
                                    false,
                                    Self::KNOB_CELL_W,
                                );
                            },
                        );
                    }
                } else {
                    ui.label("DIn \\ Out");
                    for output in 0..=max_output {
                        ui.allocate_ui_with_layout(
                            vec2(Self::KNOB_CELL_W, 18.0),
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                                self.render_alias_label(
                                    ui,
                                    RenameTarget::Out(output),
                                    true,
                                    Self::KNOB_CELL_W,
                                );
                            },
                        );
                    }
                }
                ui.end_row();

                if analog {
                    for output in 0..=max_output {
                        ui.allocate_ui_with_layout(
                            vec2(Self::ROW_LABEL_W, 18.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                self.render_alias_label(ui, RenameTarget::Out(output), true, Self::ROW_LABEL_W);
                            },
                        );
                        for input in 0..=max_input {
                            if let Some(control_idx) = by_pair.get(&(output, input)).copied() {
                                if let Some(control) = self.controls.get(control_idx) {
                                    if let Some(values) = Self::render_route_cell(ui, control) {
                                        actions.push((control_idx, values));
                                    }
                                }
                            } else {
                                ui.label("-");
                            }
                        }
                        ui.end_row();
                    }
                } else {
                    let din_send_map = self.find_fx_send_map(true);
                    for input in 0..=max_input {
                        ui.allocate_ui_with_layout(
                            vec2(Self::ROW_LABEL_W, Self::KNOB_CELL_H),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                self.render_input_row_header(
                                    ui,
                                    RenameTarget::Din(input),
                                    din_send_map.get(&input).copied(),
                                    &mut actions,
                                );
                            },
                        );
                        for output in 0..=max_output {
                            if let Some(control_idx) = by_pair.get(&(input, output)).copied() {
                                if let Some(control) = self.controls.get(control_idx) {
                                    if let Some(values) = Self::render_route_cell(ui, control) {
                                        actions.push((control_idx, values));
                                    }
                                }
                            } else {
                                ui.label("-");
                            }
                        }
                        ui.end_row();
                    }
                }
            });

        for (idx, values) in actions {
            self.apply_values_to_control(idx, values);
        }
    }

    fn render_route_cell(ui: &mut egui::Ui, control: &ControlDescriptor) -> Option<Vec<String>> {
        let mut out: Option<Vec<String>> = None;
        ui.allocate_ui_with_layout(
            vec2(Self::KNOB_CELL_W, Self::KNOB_CELL_H),
            egui::Layout::top_down(egui::Align::Center),
            |ui| match &control.kind {
            ControlKind::Integer {
                min, max, db_range, ..
            } => {
                let mut v = control
                    .values
                    .first()
                    .and_then(|x| x.parse::<i64>().ok())
                    .unwrap_or(*min);
                let changed = Self::render_knob(ui, &mut v, *min, *max, None, *db_range);
                if changed {
                    out = Some(vec![v.to_string()]);
                }
            }
            ControlKind::Boolean { .. } => {
                let mut is_on = control
                    .values
                    .first()
                    .map(|v| v.eq_ignore_ascii_case("on") || v == "1")
                    .unwrap_or(false);
                if ui.checkbox(&mut is_on, "").changed() {
                    out = Some(vec![if is_on { "on" } else { "off" }.to_string()]);
                }
            }
            _ => {
                ui.label("...");
            }
        },
        );
        out
    }

    fn render_control_editor(ui: &mut egui::Ui, control: &ControlDescriptor) -> Option<Vec<String>> {
        match &control.kind {
            ControlKind::Integer {
                min,
                max,
                channels,
                db_range,
                ..
            } => {
                let mut new_values = control.values.clone();
                let mut changed = false;
                ui.horizontal_wrapped(|ui| {
                    for ch in 0..*channels {
                        let mut v = control
                            .values
                            .get(ch)
                            .and_then(|x| x.parse::<i64>().ok())
                            .unwrap_or(*min);
                        ui.vertical(|ui| {
                            changed |= Self::render_knob(
                                ui,
                                &mut v,
                                *min,
                                *max,
                                Some(format!("Ch{}", ch + 1)),
                                *db_range,
                            );
                        });
                        if ch < new_values.len() {
                            new_values[ch] = v.to_string();
                        } else {
                            new_values.push(v.to_string());
                        }
                    }
                });
                if changed {
                    return Some(new_values);
                }
            }
            ControlKind::Boolean { channels } => {
                let mut new_values = control.values.clone();
                let mut changed = false;
                for ch in 0..*channels {
                    let mut on = control
                        .values
                        .get(ch)
                        .map(|v| v.eq_ignore_ascii_case("on") || v == "1")
                        .unwrap_or(false);
                    changed |= ui.checkbox(&mut on, format!("Ch{}", ch + 1)).changed();
                    if ch < new_values.len() {
                        new_values[ch] = if on { "on" } else { "off" }.to_string();
                    } else {
                        new_values.push(if on { "on" } else { "off" }.to_string());
                    }
                }
                if changed {
                    return Some(new_values);
                }
            }
            ControlKind::Enumerated { items, channels } => {
                let mut new_values = control.values.clone();
                let mut changed = false;
                for ch in 0..*channels {
                    let mut current = control
                        .values
                        .get(ch)
                        .cloned()
                        .unwrap_or_else(|| items.first().cloned().unwrap_or_default());
                    egui::ComboBox::from_label(format!("Ch{}", ch + 1))
                        .selected_text(current.clone())
                        .show_ui(ui, |ui| {
                            for item in items {
                                if ui.selectable_label(current == *item, item).clicked() {
                                    current = item.clone();
                                    changed = true;
                                }
                            }
                        });
                    if ch < new_values.len() {
                        new_values[ch] = current;
                    } else {
                        new_values.push(current);
                    }
                }
                if changed {
                    return Some(new_values);
                }
            }
            ControlKind::Unknown { type_name, channels } => {
                ui.label(format!("Type non mappé: {type_name}"));
                let mut new_values = control.values.clone();
                let mut changed = false;
                for ch in 0..*channels {
                    let mut text = control.values.get(ch).cloned().unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.label(format!("Ch{}:", ch + 1));
                        changed |= ui.text_edit_singleline(&mut text).changed();
                    });
                    if ch < new_values.len() {
                        new_values[ch] = text;
                    } else {
                        new_values.push(text);
                    }
                }
                if changed {
                    return Some(new_values);
                }
            }
        }
        None
    }

    fn mute_hardware_routes(&mut self) {
        let routes: Vec<RouteRef> = self.routing_index.analog_routes.clone();
        for route in routes {
            self.apply_integer_route(route.control_index, 0);
        }
        self.status_line = "Mute analog monitoring applied".to_string();
    }

    fn pass_through_inputs(&mut self) {
        let routes: Vec<RouteRef> = self.routing_index.analog_routes.clone();
        for route in routes {
            if route.output > 1 {
                continue;
            }
            let target = match self.controls.get(route.control_index).map(|c| &c.kind) {
                Some(ControlKind::Integer { max, .. }) => *max,
                _ => 100,
            };
            self.apply_integer_route(route.control_index, target);
        }
        self.status_line = "Pass-through analog monitoring to channel 1/2 applied".to_string();
    }

    fn disable_fx_controls(&mut self) {
        let indexes: Vec<usize> = self
            .controls
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let n = c.name.to_lowercase();
                if n.contains("fx") || n.contains("effect") {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();

        for idx in indexes {
            let Some(ctrl) = self.controls.get(idx) else {
                continue;
            };
            let values = match &ctrl.kind {
                ControlKind::Integer { channels, .. } => vec!["0".to_string(); *channels],
                ControlKind::Boolean { channels } => vec!["off".to_string(); *channels],
                _ => continue,
            };
            self.apply_values_to_control(idx, values);
        }
        self.status_line = "FX controls disabled".to_string();
    }

    fn mute_most_digital_routes(&mut self) {
        let routes: Vec<RouteRef> = self.routing_index.digital_routes.clone();
        for route in routes {
            if route.input != route.output {
                self.apply_integer_route(route.control_index, 0);
            }
        }
        self.status_line = "Most digital routes muted".to_string();
    }

    fn panic_mute(&mut self) {
        let mut indexes: Vec<usize> = self.routing_index.analog_routes.iter().map(|r| r.control_index).collect();
        indexes.extend(self.routing_index.digital_routes.iter().map(|r| r.control_index));
        indexes.sort_unstable();
        indexes.dedup();
        for idx in indexes {
            self.apply_integer_route(idx, 0);
        }
        self.status_line = "Mute all monitoring applied".to_string();
    }

    fn apply_integer_route(&mut self, idx: usize, target: i64) {
        let Some(ctrl) = self.controls.get(idx).cloned() else {
            return;
        };
        if let ControlKind::Integer { channels, min, max, .. } = ctrl.kind {
            let v = target.clamp(min, max).to_string();
            self.apply_values_to_control(idx, vec![v; channels]);
        }
    }

    fn save_user_config(&mut self) {
        match self.user_config.save() {
            Ok(()) => {
                self.status_line = "Configuration saved to ~/.ftu-mixer/config.json".to_string();
            }
            Err(err) => {
                self.status_line = format!("Config save failed: {err}");
            }
        }
    }

    fn render_input_row_header(
        &mut self,
        ui: &mut egui::Ui,
        target: RenameTarget,
        send_control_index: Option<usize>,
        actions: &mut Vec<(usize, Vec<String>)>,
    ) {
        ui.horizontal(|ui| {
            if let Some(send_idx) = send_control_index {
                if let Some(control) = self.controls.get(send_idx).cloned() {
                    if let ControlKind::Integer {
                        min, max, db_range, ..
                    } = control.kind
                    {
                        let mut v = control
                            .values
                            .first()
                            .and_then(|x| x.parse::<i64>().ok())
                            .unwrap_or(min);
                        ui.vertical(|ui| {
                            ui.label("FX");
                            let changed = Self::render_knob(ui, &mut v, min, max, None, db_range);
                            if changed {
                                actions.push((send_idx, vec![v.to_string()]));
                            }
                        });
                    } else {
                        ui.label("FX");
                    }
                }
            } else {
                ui.label(" ");
            }
            self.render_alias_label(ui, target, true, Self::ROW_LABEL_W - 64.0);
        });
    }

    fn is_fx_control(&self, control: &ControlDescriptor) -> bool {
        let lower = control.name.to_lowercase();
        lower.contains("fx")
            || lower.contains("effect")
            || lower.contains("reverb")
            || lower.contains("delay")
            || lower.contains("chorus")
    }

    fn is_channel_fx_send(&self, control: &ControlDescriptor) -> bool {
        let lower = control.name.to_lowercase();
        let has_channel = lower.contains("ain") || lower.contains("din");
        let send_like =
            lower.contains("send") || lower.contains("aux") || lower.contains("to fx");
        self.is_fx_control(control) && has_channel && send_like
    }

    fn find_fx_send_map(&self, digital: bool) -> HashMap<usize, usize> {
        let mut map = HashMap::new();
        let max_idx = if digital {
            self.routing_index
                .digital_routes
                .iter()
                .map(|r| r.input)
                .max()
                .unwrap_or(0)
        } else {
            self.routing_index
                .analog_routes
                .iter()
                .map(|r| r.input)
                .max()
                .unwrap_or(0)
        };

        for input in 0..=max_idx {
            let token = if digital {
                format!("din{}", input + 1)
            } else {
                format!("ain{}", input + 1)
            };
            let mut best: Option<(i32, usize)> = None;
            for (idx, c) in self.controls.iter().enumerate() {
                if !matches!(c.kind, ControlKind::Integer { .. }) {
                    continue;
                }
                let lower = c.name.to_lowercase();
                if !lower.contains(&token) || !self.is_fx_control(c) {
                    continue;
                }
                let mut score = 0;
                if lower.contains("send") {
                    score += 5;
                }
                if lower.contains("aux") {
                    score += 3;
                }
                if lower.contains("to fx") {
                    score += 2;
                }
                if lower.contains("out") {
                    score -= 1;
                }
                if best.map(|(s, _)| score > s).unwrap_or(true) {
                    best = Some((score, idx));
                }
            }
            if let Some((_, idx)) = best {
                map.insert(input, idx);
            }
        }
        map
    }

    fn render_alias_label(
        &mut self,
        ui: &mut egui::Ui,
        target: RenameTarget,
        strong: bool,
        width: f32,
    ) {
        let default_name = match target {
            RenameTarget::Ain(i) => format!("AIn{}", i + 1),
            RenameTarget::Din(i) => format!("DIn{}", i + 1),
            RenameTarget::Out(i) => format!("Out{}", i + 1),
        };
        let current_alias = match target {
            RenameTarget::Ain(i) => self.user_config.ain_aliases.get(&i).cloned(),
            RenameTarget::Din(i) => self.user_config.din_aliases.get(&i).cloned(),
            RenameTarget::Out(i) => self.user_config.out_aliases.get(&i).cloned(),
        };
        let displayed = current_alias.unwrap_or(default_name);

        if self.rename_target == Some(target) {
            let mut commit = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                let button_w = 22.0;
                let spacing = ui.spacing().item_spacing.x;
                let available = ui.available_width();
                let edit_w = (available - (button_w * 2.0) - (spacing * 2.0)).max(26.0);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.rename_buffer).desired_width(edit_w),
                );
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit = true;
                }
                if ui
                    .add_sized(
                        vec2(button_w, 20.0),
                        egui::Button::new(RichText::new("✓").size(15.0)),
                    )
                    .on_hover_text("Valider")
                    .clicked()
                {
                    commit = true;
                }
                if ui
                    .add_sized(
                        vec2(button_w, 20.0),
                        egui::Button::new(RichText::new("✕").size(15.0)),
                    )
                    .on_hover_text("Annuler")
                    .clicked()
                {
                    cancel = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
            });
            if commit {
                self.commit_alias_rename(target);
            }
            if cancel {
                self.rename_target = None;
                self.rename_buffer.clear();
            }
            return;
        }

        let char_count = displayed.chars().count();
        let font_size = if char_count > 28 {
            9.0
        } else if char_count > 20 {
            10.0
        } else if char_count > 14 {
            11.0
        } else {
            13.0
        };
        let shown_text = displayed.clone();
        let text = if strong {
            RichText::new(shown_text).strong().size(font_size)
        } else {
            RichText::new(shown_text).size(font_size)
        };
        let resp = ui.add_sized(
            vec2(width, 18.0),
            egui::Label::new(text)
                .truncate()
                .sense(egui::Sense::click()),
        );
        let resp = resp.on_hover_text(displayed);
        if resp.double_clicked() {
            self.rename_target = Some(target);
            self.rename_buffer = match target {
                RenameTarget::Ain(i) => self.user_config.ain_aliases.get(&i).cloned().unwrap_or_default(),
                RenameTarget::Din(i) => self.user_config.din_aliases.get(&i).cloned().unwrap_or_default(),
                RenameTarget::Out(i) => self.user_config.out_aliases.get(&i).cloned().unwrap_or_default(),
            };
        }
    }

    fn commit_alias_rename(&mut self, target: RenameTarget) {
        let value = self.rename_buffer.trim().to_string();
        match target {
            RenameTarget::Ain(i) => {
                if value.is_empty() {
                    self.user_config.ain_aliases.remove(&i);
                } else {
                    self.user_config.ain_aliases.insert(i, value);
                }
            }
            RenameTarget::Din(i) => {
                if value.is_empty() {
                    self.user_config.din_aliases.remove(&i);
                } else {
                    self.user_config.din_aliases.insert(i, value);
                }
            }
            RenameTarget::Out(i) => {
                if value.is_empty() {
                    self.user_config.out_aliases.remove(&i);
                } else {
                    self.user_config.out_aliases.insert(i, value);
                }
            }
        }
        self.rename_target = None;
        self.rename_buffer.clear();
        self.save_user_config();
    }

    fn render_knob(
        ui: &mut egui::Ui,
        value: &mut i64,
        min: i64,
        max: i64,
        label: Option<String>,
        db_range: Option<(i64, i64)>,
    ) -> bool {
        *value = (*value).clamp(min, max);
        let desired_size = vec2(34.0, 34.0);
        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click_and_drag());

        let old = *value;
        if response.dragged() {
            let dy = ui.input(|i| i.pointer.delta().y);
            let current = Self::knob_progress_from_value(*value, min, max, db_range);
            let next = (current - (dy / 180.0)).clamp(0.0, 1.0);
            *value = Self::value_from_knob_progress(next, min, max, db_range);
        }

        let t = Self::knob_progress_from_value(*value, min, max, db_range);
        let start_angle = -2.35_f32;
        let end_angle = 2.35_f32;
        let angle = egui::remap(t, 0.0..=1.0, start_angle..=end_angle);
        let center = rect.center();
        let radius = rect.width().min(rect.height()) * 0.44;

        let base_color = if response.hovered() {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            ui.visuals().widgets.inactive.bg_fill
        };
        let stroke = Stroke::new(1.2, ui.visuals().widgets.inactive.fg_stroke.color);
        ui.painter().circle_filled(center, radius, base_color);
        ui.painter().circle_stroke(center, radius, stroke);

        ui.painter().circle_stroke(
            center,
            radius * 0.86,
            Stroke::new(1.5, ui.visuals().widgets.noninteractive.bg_stroke.color),
        );

        let marker = center + vec2(angle.cos() * radius * 0.86, angle.sin() * radius * 0.86);
        ui.painter().circle_filled(marker, 2.4, Color32::from_rgb(90, 220, 220));

        let tick_in = radius * 0.95;
        let tick_out = radius * 1.18;
        let min_in = center + vec2(start_angle.cos() * tick_in, start_angle.sin() * tick_in);
        let min_out = center + vec2(start_angle.cos() * tick_out, start_angle.sin() * tick_out);
        let max_in = center + vec2(end_angle.cos() * tick_in, end_angle.sin() * tick_in);
        let max_out = center + vec2(end_angle.cos() * tick_out, end_angle.sin() * tick_out);
        let tick_stroke = Stroke::new(2.0, ui.visuals().widgets.inactive.fg_stroke.color);
        ui.painter().line_segment([min_in, min_out], tick_stroke);
        ui.painter().line_segment([max_in, max_out], tick_stroke);

        let tip_len = radius * 0.72;
        let tip = center + vec2(angle.cos() * tip_len, angle.sin() * tip_len);
        ui.painter()
            .line_segment([center, tip], Stroke::new(2.2, Color32::from_rgb(90, 220, 220)));

        if let Some(text) = label {
            ui.label(text);
        }

        let percent = Self::control_percent(*value, min, max, db_range);
        ui.label(format!("{percent}%"));
        old != *value
    }

    fn knob_progress_from_value(value: i64, min: i64, max: i64, db_range: Option<(i64, i64)>) -> f32 {
        if max <= min {
            return 0.0;
        }
        if let Some((db_min, db_max)) = db_range {
            if db_max > db_min {
                let raw_pos = (value - min).clamp(0, max - min) as f64 / (max - min) as f64;
                let db = db_min as f64 + raw_pos * (db_max - db_min) as f64;
                let amp_min = 10f64.powf(db_min as f64 / 6000.0);
                let amp_max = 10f64.powf(db_max as f64 / 6000.0);
                let amp = 10f64.powf(db / 6000.0);
                let denom = amp_max - amp_min;
                if denom > f64::EPSILON {
                    return ((amp - amp_min) / denom).clamp(0.0, 1.0) as f32;
                }
            }
        }
        ((value - min) as f64 / (max - min) as f64).clamp(0.0, 1.0) as f32
    }

    fn value_from_knob_progress(norm: f32, min: i64, max: i64, db_range: Option<(i64, i64)>) -> i64 {
        if max <= min {
            return min;
        }
        let n = norm.clamp(0.0, 1.0) as f64;
        if let Some((db_min, db_max)) = db_range {
            if db_max > db_min {
                let amp_min = 10f64.powf(db_min as f64 / 6000.0);
                let amp_max = 10f64.powf(db_max as f64 / 6000.0);
                let amp = amp_min + n * (amp_max - amp_min);
                if amp.is_finite() && amp > 0.0 {
                    let db = 6000.0 * amp.log10();
                    let raw_pos = ((db - db_min as f64) / (db_max - db_min) as f64).clamp(0.0, 1.0);
                    let raw = min as f64 + raw_pos * (max - min) as f64;
                    return raw.round().clamp(min as f64, max as f64) as i64;
                }
            }
        }
        let raw = min as f64 + n * (max - min) as f64;
        raw.round().clamp(min as f64, max as f64) as i64
    }

    fn control_percent(value: i64, min: i64, max: i64, db_range: Option<(i64, i64)>) -> i64 {
        if max <= min {
            return 0;
        }
        if let Some((db_min, db_max)) = db_range {
            if db_max > db_min {
                let pos = (value - min).clamp(0, max - min) as f64 / (max - min) as f64;
                let db = db_min as f64 + pos * (db_max - db_min) as f64;
                let amp_min = 10f64.powf(db_min as f64 / 6000.0);
                let amp_max = 10f64.powf(db_max as f64 / 6000.0);
                let amp = 10f64.powf(db / 6000.0);
                let denom = amp_max - amp_min;
                if denom > f64::EPSILON {
                    return (((amp - amp_min) / denom) * 100.0).round().clamp(0.0, 100.0) as i64;
                }
            }
        }
        let span = (max - min) as i128;
        let pos = (value - min).clamp(0, max - min) as i128;
        ((pos * 100) / span).clamp(0, 100) as i64
    }

    fn apply_studio_theme(&self, ctx: &egui::Context) {
        self.apply_font_fallbacks(ctx);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = vec2(5.0, 4.0);
        style.spacing.button_padding = vec2(8.0, 3.0);
        style.spacing.interact_size = vec2(20.0, 18.0);
        style.spacing.window_margin = egui::Margin::same(6);
        ctx.set_style(style);

        let mut visuals = egui::Visuals::dark();
        visuals.override_text_color = Some(Color32::from_rgb(232, 236, 240));
        visuals.panel_fill = Color32::from_rgb(14, 16, 20);
        visuals.window_fill = Color32::from_rgb(14, 16, 20);
        visuals.extreme_bg_color = Color32::from_rgb(20, 23, 28);
        visuals.faint_bg_color = Color32::from_rgb(30, 33, 40);
        visuals.selection.bg_fill = Color32::from_rgb(54, 168, 178);
        visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(180, 245, 250));
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(28, 32, 38);
        visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(24, 27, 33);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(44, 50, 58);
        visuals.widgets.active.bg_fill = Color32::from_rgb(57, 66, 76);
        visuals.widgets.open.bg_fill = Color32::from_rgb(40, 46, 54);
        visuals.widgets.noninteractive.bg_stroke =
            Stroke::new(1.0, Color32::from_rgb(52, 57, 66));
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(210, 214, 220));
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::from_rgb(235, 240, 244));
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::from_rgb(245, 250, 252));
        ctx.set_visuals(visuals);
    }

    fn apply_font_fallbacks(&self, ctx: &egui::Context) {
        let candidates = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/opentype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf",
        ];

        let mut defs = egui::FontDefinitions::default();
        for path in candidates {
            if let Ok(bytes) = fs::read(path) {
                defs.font_data
                    .insert("system_ui_fallback".to_string(), egui::FontData::from_owned(bytes).into());
                defs.families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "system_ui_fallback".to_string());
                defs.families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .push("system_ui_fallback".to_string());
                ctx.set_fonts(defs);
                break;
            }
        }
    }
}

impl eframe::App for MixerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.theme_initialized {
            self.apply_studio_theme(ctx);
            self.theme_initialized = true;
        }
        if !self.event_listener_initialized {
            self.event_listener_initialized = true;
            let egui_ctx = ctx.clone();
            self.alsa_event_rx = self
                .backend
                .start_event_listener(move || egui_ctx.request_repaint());
        }

        const AUTO_REFRESH_INTERVAL: Duration = Duration::from_millis(220);
        const EVENT_FALLBACK_INTERVAL: Duration = Duration::from_millis(500);
        const FULL_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
        let is_interacting = ctx.input(|i| i.pointer.any_down());
        let mut should_repaint = is_interacting;
        let has_event_listener = self.alsa_event_rx.is_some();
        let mut got_alsa_event = false;
        if let Some(rx) = &self.alsa_event_rx {
            while rx.try_recv().is_ok() {
                got_alsa_event = true;
            }
        }

        if !is_interacting && got_alsa_event {
            should_repaint |= self.refresh_live_values_only();
            self.last_auto_refresh = Instant::now();
        } else if !is_interacting && !has_event_listener && self.last_auto_refresh.elapsed() >= AUTO_REFRESH_INTERVAL {
            should_repaint |= self.refresh_live_values_only();
            self.last_auto_refresh = Instant::now();
        } else if !is_interacting
            && has_event_listener
            && self.last_auto_refresh.elapsed() >= EVENT_FALLBACK_INTERVAL
        {
            should_repaint |= self.refresh_live_values_only();
            self.last_auto_refresh = Instant::now();
        }
        if !is_interacting && self.last_full_refresh.elapsed() >= FULL_REFRESH_INTERVAL {
            should_repaint |= self.refresh_controls_with_status(false);
        }
        if should_repaint {
            ctx.request_repaint();
        } else {
            let wake_after = if has_event_listener {
                EVENT_FALLBACK_INTERVAL
            } else {
                AUTO_REFRESH_INTERVAL
            };
            ctx.request_repaint_after(wake_after);
        }

        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(20, 23, 29))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(44, 50, 60)))
                    .inner_margin(egui::Margin::symmetric(8, 6)),
            )
            .show(ctx, |ui| {
                self.render_toolbar(ui);
            });

        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(18, 21, 26))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(44, 50, 60)))
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| {
                ui.label(RichText::new(&self.status_line).size(12.0));
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(12, 14, 18))
                    .inner_margin(egui::Margin::symmetric(8, 6)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.selected_tab {
                        Tab::MixRouting => self.render_mix_routing_tab(ui),
                    });
                });
    }
}

//! egui views over the [`Hub`] model. Pure egui: no windowing, no renderer.
//!
//! The hub fills the screen: characters on the left, the runtime's live character preview
//! in the middle (the centre is left undrawn so the 3D view shows through), worlds — or the
//! character editor while one is open — on the right, and the enter bar at the bottom.

use egui::{Align2, Color32, Context, RichText, Ui};
use ffl_core::{ActionCategory, FieldKind, Library, PresetField};

use crate::hub::{ActionEvent, ActionsView, Hub, HubEvent, SoundMonitor, SoundSource};
use crate::settings::Settings;

/// What an editor field interaction asks for (resolved by `Hub` after the frame's UI).
enum EditorAction {
    Import(String),
    OpenLookup { key: String, catalog: String, none_label: String },
    Refresh,
    /// Build the character's modded copy now.
    Archive,
}

const LEFT_WIDTH: f32 = 330.0;
const RIGHT_WIDTH: f32 = 470.0;

/// A full-width, left-aligned selectable list row with a title and an optional subtitle.
fn list_row(ui: &mut Ui, selected: bool, title: &str, subtitle: &str) -> egui::Response {
    let height = if subtitle.is_empty() { 24.0 } else { 40.0 };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        if selected || response.hovered() {
            ui.painter().rect(rect, 4.0, visuals.bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
        }
        let x = rect.left() + 8.0;
        let title_color = if selected { visuals.text_color() } else { ui.visuals().strong_text_color() };
        ui.painter().text(egui::pos2(x, rect.top() + 4.0), Align2::LEFT_TOP, title, egui::FontId::proportional(14.0), title_color);
        if !subtitle.is_empty() {
            ui.painter().text(egui::pos2(x, rect.top() + 23.0), Align2::LEFT_TOP, subtitle, egui::FontId::proportional(11.0), ui.visuals().weak_text_color());
        }
    }
    response
}

/// The hub screen. Returns the events the host must act on.
pub fn hub_ui(ctx: &Context, hub: &mut Hub) -> Vec<HubEvent> {
    let mut events = Vec::new();
    let mut enter = false;
    // Root panels hang off a viewport-sized background Ui (egui 0.36 panels take a `Ui`).
    let mut root = Ui::new(
        ctx.clone(),
        egui::Id::new("ffl_viewport"),
        egui::UiBuilder::new().layer_id(egui::LayerId::background()).max_rect(ctx.viewport_rect()),
    );

    hub.sync_log();
    // Panels share the window: on a small window the side panels shrink so the editor
    // and the lists never run past the edge (the preview in the middle keeps at least a
    // third).
    let screen_w = ctx.viewport_rect().width();
    let left_w = LEFT_WIDTH.min(screen_w * 0.28);
    let right_w = RIGHT_WIDTH.min(screen_w * 0.38);
    egui::Panel::top("ffl_top").show(&mut root, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.heading("FFLocal");
            ui.label(RichText::new("worlds and characters from any engine").weak());
            ui.separator();
            for e in &hub.engines {
                let color = if e.available { Color32::LIGHT_GREEN } else { Color32::GRAY };
                ui.colored_label(color, format!("● {}", e.name)).on_hover_text(if e.detail.is_empty() { e.name.clone() } else { e.detail.clone() });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Quit").on_hover_text("close FFLocal").clicked() {
                    events.push(HubEvent::Quit);
                }
                if ui.selectable_label(hub.show_settings, "Settings").clicked() {
                    hub.show_settings = !hub.show_settings;
                }
                if ui.selectable_label(hub.show_games, "Games").on_hover_text("where each game is installed").clicked() {
                    hub.show_games = !hub.show_games;
                }
                if ui.selectable_label(hub.show_log, "Log").on_hover_text("everything the program reported this session").clicked() {
                    hub.show_log = !hub.show_log;
                }
                events.extend(ui_menu(ui, hub, false));
            });
        });
    });
    log_window(ctx, hub);

    egui::Panel::bottom("ffl_enter").show(&mut root, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let preset_name = hub
                .selected_preset()
                .map(|p| format!("{} ({})", p.name, hub.engine_name(&p.engine)))
                .unwrap_or_else(|| "no character (capsule)".into());
            let map_name = hub
                .selected_map()
                .map(|m| format!("{} ({})", m.name, hub.engine_name(&m.engine)))
                .unwrap_or_else(|| "no world selected".into());
            ui.label(RichText::new("Character").weak());
            ui.strong(preset_name);
            ui.add_space(12.0);
            ui.label(RichText::new("World").weak());
            ui.strong(map_name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_enter = hub.selected_map.is_some();
                let button = egui::Button::new(RichText::new("Enter world").size(18.0)).min_size(egui::vec2(160.0, 34.0));
                if ui.add_enabled(can_enter, button).clicked() {
                    enter = true;
                }
                if !hub.preview_status.is_empty() {
                    ui.colored_label(Color32::LIGHT_GRAY, &hub.preview_status);
                }
            });
        });
        if !hub.status.is_empty() {
            // The newest message on its own line: grey for notes, red for failures, cut to
            // the width (never over anything else); the whole text is in the Log window.
            let color = if Hub::status_is_error(&hub.status) { Color32::LIGHT_RED } else { Color32::LIGHT_GRAY };
            ui.horizontal(|ui| {
                if ui.small_button("×").on_hover_text("dismiss").clicked() {
                    hub.status.clear();
                }
                if ui.small_button("Log").clicked() {
                    hub.show_log = true;
                }
                ui.add(egui::Label::new(RichText::new(&hub.status).color(color)).truncate());
            });
        }
        ui.add_space(4.0);
        ui.small(format!("presets: {}   settings: {}", Library::path().display(), Settings::path().display()));
    });

    if hub.settings.ui.hub_characters {
        egui::Panel::left("ffl_characters").resizable(false).exact_size(left_w).show(&mut root, |ui| characters_panel(ui, hub));
    }

    if hub.settings.ui.hub_worlds || hub.editor.is_some() {
        egui::Panel::right("ffl_right").resizable(false).exact_size(right_w).show(&mut root, |ui| {
            if hub.editor.is_some() {
                editor_panel(ui, hub);
            } else if worlds_panel(ui, hub) {
                enter = true;
            }
        });
    }
    let free = root.available_rect_before_wrap();
    hub.free_rect = Some([free.min.x, free.min.y, free.max.x, free.max.y]);

    // Preview hint over the (transparent) centre.
    if hub.settings.ui.hub_hint {
        egui::Area::new(egui::Id::new("ffl_preview_hint")).anchor(Align2::CENTER_BOTTOM, [0.0, -70.0]).show(ctx, |ui| {
            let text = match hub.preview_preset() {
                Some(p) => format!("{}  ·  drag to turn, scroll to zoom", p.name),
                None => "no character selected".to_string(),
            };
            ui.label(RichText::new(text).color(Color32::from_white_alpha(140)));
        });
    }

    events.extend(settings_window(ctx, hub));
    events.extend(games_window(ctx, hub));

    if enter && let Some(request) = hub.enter() {
        events.push(HubEvent::EnterWorld(request));
    }
    events
}

/// The Games window: one row per engine with the install in use, a path to type or browse
/// to, and the installs a scan of the usual places found. Applying a path reopens the engine.
pub fn games_window(ctx: &Context, hub: &mut Hub) -> Vec<HubEvent> {
    let mut events = Vec::new();
    if !hub.show_games {
        return events;
    }
    let mut open = true;
    let engines = hub.engines.clone();
    egui::Window::new("Games")
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(640.0)
        .collapsible(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.small("Each engine reads one installed game in place. Pick the install folder (the one holding game/ for FFXIV, VTABLE.DAT for FFXI) or leave the path empty to search the usual places: launcher configs, Steam libraries, default install folders. A game switched off is closed: its characters, worlds and sounds are gone until it is switched on again.");
            for e in &engines {
                ui.separator();
                let enabled = hub.settings.game_enabled(&e.id);
                ui.horizontal(|ui| {
                    let mut on = enabled;
                    if ui.checkbox(&mut on, "").on_hover_text(if enabled { "switch the game off (closes its engine)" } else { "switch the game on (opens its engine)" }).changed() {
                        events.push(HubEvent::SetGameEnabled { engine: e.id.clone(), enabled: on });
                    }
                    let (color, dot) = match (enabled, e.available) {
                        (false, _) => (Color32::GRAY, "○"),
                        (true, true) => (Color32::LIGHT_GREEN, "●"),
                        (true, false) => (Color32::LIGHT_RED, "○"),
                    };
                    ui.colored_label(color, dot);
                    ui.strong(&e.name);
                    if !enabled {
                        ui.weak("off");
                    } else if !e.version.is_empty() {
                        ui.weak(&e.version);
                    }
                });
                if !enabled {
                    ui.label(RichText::new("Switched off: nothing of this game is loaded.").weak().size(11.0));
                    continue;
                }
                ui.label(RichText::new(&e.detail).weak().size(11.0));
                let edit = hub.game_edit.entry(e.id.clone()).or_default();
                ui.horizontal(|ui| {
                    ui.label("Install");
                    let r = ui.add(egui::TextEdit::singleline(edit).desired_width(360.0).hint_text("empty = auto-detect"));
                    let apply = ui.button("Use").on_hover_text("reopen the engine from this folder (saved)").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if apply {
                        events.push(HubEvent::SetGamePath { engine: e.id.clone(), path: edit.trim().to_string() });
                    }
                    if ui.button("Browse…").clicked() {
                        events.push(HubEvent::BrowseGame(e.id.clone()));
                    }
                    if ui.button("Scan").on_hover_text("look in the usual places").clicked() {
                        events.push(HubEvent::ScanGame(e.id.clone()));
                    }
                    if ui.button("Auto").on_hover_text("forget the path and search").clicked() {
                        edit.clear();
                        events.push(HubEvent::SetGamePath { engine: e.id.clone(), path: String::new() });
                    }
                });
                if let Some(found) = hub.game_candidates.get(&e.id) {
                    if found.is_empty() {
                        ui.weak("scan: nothing found in the usual places");
                    }
                    for c in found.clone() {
                        let path = c.path.display().to_string();
                        if ui.selectable_label(false, format!("{path}   ({})", c.source)).on_hover_text("use this install").clicked() {
                            hub.game_edit.insert(e.id.clone(), path.clone());
                            events.push(HubEvent::SetGamePath { engine: e.id.clone(), path });
                        }
                    }
                }
                if !e.plugins.is_empty() {
                    ui.label(RichText::new(&e.plugins).weak().size(11.0));
                    let edit = hub.plugins_edit.entry(e.id.clone()).or_default();
                    ui.horizontal(|ui| {
                        ui.label("Plugins");
                        let r = ui.add(egui::TextEdit::singleline(edit).desired_width(360.0).hint_text("empty = auto-detect (XIVLauncher's pluginConfigs)"));
                        let apply = ui.button("Use").on_hover_text("read Penumbra and Glamourer data from this folder (saved)").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                        if apply {
                            events.push(HubEvent::SetPluginsPath { engine: e.id.clone(), path: edit.trim().to_string() });
                        }
                        if ui.button("Auto").on_hover_text("forget the path and search").clicked() {
                            edit.clear();
                            events.push(HubEvent::SetPluginsPath { engine: e.id.clone(), path: String::new() });
                        }
                    });
                }
            }
            ui.separator();
            ui.small(format!("saved in {}; the FFLocal launcher sets the same before the program starts", Settings::path().display()));
        });
    if !open {
        hub.show_games = false;
    }
    events
}

/// The "UI ▾" menu: which panels are shown, here (the hub) or in a world. Saved at once.
pub fn ui_menu(ui: &mut Ui, hub: &mut Hub, in_world: bool) -> Vec<HubEvent> {
    let mut events = Vec::new();
    ui.menu_button("UI", |ui| {
        let mut changed = false;
        let mut audio_changed = false;
        let u = &mut hub.settings.ui;
        if in_world {
            ui.label(RichText::new("In world").weak());
            changed |= ui.checkbox(&mut u.diagnostics, "Diagnostics").changed();
            changed |= ui.checkbox(&mut u.actions, "Actions").changed();
            audio_changed |= ui.checkbox(&mut hub.settings.audio.show_monitor, "Sounds").changed();
            changed |= ui.checkbox(&mut hub.settings.ui.key_hints, "Key hints").changed();
        } else {
            ui.label(RichText::new("Hub").weak());
            changed |= ui.checkbox(&mut u.hub_characters, "Characters panel").changed();
            changed |= ui.checkbox(&mut u.hub_worlds, "Worlds panel").changed();
            changed |= ui.checkbox(&mut u.hub_hint, "Preview hint").changed();
        }
        ui.separator();
        let g = &mut hub.settings.graphics;
        let scale = ui.add(egui::Slider::new(&mut g.ui_scale, 0.5..=2.0).text("UI scale"));
        if scale.drag_stopped() {
            events.push(hub.commit_settings());
        }
        if changed {
            hub.commit_ui();
        }
        if audio_changed {
            events.push(hub.commit_audio());
        }
    });
    events
}

/// The strip at the top of a world: menu, settings, the UI menu.
pub fn world_bar(ctx: &Context, hub: &mut Hub, engine: &str, map: &str) -> (Vec<HubEvent>, bool) {
    let mut events = Vec::new();
    let mut menu = false;
    egui::Area::new(egui::Id::new("ffl_world_bar")).anchor(Align2::CENTER_TOP, [0.0, 6.0]).show(ctx, |ui| {
        egui::Frame::window(&ctx.global_style()).fill(Color32::from_black_alpha(170)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{engine} / {map}")).weak());
                if ui.button("Menu (Esc)").clicked() {
                    menu = true;
                }
                if ui.selectable_label(hub.show_settings, "Settings").clicked() {
                    hub.show_settings = !hub.show_settings;
                }
                events.extend(ui_menu(ui, hub, true));
                if ui.button("Quit").on_hover_text("close FFLocal").clicked() {
                    events.push(HubEvent::Quit);
                }
            });
        });
    });
    (events, menu)
}

fn characters_panel(ui: &mut Ui, hub: &mut Hub) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.heading("Characters");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_button("New…", |ui| {
                for e in hub.engines.clone().iter().filter(|e| e.available) {
                    if ui.button(format!("{} character", e.name)).clicked() {
                        hub.new_character(&e.id);
                        ui.close();
                    }
                }
            });
        });
    });
    ui.small("Presets are FFLocal configurations: import from the game or build one, then edit looks and gear.");
    ui.separator();
    let engines = hub.engines.clone();
    egui::ScrollArea::vertical().id_salt("presets").auto_shrink([false, false]).max_height(ui.available_height() - 120.0).show(ui, |ui| {
        for e in &engines {
            let presets: Vec<ffl_core::CharacterPreset> = hub.presets_of(&e.id).into_iter().cloned().collect();
            // A switched-off game keeps its presets on disk but shows nothing of them.
            if (presets.is_empty() && !e.available) || !hub.settings.game_enabled(&e.id) {
                continue;
            }
            ui.add_space(4.0);
            ui.label(RichText::new(&e.name).strong().color(Color32::LIGHT_GRAY));
            if presets.is_empty() {
                ui.weak("no characters yet");
            }
            for p in presets {
                let selected = hub.selected_preset.as_deref() == Some(p.id.as_str());
                let r = list_row(ui, selected, &p.name, &preset_summary(&p));
                if r.clicked() {
                    hub.selected_preset = Some(p.id.clone());
                    hub.confirm_delete = None;
                }
                if r.double_clicked() {
                    hub.selected_preset = Some(p.id.clone());
                    hub.edit_selected(None);
                }
            }
        }
        ui.add_space(4.0);
        if ui.selectable_label(hub.selected_preset.is_none(), "(no character)").clicked() {
            hub.selected_preset = None;
        }
    });
    ui.separator();
    let selected = hub.selected_preset().cloned();
    match selected {
        Some(p) => {
            ui.label(RichText::new(&p.name).strong());
            ui.horizontal_wrapped(|ui| {
                if ui.button("Edit").clicked() {
                    hub.edit_selected(None);
                }
                if ui.button("Gear").on_hover_text("open the editor on the gear slots").clicked() {
                    hub.edit_selected(Some("Gear"));
                }
                if ui.button("Duplicate").clicked() {
                    hub.duplicate_preset(&p.id);
                }
                let designs = hub.design_sources(&p.engine);
                if !designs.is_empty() {
                    let mut chosen: Option<String> = None;
                    egui::ComboBox::from_id_salt("design-selected").selected_text("Design…").show_ui(ui, |ui| {
                        for d in &designs {
                            let label = d.label.trim_start_matches("Glamourer design: ");
                            if ui.selectable_label(false, label).on_hover_text(&d.detail).clicked() {
                                chosen = Some(d.id.clone());
                            }
                        }
                    });
                    if let Some(id) = chosen {
                        hub.apply_design_to_selected(&id);
                    }
                }
                let confirming = hub.confirm_delete.as_deref() == Some(p.id.as_str());
                let label = if confirming { "Really delete?" } else { "Delete" };
                if ui.button(label).clicked() {
                    if confirming {
                        hub.delete_preset(&p.id);
                        hub.confirm_delete = None;
                    } else {
                        hub.confirm_delete = Some(p.id.clone());
                    }
                }
            });
        }
        None => {
            ui.weak("Select a character to edit its looks and gear, or enter a world as a capsule.");
        }
    }
}

/// Short description of what a preset holds.
fn preset_summary(p: &ffl_core::CharacterPreset) -> String {
    let items = p.settings.iter().filter(|(k, v)| (k.starts_with("item.") || k.starts_with("model.")) && !v.is_empty() && v.as_str() != "0").count();
    let mods = p.settings.get("mods").map(|m| m.split(';').filter(|s| !s.trim().is_empty()).count()).unwrap_or(0);
    let mut parts = Vec::new();
    if p.settings.contains_key("race") {
        parts.push(format!("{items} items"));
    } else if p.settings.contains_key("chara") {
        parts.push("from game files".to_string());
    }
    if mods > 0 {
        parts.push(format!("{mods} mod packs"));
    }
    parts.join(", ")
}

/// Simple field kinds; returns true when the value changed.
/// A field's title and its help text, each on its own wrapped line: nothing in the editor
/// may run past the panel edge, whatever the window size.
fn field_header(ui: &mut Ui, f: &PresetField) {
    ui.add(egui::Label::new(RichText::new(&f.label).strong()).wrap());
    if !f.hint.is_empty() {
        ui.add(egui::Label::new(RichText::new(&f.hint).weak().size(11.0)).wrap());
    }
}

fn preset_field(ui: &mut Ui, f: &PresetField, value: &mut String) -> bool {
    let width = (ui.available_width() - 8.0).max(80.0);
    match &f.kind {
        FieldKind::Choice(choices) => {
            let current = choices.iter().find(|c| &c.0 == value).map(|c| c.1.clone()).unwrap_or_else(|| value.clone());
            let before = value.clone();
            field_header(ui, f);
            egui::ComboBox::from_id_salt(&f.key).width(width.min(320.0)).selected_text(current).show_ui(ui, |ui| {
                for (v, l) in choices {
                    ui.selectable_value(value, v.clone(), l);
                }
            });
            ui.add_space(4.0);
            *value != before
        }
        FieldKind::Bool => {
            let mut b = value != "false" && value != "0" && !value.is_empty();
            let changed = ui.checkbox(&mut b, &f.label).changed();
            if !f.hint.is_empty() {
                ui.add(egui::Label::new(RichText::new(&f.hint).weak().size(11.0)).wrap());
            }
            if changed {
                *value = if b { "1".into() } else { "0".into() };
                true
            } else {
                false
            }
        }
        FieldKind::Integer { min, max } => {
            let mut n: i64 = value.parse().unwrap_or(*min);
            field_header(ui, f);
            ui.spacing_mut().slider_width = (width - 70.0).clamp(60.0, 260.0);
            if ui.add(egui::Slider::new(&mut n, *min..=*max)).changed() {
                *value = n.to_string();
                true
            } else {
                false
            }
        }
        FieldKind::Text => {
            field_header(ui, f);
            ui.add(egui::TextEdit::singleline(value).desired_width(width.min(320.0))).changed()
        }
        _ => false,
    }
}

/// The character editor (right panel while `hub.editor` is set). Every change shows in the
/// preview at once; Save writes the preset, Cancel drops the changes.
pub fn editor_panel(ui: &mut Ui, hub: &mut Hub) {
    let mut actions: Vec<EditorAction> = Vec::new();
    let mut save = false;
    let mut cancel = false;
    let mut pick: Option<Option<String>> = None;
    let mut search = false;
    let mut close_lookup = false;
    let engine_name = hub.editor.as_ref().map(|e| hub.engine_name(&e.preset.engine)).unwrap_or_default();
    let mut tab = hub.editor_tab.clone();

    let editor = hub.editor.as_mut().unwrap();
    let labels = editor.labels.clone();
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.heading("Character");
        ui.label(RichText::new(&engine_name).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            if ui.add(egui::Button::new(RichText::new("Save").strong())).clicked() {
                save = true;
            }
        });
    });
    ui.horizontal(|ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut editor.preset.name).desired_width((ui.available_width() - 8.0).clamp(80.0, 240.0)));
    });
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("import").selected_text("Import from game…").show_ui(ui, |ui| {
            for src in editor.imports.iter().filter(|s| !s.id.starts_with("glamourer:")) {
                if ui.selectable_label(false, &src.label).on_hover_text(&src.detail).clicked() {
                    actions.push(EditorAction::Import(src.id.clone()));
                }
            }
            if editor.imports.is_empty() {
                ui.label("nothing found to import");
            }
        });
        if editor.imports.iter().any(|s| s.id.starts_with("glamourer:")) {
            egui::ComboBox::from_id_salt("design").selected_text("Glamourer design…").show_ui(ui, |ui| {
                for src in editor.imports.iter().filter(|s| s.id.starts_with("glamourer:")) {
                    let label = src.label.trim_start_matches("Glamourer design: ");
                    if ui.selectable_label(false, label).on_hover_text(&src.detail).clicked() {
                        actions.push(EditorAction::Import(src.id.clone()));
                    }
                }
            });
        }
        if editor.preset.settings.get("archive").map(|v| v.trim()) == Some("1")
            && ui.button("Update modded copy").on_hover_text("copy the character's modded files now (also done on Save)").clicked()
        {
            actions.push(EditorAction::Archive);
        }
    });
    if !editor.status.is_empty() {
        ui.colored_label(Color32::LIGHT_GRAY, &editor.status);
    }
    ui.separator();

    // Lookup mode: the searchable catalog replaces the field list.
    if let Some(l) = editor.lookup.as_mut() {
        let field_label = editor.fields.iter().find(|f| f.key == l.key).map(|f| f.label.clone()).unwrap_or_else(|| l.key.clone());
        ui.horizontal(|ui| {
            if ui.button("← Back").clicked() {
                close_lookup = true;
            }
            ui.strong(format!("Choose {field_label}"));
        });
        ui.horizontal(|ui| {
            ui.label("Search");
            let r = ui.add(egui::TextEdit::singleline(&mut l.query).desired_width(220.0));
            if r.changed() {
                search = true;
            }
            r.request_focus();
            if ui.button(&l.none_label).clicked() {
                pick = Some(None);
            }
        });
        ui.small(format!("{} matches{}", l.results.len(), if l.results.len() >= crate::hub::LOOKUP_LIMIT { " (narrow the search for more)" } else { "" }));
        let current = editor.preset.settings.get(&l.key).cloned().unwrap_or_default();
        let row_height = ui.text_style_height(&egui::TextStyle::Button) + ui.spacing().item_spacing.y;
        egui::ScrollArea::vertical().id_salt("lookup").auto_shrink([false, false]).show_rows(ui, row_height, l.results.len(), |ui, range| {
            for r in &l.results[range] {
                let selected = r.id == current;
                if ui.selectable_label(selected, format!("{}  ({})", r.label, r.detail)).clicked() {
                    pick = Some(Some(r.id.clone()));
                }
            }
        });
    } else {
        // Field groups as tabs.
        let mut groups: Vec<String> = Vec::new();
        for f in &editor.fields {
            if !groups.contains(&f.group) {
                groups.push(f.group.clone());
            }
        }
        if tab.as_ref().is_none_or(|t| !groups.contains(t)) {
            tab = groups.first().cloned();
        }
        ui.horizontal_wrapped(|ui| {
            for g in &groups {
                let short = g.split(" (").next().unwrap_or(g).to_string();
                if ui.selectable_label(tab.as_deref() == Some(g.as_str()), short).on_hover_text(g).clicked() {
                    tab = Some(g.clone());
                }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("editor").auto_shrink([false, false]).show(ui, |ui| {
            let fields: Vec<PresetField> = editor.fields.iter().filter(|f| Some(&f.group) == tab.as_ref()).cloned().collect();
            for f in &fields {
                let value = editor.preset.settings.entry(f.key.clone()).or_default();
                match &f.kind {
                    FieldKind::Choice(_) | FieldKind::Bool | FieldKind::Integer { .. } | FieldKind::Text => {
                        if preset_field(ui, f, value) && matches!(f.kind, FieldKind::Choice(_)) {
                            actions.push(EditorAction::Refresh);
                        }
                    }
                    FieldKind::Palette(colors) => {
                        field_header(ui, f);
                        let current: usize = value.parse().unwrap_or(0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
                            for (i, c) in colors.iter().enumerate() {
                                let color = Color32::from_rgb(c[0], c[1], c[2]);
                                let selected = i == current;
                                let stroke = if selected { egui::Stroke::new(2.0, Color32::WHITE) } else { egui::Stroke::new(1.0, Color32::from_gray(60)) };
                                let button = egui::Button::new("").fill(color).min_size(egui::vec2(16.0, 16.0)).stroke(stroke);
                                if ui.add(button).on_hover_text(format!("{} #{i}", f.label)).clicked() {
                                    *value = i.to_string();
                                }
                            }
                        });
                        ui.add_space(4.0);
                    }
                    FieldKind::Lookup { catalog, none_label } => {
                        field_header(ui, f);
                        let width = (ui.available_width() - 40.0).clamp(80.0, 300.0);
                        ui.horizontal(|ui| {
                            let shown = if value.is_empty() || value == "0" && catalog.starts_with("gear:") && none_label.contains("bare") {
                                none_label.clone()
                            } else {
                                labels.get(&format!("{catalog}\0{value}")).cloned().unwrap_or_else(|| format!("#{value}"))
                            };
                            if ui.add_sized([width, 22.0], egui::Button::new(shown)).clicked() {
                                actions.push(EditorAction::OpenLookup {
                                    key: f.key.clone(),
                                    catalog: catalog.clone(),
                                    none_label: none_label.clone(),
                                });
                            }
                            if !value.is_empty() && ui.small_button("×").on_hover_text("clear").clicked() {
                                value.clear();
                            }
                        });
                        ui.add_space(2.0);
                    }
                    FieldKind::MultiChoice(choices) => {
                        field_header(ui, f);
                        let mut selected: Vec<String> = value.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
                        let mut changed = false;
                        for (v, l) in choices {
                            let mut on = selected.contains(v);
                            if ui.checkbox(&mut on, l).changed() {
                                changed = true;
                                if on {
                                    selected.push(v.clone());
                                } else {
                                    selected.retain(|s| s != v);
                                }
                            }
                        }
                        if changed {
                            *value = selected.join(";");
                        }
                    }
                }
            }
            if fields.is_empty() {
                ui.weak("this engine has no editable fields");
            }
        });
    }
    hub.editor_tab = tab;

    for a in actions {
        match a {
            EditorAction::Import(id) => hub.editor_import(&id),
            EditorAction::OpenLookup { key, catalog, none_label } => hub.editor_open_lookup(&key, &catalog, &none_label),
            EditorAction::Refresh => hub.refresh_editor(),
            EditorAction::Archive => hub.editor_archive(),
        }
    }
    if search {
        hub.editor_search();
    }
    if let Some(p) = pick {
        hub.editor_pick(p.as_deref());
    } else if close_lookup && let Some(e) = hub.editor.as_mut() {
        e.lookup = None;
    }
    if save {
        hub.save_editor();
    }
    if cancel {
        hub.cancel_editor();
    }
}

/// Worlds browser (right panel). Returns true when a world was double-clicked.
fn worlds_panel(ui: &mut Ui, hub: &mut Hub) -> bool {
    let mut enter = false;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.heading("Worlds");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(egui::TextEdit::singleline(&mut hub.map_filter).hint_text("search name or id").desired_width(180.0));
        });
    });
    // Engine tabs.
    ui.horizontal_wrapped(|ui| {
        if ui.selectable_label(hub.world_engine.is_none(), "All").clicked() {
            hub.world_engine = None;
            hub.world_category = None;
        }
        for e in hub.engines.clone().iter().filter(|e| e.available) {
            let n = hub.maps.get(&e.id).map(Vec::len).unwrap_or(0);
            if ui.selectable_label(hub.world_engine.as_deref() == Some(e.id.as_str()), format!("{} ({n})", e.name)).clicked() {
                hub.world_engine = Some(e.id.clone());
                hub.world_category = None;
            }
        }
    });
    // Category chips.
    let categories = hub.world_categories();
    if categories.len() > 1 {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            if ui.selectable_label(hub.world_category.is_none(), "any").clicked() {
                hub.world_category = None;
            }
            for c in categories {
                if ui.selectable_label(hub.world_category.as_deref() == Some(c.as_str()), &c).clicked() {
                    hub.world_category = Some(c);
                }
            }
        });
    }
    // Recent worlds.
    let recent: Vec<(String, String, String)> = hub.recent_maps().iter().map(|m| (m.engine.clone(), m.id.clone(), m.name.clone())).collect();
    if !recent.is_empty() {
        ui.separator();
        ui.label(RichText::new("Recent").weak());
        ui.horizontal_wrapped(|ui| {
            for (engine, id, name) in recent {
                let selected = hub.selected_map.as_ref() == Some(&(engine.clone(), id.clone()));
                let r = ui.selectable_label(selected, format!("{name} · {engine}"));
                if r.clicked() {
                    hub.selected_map = Some((engine.clone(), id.clone()));
                }
                if r.double_clicked() {
                    hub.selected_map = Some((engine, id));
                    enter = true;
                }
            }
        });
    }
    ui.separator();
    let maps: Vec<(String, String, String, String, String)> = hub
        .filtered_maps()
        .iter()
        .map(|m| (m.engine.clone(), m.id.clone(), m.name.clone(), m.category.clone(), m.detail.clone()))
        .collect();
    ui.small(format!("{} worlds", maps.len()));
    let details_height = 70.0;
    egui::ScrollArea::vertical().id_salt("maps").auto_shrink([false, false]).max_height(ui.available_height() - details_height).show(ui, |ui| {
        let show_engine = hub.world_engine.is_none();
        for (engine, id, name, category, detail) in &maps {
            let selected = hub.selected_map.as_ref() == Some(&(engine.clone(), id.clone()));
            let sub = if show_engine { format!("{category} · {} · {id}", hub.engine_name(engine)) } else { format!("{category} · {id}") };
            let r = list_row(ui, selected, name, &sub).on_hover_text(detail);
            if r.clicked() {
                hub.selected_map = Some((engine.clone(), id.clone()));
            }
            if r.double_clicked() {
                hub.selected_map = Some((engine.clone(), id.clone()));
                enter = true;
            }
        }
    });
    ui.separator();
    match hub.selected_map().cloned() {
        Some(m) => {
            ui.label(RichText::new(&m.name).strong());
            ui.small(format!("{} · {} · id {}", hub.engine_name(&m.engine), m.category, m.id));
            ui.small(&m.detail);
        }
        None => {
            ui.weak("Pick a world; double-click enters it.");
        }
    }
    enter
}

/// Graphics and audio settings window (shown while `hub.show_settings`); usable from the hub
/// and from inside a world. Changes are saved and reported as they are made (volume sliders
/// report live while dragged and save when released).
pub fn settings_window(ctx: &Context, hub: &mut Hub) -> Vec<HubEvent> {
    let mut events = Vec::new();
    if !hub.show_settings {
        return events;
    }
    let mut open = true;
    let mut changed = false;
    let mut audio_changed = false;
    let mut audio_live = false;
    let mut mods_changed: Vec<(String, Vec<String>)> = Vec::new();
    let mut profile_changed: Vec<(String, Option<String>)> = Vec::new();
    // (engine id, engine name, packs, profiles, enabled ids) snapshot so the closure only
    // borrows `g`.
    let mut engine_ids: Vec<String> = hub.mods.keys().chain(hub.profiles.keys()).cloned().collect();
    engine_ids.sort();
    engine_ids.dedup();
    let mod_lists: Vec<(String, String, Vec<ffl_core::ModInfo>, Vec<ffl_core::ModProfile>, Vec<String>)> = engine_ids
        .iter()
        .map(|id| {
            let name = hub.engines.iter().find(|e| &e.id == id).map(|e| e.name.clone()).unwrap_or(id.clone());
            (
                id.clone(),
                name,
                hub.mods.get(id).cloned().unwrap_or_default(),
                hub.profiles.get(id).cloned().unwrap_or_default(),
                hub.settings.mods.get(id).cloned().unwrap_or_default(),
            )
        })
        .collect();
    egui::Window::new("Settings")
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(360.0)
        .collapsible(false)
        .open(&mut open)
        .show(ctx, |ui| {
            let g = &mut hub.settings.graphics;
            ui.heading("Graphics");
            changed |= ui.checkbox(&mut g.fullscreen, "Borderless fullscreen").changed();
            changed |= ui.checkbox(&mut g.vsync, "Vertical sync").changed();
            ui.horizontal(|ui| {
                ui.label("Anti-aliasing (MSAA)");
                for n in [1u8, 2, 4, 8] {
                    let label = if n == 1 { "off".to_string() } else { format!("{n}x") };
                    if ui.selectable_label(g.msaa == n, label).clicked() {
                        g.msaa = n;
                        changed = true;
                    }
                }
            });
            changed |= ui.checkbox(&mut g.shadows, "Shadows").changed();
            ui.horizontal(|ui| {
                ui.label("Shadow resolution");
                for n in [1024u32, 2048, 4096] {
                    if ui.selectable_label(g.shadow_resolution == n, n.to_string()).clicked() {
                        g.shadow_resolution = n;
                        changed = true;
                    }
                }
            });
            changed |= ui
                .add(egui::Slider::new(&mut g.render_distance, 100.0..=5000.0).text("Render distance (m)").logarithmic(true))
                .drag_stopped();
            changed |= ui
                .add(egui::Slider::new(&mut g.detail_distance, 20.0..=1000.0).text("Detail distance (× object size)").logarithmic(true))
                .drag_stopped();
            changed |= ui
                .add(egui::Slider::new(&mut g.lod_distance, 0.5..=8.0).text("Model detail (× the game's LOD distances)").logarithmic(true))
                .drag_stopped();
            ui.horizontal(|ui| {
                ui.label("Anisotropic filtering");
                for n in [1u8, 2, 4, 8, 16] {
                    let label = if n == 1 { "off".to_string() } else { format!("{n}x") };
                    if ui.selectable_label(g.anisotropy == n, label).clicked() {
                        g.anisotropy = n;
                        changed = true;
                    }
                }
            });
            changed |= ui.add(egui::Slider::new(&mut g.ui_scale, 0.5..=2.0).text("UI scale")).drag_stopped();
            ui.small("Texture filtering applies to worlds entered from now on.");
            ui.separator();
            if ui.button("Reset to defaults").clicked() {
                *g = Default::default();
                changed = true;
            }
            ui.separator();
            ui.heading("Audio");
            let a = &mut hub.settings.audio;
            audio_changed |= ui.checkbox(&mut a.mute, "Mute all (M)").changed();
            audio_changed |= ui.checkbox(&mut a.music_enabled, "Music").changed();
            audio_changed |= ui.checkbox(&mut a.show_monitor, "Show playing sounds (top right)").changed();
            let master = ui.add(egui::Slider::new(&mut a.master, 0.0..=1.0).text("Master").fixed_decimals(2));
            audio_live |= master.changed();
            audio_changed |= master.drag_stopped();
            for category in ffl_core::SoundCategory::ALL {
                let r = ui.add(egui::Slider::new(a.volume_mut(category), 0.0..=1.0).text(category.label()).fixed_decimals(2));
                audio_live |= r.changed();
                audio_changed |= r.drag_stopped();
            }
            if ui.button("Reset audio").clicked() {
                *a = Default::default();
                audio_changed = true;
            }
            if !mod_lists.is_empty() {
                ui.separator();
                ui.heading("Mods");
                ui.small("Enabled mods replace game files everywhere (worlds and characters). A character can add its own in its editor.");
                for (engine_id, engine_name, packs, profiles, enabled_now) in &mod_lists {
                    ui.label(engine_name);
                    if !profiles.is_empty() {
                        let current = enabled_now.iter().find(|id| id.starts_with("penumbra:")).cloned();
                        ui.horizontal(|ui| {
                            ui.label("Penumbra collection");
                            let shown = current.as_ref().and_then(|c| profiles.iter().find(|p| &p.id == c)).map(|p| p.name.clone()).unwrap_or_else(|| "none".into());
                            egui::ComboBox::from_id_salt(format!("profile-{engine_id}")).selected_text(shown).show_ui(ui, |ui| {
                                if ui.selectable_label(current.is_none(), "none").clicked() {
                                    profile_changed.push((engine_id.clone(), None));
                                }
                                for p in profiles {
                                    let on = current.as_deref() == Some(p.id.as_str());
                                    if ui.selectable_label(on, format!("{} ({} mods)", p.name, p.mods)).on_hover_text(&p.detail).clicked() {
                                        profile_changed.push((engine_id.clone(), Some(p.id.clone())));
                                    }
                                }
                            });
                        });
                    }
                    let mut enabled = enabled_now.clone();
                    let mut touched = false;
                    for p in packs {
                        let mut on = enabled.contains(&p.id);
                        let text = format!("{}  ({} files)", p.name, p.files);
                        if ui.checkbox(&mut on, text).on_hover_text(&p.description).changed() {
                            touched = true;
                            if on {
                                enabled.push(p.id.clone());
                            } else {
                                enabled.retain(|e| e != &p.id);
                            }
                        }
                    }
                    if touched {
                        mods_changed.push((engine_id.clone(), enabled));
                    }
                }
            }
        });
    if !open {
        hub.show_settings = false;
    }
    if changed {
        events.push(hub.commit_settings());
    }
    if audio_changed {
        events.push(hub.commit_audio());
    } else if audio_live {
        events.push(hub.audio_event());
    }
    for (engine, ids) in mods_changed {
        events.push(hub.set_mods(&engine, ids));
    }
    for (engine, profile) in profile_changed {
        events.push(hub.set_profile(&engine, profile.as_deref()));
    }
    events
}

/// In-world "Actions" window (addon toggles, idle poses, emotes).
/// Top-right list of the sounds playing right now: who owns each (character / world), the
/// engine it came from, its category, file and reason. Ended one-shots linger dimmed for a
/// moment so footsteps can be read.
pub fn sound_monitor(ctx: &Context, monitor: &SoundMonitor) {
    // Below the top bar (Menu / Settings / UI / Quit), which sits in the top 40 px.
    egui::Window::new("Sounds")
        .anchor(egui::Align2::RIGHT_TOP, [-10.0, 48.0])
        .title_bar(false)
        .resizable(false)
        .default_width(420.0)
        .frame(egui::Frame::window(&ctx.global_style()).fill(Color32::from_black_alpha(190)))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Sounds");
                ui.colored_label(Color32::LIGHT_GRAY, format!("character: {}   world: {}   ground: {}", monitor.character_engine, monitor.world_engine, monitor.ground));
                if monitor.muted {
                    ui.colored_label(Color32::YELLOW, "MUTED");
                }
            });
            if monitor.entries.is_empty() {
                ui.weak("nothing playing");
                return;
            }
            egui::Grid::new("sound_monitor").num_columns(5).spacing([10.0, 2.0]).show(ui, |ui| {
                for e in &monitor.entries {
                    let playing = e.ended_for <= 0.0;
                    let tint = |c: Color32| if playing { c } else { c.gamma_multiply(0.45) };
                    let source = match e.source {
                        SoundSource::Character => tint(Color32::from_rgb(120, 200, 255)),
                        SoundSource::World => tint(Color32::from_rgb(160, 230, 140)),
                    };
                    ui.colored_label(source, format!("{} · {}", e.source.label(), e.engine));
                    ui.colored_label(tint(Color32::LIGHT_GRAY), e.category.label());
                    ui.colored_label(tint(Color32::WHITE), &e.name);
                    ui.colored_label(tint(Color32::GRAY), &e.detail);
                    ui.horizontal(|ui| {
                        let bar = egui::ProgressBar::new(if playing { e.level.min(1.0) } else { 0.0 }).desired_width(60.0).desired_height(8.0).fill(tint(Color32::LIGHT_GREEN));
                        ui.add(bar);
                        ui.colored_label(tint(Color32::LIGHT_GREEN), format!("{:.2}", e.level));
                    });
                    ui.end_row();
                }
            });
        });
}

/// Bottom-right actions panel, styled like the sound monitor: the character and its engine
/// in the header, then only the sections the character has (weapons, stances, emotes).
pub fn actions_window(ctx: &Context, view: ActionsView<'_>) -> Vec<ActionEvent> {
    let mut events = Vec::new();
    let screen = ctx.content_rect();
    egui::Window::new("Actions")
        .anchor(Align2::RIGHT_BOTTOM, [-10.0, -10.0])
        .title_bar(false)
        .resizable(false)
        .fixed_size([340.0, (screen.height() * 0.42).max(220.0)])
        .frame(egui::Frame::window(&ctx.global_style()).fill(Color32::from_black_alpha(190)))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong(view.character);
                ui.colored_label(Color32::LIGHT_GRAY, view.engine);
                if !view.status.is_empty() {
                    ui.colored_label(Color32::YELLOW, view.status);
                }
            });
            if !view.addons.is_empty() {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(Color32::LIGHT_GRAY, "Weapons");
                    for (def, on) in view.addons.iter_mut() {
                        ui.checkbox(on, &def.name);
                    }
                });
            }
            let idles: Vec<&ffl_core::ActionDef> = view.actions.iter().filter(|a| a.category == ActionCategory::Idle).collect();
            if !idles.is_empty() {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(Color32::LIGHT_GRAY, "Stance");
                    for a in idles {
                        if ui.selectable_label(view.selected_idle == a.id, &a.name).clicked() {
                            events.push(ActionEvent::Play(a.clone()));
                        }
                    }
                });
            }
            let emotes: Vec<&ffl_core::ActionDef> = view.actions.iter().filter(|a| a.category == ActionCategory::Emote).collect();
            if !emotes.is_empty() {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::LIGHT_GRAY, "Emotes");
                    ui.add(egui::TextEdit::singleline(view.filter).hint_text("search").desired_width(160.0));
                    if view.playing.is_some() && ui.button("Stop").clicked() {
                        events.push(ActionEvent::Stop);
                    }
                });
                let filter = view.filter.to_lowercase();
                egui::ScrollArea::vertical().id_salt("emotes").auto_shrink([false, false]).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                        for a in emotes {
                            if !filter.is_empty() && !a.name.to_lowercase().contains(&filter) {
                                continue;
                            }
                            let active = view.playing == Some(a.id.as_str());
                            let label = if a.looped { format!("{} (loop)", a.name) } else { a.name.clone() };
                            if ui.selectable_label(active, label).clicked() {
                                events.push(ActionEvent::Play(a.clone()));
                            }
                        }
                    });
                });
            }
        });
    events
}


/// The "Log" window: every message of the session, wrapped, newest at the bottom.
pub fn log_window(ctx: &Context, hub: &mut Hub) {
    if !hub.show_log {
        return;
    }
    let mut open = true;
    let mut clear = false;
    egui::Window::new("Log")
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_size([640.0, 360.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.small(format!("{} messages this session; the full program log is in {}", hub.log.len(), Library::path().with_file_name("fflocal.log").display()));
                if ui.small_button("Clear").clicked() {
                    clear = true;
                }
            });
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for e in &hub.log {
                    let color = if e.error { Color32::LIGHT_RED } else { Color32::LIGHT_GRAY };
                    ui.horizontal_top(|ui| {
                        ui.label(RichText::new(format!("{:02}:{:02}", (e.at / 60.0) as u32, (e.at % 60.0) as u32)).weak().monospace());
                        ui.add(egui::Label::new(RichText::new(&e.text).color(color)).wrap());
                    });
                }
                if hub.log.is_empty() {
                    ui.weak("nothing yet");
                }
            });
        });
    if clear {
        hub.log.clear();
    }
    if !open {
        hub.show_log = false;
    }
}

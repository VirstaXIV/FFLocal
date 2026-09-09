//! Hub model: engines, character presets, worlds, selection and settings.

use std::collections::HashMap;
use std::sync::Arc;

use ffl_core::{ActionDef, AddonDef, CatalogEntry, CharacterPreset, Engine, EngineInfo, FieldKind, ImportSource, InstallCandidate, Library, MapInfo, ModInfo, ModProfile, PresetField, SoundCategory};

use crate::settings::{AudioSettings, GraphicsSettings, Settings};

/// What to load when entering a world.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldRequest {
    pub engine: String,
    pub map: String,
    pub map_name: String,
    pub character: Option<CharacterPreset>,
}

/// What the hub asks its host to do.
#[derive(Debug, Clone, PartialEq)]
pub enum HubEvent {
    EnterWorld(WorldRequest),
    /// Close the application.
    Quit,
    /// Use this install for an engine (saved; empty = auto-detect) and reopen the engine.
    SetGamePath { engine: String, path: String },
    /// Scan the machine for installs of an engine's game (fills `Hub::game_candidates`).
    ScanGame(String),
    /// Turn an engine's game off (its engine closes: characters, worlds and sounds of that game
    /// disappear) or on again (saved; the engine reopens from the saved install).
    SetGameEnabled { engine: String, enabled: bool },
    /// Open a folder picker for an engine's install.
    BrowseGame(String),
    /// Use this directory for an engine's plugin data (saved; empty = auto-detect) and reopen
    /// the engine.
    SetPluginsPath { engine: String, path: String },
    /// Graphics settings changed (already saved); the runtime should apply them.
    GraphicsChanged(GraphicsSettings),
    /// The globally enabled mod packs of an engine changed (already saved).
    ModsChanged(String, Vec<String>),
    /// Audio settings changed; the runtime should apply them (saved unless the change is a
    /// slider still being dragged, which commits when released).
    AudioChanged(AudioSettings),
}

/// Most entries a lookup lists at once (the list is virtualised, so thousands are fine).
pub const LOOKUP_LIMIT: usize = 20_000;

/// A lookup popup for one [`FieldKind::Lookup`] field.
#[derive(Debug, Default)]
pub struct LookupState {
    pub key: String,
    pub catalog: String,
    pub none_label: String,
    pub query: String,
    pub results: Vec<CatalogEntry>,
    pub searched: bool,
}

/// One line of the hub's message log (the "Log" window): what happened, when, whether it
/// failed. The status line under the enter bar shows the newest one; the log keeps them.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Seconds since the hub started.
    pub at: f64,
    pub error: bool,
    pub text: String,
}

/// The character being edited.
#[derive(Debug, Default)]
pub struct PresetEditor {
    pub preset: CharacterPreset,
    pub fields: Vec<PresetField>,
    pub imports: Vec<ImportSource>,
    pub lookup: Option<LookupState>,
    /// `catalog\0id` → label.
    pub labels: HashMap<String, String>,
    pub status: String,
}

#[derive(Default)]
pub struct Hub {
    pub engines: Vec<EngineInfo>,
    /// The engines themselves (imports, catalogs, mod lists).
    pub engine_impls: Vec<Arc<dyn Engine>>,
    pub maps: HashMap<String, Vec<MapInfo>>,
    /// Mod packs per engine id.
    pub mods: HashMap<String, Vec<ModInfo>>,
    /// Mod profiles (Penumbra collections) per engine id.
    pub profiles: HashMap<String, Vec<ModProfile>>,
    pub library: Library,
    pub settings: Settings,
    pub selected_preset: Option<String>,
    pub selected_map: Option<(String, String)>,
    pub map_filter: String,
    /// Worlds browser: engine tab (`None` = every engine) and category filter.
    pub world_engine: Option<String>,
    pub world_category: Option<String>,
    pub editor: Option<PresetEditor>,
    /// Field group the editor shows (`None` = the first).
    pub editor_tab: Option<String>,
    /// Preset id whose deletion awaits a second click.
    pub confirm_delete: Option<String>,
    /// What the runtime's character preview is doing ("loading…", an error), for display.
    pub preview_status: String,
    /// The part of the hub screen no panel covers (x0, y0, x1, y1), set by `hub_ui`: the
    /// runtime only takes pointer input for the preview inside it.
    pub free_rect: Option<[f32; 4]>,
    pub status: String,
    /// Every status and note so far (newest last); shown in the Log window.
    pub log: Vec<LogEntry>,
    pub show_log: bool,
    /// The status text already copied into the log.
    pub status_logged: String,
    pub started: Option<std::time::Instant>,
    pub show_settings: bool,
    /// The Games window (installs per engine).
    pub show_games: bool,
    /// Path being typed per engine id.
    pub game_edit: HashMap<String, String>,
    /// Plugin-data path being typed per engine id.
    pub plugins_edit: HashMap<String, String>,
    /// Installs a scan found per engine id.
    pub game_candidates: HashMap<String, Vec<InstallCandidate>>,
    /// Bumped whenever an engine is reopened, so caches keyed on engine data (the preview)
    /// know to reload.
    pub engine_generation: u64,
    pub built: bool,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub").field("engines", &self.engines).field("presets", &self.library.presets.len()).finish()
    }
}

impl Hub {
    pub fn new(library: Library, settings: Settings) -> Hub {
        Hub {
            selected_preset: library.last_preset.clone(),
            selected_map: library.last_map.clone(),
            library,
            settings,
            ..Default::default()
        }
    }

    /// Forget the engine lists and query the engines again (after a game install changed).
    pub fn rebuild(&mut self, engines: &[Arc<dyn Engine>]) {
        self.built = false;
        self.engine_generation += 1;
        self.maps.clear();
        self.mods.clear();
        self.profiles.clear();
        self.build(engines);
        if self.selected_map().is_none() {
            self.selected_map = None;
        }
    }

    /// Query the engines for maps, mod packs and default presets (once).
    pub fn build(&mut self, engines: &[Arc<dyn Engine>]) {
        if self.built {
            return;
        }
        self.engine_impls = engines.to_vec();
        self.engines = engines.iter().map(|e| e.info()).collect();
        for e in &self.engines {
            let saved = self.settings.games.get(&e.id).map(|g| g.path.clone()).unwrap_or_default();
            self.game_edit.entry(e.id.clone()).or_insert_with(|| if saved.is_empty() { e.path.clone().unwrap_or_default() } else { saved });
            let plugins = self.settings.games.get(&e.id).map(|g| g.plugins_path.clone()).unwrap_or_default();
            self.plugins_edit.entry(e.id.clone()).or_insert(plugins);
        }
        // Nothing to play with: the Games window is the first thing to see.
        if self.engines.iter().all(|e| !e.available) {
            self.show_games = true;
        }
        for engine in engines {
            let info = engine.info();
            if !info.available {
                continue;
            }
            match engine.list_maps() {
                Ok(maps) => {
                    self.maps.insert(info.id.clone(), maps);
                }
                Err(err) => self.status = format!("{}: maps failed: {err:#}", info.name),
            }
            let mods = engine.list_mods();
            if !mods.is_empty() {
                self.mods.insert(info.id.clone(), mods);
            }
            let profiles = engine.mod_profiles();
            if !profiles.is_empty() {
                self.profiles.insert(info.id.clone(), profiles);
            }
            if let Some(enabled) = self.settings.mods.get(&info.id) {
                engine.set_enabled_mods(enabled);
            }
            if self.library.presets.iter().all(|p| p.engine != info.id)
                && let Ok(defaults) = engine.default_presets()
            {
                for p in defaults {
                    self.library.upsert(p);
                }
                if let Err(err) = self.library.save() {
                    tracing::warn!("saving presets: {err:#}");
                }
            }
        }
        if self.selected_preset.is_none() {
            self.selected_preset = self.library.presets.first().map(|p| p.id.clone());
        }
        self.built = true;
    }

    pub fn engine(&self, id: &str) -> Option<&Arc<dyn Engine>> {
        self.engine_impls.iter().find(|e| e.info().id == id)
    }

    pub fn engine_name(&self, id: &str) -> String {
        self.engines.iter().find(|e| e.id == id).map(|e| e.name.clone()).unwrap_or_else(|| id.to_string())
    }

    /// The character the preview should show: the one being edited, else the selection.
    pub fn preview_preset(&self) -> Option<&CharacterPreset> {
        self.editor.as_ref().map(|e| &e.preset).or_else(|| self.selected_preset())
    }

    /// Open the editor on the selected preset, at a field group (e.g. "Gear").
    /// Glamourer designs (or any other "design" import source) the selected character's
    /// engine offers.
    pub fn design_sources(&self, engine_id: &str) -> Vec<ImportSource> {
        self.engine(engine_id).map(|e| e.import_sources().into_iter().filter(|s| s.id.starts_with("glamourer:")).collect()).unwrap_or_default()
    }

    /// Apply a design to the selected character and save it (the preview reloads).
    pub fn apply_design_to_selected(&mut self, source_id: &str) {
        let Some(mut preset) = self.selected_preset().cloned() else {
            return;
        };
        let Some(engine) = self.engine(&preset.engine) else {
            return;
        };
        match engine.import_preset(&mut preset, source_id) {
            Ok(note) => {
                let name = preset.name.clone();
                let wants_copy = preset.settings.get("archive").map(|v| v.trim()) == Some("1");
                self.save_preset(preset.clone());
                if wants_copy {
                    match self.archive_preset(&preset) {
                        Ok((_, detail)) => self.log(detail),
                        Err(err) => self.log(format!("modded copy of {name}: {err:#}")),
                    }
                }
                self.note(format!("{name}: {note}"), format!("{name}: {note}"));
            }
            Err(err) => self.status = format!("design failed: {err:#}"),
        }
    }

    pub fn edit_selected(&mut self, tab: Option<&str>) {
        if let Some(p) = self.selected_preset().cloned() {
            self.start_edit(p);
            self.editor_tab = tab.map(str::to_string);
        }
    }

    /// Categories of the worlds under the current engine tab, in first-seen order.
    pub fn world_categories(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in &self.engines {
            if self.world_engine.as_deref().is_some_and(|w| w != e.id) {
                continue;
            }
            for m in self.maps.get(&e.id).map(Vec::as_slice).unwrap_or(&[]) {
                if !out.contains(&m.category) {
                    out.push(m.category.clone());
                }
            }
        }
        out
    }

    /// Worlds matching the engine tab, category and text filter.
    pub fn filtered_maps(&self) -> Vec<&MapInfo> {
        let filter = self.map_filter.trim().to_lowercase();
        let mut out = Vec::new();
        for e in &self.engines {
            if self.world_engine.as_deref().is_some_and(|w| w != e.id) {
                continue;
            }
            for m in self.maps.get(&e.id).map(Vec::as_slice).unwrap_or(&[]) {
                if self.world_category.as_deref().is_some_and(|c| c != m.category) {
                    continue;
                }
                if !filter.is_empty() && !(m.name.to_lowercase().contains(&filter) || m.id.contains(&filter) || m.detail.to_lowercase().contains(&filter)) {
                    continue;
                }
                out.push(m);
            }
        }
        out
    }

    /// Recently entered worlds that still exist, newest first.
    pub fn recent_maps(&self) -> Vec<&MapInfo> {
        self.library.recent_maps.iter().filter_map(|(e, id)| self.map(e, id)).collect()
    }

    /// Presets of one engine, in library order.
    pub fn presets_of(&self, engine_id: &str) -> Vec<&CharacterPreset> {
        self.library.presets.iter().filter(|p| p.engine == engine_id).collect()
    }

    // ---- character editor ----

    /// Open the editor on a copy of `preset`.
    pub fn start_edit(&mut self, preset: CharacterPreset) {
        let mut editor = PresetEditor {
            preset,
            ..Default::default()
        };
        if let Some(engine) = self.engine(&editor.preset.engine) {
            editor.imports = engine.import_sources();
            match engine.prepare_for_edit(&mut editor.preset) {
                Ok(Some(note)) => editor.status = note,
                Ok(None) => {}
                Err(err) => editor.status = format!("{err:#}"),
            }
        }
        self.editor = Some(editor);
        self.refresh_editor();
    }

    /// Start a fresh character for `engine_id`.
    pub fn new_character(&mut self, engine_id: &str) {
        let Some(engine) = self.engine(engine_id) else {
            return;
        };
        match engine.new_preset(&format!("New {} character", engine.info().name)) {
            Ok(p) => self.start_edit(p),
            Err(err) => self.status = format!("new character: {err:#}"),
        }
    }

    /// Re-query the fields for the preset being edited (choices depend on race etc.) and
    /// coerce values that are no longer offered.
    pub fn refresh_editor(&mut self) {
        let Some(mut editor) = self.editor.take() else {
            return;
        };
        if let Some(engine) = self.engine(&editor.preset.engine) {
            match engine.preset_fields(Some(&editor.preset)) {
                Ok(fields) => editor.fields = fields,
                Err(err) => editor.status = format!("fields: {err:#}"),
            }
            for f in &editor.fields {
                let value = editor.preset.settings.entry(f.key.clone()).or_default();
                match &f.kind {
                    FieldKind::Choice(c) if !c.is_empty() && !c.iter().any(|(v, _)| v == value) => {
                        *value = c[0].0.clone();
                    }
                    FieldKind::Lookup { catalog, .. } if !value.is_empty() => {
                        let k = format!("{catalog}\0{value}");
                        if !editor.labels.contains_key(&k)
                            && let Some(label) = engine.catalog_label(catalog, value)
                        {
                            editor.labels.insert(k, label);
                        }
                    }
                    _ => {}
                }
            }
        }
        self.editor = Some(editor);
    }

    pub fn editor_import(&mut self, source_id: &str) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let Some(engine) = self.engine_impls.iter().find(|e| e.info().id == editor.preset.engine) else {
            return;
        };
        match engine.import_preset(&mut editor.preset, source_id) {
            Ok(note) => editor.status = note,
            Err(err) => editor.status = format!("import failed: {err:#}"),
        }
        self.refresh_editor();
    }

    pub fn editor_open_lookup(&mut self, key: &str, catalog: &str, none_label: &str) {
        if let Some(editor) = self.editor.as_mut() {
            editor.lookup = Some(LookupState {
                key: key.into(),
                catalog: catalog.into(),
                none_label: none_label.into(),
                ..Default::default()
            });
            self.editor_search();
        }
    }

    pub fn editor_search(&mut self) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let Some(engine) = self.engine_impls.iter().find(|e| e.info().id == editor.preset.engine) else {
            return;
        };
        if let Some(l) = editor.lookup.as_mut() {
            l.results = engine.catalog(&l.catalog, &l.query, LOOKUP_LIMIT).unwrap_or_default();
            l.searched = true;
        }
    }

    /// Pick a lookup result (`None` clears the field) and close the popup.
    pub fn editor_pick(&mut self, id: Option<&str>) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let Some(l) = editor.lookup.take() else {
            return;
        };
        let value = id.unwrap_or("").to_string();
        if let Some(entry) = l.results.iter().find(|r| r.id == value) {
            editor.labels.insert(format!("{}\0{}", l.catalog, value), entry.label.clone());
        }
        editor.preset.settings.insert(l.key, value);
    }

    pub fn editor_label(&self, catalog: &str, id: &str) -> Option<String> {
        self.editor.as_ref()?.labels.get(&format!("{catalog}\0{id}")).cloned()
    }

    pub fn save_editor(&mut self) {
        if let Some(editor) = self.editor.take() {
            let wants_copy = editor.preset.settings.get("archive").map(|v| v.trim()) == Some("1");
            let preset = editor.preset;
            if wants_copy {
                // The copy follows the character: rebuilt whenever the preset is saved.
                match self.archive_preset(&preset) {
                    Ok((status, detail)) => self.note(status, detail),
                    Err(err) => self.status = format!("modded copy of {}: {err:#}", preset.name),
                }
            }
            self.save_preset(preset);
        }
        self.editor_tab = None;
    }

    /// Where a preset's modded copy lives: `<data dir>/characters/<engine>/<preset id>/`.
    pub fn archive_dir(preset: &CharacterPreset) -> std::path::PathBuf {
        Library::path().with_file_name("characters").join(&preset.engine).join(&preset.id)
    }

    /// Copy the preset's modded files through its engine (see `Engine::archive_character`).
    /// Returns (short status, detailed log line).
    pub fn archive_preset(&self, preset: &CharacterPreset) -> anyhow::Result<(String, String)> {
        let engine = self.engine(&preset.engine).ok_or_else(|| anyhow::anyhow!("no engine {}", preset.engine))?;
        let dir = Self::archive_dir(preset);
        let r = engine.archive_character(preset, &dir)?;
        let status = format!("Modded copy of {} updated: {} modded files ({:.1} MB), {} from the game", preset.name, r.files, r.bytes as f64 / 1e6, r.vanilla);
        let detail = format!(
            "modded copy of {}: {} modded files ({:.1} MB) from {}; {} vanilla files stay in the game; written to {}",
            preset.name,
            r.files,
            r.bytes as f64 / 1e6,
            if r.packs.is_empty() { "no mods".to_string() } else { r.packs.join(", ") },
            r.vanilla,
            r.dir.display()
        );
        Ok((status, detail))
    }

    /// Build the modded copy of the preset being edited now (without saving).
    pub fn editor_archive(&mut self) {
        let Some(preset) = self.editor.as_ref().map(|e| e.preset.clone()) else {
            return;
        };
        let status = match self.archive_preset(&preset) {
            Ok((status, detail)) => {
                self.log(detail);
                status
            }
            Err(err) => format!("modded copy: {err:#}"),
        };
        if let Some(editor) = self.editor.as_mut() {
            editor.status = status;
        }
    }

    /// Replace the profile (collection) an engine uses globally; `None` = no profile. Pack
    /// ids stay as they are.
    pub fn set_profile(&mut self, engine_id: &str, profile: Option<&str>) -> HubEvent {
        let mut ids: Vec<String> = self.settings.mods.get(engine_id).cloned().unwrap_or_default();
        ids.retain(|id| !id.starts_with("penumbra:"));
        if let Some(p) = profile {
            ids.insert(0, p.to_string());
        }
        self.set_mods(engine_id, ids)
    }

    pub fn cancel_editor(&mut self) {
        self.editor = None;
        self.editor_tab = None;
    }

    /// Duplicate a preset into the editor.
    pub fn duplicate_preset(&mut self, id: &str) {
        if let Some(p) = self.preset(id).cloned() {
            let mut copy = p;
            copy.id = Library::new_id();
            copy.name = format!("{} (copy)", copy.name);
            self.start_edit(copy);
        }
    }

    // ---- mods ----

    pub fn set_mods(&mut self, engine_id: &str, ids: Vec<String>) -> HubEvent {
        self.settings.mods.insert(engine_id.to_string(), ids.clone());
        if let Err(err) = self.settings.save() {
            self.status = format!("saving settings: {err:#}");
        }
        if let Some(engine) = self.engine(engine_id) {
            engine.set_enabled_mods(&ids);
        }
        // The preview keys on the engine generation: the character must reload with the
        // new mods (a collection swaps its body, face and gear files).
        self.engine_generation += 1;
        HubEvent::ModsChanged(engine_id.to_string(), ids)
    }

    /// Seconds since the hub was created.
    pub fn uptime(&mut self) -> f64 {
        self.started.get_or_insert_with(std::time::Instant::now).elapsed().as_secs_f64()
    }

    /// Append to the log without touching the status line (details behind a short status).
    pub fn log(&mut self, text: impl Into<String>) {
        let text = text.into();
        let error = Self::status_is_error(&text);
        let at = self.uptime();
        self.log.push(LogEntry { at, error, text });
        if self.log.len() > 500 {
            self.log.drain(..100);
        }
    }

    /// A short status line plus a detailed log entry.
    pub fn note(&mut self, status: impl Into<String>, detail: impl Into<String>) {
        let status = status.into();
        self.status = status.clone();
        self.status_logged = status;
        self.log(detail);
    }

    /// Called every frame by the UI: a status set directly (by any of the failure paths)
    /// lands in the log once.
    pub fn sync_log(&mut self) {
        if !self.status.is_empty() && self.status != self.status_logged {
            self.status_logged = self.status.clone();
            let text = self.status.clone();
            self.log(text);
        }
    }

    /// Whether a status line reports a failure (drawn red) rather than a note.
    pub fn status_is_error(text: &str) -> bool {
        let t = text.to_ascii_lowercase();
        ["fail", "error", "cannot", "unavailable", "panick", "not found", "no engine", "unknown", "unreadable", "missing"].iter().any(|w| t.contains(w))
    }

    pub fn preset(&self, id: &str) -> Option<&CharacterPreset> {
        self.library.presets.iter().find(|p| p.id == id)
    }

    pub fn selected_preset(&self) -> Option<&CharacterPreset> {
        self.selected_preset.as_deref().and_then(|id| self.preset(id))
    }

    pub fn map(&self, engine: &str, id: &str) -> Option<&MapInfo> {
        self.maps.get(engine).and_then(|m| m.iter().find(|m| m.id == id))
    }

    pub fn selected_map(&self) -> Option<&MapInfo> {
        self.selected_map.as_ref().and_then(|(e, id)| self.map(e, id))
    }

    pub fn save_preset(&mut self, preset: CharacterPreset) {
        self.selected_preset = Some(preset.id.clone());
        self.library.upsert(preset);
        if let Err(err) = self.library.save() {
            self.status = format!("saving presets: {err:#}");
        }
    }

    pub fn delete_preset(&mut self, id: &str) {
        self.library.presets.retain(|p| p.id != id);
        if self.selected_preset.as_deref() == Some(id) {
            self.selected_preset = None;
        }
        if let Err(err) = self.library.save() {
            self.status = format!("saving presets: {err:#}");
        }
    }

    /// Build the request for the current selection and remember it as the last used.
    pub fn enter(&mut self) -> Option<WorldRequest> {
        let (engine, id) = self.selected_map.clone()?;
        let map_name = self.map(&engine, &id).map(|m| m.name.clone()).unwrap_or_default();
        let character = self.selected_preset().cloned();
        self.library.remember_map(&engine, &id);
        self.library.last_preset = self.selected_preset.clone();
        if let Err(err) = self.library.save() {
            tracing::warn!("saving presets: {err:#}");
        }
        Some(WorldRequest {
            engine,
            map: id,
            map_name,
            character,
        })
    }

    /// Persist the settings and report the change.
    pub fn commit_settings(&mut self) -> HubEvent {
        self.settings.graphics = self.settings.graphics.clone().sanitized();
        if let Err(err) = self.settings.save() {
            self.status = format!("saving settings: {err:#}");
        }
        HubEvent::GraphicsChanged(self.settings.graphics.clone())
    }

    /// Persist the audio settings and report the change.
    pub fn commit_audio(&mut self) -> HubEvent {
        self.settings.audio = self.settings.audio.clone().sanitized();
        if let Err(err) = self.settings.save() {
            self.status = format!("saving settings: {err:#}");
        }
        HubEvent::AudioChanged(self.settings.audio.clone())
    }

    /// Persist the UI panel settings.
    pub fn commit_ui(&mut self) {
        if let Err(err) = self.settings.save() {
            self.status = format!("saving settings: {err:#}");
        }
    }

    /// Report the audio settings without saving (live slider feedback).
    pub fn audio_event(&self) -> HubEvent {
        HubEvent::AudioChanged(self.settings.audio.clone().sanitized())
    }

    /// Flip "mute all" (the `M` hotkey) and persist it.
    pub fn toggle_mute(&mut self) -> HubEvent {
        self.settings.audio.mute = !self.settings.audio.mute;
        self.commit_audio()
    }
}

/// In-world actions panel, as plain data the runtime fills each frame.
pub struct ActionsView<'a> {
    /// The character's name and the engine it comes from (the panel's header).
    pub character: &'a str,
    pub engine: &'a str,
    pub status: &'a str,
    pub addons: &'a mut [(AddonDef, bool)],
    pub actions: &'a [ActionDef],
    /// Id of the idle action currently in use.
    pub selected_idle: &'a str,
    /// Id of the emote currently playing, if any.
    pub playing: Option<&'a str>,
    pub filter: &'a mut String,
}

/// Who a playing sound belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundSource {
    /// The character's own sounds (footsteps, voice, emote effects).
    Character,
    /// The world's sounds (music, placed ambience).
    World,
}

impl SoundSource {
    pub fn label(self) -> &'static str {
        match self {
            SoundSource::Character => "character",
            SoundSource::World => "world",
        }
    }
}

/// One line of the in-world sound monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundActivity {
    pub source: SoundSource,
    /// Engine that provided the sound (`ff14`, `ff11`).
    pub engine: String,
    pub category: SoundCategory,
    /// What is playing (file stem and entry).
    pub name: String,
    /// Why (surface / gait / condition, music slot, emitter distance).
    pub detail: String,
    /// Final linear level after category, master and mute.
    pub level: f32,
    /// Seconds since a one-shot ended (0 while playing); ended sounds linger dimmed.
    pub ended_for: f32,
}

/// The in-world sound monitor, as plain data the runtime fills each frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SoundMonitor {
    pub entries: Vec<SoundActivity>,
    /// Ground condition the footsteps use ("dry", "wet").
    pub ground: String,
    /// Which engine the character and the world come from.
    pub character_engine: String,
    pub world_engine: String,
    pub muted: bool,
}

#[derive(Debug, Clone)]
pub enum ActionEvent {
    Play(ActionDef),
    Stop,
}

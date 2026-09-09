//! FFLocal launcher: the small window that runs before the program. It shows which games
//! were found, lets the player point at installs or switch a game off, sets the basic graphics
//! options, and starts the runtime (`ffl-app`). From a source checkout it builds the runtime
//! first and says so, since that takes minutes the first time. Everything it changes goes into
//! the same `settings.toml` the program reads, so the in-app Games and Settings windows show
//! the same state.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use ffl_core::{InstallCandidate, Library};
use ffl_hub::Settings;

mod update;

/// What the background version check found, shown next to the FFLocal heading.
#[derive(Clone)]
enum UpdateState {
    Checking,
    UpToDate,
    Available(update::Update),
    Error(String),
}

/// Result of an in-flight download+apply, `None` until it finishes.
type UpdateApply = Arc<Mutex<Option<Result<(), String>>>>;

/// What the launcher knows about an engine's game: the same ids `ffl-app` registers.
struct GameDef {
    id: &'static str,
    name: &'static str,
    hint: &'static str,
    scan: fn() -> Vec<InstallCandidate>,
    /// Validate an explicit folder (or search when `None`); returns the install.
    check: fn(Option<&Path>) -> Result<PathBuf, String>,
}

fn check_ff14(path: Option<&Path>) -> Result<PathBuf, String> {
    ffl_ff14_assets::locate::locate_game(path).map(|g| g.game_dir).map_err(|e| format!("{e:#}"))
}

fn check_ff11(path: Option<&Path>) -> Result<PathBuf, String> {
    ffl_ff11::dat::locate(path).map_err(|e| format!("{e:#}"))
}

const GAMES: [GameDef; 2] = [
    GameDef {
        id: "ff14",
        name: "Final Fantasy XIV",
        hint: "the install folder (holding game/) or its game/ folder",
        scan: ffl_ff14_assets::locate::scan,
        check: check_ff14,
    },
    GameDef {
        id: ffl_ff11::ENGINE_ID,
        name: "Final Fantasy XI",
        hint: "the install folder holding VTABLE.DAT",
        scan: ffl_ff11::dat::scan,
        check: check_ff11,
    },
];

/// Result of checking a game's path.
enum Found {
    Yes(PathBuf),
    No(String),
}

struct GameRow {
    def: &'static GameDef,
    enabled: bool,
    path: String,
    /// The path text the status was computed for.
    checked: Option<String>,
    status: Found,
    candidates: Option<Vec<InstallCandidate>>,
    /// Companion plugin data found for the game (FFXIV: Dalamud's Penumbra and Glamourer).
    plugins: Option<String>,
}

impl GameRow {
    fn new(def: &'static GameDef, settings: &Settings) -> Self {
        let g = settings.games.get(def.id).cloned().unwrap_or_default();
        let plugins = (def.id == "ff14").then(|| {
            let explicit = settings.plugins_path(def.id);
            match ffl_ff14_assets::dalamud::Dalamud::locate(explicit.as_deref()) {
                Some(d) => d.summary(),
                None => "Dalamud plugin data: not found (Penumbra collections and Glamourer designs need XIVLauncher's pluginConfigs; set it in the program's Games window)".into(),
            }
        });
        Self { def, enabled: g.enabled, path: g.path, checked: None, status: Found::No(String::new()), candidates: None, plugins }
    }

    /// Re-run the locator when the path text changed.
    fn refresh(&mut self) {
        if self.checked.as_deref() == Some(self.path.as_str()) {
            return;
        }
        let trimmed = self.path.trim();
        let explicit = (!trimmed.is_empty()).then(|| PathBuf::from(trimmed));
        self.status = match (self.def.check)(explicit.as_deref()) {
            Ok(p) => Found::Yes(p),
            Err(e) => Found::No(e),
        };
        self.checked = Some(self.path.clone());
    }
}

/// Where the runtime is and how to start it.
enum Runtime {
    /// A source checkout: build with cargo (it is quick when nothing changed), then run.
    Source { workspace: PathBuf, binary: PathBuf },
    /// An installed program: `ffl-app` next to the launcher.
    Packaged(PathBuf),
    Missing,
}

fn runtime_name() -> &'static str {
    if cfg!(windows) { "ffl-app.exe" } else { "ffl-app" }
}

/// The workspace root above `start` (the checkout this launcher was built in).
fn workspace_above(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|d| d.join("Cargo.toml").is_file() && d.join("crates/ffl-app/Cargo.toml").is_file()).map(Path::to_path_buf)
}

fn find_runtime() -> Runtime {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let cwd = std::env::current_dir().ok();
    if let Some(ws) = exe_dir.as_deref().and_then(workspace_above).or_else(|| cwd.as_deref().and_then(workspace_above)) {
        let binary = ws.join("target/release").join(runtime_name());
        return Runtime::Source { workspace: ws, binary };
    }
    if let Some(dir) = exe_dir {
        let beside = dir.join(runtime_name());
        if beside.is_file() {
            return Runtime::Packaged(beside);
        }
    }
    Runtime::Missing
}

fn cargo_available() -> bool {
    Command::new("cargo").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// A build running on a thread; the window shows its last lines.
#[derive(Default)]
struct Build {
    lines: VecDeque<String>,
    finished: Option<Result<(), String>>,
}

struct Launcher {
    settings: Settings,
    games: Vec<GameRow>,
    runtime: Runtime,
    cargo: bool,
    build: Option<Arc<Mutex<Build>>>,
    /// Start the program once the build succeeds.
    launch_after_build: bool,
    message: String,
    saved_at: Option<std::time::Instant>,
    /// Verification aids (see `test_aids`).
    screenshot: Option<PathBuf>,
    screenshot_at: Duration,
    autolaunch: bool,
    started: std::time::Instant,
    shot_requested: bool,
    /// Background version check against the repo's tags (see `update.rs`).
    update: Arc<Mutex<UpdateState>>,
    /// Set while a download+apply is running; `Some(Ok(()))` once it lands.
    update_apply: Option<UpdateApply>,
}

impl Launcher {
    fn new() -> Self {
        let settings = Settings::load();
        let games = GAMES.iter().map(|d| GameRow::new(d, &settings)).collect();
        let runtime = find_runtime();
        let cargo = matches!(runtime, Runtime::Source { .. }) && cargo_available();
        let mut me = Self {
            settings,
            games,
            runtime,
            cargo,
            build: None,
            launch_after_build: false,
            message: String::new(),
            saved_at: None,
            screenshot: std::env::var_os("FFL_LAUNCHER_SCREENSHOT").map(PathBuf::from),
            screenshot_at: Duration::from_secs_f64(std::env::var("FFL_LAUNCHER_SCREENSHOT_AT").ok().and_then(|v| v.parse().ok()).unwrap_or(1.5)),
            autolaunch: std::env::var_os("FFL_LAUNCHER_AUTOLAUNCH").is_some(),
            started: std::time::Instant::now(),
            shot_requested: false,
            update: Arc::new(Mutex::new(UpdateState::Checking)),
            update_apply: None,
        };
        for g in &mut me.games {
            g.refresh();
        }
        if std::env::var_os("FFL_NO_UPDATE_CHECK").is_none() {
            let state = me.update.clone();
            std::thread::spawn(move || {
                let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("workspace version is valid semver");
                let result = match update::check(&current) {
                    Ok(Some(u)) => UpdateState::Available(u),
                    Ok(None) => UpdateState::UpToDate,
                    Err(err) => UpdateState::Error(err.to_string()),
                };
                *state.lock().unwrap() = result;
            });
        }
        me
    }

    /// Download and apply an update in the background; `poll_update` picks up the result.
    fn start_update(&mut self, update: update::Update, workspace: PathBuf) {
        let state = Arc::new(Mutex::new(None));
        self.update_apply = Some(state.clone());
        std::thread::spawn(move || {
            let result = update::apply(&update, &workspace).map_err(|e| e.to_string());
            *state.lock().unwrap() = Some(result);
        });
    }

    fn poll_update(&mut self, ctx: &egui::Context) {
        let Some(state) = self.update_apply.clone() else {
            return;
        };
        let result = state.lock().unwrap().clone();
        match result {
            None => ctx.request_repaint_after(Duration::from_millis(150)),
            Some(Ok(())) => {
                self.update_apply = None;
                *self.update.lock().unwrap() = UpdateState::UpToDate;
                self.message.clear();
                if let Runtime::Source { workspace, .. } = &self.runtime {
                    let ws = workspace.clone();
                    self.start_build(ws);
                }
            }
            Some(Err(err)) => {
                self.update_apply = None;
                self.message = format!("update failed: {err}");
            }
        }
    }

    fn update_line(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.update_apply.is_some() {
            ui.label(RichText::new("Downloading and applying the update…").color(Color32::YELLOW).size(11.0));
            return;
        }
        let snapshot = self.update.lock().unwrap().clone();
        match snapshot {
            UpdateState::Checking => {
                ui.label(RichText::new("checking for updates…").weak().size(10.0));
                ctx.request_repaint_after(Duration::from_millis(300));
            }
            UpdateState::UpToDate => {}
            UpdateState::Error(err) => {
                ui.label(RichText::new(format!("update check failed: {err}")).weak().size(10.0));
            }
            UpdateState::Available(u) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Update available: v{}", u.version)).color(Color32::LIGHT_GREEN).size(11.0));
                    if let Runtime::Source { workspace, .. } = &self.runtime {
                        let workspace = workspace.clone();
                        if ui.small_button("Download & apply").clicked() {
                            self.start_update(u.clone(), workspace);
                        }
                    } else {
                        ui.hyperlink_to("Download", "https://github.com/VirstaXIV/FFLocal/releases");
                    }
                });
            }
        }
    }

    /// Write the rows and options back to `settings.toml`.
    fn save(&mut self) -> bool {
        for g in &self.games {
            let e = self.settings.games.entry(g.def.id.to_string()).or_default();
            e.enabled = g.enabled;
            e.path = g.path.trim().to_string();
        }
        self.settings.graphics = self.settings.graphics.clone().sanitized();
        self.settings.audio = self.settings.audio.clone().sanitized();
        match self.settings.save() {
            Ok(()) => {
                self.saved_at = Some(std::time::Instant::now());
                true
            }
            Err(err) => {
                self.message = format!("could not save settings: {err:#}");
                false
            }
        }
    }

    /// Save, then start the runtime (building first in a source checkout).
    fn launch(&mut self, ctx: &egui::Context) {
        if !self.save() {
            return;
        }
        match &self.runtime {
            Runtime::Packaged(bin) => {
                let bin = bin.clone();
                self.start(&bin, ctx);
            }
            Runtime::Source { workspace, binary } => {
                if self.cargo {
                    self.launch_after_build = true;
                    self.start_build(workspace.clone());
                } else if binary.is_file() {
                    let bin = binary.clone();
                    self.start(&bin, ctx);
                } else {
                    self.message = "The program is not built yet and cargo was not found: install a Rust toolchain (https://rustup.rs) and start the launcher again.".into();
                }
            }
            Runtime::Missing => {
                self.message = format!("No {} next to the launcher and no source checkout above it.", runtime_name());
            }
        }
    }

    fn start(&mut self, binary: &Path, ctx: &egui::Context) {
        let dir = binary.parent().map(Path::to_path_buf).unwrap_or_default();
        let extra: Vec<String> = std::env::var("FFL_LAUNCHER_APP_ARGS").map(|v| v.split_whitespace().map(str::to_string).collect()).unwrap_or_default();
        match Command::new(binary).args(&extra).current_dir(dir).spawn() {
            Ok(child) => {
                eprintln!("started {} (pid {})", binary.display(), child.id());
                self.message = "FFLocal is starting…".into();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(err) => self.message = format!("could not start {}: {err}", binary.display()),
        }
    }

    /// `cargo build --release -p ffl-app` on a thread, stderr lines into the window.
    fn start_build(&mut self, workspace: PathBuf) {
        let state = Arc::new(Mutex::new(Build::default()));
        self.build = Some(state.clone());
        self.message.clear();
        std::thread::spawn(move || {
            let mut child = match Command::new("cargo").args(["build", "--release", "-p", "ffl-app"]).current_dir(&workspace).stdout(Stdio::null()).stderr(Stdio::piped()).spawn() {
                Ok(c) => c,
                Err(err) => {
                    state.lock().unwrap().finished = Some(Err(format!("could not run cargo: {err}")));
                    return;
                }
            };
            if let Some(err) = child.stderr.take() {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    let mut s = state.lock().unwrap();
                    s.lines.push_back(line);
                    while s.lines.len() > 200 {
                        s.lines.pop_front();
                    }
                }
            }
            let result = match child.wait() {
                Ok(status) if status.success() => Ok(()),
                Ok(status) => Err(format!("cargo build failed ({status}); the lines above say why")),
                Err(err) => Err(format!("cargo build: {err}")),
            };
            state.lock().unwrap().finished = Some(result);
        });
    }

    fn poll_build(&mut self, ctx: &egui::Context) {
        let Some(state) = self.build.clone() else {
            return;
        };
        let finished = state.lock().unwrap().finished.clone();
        match finished {
            None => ctx.request_repaint_after(Duration::from_millis(150)),
            Some(Ok(())) => {
                self.build = None;
                self.message = "Build finished.".into();
                if self.launch_after_build
                    && let Runtime::Source { binary, .. } = &self.runtime
                {
                    let bin = binary.clone();
                    self.launch_after_build = false;
                    self.start(&bin, ctx);
                }
            }
            Some(Err(err)) => {
                self.build = None;
                self.launch_after_build = false;
                self.message = err;
            }
        }
    }

    /// Verification aids, all environment variables: `FFL_LAUNCHER_AUTOLAUNCH=1` presses
    /// Launch after 1 s; `FFL_LAUNCHER_SCREENSHOT=<png>` saves the window at
    /// `FFL_LAUNCHER_SCREENSHOT_AT` seconds (1.5) and closes it, unless a build or launch is
    /// under way (then the window closes when that ends); `FFL_LAUNCHER_APP_ARGS` are passed
    /// to the runtime when it starts.
    fn test_aids(&mut self, ctx: &egui::Context) {
        if self.autolaunch && self.started.elapsed() > Duration::from_secs(1) {
            self.autolaunch = false;
            self.launch(ctx);
        }
        let Some(path) = self.screenshot.clone() else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(100));
        if !self.shot_requested && self.started.elapsed() > self.screenshot_at {
            self.shot_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            match image::RgbaImage::from_raw(w as u32, h as u32, image.as_raw().to_vec()).map(|img| img.save(&path)) {
                Some(Ok(())) => eprintln!("launcher screenshot: {}", path.display()),
                Some(Err(err)) => eprintln!("launcher screenshot failed: {err}"),
                None => eprintln!("launcher screenshot: bad image size"),
            }
            if self.build.is_none() && !self.launch_after_build {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn games_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Games");
        ui.small("Each game is read in place from its own install; nothing is copied. A game switched off is not opened at all: its characters, worlds and sounds stay hidden until it is on again.");
        for g in &mut self.games {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut g.enabled, "");
                let (color, dot) = match (g.enabled, &g.status) {
                    (false, _) => (Color32::GRAY, "○"),
                    (true, Found::Yes(_)) => (Color32::LIGHT_GREEN, "●"),
                    (true, Found::No(_)) => (Color32::LIGHT_RED, "○"),
                };
                ui.colored_label(color, dot);
                ui.strong(g.def.name);
                match (&g.status, g.enabled) {
                    (_, false) => ui.weak("off"),
                    (Found::Yes(_), true) => ui.weak("found"),
                    (Found::No(_), true) => ui.colored_label(Color32::LIGHT_RED, "not found"),
                };
            });
            if !g.enabled {
                continue;
            }
            match &g.status {
                Found::Yes(p) => ui.label(RichText::new(p.display().to_string()).weak().size(11.0)),
                Found::No(why) => ui.label(RichText::new(why).weak().size(11.0)),
            };
            if let Some(plugins) = &g.plugins {
                ui.label(RichText::new(plugins).weak().size(11.0));
            }
            ui.horizontal(|ui| {
                ui.label("Install");
                ui.add(egui::TextEdit::singleline(&mut g.path).desired_width(ui.available_width() - 170.0).hint_text(format!("empty = search; else {}", g.def.hint)));
                if ui.button("Browse…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_title(format!("{} install folder", g.def.name)).pick_folder()
                {
                    g.path = dir.display().to_string();
                }
                if ui.button("Scan").on_hover_text("look in the usual places").clicked() {
                    g.candidates = Some((g.def.scan)());
                }
            });
            if let Some(found) = &g.candidates {
                if found.is_empty() {
                    ui.weak("scan: nothing found in the usual places");
                }
                let mut pick = None;
                for c in found {
                    let path = c.path.display().to_string();
                    if ui.selectable_label(false, format!("{path}   ({})", c.source)).on_hover_text("use this install").clicked() {
                        pick = Some(path);
                    }
                }
                if let Some(p) = pick {
                    g.path = p;
                    g.candidates = None;
                }
            }
            g.refresh();
        }
    }

    fn options_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Graphics");
        let g = &mut self.settings.graphics;
        ui.horizontal(|ui| {
            ui.checkbox(&mut g.fullscreen, "Fullscreen");
            ui.checkbox(&mut g.vsync, "VSync");
            ui.checkbox(&mut g.shadows, "Shadows");
        });
        ui.horizontal(|ui| {
            ui.label("Anti-aliasing");
            egui::ComboBox::from_id_salt("msaa").selected_text(if g.msaa <= 1 { "off".to_string() } else { format!("{}×", g.msaa) }).show_ui(ui, |ui| {
                for (v, label) in [(1u8, "off"), (2, "2×"), (4, "4×"), (8, "8×")] {
                    ui.selectable_value(&mut g.msaa, v, label);
                }
            });
            ui.label("Shadow map");
            egui::ComboBox::from_id_salt("shadow_res").selected_text(g.shadow_resolution.to_string()).show_ui(ui, |ui| {
                for v in [1024u32, 2048, 4096] {
                    ui.selectable_value(&mut g.shadow_resolution, v, v.to_string());
                }
            });
            ui.label("Anisotropy");
            egui::ComboBox::from_id_salt("aniso").selected_text(if g.anisotropy <= 1 { "off".to_string() } else { format!("{}×", g.anisotropy) }).show_ui(ui, |ui| {
                for v in [1u8, 2, 4, 8, 16] {
                    ui.selectable_value(&mut g.anisotropy, v, if v <= 1 { "off".to_string() } else { format!("{v}×") });
                }
            });
        });
        ui.add(egui::Slider::new(&mut g.render_distance, 100.0..=5000.0).text("Render distance (m)").logarithmic(true));
        ui.add(egui::Slider::new(&mut g.detail_distance, 20.0..=1000.0).text("Detail distance").logarithmic(true));
        ui.add(egui::Slider::new(&mut g.lod_distance, 0.5..=8.0).text("Model detail (× LOD distance)").logarithmic(true));
        ui.add(egui::Slider::new(&mut g.ui_scale, 0.5..=2.0).text("UI scale"));
        ui.add_space(6.0);
        ui.heading("Audio");
        let a = &mut self.settings.audio;
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut a.master, 0.0..=1.0).text("Master volume"));
            ui.checkbox(&mut a.mute, "Mute");
        });
        ui.small("Everything else (volumes per category, mods, UI panels) is in the program's Settings.");
    }

    fn runtime_line(&self) -> (Color32, String) {
        match &self.runtime {
            Runtime::Packaged(p) => (Color32::LIGHT_GREEN, format!("program: {}", p.display())),
            Runtime::Source { workspace, binary } => {
                let built = binary.is_file();
                match (self.cargo, built) {
                    (true, true) => (Color32::LIGHT_GREEN, format!("source checkout {} — Launch rebuilds when the code changed", workspace.display())),
                    (true, false) => (Color32::YELLOW, format!("source checkout {} — not built yet: Launch builds it first (minutes the first time)", workspace.display())),
                    (false, true) => (Color32::LIGHT_GREEN, format!("program: {} (cargo not found, no rebuilds)", binary.display())),
                    (false, false) => (Color32::LIGHT_RED, "not built, and cargo was not found (install a Rust toolchain from https://rustup.rs)".into()),
                }
            }
            Runtime::Missing => (Color32::LIGHT_RED, format!("no {} next to the launcher", runtime_name())),
        }
    }
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.poll_build(ctx);
        self.poll_update(ctx);
        self.test_aids(ctx);
        egui::Frame::new().inner_margin(12.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("FFLocal");
                ui.weak(format!("v{}", env!("CARGO_PKG_VERSION")));
                ui.weak("worlds and characters from installed Final Fantasy games");
            });
            self.update_line(ui, ctx);
            let (color, line) = self.runtime_line();
            ui.label(RichText::new(line).color(color).size(11.0));
            ui.separator();
            // Room under the list for the buttons or the build status, plus the footer line.
            let reserve = if self.build.is_some() { 150.0 } else { 96.0 };
            egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(ui.available_height() - reserve).show(ui, |ui| {
                self.games_section(ui);
                ui.add_space(8.0);
                ui.separator();
                self.options_section(ui);
            });
            ui.separator();
            if let Some(build) = self.build.clone() {
                ui.label(RichText::new("Building FFLocal… this takes a few minutes the first time, seconds afterwards.").color(Color32::YELLOW));
                let b = build.lock().unwrap();
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    for line in b.lines.iter().rev().take(4).collect::<Vec<_>>().into_iter().rev() {
                        ui.label(RichText::new(line).monospace().size(10.0));
                    }
                    if b.lines.is_empty() {
                        ui.label(RichText::new("starting cargo…").monospace().size(10.0));
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    let any_game = self.games.iter().any(|g| g.enabled && matches!(g.status, Found::Yes(_)));
                    let launch = ui.add(egui::Button::new(RichText::new("Launch").strong()).min_size(egui::vec2(110.0, 28.0)));
                    if launch.clicked() {
                        self.launch(ctx);
                    }
                    if !any_game {
                        ui.label(RichText::new("no game found: FFLocal starts with its Games window").color(Color32::YELLOW).size(11.0));
                    }
                    if ui.button("Save").on_hover_text("write settings.toml without starting").clicked() {
                        self.save();
                    }
                    if let (true, Runtime::Source { workspace, .. }) = (self.cargo, &self.runtime) {
                        let ws = workspace.clone();
                        if ui.button("Build").on_hover_text("rebuild the program without starting it").clicked() {
                            self.start_build(ws);
                        }
                    }
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                if !self.message.is_empty() {
                    ui.label(RichText::new(&self.message).color(Color32::LIGHT_RED).size(11.0));
                } else if self.saved_at.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
                    ui.label(RichText::new("saved").weak().size(11.0));
                    ctx.request_repaint_after(Duration::from_millis(500));
                }
            }
            let data = Library::data_dir();
            ui.label(RichText::new(format!("settings: {}   log: {}", Settings::path().display(), data.join("fflocal.log").display())).weak().size(10.0));
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("FFLocal").with_inner_size([660.0, 580.0]).with_min_inner_size([560.0, 440.0]),
        ..Default::default()
    };
    eframe::run_native("FFLocal", options, Box::new(|_cc| Ok(Box::new(Launcher::new()))))
}

use crate::{launcher::{self, InstalledVersion, InstallProgress}, minecraft::{self, VersionEntry, VersionManifest}, profiles::ProfileStore};
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Stroke, Vec2};
use std::{fs, path::PathBuf, process::{Child, Command}, sync::mpsc::{self, Receiver, TryRecvError}};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct LauncherPreferences {
    memory_mb: u32,
    snapshots: bool,
}

impl Default for LauncherPreferences {
    fn default() -> Self {
        Self { memory_mb: 4096, snapshots: false }
    }
}

impl LauncherPreferences {
    fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("dev", "SilverDevStudios", "LUNAR")
            .map(|dirs| dirs.config_dir().join("preferences.json"))
    }

    fn load() -> Self {
        Self::path()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        if let Some(path) = Self::path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(bytes) = serde_json::to_vec_pretty(self) {
                let temporary = path.with_extension("tmp");
                if fs::write(&temporary, bytes).is_ok() {
                    let backup = path.with_extension("bak");
                    let had_previous = path.exists();
                    if had_previous {
                        let _ = fs::remove_file(&backup);
                        if fs::rename(&path, &backup).is_err() {
                            let _ = fs::remove_file(&temporary);
                            return;
                        }
                    }
                    if fs::rename(&temporary, &path).is_ok() {
                        if had_previous {
                            let _ = fs::remove_file(backup);
                        }
                    } else if had_previous {
                        let _ = fs::rename(backup, &path);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Home,
    Play,
    Versions,
    Profiles,
    Settings,
    Content,
}

pub struct LunarApp {
    profiles: ProfileStore,
    screen: Screen,
    username: String,
    rename_name: String,
    rename_target: Option<String>,
    manifest: Option<VersionManifest>,
    version_rx: Option<Receiver<Result<VersionManifest, String>>>,
    selected_version: String,
    version_filter: String,
    snapshots: bool,
    status: String,
    accent: Color32,
    game_root: PathBuf,
    installed_ids: std::collections::BTreeSet<String>,
    installed_version: Option<InstalledVersion>,
    install_rx: Option<Receiver<Result<InstalledVersion, String>>>,
    progress_rx: Option<Receiver<InstallProgress>>,
    install_progress: Option<InstallProgress>,
    java_status: String,
    memory_mb: u32,
    game_process: Option<Child>,
    content_kind: String,
    content_query: String,
    content_results: Vec<crate::content::Project>,
    content_busy: bool,
    content_search_rx: Option<Receiver<Result<Vec<crate::content::Project>, String>>>,
    content_install_rx: Option<Receiver<Result<String, String>>>,
}

impl LunarApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals.dark_mode = true;
        style.visuals.window_fill = Color32::from_rgb(17, 20, 28);
        style.visuals.panel_fill = Color32::from_rgb(13, 16, 23);
        style.visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(23, 27, 37);
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(29, 34, 46);
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(47, 57, 75);
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(77, 112, 180);
        style.spacing.item_spacing = Vec2::new(12.0, 12.0);
        cc.egui_ctx.set_style(style);

        let game_root = launcher::game_directory().unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(".lunar-data"));
        let installed_ids = launcher::installed_versions(&game_root).unwrap_or_default();
        let java_status = launcher::detect_java().unwrap_or_else(|error| error.to_string());
        let preferences = LauncherPreferences::load();
        let mut app = Self {
            profiles: ProfileStore::load(),
            screen: Screen::Home,
            username: String::new(),
            rename_name: String::new(),
            rename_target: None,
            manifest: None,
            version_rx: None,
            selected_version: String::new(),
            version_filter: String::new(),
            snapshots: preferences.snapshots,
            status: "Ready".into(),
            accent: Color32::from_rgb(137, 169, 232),
            game_root,
            installed_ids,
            installed_version: None,
            install_rx: None,
            progress_rx: None,
            install_progress: None,
            java_status,
            memory_mb: preferences.memory_mb.clamp(512, 16384),
            game_process: None,
            content_kind: "modpack".into(),
            content_query: String::new(),
            content_results: Vec::new(),
            content_busy: false,
            content_search_rx: None,
            content_install_rx: None,
        };
        app.refresh_versions();
        app
    }

    fn refresh_versions(&mut self) {
        if self.version_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.version_rx = Some(rx);
        self.status = "Fetching official Minecraft versions…".into();
        std::thread::spawn(move || {
            let result = minecraft::fetch_manifest().map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
    }

    fn start_install(&mut self) {
        if self.install_rx.is_some() || self.selected_version.is_empty() {
            return;
        }
        let version = self.selected_version.clone();
        let root = self.game_root.clone();
        self.installed_ids.remove(&version);
        if self.installed_version.as_ref().is_some_and(|installed| installed.id == version) {
            self.installed_version = None;
        }
        let (result_tx, result_rx) = mpsc::channel();
        let (progress_tx, progress_rx) = mpsc::channel();
        self.install_rx = Some(result_rx);
        self.progress_rx = Some(progress_rx);
        self.install_progress = Some(InstallProgress { phase: "Preparing".into(), completed: 0, total: 0 });
        self.status = format!("Installing Minecraft {version}…");
        std::thread::spawn(move || {
            let result = launcher::install(&version, &root, |progress| {
                let _ = progress_tx.send(progress);
            }).map_err(|error| format!("{error:#}"));
            let _ = result_tx.send(result);
        });
    }

    fn poll_launcher(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.progress_rx {
            while let Ok(progress) = rx.try_recv() {
                self.install_progress = Some(progress);
            }
        }
        let result = self.install_rx.as_ref().map(Receiver::try_recv);
        match result {
            Some(Ok(Ok(installed))) => {
                self.selected_version = installed.id.clone();
                self.installed_ids.insert(installed.id.clone());
                self.installed_version = Some(installed);
                self.install_rx = None;
                self.progress_rx = None;
                self.install_progress = None;
                self.status = "Minecraft installation complete".into();
            }
            Some(Ok(Err(error))) => {
                self.status = error;
                self.install_rx = None;
                self.progress_rx = None;
                self.install_progress = None;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.status = "Minecraft installation stopped unexpectedly".into();
                self.install_rx = None;
                self.progress_rx = None;
                self.install_progress = None;
            }
            _ => {}
        }
        let process_state = self.game_process.as_mut().map(Child::try_wait);
        match process_state {
            Some(Ok(Some(status))) => {
                self.status = format!("Minecraft exited with {status}");
                self.game_process = None;
            }
            Some(Ok(None)) => self.status = "Minecraft is running".into(),
            Some(Err(error)) => {
                self.status = format!("Could not read Minecraft process: {error}");
                self.game_process = None;
            }
            None => {}
        }
        if self.install_rx.is_some() || self.game_process.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn stop_game(&mut self) {
        if let Some(mut process) = self.game_process.take() {
            let _ = process.kill();
            let _ = process.wait();
            self.status = "Minecraft stopped".into();
        }
    }

    fn launch_selected(&mut self) {
        let Some(name) = self.profiles.selected.clone() else {
            self.status = "Create or select an offline profile first".into();
            return;
        };
        let Some(profile) = self.profiles.profiles.iter().find(|profile| profile.name == name) else {
            self.status = "Selected offline profile could not be found".into();
            return;
        };
        let installed = match self.installed_version.clone() {
            Some(installed) if installed.id == self.selected_version => installed,
            _ => match launcher::load_installed(&self.selected_version, &self.game_root) {
                Ok(installed) => installed,
                Err(error) => {
                    self.status = error.to_string();
                    return;
                }
            },
        };
        match launcher::launch(&installed, &profile.name, &profile.uuid, self.memory_mb) {
            Ok(process) => {
                self.installed_version = Some(installed);
                self.game_process = Some(process);
                self.status = "Minecraft is running".into();
            }
            Err(error) => self.status = format!("{error:#}"),
        }
    }

    fn poll_versions(&mut self, ctx: &egui::Context) {
        let result = self.version_rx.as_ref().map(|rx| rx.try_recv());
        match result {
            Some(Ok(Ok(manifest))) => {
                self.selected_version = manifest.latest.release.clone();
                self.manifest = Some(manifest);
                self.version_rx = None;
                self.status = "Version manifest updated".into();
            }
            Some(Ok(Err(error))) => {
                self.status = error;
                self.version_rx = None;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.status = "Version request stopped unexpectedly".into();
                self.version_rx = None;
            }
            _ => {}
        }
        if self.version_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
        ui.add_space(8.0);
        ui.label(RichText::new(title).font(FontId::proportional(30.0)).strong());
        ui.label(RichText::new(subtitle).color(Color32::from_rgb(145, 153, 171)).size(13.0));
        ui.add_space(20.0);
    }

    fn nav_button(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
        let text = if active {
            RichText::new(label).color(Color32::WHITE).strong()
        } else {
            RichText::new(label).color(Color32::from_rgb(157, 166, 185))
        };
        ui.add_sized([174.0, 38.0], egui::Button::new(text).fill(
            if active { Color32::from_rgb(43, 57, 82) } else { Color32::TRANSPARENT }
        ).stroke(Stroke::NONE))
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("lunar_navigation")
            .exact_width(206.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(Color32::from_rgb(12, 15, 22)).inner_margin(egui::Margin::same(18)))
            .show(ctx, |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("◒").size(27.0).color(self.accent).strong());
                    ui.label(RichText::new("LUNAR").size(21.0).strong());
                });
                ui.add_space(34.0);
                for (label, screen) in [
                    ("Home", Screen::Home),
                    ("Play", Screen::Play),
                    ("Versions", Screen::Versions),
                    ("Profiles", Screen::Profiles),
                    ("Content", Screen::Content),
                    ("Settings", Screen::Settings),
                ] {
                    if Self::nav_button(ui, label, self.screen == screen).clicked() {
                        self.screen = screen;
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.separator();
                    ui.label(RichText::new("OFFLINE PROFILES").size(9.0).color(Color32::GRAY).strong());
                    let selected = self.profiles.selected.as_deref().unwrap_or("No profile");
                    ui.label(RichText::new(selected).color(Color32::WHITE).strong());
                    ui.label(RichText::new("Local identity").size(11.0).color(Color32::GRAY));
                });
            });
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Welcome to LUNAR", "Your Minecraft setup, without the clutter.");
        egui::Frame::new()
            .fill(Color32::from_rgb(23, 31, 47))
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(53, 68, 94)))
            .corner_radius(egui::CornerRadius::same(18))
            .inner_margin(egui::Margin::same(28))
            .show(ui, |ui| {
                ui.set_min_height(220.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("MINECRAFT JAVA EDITION").size(10.0).strong().color(self.accent));
                    ui.add_space(10.0);
                    ui.label(RichText::new("A world of your own.").size(31.0).strong());
                    ui.label(RichText::new("A faster, cleaner way to get into the game.").color(Color32::from_rgb(174, 186, 208)));
                    ui.add_space(26.0);
                    if ui.add_sized([170.0, 44.0], egui::Button::new(RichText::new("Open Play").strong()).fill(Color32::from_rgb(78, 111, 171))).clicked() {
                        self.screen = Screen::Play;
                    }
                });
            });
        ui.add_space(20.0);
        let card_style = ui.style().clone();
        ui.columns(2, |columns| {
            egui::Frame::group(&card_style).show(&mut columns[0], |ui| {
                ui.label(RichText::new("ACTIVE PROFILE").size(10.0).color(Color32::GRAY).strong());
                ui.add_space(8.0);
                ui.label(RichText::new(self.profiles.selected.as_deref().unwrap_or("No profile selected")).size(20.0).strong());
                ui.label(RichText::new("Offline identity").size(12.0).color(Color32::GRAY));
                if ui.button("Manage profiles").clicked() { self.screen = Screen::Profiles; }
            });
            egui::Frame::group(&card_style).show(&mut columns[1], |ui| {
                ui.label(RichText::new("LATEST RELEASE").size(10.0).color(Color32::GRAY).strong());
                ui.add_space(8.0);
                let latest = self.manifest.as_ref().map(|m| m.latest.release.as_str()).unwrap_or("Not loaded");
                ui.label(RichText::new(latest).size(20.0).strong());
                ui.label(RichText::new("Official Mojang manifest").size(12.0).color(Color32::GRAY));
                if ui.button("Check versions").clicked() { self.refresh_versions(); }
            });
        });
    }

    fn play(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Play", "Choose an offline profile, select a version and launch.");
        ui.group(|ui| {
            ui.label(RichText::new("PLAYER").size(10.0).color(Color32::GRAY).strong());
            ui.add_space(6.0);
            let previous_profile = self.profiles.selected.clone();
            egui::ComboBox::from_id_salt("play_profile")
                .selected_text(self.profiles.selected.as_deref().unwrap_or("Select offline profile"))
                .show_ui(ui, |ui| {
                    for profile in &self.profiles.profiles {
                        ui.selectable_value(&mut self.profiles.selected, Some(profile.name.clone()), &profile.name);
                    }
                });
            if previous_profile != self.profiles.selected
                && let Some(name) = self.profiles.selected.clone()
                && let Err(error) = self.profiles.select(&name)
            {
                self.status = error.to_string();
            }
            if ui.button("Manage profiles").clicked() {
                self.screen = Screen::Profiles;
            }
        });
        ui.add_space(12.0);
        ui.group(|ui| {
            ui.label(RichText::new("GAME VERSION").size(10.0).color(Color32::GRAY).strong());
            ui.add_space(6.0);
            if let Some(manifest) = &self.manifest {
                egui::ComboBox::from_id_salt("play_version")
                    .selected_text(if self.selected_version.is_empty() { "Select Minecraft version" } else { &self.selected_version })
                    .show_ui(ui, |ui| {
                        for version in manifest.versions.iter().filter(|entry| entry.kind == "release" || self.snapshots) {
                            ui.selectable_value(&mut self.selected_version, version.id.clone(), &version.id);
                        }
                    });
            } else {
                ui.label("Loading official version list…");
            }
            let installed = self.installed_ids.contains(&self.selected_version);
            ui.horizontal(|ui| {
                if self.install_rx.is_some() {
                    ui.add_enabled(false, egui::Button::new("Installing…"));
                } else if self.game_process.is_some() {
                    if ui.button("Stop Minecraft").clicked() {
                        self.stop_game();
                    }
                } else if installed {
                    if ui.add_sized([150.0, 38.0], egui::Button::new(RichText::new("Launch Minecraft").strong()).fill(Color32::from_rgb(78, 111, 171))).clicked() {
                        self.launch_selected();
                    }
                    if ui.button("Reinstall").clicked() {
                        self.start_install();
                    }
                } else if ui.add_sized([150.0, 38.0], egui::Button::new(RichText::new("Install").strong()).fill(Color32::from_rgb(78, 111, 171))).clicked() {
                    self.start_install();
                }
                if ui.button("Refresh versions").clicked() {
                    self.refresh_versions();
                }
            });
            if let Some(progress) = &self.install_progress {
                let fraction = if progress.total == 0 { 0.0 } else { progress.completed as f32 / progress.total as f32 };
                ui.add(egui::ProgressBar::new(fraction).text(format!("{}  {}/{}", progress.phase, progress.completed, progress.total)));
            }
        });
        ui.add_space(12.0);
        ui.group(|ui| {
            ui.label(RichText::new("JAVA RUNTIME").size(10.0).color(Color32::GRAY).strong());
            ui.label(&self.java_status);
            ui.label(RichText::new(format!("Game data: {}", self.game_root.display())).size(10.0).color(Color32::GRAY));
        });
    }

    fn versions(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Versions", "Official Minecraft Java Edition release metadata.");
        ui.horizontal(|ui| {
            ui.add_sized([260.0, 30.0], egui::TextEdit::singleline(&mut self.version_filter).hint_text("Search every version"));
            if ui.button("Refresh manifest").clicked() { self.refresh_versions(); }
            let previous_snapshots = self.snapshots;
            ui.checkbox(&mut self.snapshots, "Include snapshots");
            if previous_snapshots != self.snapshots {
                LauncherPreferences { memory_mb: self.memory_mb, snapshots: self.snapshots }.save();
            }
        });
        if let Some(manifest) = &self.manifest {
            ui.label(format!("Latest release: {}   ·   Latest snapshot: {}", manifest.latest.release, manifest.latest.snapshot));
            egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
                let versions: Vec<VersionEntry> = manifest.versions.iter()
                    .filter(|entry| entry.kind == "release" || self.snapshots)
                    .filter(|entry| self.version_filter.is_empty() || entry.id.to_lowercase().contains(&self.version_filter.to_lowercase()))
                    .cloned().collect();
                for version in versions {
                    ui.horizontal(|ui| {
                        let selected = self.selected_version == version.id;
                        if ui.selectable_label(selected, RichText::new(&version.id).strong()).clicked() {
                            self.selected_version = version.id.clone();
                        }
                        ui.label(RichText::new(&version.kind).size(11.0).color(Color32::GRAY));
                        if self.installed_ids.contains(&version.id) {
                            ui.label(RichText::new("Installed").size(10.0).color(self.accent));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if self.installed_ids.contains(&version.id) && ui.small_button("Remove").clicked() {
                                match launcher::remove_version(&version.id, &self.game_root) {
                                    Ok(()) => {
                                        self.installed_ids.remove(&version.id);
                                        if self.installed_version.as_ref().is_some_and(|installed| installed.id == version.id) {
                                            self.installed_version = None;
                                        }
                                        self.status = format!("Removed Minecraft {}", version.id);
                                    }
                                    Err(error) => self.status = error.to_string(),
                                }
                            }
                            ui.label(RichText::new(version.release_time.get(..10).unwrap_or("")).size(11.0).color(Color32::GRAY));
                        });
                    });
                    ui.separator();
                }
            });
        } else {
            ui.label("The version manifest has not been loaded.");
        }
    }

    fn profiles(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Profiles", "Local offline identities. These do not authenticate with Minecraft services.");
        ui.group(|ui| {
            ui.label(RichText::new("CREATE PROFILE").size(10.0).color(Color32::GRAY).strong());
            ui.horizontal(|ui| {
                ui.add_sized([240.0, 32.0], egui::TextEdit::singleline(&mut self.username).hint_text("Username"));
                if ui.button("Create").clicked() {
                    match self.profiles.add(&self.username) {
                        Ok(()) => { self.username.clear(); self.status = "Profile created".into(); }
                        Err(error) => self.status = error.to_string(),
                    }
                }
            });
        });
        ui.add_space(12.0);
        let profiles = self.profiles.profiles.clone();
        for profile in profiles {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&profile.name).strong().size(16.0));
                        ui.label(RichText::new(&profile.uuid).size(10.0).color(Color32::GRAY).monospace());
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Delete").clicked()
                            && let Err(error) = self.profiles.delete(&profile.name)
                        {
                            self.status = error.to_string();
                        }
                        if ui.button("Rename").clicked() {
                            self.rename_target = Some(profile.name.clone());
                            self.rename_name = profile.name.clone();
                        }
                        let active = self.profiles.selected.as_deref() == Some(&profile.name);
                        if !active && ui.button("Select").clicked() {
                            if let Err(error) = self.profiles.select(&profile.name) { self.status = error.to_string(); }
                        } else if active {
                            ui.label(RichText::new("Selected").color(self.accent).strong());
                        }
                    });
                });
            });
        }
        if let Some(old_name) = self.rename_target.clone() {
            egui::Window::new("Rename profile").collapsible(false).resizable(false).show(ui.ctx(), |ui| {
                ui.text_edit_singleline(&mut self.rename_name);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        match self.profiles.rename(&old_name, &self.rename_name) {
                            Ok(()) => { self.rename_target = None; self.status = "Profile renamed".into(); }
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                    if ui.button("Cancel").clicked() { self.rename_target = None; }
                });
            });
        }
    }


    fn content(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Content", "Discover and install Minecraft content through Modrinth.");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.content_kind, "modpack".into(), "Modpacks");
            ui.selectable_value(&mut self.content_kind, "resourcepack".into(), "Resource packs");
        });
        ui.horizontal(|ui| {
            ui.add_sized([360.0, 32.0], egui::TextEdit::singleline(&mut self.content_query).hint_text("Search Modrinth"));
            if ui.add_enabled(!self.content_busy, egui::Button::new("Search")).clicked() {
                let query = self.content_query.clone();
                let kind = self.content_kind.clone();
                let (tx, rx) = mpsc::channel();
                self.content_search_rx = Some(rx);
                self.content_busy = true;
                self.status = "Searching Modrinth…".into();
                std::thread::spawn(move || { let _ = tx.send(crate::content::search(&query, &kind).map_err(|error| error.to_string())); });
            }
        });
        if self.content_kind == "modpack" {
            ui.label(RichText::new("Modpacks install into separate instance folders with mods, configs and overrides downloaded automatically. Loader launch integration remains pending.").size(11.0).color(Color32::GRAY));
        } else {
            ui.label(RichText::new("Resource packs install directly into Minecraft's resourcepacks folder.").size(11.0).color(Color32::GRAY));
        }
        egui::ScrollArea::vertical().max_height(430.0).show(ui, |ui| {
            for project in self.content_results.clone() {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&project.title).strong().size(15.0));
                            ui.label(RichText::new(format!("{} downloads · {}", project.downloads, project.slug)).size(10.0).color(Color32::GRAY));
                            ui.label(RichText::new(&project.description).size(11.0).color(Color32::GRAY));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add_enabled(!self.content_busy, egui::Button::new("Install")).clicked() {
                                let id = project.project_id.clone();
                                let root = self.game_root.clone();
                                let kind = self.content_kind.clone();
                                let (tx, rx) = mpsc::channel();
                                self.content_install_rx = Some(rx);
                                self.content_busy = true;
                                self.status = format!("Installing {}…", project.title);
                                std::thread::spawn(move || {
                                    let result = if kind == "resourcepack" { crate::content::install_resource_pack(&id, &root) } else { crate::content::install_modpack(&id, &root) }.map_err(|error| format!("{error:#}"));
                                    let _ = tx.send(result);
                                });
                            }
                        });
                    });
                });
            }
        });
        if self.content_busy { ui.spinner(); }
    }
\n

    fn settings(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Settings", "Launcher preferences.");
        let previous_memory = self.memory_mb;
        ui.group(|ui| {
            ui.label(RichText::new("IDENTITY").strong());
            ui.label("Authentication mode: Offline profiles only");
            ui.label("Online account authentication is not included.");
        });
        ui.add_space(10.0);
        ui.group(|ui| {
            ui.label(RichText::new("JAVA RUNTIME").strong());
            ui.label(&self.java_status);
            if ui.button("Detect Java again").clicked() {
                self.java_status = launcher::detect_java().unwrap_or_else(|error| error.to_string());
            }
        });
        ui.add_space(10.0);
        ui.group(|ui| {
            ui.label(RichText::new("MEMORY ALLOCATION").strong());
            ui.add(egui::Slider::new(&mut self.memory_mb, 512..=16384).step_by(512.0).suffix(" MB"));
        });
        ui.add_space(10.0);
        if previous_memory != self.memory_mb {
            LauncherPreferences { memory_mb: self.memory_mb, snapshots: self.snapshots }.save();
        }
        ui.group(|ui| {
            ui.label(RichText::new("GAME DIRECTORY").strong());
            ui.label(self.game_root.display().to_string());
            ui.label("Minecraft versions, libraries, assets and logs are stored locally.");
            if ui.button("Open game directory").clicked() {
                let result = if cfg!(target_os = "windows") {
                    Command::new("explorer").arg(&self.game_root).spawn()
                } else if cfg!(target_os = "macos") {
                    Command::new("open").arg(&self.game_root).spawn()
                } else {
                    Command::new("xdg-open").arg(&self.game_root).spawn()
                };
                self.status = result.map(|_| "Opened game directory".to_owned()).unwrap_or_else(|error| format!("Could not open game directory: {error}"));
            }
            if ui.button("Open game log").clicked() {
                let log = self.game_root.join("logs").join("lunar-game.log");
                let result = if cfg!(target_os = "windows") {
                    Command::new("notepad").arg(&log).spawn()
                } else if cfg!(target_os = "macos") {
                    Command::new("open").arg(&log).spawn()
                } else {
                    Command::new("xdg-open").arg(&log).spawn()
                };
                self.status = result.map(|_| "Opened game log".to_owned()).unwrap_or_else(|error| format!("Could not open game log: {error}"));
            }
        });
    }
}

impl eframe::App for LunarApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_versions(ctx);
        self.poll_launcher(ctx);
        if let Some(rx) = &self.content_search_rx {
            if let Ok(result) = rx.try_recv() {
                match result { Ok(items) => { self.content_results = items; self.status = format!("Found {} projects", self.content_results.len()); }, Err(error) => self.status = error }
                self.content_search_rx = None;
                self.content_busy = false;
            }
        }
        if let Some(rx) = &self.content_install_rx {
            if let Ok(result) = rx.try_recv() {
                match result { Ok(path) => self.status = format!("Installed to {path}"), Err(error) => self.status = error }
                self.content_install_rx = None;
                self.content_busy = false;
            }
        }
        if self.content_search_rx.is_some() || self.content_install_rx.is_some() { ctx.request_repaint_after(std::time::Duration::from_millis(150)); }
        self.sidebar(ctx);
        egui::TopBottomPanel::bottom("status_bar")
            .frame(egui::Frame::new().fill(Color32::from_rgb(12, 15, 22)).inner_margin(egui::Margin::symmetric(18, 8)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(self.accent).size(9.0));
                    ui.label(RichText::new(&self.status).size(11.0).color(Color32::from_rgb(157, 166, 185)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new("LUNAR 0.1.0").size(10.0).color(Color32::GRAY));
                    });
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::from_rgb(17, 20, 28)).inner_margin(egui::Margin::same(30)))
            .show(ctx, |ui| {
                match self.screen {
                    Screen::Home => self.home(ui),
                    Screen::Play => self.play(ui),
                    Screen::Versions => self.versions(ui),
                    Screen::Profiles => self.profiles(ui),
                    Screen::Content => self.content(ui),
                    Screen::Settings => self.settings(ui),
                }
            });
    }
}

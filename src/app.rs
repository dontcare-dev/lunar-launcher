use anyhow::Context;
use crate::{launcher::{self, InstalledVersion, InstallProgress}, minecraft::{self, VersionEntry, VersionManifest}, profiles::ProfileStore};
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Stroke, Vec2};
use image::{AnimationDecoder, codecs::gif::GifDecoder, imageops::FilterType};
use std::{fs, io::BufReader, path::PathBuf, process::{Child, Command}, sync::{mpsc::{self, Receiver, TryRecvError}}, time::{Duration, Instant}};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct LauncherPreferences {
    memory_mb: u32,
    snapshots: bool,
    background_gif: String,
    performance_mode: String,
    auto_sodium: bool,
    animations_enabled: bool,
    accent_rgb: [u8; 3],
}

impl Default for LauncherPreferences {
    fn default() -> Self {
        Self { memory_mb: 4096, snapshots: false, background_gif: String::new(), performance_mode: "Performance".into(), auto_sodium: true, animations_enabled: true, accent_rgb: [151, 169, 255] }
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
    Servers,
    Workspace,
}

type ServerPingResults = Vec<(String, Result<crate::server::ServerStatus, String>)>;
type BackgroundFrames = Vec<(egui::ColorImage, u64)>;
type BackgroundLoadResult = Result<BackgroundFrames, String>;
type NavigationItem = (&'static str, Screen, Option<&'static str>, &'static str, &'static [u8]);

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
    performance_mode: String,
    auto_sodium: bool,
    animations_enabled: bool,
    startup_at: Instant,
    splash_complete: bool,
    game_process: Option<Child>,
    content_kind: String,
    content_results_kind: String,
    content_search_kind: String,
    content_query: String,
    content_results: Vec<crate::content::Project>,
    content_images: std::collections::BTreeMap<String, egui::TextureHandle>,
    content_busy: bool,
    content_search_rx: Option<Receiver<Result<Vec<crate::content::Project>, String>>>,
    content_install_rx: Option<Receiver<Result<String, String>>>,
    java_install_rx: Option<Receiver<Result<String, String>>>,
    content_loader: String,
    installed_packs: Vec<crate::content::InstalledPack>,
    modpack_launch_rx: Option<Receiver<Result<InstalledVersion, String>>>,
    servers: Vec<crate::server::SavedServer>,
    server_name: String,
    server_address: String,
    server_statuses: std::collections::BTreeMap<String, crate::server::ServerStatus>,
    server_rx: Option<Receiver<ServerPingResults>>,
    background_gif: String,
    background_frames: Vec<(egui::ColorImage, u64)>,
    background_texture: Option<egui::TextureHandle>,
    background_frame: usize,
    background_next: Instant,
    background_rx: Option<Receiver<BackgroundLoadResult>>,
    workspace_instances: Vec<crate::workspace::Instance>,
    workspace_selected: Option<String>,
    workspace_new_name: String,
    workspace_search: String,
    workspace_results: Vec<String>,
    workspace_section: String,
    workspace_preview: Option<String>,
    screenshot_textures: std::collections::BTreeMap<String, egui::TextureHandle>,
    download_manager: crate::downloads::DownloadManager,
    download_url: String,
    download_folder: String,
    system_monitor: sysinfo::System,
    last_metrics_refresh: Instant,
}

impl LunarApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals.dark_mode = true;
        style.visuals.window_fill = Color32::from_rgb(5, 8, 16);
        style.visuals.panel_fill = Color32::from_rgba_unmultiplied(10, 14, 25, 168);
        style.visuals.widgets.noninteractive.bg_fill = Color32::from_rgba_unmultiplied(20, 27, 43, 148);
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgba_unmultiplied(29, 37, 57, 168);
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgba_unmultiplied(53, 69, 103, 205);
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(91, 116, 190);
        style.visuals.window_corner_radius = egui::CornerRadius::same(16);
        style.visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(10);
        style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(10);
        style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(10);
        style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(10);
        style.visuals.selection.bg_fill = Color32::from_rgb(75, 98, 160);
        style.spacing.button_padding = Vec2::new(15.0, 9.0);
        style.spacing.item_spacing = Vec2::new(12.0, 12.0);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        cc.egui_ctx.set_style(style);

        let game_root = launcher::game_directory().unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(".lunar-data"));
        let installed_ids = launcher::installed_versions(&game_root).unwrap_or_default();
        let java_status = launcher::detect_java().unwrap_or_else(|_| "No system Java found. Lunar will download the compatible runtime automatically when Minecraft launches.".into());
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
            accent: Color32::from_rgb(preferences.accent_rgb[0], preferences.accent_rgb[1], preferences.accent_rgb[2]),
            game_root: game_root.clone(),
            installed_ids,
            installed_version: None,
            install_rx: None,
            progress_rx: None,
            install_progress: None,
            java_status,
            memory_mb: preferences.memory_mb.clamp(2048, 8192),
            performance_mode: preferences.performance_mode.clone(),
            auto_sodium: preferences.auto_sodium,
            animations_enabled: preferences.animations_enabled,
            startup_at: Instant::now(),
            splash_complete: false,
            game_process: None,
            content_kind: "modpack".into(),
            content_results_kind: "modpack".into(),
            content_search_kind: "modpack".into(),
            content_query: String::new(),
            content_results: Vec::new(),
            content_images: std::collections::BTreeMap::new(),
            content_busy: false,
            content_search_rx: None,
            content_install_rx: None,
            java_install_rx: None,
            content_loader: "fabric".into(),
            installed_packs: crate::content::installed_modpacks(&game_root).unwrap_or_default(),
            modpack_launch_rx: None,
            servers: crate::server::load(),
            server_name: String::new(),
            server_address: String::new(),
            server_statuses: std::collections::BTreeMap::new(),
            server_rx: None,
            background_gif: preferences.background_gif.clone(),
            background_frames: Vec::new(),
            background_texture: None,
            background_frame: 0,
            background_next: Instant::now(),
            background_rx: None,
            workspace_instances: crate::workspace::list_instances(&game_root).unwrap_or_default(),
            workspace_selected: None,
            workspace_new_name: String::new(),
            workspace_search: String::new(),
            workspace_results: Vec::new(),
            workspace_section: "Overview".into(),
            workspace_preview: None,
            screenshot_textures: std::collections::BTreeMap::new(),
            download_manager: crate::downloads::DownloadManager::default(),
            download_url: String::new(),
            download_folder: "downloads".into(),
            system_monitor: sysinfo::System::new_all(),
            last_metrics_refresh: Instant::now(),
        };
        app.refresh_versions();
        let (content_tx, content_rx) = mpsc::channel();
        app.content_search_rx = Some(content_rx);
        app.content_busy = true;
        app.content_search_kind = "modpack".into();
        std::thread::spawn(move || {
            let _ = content_tx.send(crate::content::search("", "modpack").map_err(|error| error.to_string()));
        });
        if !app.background_gif.is_empty() {
            let path = app.background_gif.clone();
            app.load_background_gif(path);
        }
        app
    }

    fn choose_gif_file() -> Option<String> {
        let output = if cfg!(target_os = "windows") {
            let script = "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Filter = 'GIF images (*.gif)|*.gif'; $d.Multiselect = $false; if ($d.ShowDialog() -eq 'OK') { [Console]::Write($d.FileName) }";
            Command::new("powershell").args(["-NoProfile", "-Command", script]).output().ok()?
        } else if cfg!(target_os = "macos") {
            Command::new("osascript").args(["-e", "POSIX path of (choose file of type {\"com.compuserve.gif\"})"]).output().ok()?
        } else {
            Command::new("zenity").args(["--file-selection", "--title=Choose Lunar Launcher background GIF", "--file-filter=GIF images | *.gif"]).output()
                .or_else(|_| Command::new("kdialog").args(["--getopenfilename", ".", "*.gif"]).output()).ok()?
        };
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (!path.is_empty()).then_some(path)
    }

    fn load_background_gif(&mut self, path: String) {
        self.background_gif = path.clone();
        LauncherPreferences {
            memory_mb: self.memory_mb,
            snapshots: self.snapshots,
            background_gif: self.background_gif.clone(),
            performance_mode: self.performance_mode.clone(),
            auto_sodium: self.auto_sodium, animations_enabled: self.animations_enabled, accent_rgb: [self.accent.r(), self.accent.g(), self.accent.b()],
        }.save();
        let (tx, rx) = mpsc::channel();
        self.background_rx = Some(rx);
        self.status = "Loading animated background…".into();
        std::thread::spawn(move || {
            let result = (|| -> Result<Vec<(egui::ColorImage, u64)>, String> {
                let metadata = fs::metadata(&path).map_err(|error| error.to_string())?;
                if metadata.len() > 50 * 1024 * 1024 {
                    return Err("GIF files must be 50 MB or smaller".into());
                }
                let file = fs::File::open(&path).map_err(|error| error.to_string())?;
                let decoder = GifDecoder::new(BufReader::new(file)).map_err(|error| error.to_string())?;
                let decoded = decoder.into_frames().take(48).collect::<std::result::Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
                if decoded.is_empty() {
                    return Err("The selected GIF contains no frames".into());
                }
                let mut frames = Vec::with_capacity(decoded.len());
                for frame in decoded {
                    let delay = frame.delay().numer_denom_ms();
                    let delay_ms = ((delay.0 as f64 / delay.1.max(1) as f64).round() as u64).clamp(20, 1200);
                    let mut image = frame.into_buffer();
                    let (width, height) = image.dimensions();
                    let scale = (960.0_f32 / width as f32).min(540.0_f32 / height as f32).min(1.0);
                    if scale < 1.0 {
                        image = image::imageops::resize(
                            &image,
                            (width as f32 * scale) as u32,
                            (height as f32 * scale) as u32,
                            FilterType::Triangle,
                        );
                    }
                    let image = image::imageops::blur(&image, 5.5);
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [image.width() as usize, image.height() as usize],
                        image.as_raw(),
                    );
                    frames.push((color, delay_ms));
                }
                Ok(frames)
            })();
            let _ = tx.send(result);
        });
    }

    fn poll_background(&mut self, ctx: &egui::Context) {
        let result = self.background_rx.as_ref().map(Receiver::try_recv);
        match result {
            Some(Ok(Ok(frames))) => {
                self.background_frames = frames;
                self.background_frame = 0;
                if let Some((image, delay)) = self.background_frames.first() {
                    self.background_texture = Some(ctx.load_texture(
                        "lunar-animated-background",
                        image.clone(),
                        egui::TextureOptions::LINEAR,
                    ));
                    self.background_next = Instant::now() + Duration::from_millis(*delay);
                }
                self.background_rx = None;
                self.status = "Animated background applied".into();
            }
            Some(Ok(Err(error))) => {
                self.background_rx = None;
                self.status = format!("Could not load GIF: {error}");
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.background_rx = None;
                self.status = "GIF loading stopped unexpectedly".into();
            }
            _ => {}
        }
        if self.game_process.is_some() {
            return;
        }
        if !self.background_frames.is_empty() {
            if Instant::now() >= self.background_next {
                self.background_frame = (self.background_frame + 1) % self.background_frames.len();
                if let Some((image, delay)) = self.background_frames.get(self.background_frame) {
                    if let Some(texture) = &mut self.background_texture {
                        texture.set(image.clone(), egui::TextureOptions::LINEAR);
                    }
                    self.background_next = Instant::now() + Duration::from_millis(*delay);
                }
            }
            let until_next_frame = self.background_next.saturating_duration_since(Instant::now());
            ctx.request_repaint_after(until_next_frame.max(Duration::from_millis(1)));
        }
        if self.background_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn paint_background(&self, ctx: &egui::Context) {
        let time = ctx.input(|input| input.time);
        if self.animations_enabled {
            let rect = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::background());
            for i in 0..34 {
                let seed = i as f64;
                let x = rect.left() + ((seed * 73.7).sin() * 0.5 + 0.5) as f32 * rect.width();
                let y = rect.top() + (((seed * 1.71 + time * (0.035 + (i % 4) as f64 * 0.009)).cos() * 0.5 + 0.5) as f32) * rect.height();
                let pulse = ((time * 0.8 + seed).sin() * 0.5 + 0.5) as f32;
                painter.circle_filled(egui::pos2(x, y), 0.8 + pulse * 1.5, Color32::from_rgba_unmultiplied(174, 194, 255, (22.0 + pulse * 65.0) as u8));
            }
            ctx.request_repaint_after(Duration::from_millis(33));
        }
        let rect = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::background());
        if let Some(texture) = &self.background_texture {
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0_f32, 1.0_f32)),
                Color32::WHITE,
            );
        }
        painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(4, 7, 15, 158));
        painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(24, 30, 53, 34));
    }

    fn svg_icon(ui: &mut egui::Ui, uri: &'static str, data: &'static [u8], size: f32) {
        ui.add(
            egui::Image::from_bytes(uri, data.to_vec())
                .fit_to_exact_size(Vec2::splat(size)),
        );
    }

    fn paint_splash(&mut self, ctx: &egui::Context) {
        if self.splash_complete {
            return;
        }
        let elapsed = self.startup_at.elapsed().as_secs_f32();
        if elapsed >= 2.65 {
            self.splash_complete = true;
            return;
        }
        let rect = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("lunar-splash")));
        painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(4, 7, 16, 246));
        let center = rect.center();
        let pulse = (elapsed * 2.4).sin() * 0.5 + 0.5;
        let reveal = (elapsed / 0.75).clamp(0.0, 1.0);
        painter.circle_filled(center, 54.0 + pulse * 8.0, Color32::from_rgba_unmultiplied(111, 139, 255, 22));
        painter.circle_stroke(center, 72.0 + pulse * 5.0, Stroke::new(1.2_f32, Color32::from_rgba_unmultiplied(145, 169, 255, 70)));
        painter.circle_stroke(center, 91.0 + pulse * 7.0, Stroke::new(0.8_f32, Color32::from_rgba_unmultiplied(145, 169, 255, 35)));
        painter.circle_filled(center + egui::vec2((elapsed * 0.8).cos() * 78.0, (elapsed * 0.8).sin() * 78.0), 3.0, Color32::from_rgb(190, 207, 255));
        let moon = center + egui::vec2(-15.0, -18.0);
        painter.circle_filled(moon, 36.0, Color32::from_rgb(174, 195, 255));
        painter.circle_filled(moon + egui::vec2(18.0, -12.0), 32.0, Color32::from_rgb(7, 11, 23));
        painter.text(center + egui::vec2(0.0, 116.0), egui::Align2::CENTER_CENTER, "Lunar Launcher", FontId::proportional(23.0), Color32::WHITE);
        painter.text(center + egui::vec2(0.0, 148.0), egui::Align2::CENTER_CENTER, "A better way to play", FontId::proportional(12.0), Color32::from_rgb(151, 165, 195));
        let bar = egui::Rect::from_center_size(center + egui::vec2(0.0, 190.0), egui::vec2(160.0, 3.0));
        painter.rect_filled(bar, 2.0, Color32::from_rgb(33, 42, 66));
        painter.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * reveal, bar.height())), 2.0, Color32::from_rgb(142, 165, 255));
        ctx.request_repaint_after(Duration::from_millis(16));
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
                self.content_busy = false;
                self.status = "Minecraft installation complete".into();
            }
            Some(Ok(Err(error))) => {
                self.status = error;
                self.install_rx = None;
                self.progress_rx = None;
                self.install_progress = None;
                self.content_busy = false;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.status = "Minecraft installation stopped unexpectedly".into();
                self.install_rx = None;
                self.progress_rx = None;
                self.install_progress = None;
                self.content_busy = false;
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
        if self.auto_sodium && installed.game_directory.is_none() {
            let minecraft = self.selected_version.clone();
            let root = self.game_root.clone();
            let instance = root.join("instances").join(format!("sodium-{minecraft}"));
            let (tx, rx) = mpsc::channel();
            self.modpack_launch_rx = Some(rx);
            self.status = format!("Preparing Fabric + Sodium for Minecraft {minecraft}…");
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<InstalledVersion> {
                    let installed = crate::loaders::install_fabric_profile(&minecraft, &root, &instance)?;
                    crate::content::install_mod("AANobbMI", &instance, &minecraft, "fabric")?;
                    Ok(installed)
                })().map_err(|error| format!("{error:#}"));
                let _ = tx.send(result);
            });
            return;
        }
        match launcher::launch(&installed, &profile.name, &profile.uuid, self.memory_mb, &self.performance_mode) {
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

    fn nav_button(ui: &mut egui::Ui, label: &str, active: bool, uri: &'static str, icon: &'static [u8]) -> egui::Response {
        let text = if active {
            RichText::new(label).color(Color32::WHITE).strong()
        } else {
            RichText::new(label).color(Color32::from_rgb(174, 183, 204))
        };
        let image = egui::Image::from_bytes(uri, icon.to_vec())
            .fit_to_exact_size(Vec2::splat(18.0));
        ui.add_sized([178.0, 42.0], egui::Button::image_and_text(image, text).fill(
            if active { Color32::from_rgba_unmultiplied(83, 105, 160, 150) } else { Color32::TRANSPARENT }
        ).stroke(Stroke::NONE))
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("lunar_navigation")
            .exact_width(218.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(Color32::from_rgba_unmultiplied(7, 10, 19, 178)).stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(180, 195, 235, 24))).inner_margin(egui::Margin::same(18)))
            .show(ctx, |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    Self::svg_icon(ui, "bytes://lunar-moon.svg", include_bytes!("../assets/icons/moon.svg"), 34.0);
                    ui.label(RichText::new("Lunar Launcher").size(21.0).strong());
                });
                ui.add_space(34.0);
                egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                    let navigation: [NavigationItem; 13] = [
                        ("Home", Screen::Home, None, "bytes://lunar-home.svg", include_bytes!("../assets/icons/home.svg")),
                        ("Play", Screen::Play, None, "bytes://lunar-play.svg", include_bytes!("../assets/icons/play.svg")),
                        ("Versions", Screen::Versions, None, "bytes://lunar-versions.svg", include_bytes!("../assets/icons/versions.svg")),
                        ("Profiles", Screen::Profiles, None, "bytes://lunar-profiles.svg", include_bytes!("../assets/icons/profiles.svg")),
                        ("Content", Screen::Content, None, "bytes://lunar-content.svg", include_bytes!("../assets/icons/content.svg")),
                        ("Instance Manager", Screen::Workspace, Some("Overview"), "bytes://lunar-content.svg", include_bytes!("../assets/icons/content.svg")),
                        ("Mod Manager", Screen::Content, None, "bytes://lunar-mods.svg", include_bytes!("../assets/icons/mods.svg")),
                        ("Resource Packs", Screen::Content, None, "bytes://lunar-packs.svg", include_bytes!("../assets/icons/resource-packs.svg")),
                        ("Shader Manager", Screen::Content, None, "bytes://lunar-shaders.svg", include_bytes!("../assets/icons/shaders.svg")),
                        ("Performance Center", Screen::Workspace, Some("Performance"), "bytes://lunar-monitor.svg", include_bytes!("../assets/icons/monitor.svg")),
                        ("Launcher Customization", Screen::Settings, None, "bytes://lunar-settings.svg", include_bytes!("../assets/icons/settings.svg")),
                        ("Servers", Screen::Servers, None, "bytes://lunar-servers.svg", include_bytes!("../assets/icons/servers.svg")),
                        ("Settings", Screen::Settings, None, "bytes://lunar-settings.svg", include_bytes!("../assets/icons/settings.svg")),
                    ];
                    for (label, screen, section, uri, icon) in navigation {
                        let category_active = match label {
                            "Mod Manager" => self.content_kind == "mod",
                            "Resource Packs" => self.content_kind == "resourcepack",
                            "Shader Manager" => self.content_kind == "shader",
                            "Content" => self.content_kind == "modpack",
                            _ => true,
                        };
                        let active = self.screen == screen
                            && section.is_none_or(|value| self.workspace_section == value)
                            && category_active;
                        if Self::nav_button(ui, label, active, uri, icon).clicked() {
                            self.screen = screen;
                            match label {
                                "Mod Manager" => self.content_kind = "mod".into(),
                                "Resource Packs" => self.content_kind = "resourcepack".into(),
                                "Shader Manager" => self.content_kind = "shader".into(),
                                _ => {}
                            }
                            if screen == Screen::Content {
                                self.content_results.clear();
                                self.content_images.clear();
                                self.content_results_kind.clear();
                            }
                            if let Some(section) = section {
                                self.workspace_section = section.into();
                                self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default();
                                if self.workspace_selected.is_none() {
                                    self.workspace_selected = self.workspace_instances.first().map(|instance| instance.id.clone());
                                }
                            }
                        }
                    }
                });
            });
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Welcome to Lunar Launcher", "Your Minecraft setup, without the clutter.");
        egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(23, 31, 47, 158))
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
        let featured: Vec<_> = self.content_results.iter().take(4).map(|project| (
            project.project_id.clone(),
            project.title.clone(),
            self.content_images.get(&project.project_id).cloned(),
        )).collect();
        if !featured.is_empty() {
            ui.label(RichText::new("DISCOVER MODPACKS").size(10.0).strong().color(self.accent));
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for (id, title, texture) in featured {
                    let response = egui::Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(22, 30, 49, 190))
                        .stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(143, 164, 220, 42)))
                        .corner_radius(egui::CornerRadius::same(13))
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_min_width(126.0);
                            if let Some(texture) = texture {
                                ui.add(egui::Image::new((texture.id(), Vec2::splat(58.0))));
                            }
                            ui.add_space(5.0);
                            ui.label(RichText::new(title).size(11.0).strong());
                        }).response;
                    if response.interact(egui::Sense::click()).clicked() {
                        self.content_kind = "modpack".into();
                        self.content_query = id;
                        self.screen = Screen::Content;
                    }
                }
            });
            ui.add_space(12.0);
        }
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
            ui.label(RichText::new("Lunar installs a compatible Java runtime automatically if one is missing.").size(10.0).color(Color32::GRAY));
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
                LauncherPreferences { memory_mb: self.memory_mb, snapshots: self.snapshots, background_gif: self.background_gif.clone(), performance_mode: self.performance_mode.clone(), auto_sodium: self.auto_sodium, animations_enabled: self.animations_enabled, accent_rgb: [self.accent.r(), self.accent.g(), self.accent.b()] }.save();
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
        let (page_title, page_description) = match self.content_kind.as_str() {
            "modpack" => ("Modpack Browser", "Discover complete Minecraft setups and install them as isolated instances."),
            "resourcepack" => ("Resource Pack Manager", "Find, install and manage visual resource packs for your selected instance."),
            "shader" => ("Shader Manager", "Discover shader packs and keep them separate from mods and resource packs."),
            _ => ("Mod Manager", "Browse compatible mods and install them into the selected Minecraft instance."),
        };
        Self::heading(ui, page_title, page_description);
        if self.content_kind != "modpack" {
            let selected_label = self.workspace_instances.iter()
                .find(|entry| Some(entry.id.as_str()) == self.workspace_selected.as_deref())
                .map(|entry| entry.name.as_str())
                .unwrap_or("Global Minecraft folder");
            ui.horizontal(|ui| {
                ui.label(RichText::new("INSTALL TARGET").size(10.0).strong().color(self.accent));
                egui::ComboBox::from_id_salt("content_install_target")
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.workspace_selected, None, "Global Minecraft folder");
                        for instance in &self.workspace_instances {
                            ui.selectable_value(&mut self.workspace_selected, Some(instance.id.clone()), &instance.name);
                        }
                    });
            });
        }
        let content_root = self.workspace_instances.iter()
            .find(|entry| Some(entry.id.as_str()) == self.workspace_selected.as_deref())
            .map(|entry| crate::workspace::instance_path(&self.game_root, entry))
            .unwrap_or_else(|| self.game_root.clone());
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.content_kind, "modpack".into(), "Modpacks");
            ui.selectable_value(&mut self.content_kind, "resourcepack".into(), "Resource packs");
            ui.selectable_value(&mut self.content_kind, "mod".into(), "Mods");
            ui.selectable_value(&mut self.content_kind, "shader".into(), "Shaders");
        });
        if self.content_results_kind != self.content_kind && !self.content_busy {
            self.content_results.clear();
            self.content_images.clear();
            let query = self.content_query.clone();
            let kind = self.content_kind.clone();
            let (tx, rx) = mpsc::channel();
            self.content_search_rx = Some(rx);
            self.content_busy = true;
            self.content_search_kind = kind.clone();
            self.status = format!("Loading {}…", page_title);
            std::thread::spawn(move || {
                let _ = tx.send(crate::content::search(&query, &kind).map_err(|error| error.to_string()));
            });
        }
        if self.content_kind == "mod" {
            egui::ComboBox::from_id_salt("content_loader").selected_text(&self.content_loader).show_ui(ui, |ui| {
                for loader in ["fabric", "forge", "neoforge", "quilt"] { ui.selectable_value(&mut self.content_loader, loader.to_owned(), loader); }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!self.content_busy && !self.selected_version.is_empty() && self.content_loader == "fabric", egui::Button::new("Install Sodium + Fabric")).clicked() {
                    let minecraft = self.selected_version.clone();
                    let root = self.game_root.clone();
                    let instance = root.join("instances").join(format!("sodium-{minecraft}"));
                    let (tx, rx) = mpsc::channel();
                    self.install_rx = Some(rx);
                    self.content_busy = true;
                    self.status = "Installing Fabric and Sodium…".into();
                    std::thread::spawn(move || {
                        let result = (|| -> anyhow::Result<InstalledVersion> {
                            let installed = crate::loaders::install_fabric_profile(&minecraft, &root, &instance)?;
                            crate::content::install_mod("AANobbMI", &instance, &minecraft, "fabric")?;
                            Ok(installed)
                        })().map_err(|error| format!("{error:#}"));
                        let _ = tx.send(result);
                    });
                }
                if ui.button("OptiFine downloads").clicked() {
                    let url = "https://optifine.net/downloads";
                    let result = if cfg!(target_os = "windows") {
                        Command::new("cmd").args(["/C", "start", "", url]).spawn()
                    } else if cfg!(target_os = "macos") {
                        Command::new("open").arg(url).spawn()
                    } else {
                        Command::new("xdg-open").arg(url).spawn()
                    };
                    self.status = if result.is_ok() { "Opened official OptiFine downloads".into() } else { "Could not open OptiFine downloads".into() };
                }
            });
            ui.label(RichText::new("Sodium is the recommended Intel iGPU option. OptiFine is a separate alternative and should not be installed alongside Sodium.").size(10.0).color(Color32::from_rgb(170, 180, 201)));
        }
        ui.horizontal(|ui| {
            Self::svg_icon(ui, "bytes://lunar-search.svg", include_bytes!("../assets/icons/search.svg"), 18.0);
            ui.add_sized([336.0, 32.0], egui::TextEdit::singleline(&mut self.content_query).hint_text("Search Modrinth"));
            if ui.add_enabled(!self.content_busy, egui::Button::new("Search")).clicked() {
                let query = self.content_query.clone();
                let kind = self.content_kind.clone();
                let (tx, rx) = mpsc::channel();
                self.content_search_rx = Some(rx);
                self.content_busy = true;
                self.content_results.clear();
                self.content_images.clear();
                self.content_results_kind = kind.clone();
                self.content_search_kind = kind.clone();
                self.status = format!("Searching {page_title}…");
                std::thread::spawn(move || { let _ = tx.send(crate::content::search(&query, &kind).map_err(|error| error.to_string())); });
            }
        });
        let description = match self.content_kind.as_str() {
            "modpack" => "Modpacks install into separate instances with their mods and configurations.",
            "resourcepack" => "Resource packs install into the selected instance's resource-pack library.",
            "shader" => "Browse and install shader packs into the selected instance's shaderpacks folder.",
            _ => "Install mods into the selected instance's mods folder.",
        };
        ui.label(RichText::new(description).size(11.0).color(Color32::GRAY));
        if self.content_kind != "modpack" {
            ui.horizontal_wrapped(|ui| {
                let label = match self.content_kind.as_str() {
                    "mod" => "Import local .jar",
                    "resourcepack" => "Import local .zip",
                    _ => "Import shader .zip",
                };
                if ui.button(label).clicked()
                    && let Some(source) = Self::choose_content_file(&self.content_kind)
                {
                    let folder = match self.content_kind.as_str() {
                        "mod" => "mods",
                        "resourcepack" => "resourcepacks",
                        _ => "shaderpacks",
                    };
                    let extensions: &[&str] = if self.content_kind == "mod" { &["jar"] } else { &["zip"] };
                    match crate::workspace::import_content_file(&source, &content_root.join(folder), extensions) {
                        Ok(destination) => self.status = format!("Imported {}", destination.file_name().unwrap_or_default().to_string_lossy()),
                        Err(error) => self.status = error.to_string(),
                    }
                }
                if ui.button("Open folder").clicked() {
                    let folder = match self.content_kind.as_str() {
                        "mod" => "mods",
                        "resourcepack" => "resourcepacks",
                        _ => "shaderpacks",
                    };
                    self.open_local_path(content_root.join(folder));
                }
            });
        }
        if self.content_busy {
            ui.horizontal(|ui| { ui.spinner(); ui.label(format!("Loading {}…", page_title.to_lowercase())); });
        }
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for project in self.content_results.clone() {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        if let Some(texture) = self.content_images.get(&project.project_id) {
                            ui.add(egui::Image::new((texture.id(), Vec2::splat(72.0))).corner_radius(10.0));
                        } else {
                            ui.allocate_ui(Vec2::splat(72.0), |ui| {
                                ui.painter().rect_filled(ui.max_rect(), 10.0, Color32::from_rgb(32, 40, 58));
                            });
                        }
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&project.title).strong().size(15.0));
                            ui.label(RichText::new(format!("{} downloads · {}", project.downloads, project.slug)).size(10.0).color(Color32::GRAY));
                            ui.label(RichText::new(&project.description).size(11.0).color(Color32::GRAY));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let install_icon = egui::Image::from_bytes(
                                "bytes://lunar-download.svg",
                                include_bytes!("../assets/icons/download.svg").as_slice(),
                            ).fit_to_exact_size(Vec2::splat(16.0));
                            let target_instance = self.workspace_instances.iter()
                                .find(|entry| Some(entry.id.as_str()) == self.workspace_selected.as_deref());
                            let target_minecraft = target_instance.map(|entry| entry.minecraft.as_str())
                                .unwrap_or(self.selected_version.as_str());
                            let install_ready = !self.content_busy
                                && (matches!(self.content_kind.as_str(), "modpack" | "resourcepack" | "shader")
                                    || !target_minecraft.is_empty());
                            if ui.add_enabled(install_ready, egui::Button::image_and_text(install_icon, "Install")).clicked() {
                                let id = project.project_id.clone();
                                let root = self.game_root.clone();
                                let kind = self.content_kind.clone();
                                let selected_instance = self.workspace_instances.iter()
                                    .find(|entry| Some(entry.id.as_str()) == self.workspace_selected.as_deref()).cloned();
                                let minecraft = selected_instance.as_ref().map(|entry| entry.minecraft.clone())
                                    .unwrap_or_else(|| self.selected_version.clone());
                                let loader = self.content_loader.clone();
                                let instance_target = selected_instance.as_ref()
                                    .map(|entry| crate::workspace::instance_path(&self.game_root, entry));
                                let (tx, rx) = mpsc::channel();
                                self.content_install_rx = Some(rx);
                                self.content_busy = true;
                                self.status = format!("Installing {}…", project.title);
                                std::thread::spawn(move || {
                                    let result = (|| -> anyhow::Result<String> {
                                        let target = instance_target.as_deref().unwrap_or(&root);
                                        match kind.as_str() {
                                            "resourcepack" => crate::content::install_resource_pack(&id, target, &minecraft),
                                            "shader" => crate::content::install_shader_pack(&id, target, &minecraft),
                                            "mod" => {
                                                crate::content::install_mod(&id, target, &minecraft, &loader)?;
                                                if let Some(instance) = selected_instance {
                                                    let mut updated = instance;
                                                    updated.loader = Some(loader.clone());
                                                    crate::workspace::save_instance(&root, &updated)?;
                                                    crate::loaders::prepare_instance(&minecraft, Some(&loader), &root, target)?;
                                                }
                                                Ok(format!("Installed mod and required dependencies to {}", target.display()))
                                            }
                                            _ => crate::content::install_modpack(&id, &root),
                                        }
                                    })().map_err(|error| format!("{error:#}"));
                                    let _ = tx.send(result);
                                });
                            }
                        });
                    });
                });
            }
        });
        if self.content_kind == "modpack" {
        ui.add_space(12.0);
        ui.label(RichText::new("INSTALLED MODPACKS").strong().size(11.0).color(self.accent));
        if self.installed_packs.is_empty() { ui.label(RichText::new("No modpacks installed.").size(11.0).color(Color32::GRAY)); }
        for pack in self.installed_packs.clone() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&pack.name).strong());
                        ui.label(RichText::new(format!("Minecraft {} · {}", pack.minecraft, pack.loaders.keys().cloned().collect::<Vec<_>>().join(", "))).size(10.0).color(Color32::GRAY));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Remove").clicked() {
                            match crate::content::remove_modpack(&pack, &self.game_root) {
                                Ok(()) => { self.installed_packs = crate::content::installed_modpacks(&self.game_root).unwrap_or_default(); self.status = format!("Removed {}", pack.name); }
                                Err(error) => self.status = error.to_string(),
                            }
                        }
                        if ui.add_enabled(self.game_process.is_none() && self.modpack_launch_rx.is_none() && !self.content_busy, egui::Button::new("Launch")).clicked() {
                            if self.profiles.selected.as_ref().is_some_and(|name| self.profiles.profiles.iter().any(|p| &p.name == name)) {
                                let root = self.game_root.clone();
                                let (tx, rx) = mpsc::channel();
                                self.modpack_launch_rx = Some(rx);
                                self.status = format!("Preparing {}…", pack.name);
                                std::thread::spawn(move || { let _ = tx.send(crate::loaders::prepare_modpack(&pack, &root).map_err(|error| format!("{error:#}"))); });
                            } else { self.status = "Select an offline profile first".into(); }
                        }
                    });
                });
            });
        }
        }
        if self.content_kind == "resourcepack" {
        ui.add_space(12.0);
        ui.label(RichText::new("INSTALLED RESOURCE PACKS").strong().size(11.0).color(self.accent));
        for file in crate::content::list_files(&content_root, "resourcepacks").unwrap_or_default() {
            if let Some(filename) = file.file_name().and_then(|name| name.to_str()) {
                ui.horizontal(|ui| {
                    let mut enabled = crate::content::resource_pack_enabled(&content_root, filename);
                    if ui.checkbox(&mut enabled, filename).changed() {
                        match crate::content::set_resource_pack_enabled(&content_root, filename, enabled) {
                            Ok(()) => self.status = format!("Updated {filename}; restart Minecraft to apply"),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                    if ui.small_button("Remove").clicked() {
                        match crate::content::remove_file(&file, &content_root, "resourcepacks") {
                            Ok(()) => self.status = format!("Removed {filename}"),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                });
            }
        }
        }
        if self.content_kind == "mod" {
        ui.add_space(12.0);
        ui.label(RichText::new("INSTALLED MODS").strong().size(11.0).color(self.accent));
        let mod_files = crate::content::list_files(&content_root, "mods").unwrap_or_default();
        if mod_files.is_empty() { ui.label(RichText::new("No mods installed in this instance yet.").size(11.0).color(Color32::GRAY)); }
        for file in mod_files {
            if let Some(filename) = file.file_name().and_then(|name| name.to_str()).map(str::to_owned) {
                ui.horizontal(|ui| {
                    let mut enabled = !filename.ends_with(".jar.disabled");
                    if ui.checkbox(&mut enabled, &filename).changed() {
                        match crate::workspace::toggle_mod(&file, enabled) {
                            Ok(_) => self.status = format!("Updated {filename}"),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                    if ui.small_button("Remove").clicked() {
                        match crate::content::remove_file(&file, &content_root, "mods") {
                            Ok(()) => self.status = format!("Removed {filename}"),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                });
            }
        }
        }
        if self.content_kind == "shader" {
            ui.add_space(12.0);
            ui.label(RichText::new("INSTALLED SHADERS").strong().size(11.0).color(self.accent));
            let shader_files = crate::content::list_files(&content_root, "shaderpacks").unwrap_or_default();
            if shader_files.is_empty() { ui.label(RichText::new("No shader packs installed yet.").size(11.0).color(Color32::GRAY)); }
            for file in shader_files {
                let filename = file.file_name().and_then(|name| name.to_str()).unwrap_or("shader").to_owned();
                ui.horizontal(|ui| {
                    let mut enabled = !filename.ends_with(".disabled");
                    if ui.checkbox(&mut enabled, &filename).changed() {
                        match crate::content::set_file_enabled(&file, enabled) {
                            Ok(_) => self.status = format!("Updated {filename}"),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                    if ui.small_button("Remove").clicked() {
                        match crate::content::remove_file(&file, &content_root, "shaderpacks") {
                            Ok(()) => self.status = format!("Removed {filename}"),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                });
            }
        }
        if self.content_kind == "resourcepack" || self.content_kind == "mod" || self.content_kind == "shader" {
        ui.horizontal(|ui| {
            for (label, folder) in [("Open resource packs", "resourcepacks"), ("Open mods", "mods"), ("Open shaders", "shaderpacks")] {
                if ui.button(label).clicked() {
                    let path = content_root.join(folder);
                    let _ = fs::create_dir_all(&path);
                    let result = if cfg!(target_os = "windows") { Command::new("explorer").arg(&path).spawn() }
                        else if cfg!(target_os = "macos") { Command::new("open").arg(&path).spawn() }
                        else { Command::new("xdg-open").arg(&path).spawn() };
                    self.status = result.map(|_| format!("Opened {label}")).unwrap_or_else(|error| error.to_string());
                }
            }
        });
        }
        if self.modpack_launch_rx.is_some() { ui.spinner(); }
    }

    fn servers(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Server browser", "Save servers and check their live Java Edition status.");
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(RichText::new("ADD SERVER").strong().size(11.0).color(self.accent));
            ui.horizontal(|ui| {
                ui.add_sized([180.0, 32.0], egui::TextEdit::singleline(&mut self.server_name).hint_text("Server name"));
                ui.add_sized([220.0, 32.0], egui::TextEdit::singleline(&mut self.server_address).hint_text("Address or host:port"));
                if ui.add_enabled(!self.server_name.trim().is_empty() && !self.server_address.trim().is_empty(), egui::Button::new("Add server")).clicked() {
                    let server = crate::server::SavedServer { name: self.server_name.trim().to_owned(), address: self.server_address.trim().to_owned() };
                    if !self.servers.iter().any(|saved| saved.address.eq_ignore_ascii_case(&server.address)) {
                        self.servers.push(server);
                        match crate::server::save(&self.servers) { Ok(()) => self.status = "Server saved".into(), Err(error) => self.status = error.to_string() }
                    } else { self.status = "That server is already saved".into(); }
                    self.server_name.clear();
                    self.server_address.clear();
                }
            });
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.servers.is_empty() && self.server_rx.is_none(), egui::Button::new("Refresh all")).clicked() {
                let servers = self.servers.clone();
                let (tx, rx) = mpsc::channel();
                self.server_rx = Some(rx);
                std::thread::spawn(move || {
                    let results = servers.iter().map(|server| (server.address.clone(), crate::server::ping(server).map_err(|error| error.to_string()))).collect();
                    let _ = tx.send(results);
                });
            }
            ui.label(RichText::new("Status checks use the Minecraft Java server protocol.").size(10.0).color(Color32::GRAY));
        });
        for server in self.servers.clone() {
            ui.add_space(6.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&server.name).strong().size(15.0));
                        ui.label(RichText::new(&server.address).size(11.0).color(Color32::GRAY));
                        if let Some(status) = self.server_statuses.get(&server.address) {
                            if status.online {
                                ui.label(RichText::new(format!("{} · {} · {} ms", status.version, status.players, status.latency_ms)).size(11.0).color(Color32::from_rgb(112, 205, 151)));
                                ui.label(RichText::new(&status.motd).size(11.0).color(Color32::GRAY));
                            } else { ui.label(RichText::new("Offline or unreachable").size(11.0).color(Color32::from_rgb(220, 130, 130))); }
                        } else { ui.label(RichText::new("Not checked").size(11.0).color(Color32::GRAY)); }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Remove").clicked() {
                            self.servers.retain(|saved| saved.address != server.address);
                            self.server_statuses.remove(&server.address);
                            if let Err(error) = crate::server::save(&self.servers) { self.status = error.to_string(); }
                        }
                        if ui.add_enabled(self.server_rx.is_none(), egui::Button::new("Ping")).clicked() {
                            let server = server.clone();
                            let address = server.address.clone();
                            let (tx, rx) = mpsc::channel();
                            self.server_rx = Some(rx);
                            std::thread::spawn(move || { let _ = tx.send(vec![(address, crate::server::ping(&server).map_err(|error| error.to_string()))]); });
                        }
                    });
                });
            });
        }
        if self.server_rx.is_some() { ui.spinner(); }
    }

    fn workspace(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Instance Workspace", "Separate game folders, content, worlds, screenshots and settings.");
        ui.horizontal(|ui| {
            ui.add_sized([230.0, 32.0], egui::TextEdit::singleline(&mut self.workspace_new_name).hint_text("New instance name"));
            if ui.add_enabled(!self.workspace_new_name.trim().is_empty() && !self.selected_version.is_empty(), egui::Button::new("Create instance")).clicked() {
                match crate::workspace::create_instance(&self.game_root, &self.workspace_new_name, &self.selected_version) {
                    Ok(item) => { self.workspace_selected = Some(item.id.clone()); self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default(); self.workspace_new_name.clear(); self.status = format!("Created {}", item.name); }
                    Err(error) => self.status = error.to_string(),
                }
            }
            if ui.button("Refresh").clicked() { self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default(); }
        });
        ui.horizontal(|ui| {
            ui.add_sized([280.0, 30.0], egui::TextEdit::singleline(&mut self.workspace_search).hint_text("Search instances, mods, packs, screenshots and logs"));
            if ui.button("Search files").clicked() {
                self.workspace_results = crate::workspace::search(&self.game_root, &self.workspace_search).unwrap_or_default();
                self.workspace_section = "Search".into();
            }
        });
        if self.workspace_instances.is_empty() {
            ui.group(|ui| { ui.label(RichText::new("No instances yet").strong()); ui.label("Install a Minecraft version on the Play page, then create an isolated instance here."); });
        }
        ui.horizontal_wrapped(|ui| {
            for item in self.workspace_instances.clone() {
                let selected = self.workspace_selected.as_deref() == Some(item.id.as_str());
                if ui.selectable_label(selected, format!("{} · {}", item.name, item.minecraft)).clicked() {
                    self.workspace_selected = Some(item.id.clone());
                    self.workspace_section = "Overview".into();
                }
            }
        });
        let Some(mut item) = self.workspace_instances.iter().find(|item| Some(item.id.as_str()) == self.workspace_selected.as_deref()).cloned() else {
            if self.workspace_section == "Search" {
                ui.add_space(12.0);
                ui.label(RichText::new(format!("{} matching files", self.workspace_results.len())).strong());
                for path in self.workspace_results.clone() {
                    ui.horizontal(|ui| { ui.label(path.clone()); if ui.small_button("Open").clicked() { self.open_local_path(PathBuf::from(path)); } });
                }
            }
            return;
        };
        let path = crate::workspace::instance_path(&self.game_root, &item);
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            for section in ["Overview", "Mods", "Resource Packs", "Shaders", "Worlds & Backups", "Screenshots", "Crash Logs", "Performance", "Downloads"] {
                ui.selectable_value(&mut self.workspace_section, section.into(), section);
            }
        });
        ui.separator();
        match self.workspace_section.as_str() {
            "Overview" => {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&item.name).size(22.0).strong());
                        ui.label(format!("Minecraft {} · {} launches · Last played {}", item.minecraft, item.play_count, crate::workspace::format_time(item.last_played)));
                        ui.label(RichText::new(path.display().to_string()).size(10.0).color(Color32::GRAY));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Delete instance").clicked() {
                            match crate::workspace::delete_instance(&self.game_root, &item) {
                                Ok(()) => { self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default(); self.workspace_selected = None; self.status = "Instance deleted".into(); }
                                Err(error) => self.status = error.to_string(),
                            }
                        }
                        if ui.button("Open folder").clicked() { self.open_local_path(path.clone()); }
                    });
                });
                ui.add_space(12.0);
                ui.label(RichText::new("INSTANCE NAME").strong().size(10.0).color(self.accent));
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut item.name);
                    if ui.button("Save name").clicked() {
                        match crate::workspace::save_instance(&self.game_root, &item) {
                            Ok(()) => { self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default(); self.status = "Instance settings saved".into(); }
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                });
                ui.label(RichText::new("MEMORY ALLOCATION").strong().size(10.0).color(self.accent));
                ui.add(egui::Slider::new(&mut item.memory_mb, 2048..=8192).step_by(512.0).suffix(" MB"));
                ui.label(RichText::new("JAVA RUNTIME OVERRIDE").strong().size(10.0).color(self.accent));
                egui::ComboBox::from_id_salt("instance_java_override").selected_text(item.java_major.map(|value| format!("Java {value}")).unwrap_or_else(|| "Automatic (Minecraft requirement)".into())).show_ui(ui, |ui| {
                    ui.selectable_value(&mut item.java_major, None, "Automatic (Minecraft requirement)");
                    for major in [8_u32, 17, 21] { ui.selectable_value(&mut item.java_major, Some(major), format!("Java {major}")); }
                });
                if ui.button("Save instance settings").clicked() {
                    match crate::workspace::save_instance(&self.game_root, &item) {
                        Ok(()) => { self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default(); self.status = "Instance settings saved".into(); }
                        Err(error) => self.status = error.to_string(),
                    }
                }
                ui.add_space(10.0);
                ui.label(RichText::new("LAUNCH THIS INSTANCE").strong().size(10.0).color(self.accent));
                if ui.add_enabled(self.game_process.is_none() && self.install_rx.is_none(), egui::Button::new(RichText::new("Launch instance").strong())).clicked() {
                    if let Some(profile) = self.profiles.profiles.iter().find(|profile| Some(profile.name.as_str()) == self.profiles.selected.as_deref()) {
                        if item.loader.is_some() {
                            let root = self.game_root.clone();
                            let mc = item.minecraft.clone();
                            let loader = item.loader.clone();
                            let instance_path = path.clone();
                            let (tx, rx) = mpsc::channel();
                            self.modpack_launch_rx = Some(rx);
                            self.status = format!("Preparing {} with {}…", item.name, loader.as_deref().unwrap_or("loader"));
                            std::thread::spawn(move || {
                                let _ = tx.send(crate::loaders::prepare_instance(&mc, loader.as_deref(), &root, &instance_path)
                                    .map_err(|error| format!("{error:#}")));
                            });
                        } else {
                            match launcher::load_installed(&item.minecraft, &self.game_root) {
                                Ok(mut installed) => {
                                    installed.game_directory = Some(path.clone());
                                    match launcher::launch_with_java(&installed, &profile.name, &profile.uuid, item.memory_mb, &self.performance_mode, item.java_major) {
                                        Ok(process) => { self.game_process = Some(process); if let Err(error) = crate::workspace::record_launch(&self.game_root, &mut item) { self.status = error.to_string(); } self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default(); self.status = format!("Launching {}", item.name); }
                                        Err(error) => self.status = format!("{error:#}"),
                                    }
                                }
                                Err(error) => self.status = error.to_string(),
                            }
                        }
                    } else { self.status = "Select an offline profile on the Play page first".into(); }
                }
                ui.label(RichText::new("This instance uses its own mods, resource packs, shaders, saves and logs folders. Fabric/Forge loader installation is managed through Content.").size(11.0).color(Color32::GRAY));
                let files = crate::workspace::list_files(&path.join("mods"), &["jar", "disabled"]).unwrap_or_default();
                ui.label(format!("{} mod files · {} resource packs · {} shader packs", files.len(), crate::workspace::list_files(&path.join("resourcepacks"), &["zip"]).unwrap_or_default().len(), crate::workspace::list_files(&path.join("shaderpacks"), &["zip", "jar"]).unwrap_or_default().len()));
                if ui.button("Export instance pack").clicked() {
                    let destination = self.game_root.join("exports").join(format!("{}.lunarpack", item.id));
                    match crate::workspace::export_instance(&path, &destination) { Ok(()) => self.status = format!("Exported instance to {}", destination.display()), Err(error) => self.status = error.to_string() }
                }
                if ui.button("Import instance pack").clicked()
                    && let Some(source) = Self::choose_archive_file(false)
                {
                    match crate::workspace::import_instance(&source, &self.game_root, "Imported Instance") {
                        Ok(imported) => { self.workspace_selected = Some(imported.id); self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default(); self.status = "Instance pack imported".into(); }
                        Err(error) => self.status = error.to_string(),
                    }
                }
            }
            "Mods" => {
                ui.label("Per-instance mods. Add local JAR files, browse Modrinth, disable mods without deleting them, or remove them permanently.");
                if ui.button("Browse Modrinth mods").clicked() {
                    self.content_kind = "mod".into();
                    self.screen = Screen::Content;
                }
                if ui.button("Add mod JAR").clicked() && let Some(source) = Self::choose_content_file("mod") {
                    match crate::workspace::import_content_file(&source, &path.join("mods"), &["jar"]) {
                        Ok(destination) => self.status = format!("Added {}", destination.file_name().unwrap_or_default().to_string_lossy()),
                        Err(error) => self.status = error.to_string(),
                    }
                }
                for file in crate::workspace::list_files(&path.join("mods"), &["jar", "disabled"]).unwrap_or_default() {
                    let filename = file.file_name().and_then(|value| value.to_str()).unwrap_or("mod").to_owned();
                    let enabled = !filename.ends_with(".jar.disabled");
                    ui.horizontal(|ui| {
                        let mut state = enabled;
                        if ui.checkbox(&mut state, &filename).changed() {
                            match crate::workspace::toggle_mod(&file, state) { Ok(_) => self.status = format!("Updated {filename}"), Err(error) => self.status = error.to_string() }
                        }
                        if ui.small_button("Remove").clicked() {
                            match fs::remove_file(&file) { Ok(()) => self.status = format!("Removed {filename}"), Err(error) => self.status = error.to_string() }
                        }
                    });
                }
                if ui.button("Open mods folder").clicked() { self.open_local_path(path.join("mods")); }
            }
            "Resource Packs" => {
                ui.label("Import ZIP packs, browse Modrinth, enable or disable them, and remove packs from this instance.");
                if ui.button("Browse Modrinth resource packs").clicked() {
                    self.content_kind = "resourcepack".into();
                    self.screen = Screen::Content;
                }
                if ui.button("Add resource-pack ZIP").clicked() && let Some(source) = Self::choose_content_file("resourcepack") {
                    match crate::workspace::import_content_file(&source, &path.join("resourcepacks"), &["zip"]) {
                        Ok(destination) => self.status = format!("Added {}", destination.file_name().unwrap_or_default().to_string_lossy()),
                        Err(error) => self.status = error.to_string(),
                    }
                }
                for file in crate::workspace::list_files(&path.join("resourcepacks"), &["zip"]).unwrap_or_default() {
                    let filename = file.file_name().and_then(|value| value.to_str()).unwrap_or("pack").to_owned();
                    let mut enabled = crate::content::resource_pack_enabled(&path, &filename);
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut enabled, &filename).changed() {
                            match crate::content::set_resource_pack_enabled(&path, &filename, enabled) { Ok(()) => self.status = format!("Updated {filename}"), Err(error) => self.status = error.to_string() }
                        }
                        if ui.small_button("Remove").clicked() { match fs::remove_file(&file) { Ok(()) => self.status = format!("Removed {filename}"), Err(error) => self.status = error.to_string() } }
                    });
                }
                if ui.button("Open resource packs folder").clicked() { self.open_local_path(path.join("resourcepacks")); }
            }
            "Shaders" => {
                ui.label("Import, enable, disable and remove shader ZIP files. Select the active shader in Minecraft video settings.");
                if ui.button("Add shader-pack ZIP").clicked() && let Some(source) = Self::choose_content_file("shader") {
                    match crate::workspace::import_content_file(&source, &path.join("shaderpacks"), &["zip"]) {
                        Ok(destination) => self.status = format!("Added {}", destination.file_name().unwrap_or_default().to_string_lossy()),
                        Err(error) => self.status = error.to_string(),
                    }
                }
                for file in crate::workspace::list_files(&path.join("shaderpacks"), &["zip", "jar", "disabled"]).unwrap_or_default() {
                    let filename = file.file_name().and_then(|value| value.to_str()).unwrap_or("shader").to_owned();
                    let enabled = !filename.ends_with(".disabled");
                    ui.horizontal(|ui| {
                        let mut state = enabled;
                        if ui.checkbox(&mut state, &filename).changed() {
                            let destination = if state { file.with_extension("") } else { file.with_extension(format!("{}.disabled", file.extension().and_then(|value| value.to_str()).unwrap_or("zip"))) };
                            match fs::rename(&file, destination) { Ok(()) => self.status = format!("Updated {filename}"), Err(error) => self.status = error.to_string() }
                        }
                        if ui.small_button("Remove").clicked() { match fs::remove_file(&file) { Ok(()) => self.status = format!("Removed {filename}"), Err(error) => self.status = error.to_string() } }
                    });
                }
                if ui.button("Browse Modrinth shaders").clicked() {
                    self.content_kind = "shader".into();
                    self.screen = Screen::Content;
                }
                if ui.button("Open shader packs folder").clicked() { self.open_local_path(path.join("shaderpacks")); }
            }
            "Worlds & Backups" => {
                let saves = path.join("saves");
                let worlds = fs::read_dir(&saves).ok().into_iter().flatten().filter_map(|entry| entry.ok()).filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir())).collect::<Vec<_>>();
                ui.label(RichText::new("WORLD BACKUPS").strong());
                for world in worlds {
                    let name = world.file_name().to_string_lossy().to_string();
                    ui.horizontal(|ui| { ui.label(name.clone()); if ui.button("Back up").clicked() { match crate::workspace::backup_world(&path, &name) { Ok(archive) => self.status = format!("Backup created: {}", archive.display()), Err(error) => self.status = error.to_string() } } });
                }
                ui.add_space(8.0);
                ui.label(RichText::new("RESTORE BACKUP").strong());
                for backup in crate::workspace::list_files(&path.join("backups"), &["zip"]).unwrap_or_default() {
                    let name = backup.file_name().and_then(|value| value.to_str()).unwrap_or("backup").to_owned();
                    ui.horizontal(|ui| { ui.label(name); if ui.button("Restore").clicked() { match crate::workspace::restore_world(&backup, &path) { Ok(message) => self.status = message, Err(error) => self.status = error.to_string() } } });
                }
                if ui.button("Open saves folder").clicked() { self.open_local_path(saves); }
            }
            "Screenshots" => {
                let shots = crate::workspace::list_files(&path.join("screenshots"), &["png", "jpg", "jpeg"]).unwrap_or_default();
                ui.label(format!("{} screenshots", shots.len()));
                if shots.is_empty() {
                    ui.label("Screenshots from this instance will appear here.");
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for shot in shots {
                            let key = shot.to_string_lossy().to_string();
                            if !self.screenshot_textures.contains_key(&key)
                                && let Ok(image) = image::open(&shot)
                            {
                                let image = image.thumbnail(240, 160).to_rgba8();
                                let size = [image.width() as usize, image.height() as usize];
                                let pixels = image.into_raw();
                                let color = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                                self.screenshot_textures.insert(key.clone(), ui.ctx().load_texture(format!("screenshot-{key}"), color, egui::TextureOptions::LINEAR));
                            }
                            if let Some(texture) = self.screenshot_textures.get(&key) {
                                let response = ui.add(egui::Image::new((texture.id(), Vec2::new(160.0, 110.0))).sense(egui::Sense::click()));
                                if response.clicked() { self.open_local_path(shot.clone()); }
                            } else {
                                ui.label(shot.file_name().unwrap_or_default().to_string_lossy());
                            }
                        }
                    });
                }
                if ui.button("Open screenshots folder").clicked() { self.open_local_path(path.join("screenshots")); }
            }
            "Crash Logs" => {
                ui.label("Recent crash reports and launcher game logs.");
                for file in crate::workspace::list_files(&path.join("crash-reports"), &["txt"]).unwrap_or_default() {
                    let name = file.file_name().and_then(|value| value.to_str()).unwrap_or("Crash report").to_owned();
                    ui.horizontal(|ui| { if ui.button(&name).clicked() { self.workspace_preview = crate::workspace::read_tail(&file, 80).ok(); } if ui.small_button("Open").clicked() { self.open_local_path(file); } });
                }
                let log = path.join("logs").join("latest.log");
                if log.is_file() && ui.button("View latest.log").clicked() { self.workspace_preview = crate::workspace::read_tail(&log, 100).ok(); }
                if let Some(preview) = &self.workspace_preview { egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| { ui.add(egui::TextEdit::multiline(&mut preview.clone()).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace)); }); }
            }
            "Performance" => {
                ui.label(RichText::new("LIVE SYSTEM MONITOR").strong().color(self.accent));
                let cpu = self.system_monitor.global_cpu_usage();
                let total_memory = self.system_monitor.total_memory() / 1024 / 1024;
                let used_memory = self.system_monitor.used_memory() / 1024 / 1024;
                ui.horizontal_wrapped(|ui| {
                    ui.group(|ui| {
                        ui.label(RichText::new("CPU LOAD").size(10.0).color(Color32::GRAY));
                        ui.label(RichText::new(format!("{cpu:.0}%")).size(24.0).strong());
                        ui.add(egui::ProgressBar::new((cpu / 100.0).clamp(0.0, 1.0)).desired_width(150.0));
                    });
                    ui.group(|ui| {
                        ui.label(RichText::new("SYSTEM RAM").size(10.0).color(Color32::GRAY));
                        ui.label(RichText::new(format!("{used_memory} / {total_memory} MB")).size(19.0).strong());
                        ui.add(egui::ProgressBar::new((used_memory as f32 / total_memory.max(1) as f32).clamp(0.0, 1.0)).desired_width(180.0));
                    });
                    ui.group(|ui| {
                        ui.label(RichText::new("CPU THREADS").size(10.0).color(Color32::GRAY));
                        ui.label(RichText::new(format!("{}", self.system_monitor.cpus().len())).size(24.0).strong());
                        ui.label("Logical processors");
                    });
                });
                if let Some(process) = self.game_process.as_ref() {
                    let pid = sysinfo::Pid::from_u32(process.id());
                    if let Some(game) = self.system_monitor.process(pid) {
                        ui.group(|ui| {
                            ui.label(RichText::new("MINECRAFT PROCESS").strong());
                            ui.horizontal(|ui| {
                                ui.label(format!("RAM: {} MB", game.memory() / 1024 / 1024));
                                ui.label(format!("CPU: {:.1}%", game.cpu_usage()));
                                ui.label(format!("PID: {}", process.id()));
                            });
                        });
                    }
                } else {
                    ui.label(RichText::new("Minecraft is not running. Live game-process metrics will appear here after launch.").size(11.0).color(Color32::GRAY));
                }
                ui.add_space(8.0);
                ui.label(RichText::new("INSTANCE MEMORY").strong().color(self.accent));
                ui.label(format!("{} · Minecraft {}", item.name, item.minecraft));
                let previous_allocation = item.memory_mb;
                let memory_limit = (total_memory.saturating_sub(2048) as u32).clamp(2048, 16384);
                ui.add(egui::Slider::new(&mut item.memory_mb, 2048..=memory_limit.max(2048)).step_by(512.0).suffix(" MB"));
                if previous_allocation != item.memory_mb {
                    match crate::workspace::save_instance(&self.game_root, &item) {
                        Ok(()) => { self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default(); self.status = format!("Instance RAM set to {} MB", item.memory_mb); }
                        Err(error) => self.status = error.to_string(),
                    }
                }
                ui.label("Keep enough memory for Windows and background apps. More allocated RAM does not automatically mean more FPS.");
                ui.add_space(8.0);
                ui.label(RichText::new("ACTIVE GAME CONFIGURATION").strong().color(self.accent));
                ui.label(format!("Performance profile: {}", self.performance_mode));
                ui.label("Render distance: 3 chunks · Simulation distance: 3 chunks");
                ui.label(format!("Mod loader: {}", item.loader.as_deref().unwrap_or("Vanilla")));
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Install recommended optimizations").clicked() {
                        let root = self.game_root.clone();
                        let instance_path = path.clone();
                        let mut instance = item.clone();
                        let mc = item.minecraft.clone();
                        let (tx, rx) = mpsc::channel();
                        self.content_install_rx = Some(rx);
                        self.content_busy = true;
                        self.status = "Installing Fabric and performance mods…".into();
                        std::thread::spawn(move || {
                            let result = (|| -> anyhow::Result<String> {
                                instance.loader = Some("fabric".into());
                                crate::workspace::save_instance(&root, &instance)?;
                                crate::loaders::prepare_instance(&mc, Some("fabric"), &root, &instance_path)?;
                                let mut installed = Vec::new();
                                for (id, name) in [("AANobbMI", "Sodium"), ("gvQqBUqZ", "Lithium"), ("uXXizFIs", "FerriteCore"), ("NNAgCjsB", "EntityCulling")] {
                                    crate::content::install_mod(id, &instance_path, &mc, "fabric")
                                        .with_context(|| format!("Could not install {name}"))?;
                                    installed.push(name);
                                }
                                Ok(format!("Installed {} performance mods", installed.join(", ")))
                            })().map_err(|error| format!("{error:#}"));
                            let _ = tx.send(result);
                        });
                    }
                    if ui.button("Install customizable HUD widgets").clicked() {
                        let root = self.game_root.clone();
                        let instance_path = path.clone();
                        let mut instance = item.clone();
                        let mc = item.minecraft.clone();
                        let (tx, rx) = mpsc::channel();
                        self.content_install_rx = Some(rx);
                        self.content_busy = true;
                        self.status = "Installing customizable HUD widgets…".into();
                        std::thread::spawn(move || {
                            let result = (|| -> anyhow::Result<String> {
                                instance.loader = Some("fabric".into());
                                crate::workspace::save_instance(&root, &instance)?;
                                crate::loaders::prepare_instance(&mc, Some("fabric"), &root, &instance_path)?;
                                crate::content::install_mod("hud-better-widgets", &instance_path, &mc, "fabric")?;
                                Ok("Installed Better HUD Widgets. Open its in-game editor with K to place and configure widgets.".into())
                            })().map_err(|error| format!("{error:#}"));
                            let _ = tx.send(result);
                        });
                    }
                    if ui.button("Open instance mods").clicked() { self.open_local_path(path.join("mods")); }
                });
                ui.label(RichText::new("The HUD widget mod adds draggable FPS, ping, coordinates, armor, hunger and saturation displays. Its editor lets you position and resize each widget in-game.").size(11.0).color(Color32::GRAY));
                ui.label(RichText::new("Intel integrated graphics: keep shaders off or use lightweight shaders only; shader effects can reduce frame rate significantly.").size(10.0).color(Color32::GRAY));
                if ui.button("Open Performance settings").clicked() { self.screen = Screen::Settings; }
            }
            "Downloads" => {
                ui.label(RichText::new("DOWNLOAD QUEUE").strong());
                ui.label("Queue direct file links and pause, resume or cancel transfers. Downloads are streamed to a temporary file and moved into place when complete.");
                ui.horizontal_wrapped(|ui| {
                    ui.add_sized([360.0, 32.0], egui::TextEdit::singleline(&mut self.download_url).hint_text("https://example.com/file.jar"));
                    egui::ComboBox::from_id_salt("download_destination").selected_text(&self.download_folder).show_ui(ui, |ui| {
                        for folder in ["downloads", "mods", "resourcepacks", "shaderpacks", "screenshots"] { ui.selectable_value(&mut self.download_folder, folder.to_owned(), folder); }
                    });
                    if ui.add_enabled(!self.download_url.trim().is_empty(), egui::Button::new("Add to queue")).clicked() {
                        let base = self.workspace_instances.iter().find(|entry| Some(entry.id.as_str()) == self.workspace_selected.as_deref())
                            .map(|entry| crate::workspace::instance_path(&self.game_root, entry)).unwrap_or_else(|| self.game_root.clone());
                        match self.download_manager.enqueue(self.download_url.trim(), base.join(&self.download_folder)) {
                            Ok(_) => { self.download_url.clear(); self.status = "Download queued".into(); }
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                });
                if ui.button("Clear completed").clicked() { self.download_manager.clear_finished(); }
                for task in self.download_manager.tasks().to_vec() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&task.filename).strong());
                                ui.label(RichText::new(task.state.label()).size(10.0).color(Color32::GRAY));
                                ui.label(RichText::new(task.destination.display().to_string()).size(9.0).color(Color32::GRAY));
                                ui.label(RichText::new(&task.url).size(9.0).color(Color32::GRAY));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if task.state == crate::downloads::DownloadState::Paused {
                                    if ui.small_button("Resume").clicked() { self.download_manager.resume(task.id); }
                                } else if !matches!(task.state, crate::downloads::DownloadState::Completed | crate::downloads::DownloadState::Cancelled | crate::downloads::DownloadState::Failed(_))
                                    && ui.small_button("Pause").clicked() { self.download_manager.pause(task.id); }
                                if !matches!(task.state, crate::downloads::DownloadState::Completed | crate::downloads::DownloadState::Cancelled | crate::downloads::DownloadState::Failed(_))
                                    && ui.small_button("Cancel").clicked() { self.download_manager.cancel(task.id); }
                            });
                        });
                        if let Some(total) = task.total.filter(|total| *total > 0) {
                            ui.add(egui::ProgressBar::new((task.downloaded as f32 / total as f32).clamp(0.0, 1.0)).text(format!("{} / {} MB", task.downloaded / 1_048_576, total / 1_048_576)));
                        } else if task.state == crate::downloads::DownloadState::Downloading { ui.spinner(); }
                    });
                }
                if let Some(progress) = &self.install_progress {
                    let fraction = if progress.total == 0 { 0.0 } else { progress.completed as f32 / progress.total as f32 };
                    ui.add(egui::ProgressBar::new(fraction).text(format!("Minecraft: {} · {}/{}", progress.phase, progress.completed, progress.total)));
                }
            }
            "Search" => {
                for result in self.workspace_results.clone() { ui.label(result); }
            }
            _ => {}
        }
    }

    fn open_local_path(&mut self, path: PathBuf) {
        let result = if cfg!(target_os = "windows") { Command::new("explorer").arg(&path).spawn() }
            else if cfg!(target_os = "macos") { Command::new("open").arg(&path).spawn() }
            else { Command::new("xdg-open").arg(&path).spawn() };
        self.status = result.map(|_| format!("Opened {}", path.display())).unwrap_or_else(|error| format!("Could not open path: {error}"));
    }

    fn choose_archive_file(save: bool) -> Option<PathBuf> {
        let output = if cfg!(target_os = "windows") {
            let script = if save { "Add-Type -AssemblyName System.Windows.Forms; $d=New-Object System.Windows.Forms.SaveFileDialog; $d.Filter='Lunar instance (*.lunarpack)|*.lunarpack'; if($d.ShowDialog() -eq 'OK'){[Console]::Write($d.FileName)}" } else { "Add-Type -AssemblyName System.Windows.Forms; $d=New-Object System.Windows.Forms.OpenFileDialog; $d.Filter='Lunar instance (*.lunarpack)|*.lunarpack'; if($d.ShowDialog() -eq 'OK'){[Console]::Write($d.FileName)}" };
            Command::new("powershell").args(["-NoProfile", "-Command", script]).output().ok()?
        } else {
            let mut command = Command::new("zenity");
            if save { command.args(["--file-selection", "--save", "--confirm-overwrite", "--file-filter=Lunar instance | *.lunarpack"]); }
            else { command.args(["--file-selection", "--file-filter=Lunar instance | *.lunarpack"]); }
            command.output().ok().or_else(|| Command::new("kdialog").args(if save { vec!["--getsavefilename", ".", "*.lunarpack"] } else { vec!["--getopenfilename", ".", "*.lunarpack"] }).output().ok())?
        };
        if !output.status.success() { return None; }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (!value.is_empty()).then(|| PathBuf::from(value))
    }

    fn choose_content_file(kind: &str) -> Option<PathBuf> {
        let (filter, description) = match kind {
            "mod" => ("*.jar", "Minecraft mod (*.jar)"),
            "resourcepack" | "shader" => ("*.zip", "ZIP archive (*.zip)"),
            _ => return None,
        };
        let output = if cfg!(target_os = "windows") {
            let script = format!("Add-Type -AssemblyName System.Windows.Forms; $d=New-Object System.Windows.Forms.OpenFileDialog; $d.Filter='{}|{}'; if($d.ShowDialog() -eq 'OK'){{[Console]::Write($d.FileName)}}", description, filter);
            Command::new("powershell").args(["-NoProfile", "-Command", &script]).output().ok()?
        } else {
            Command::new("zenity").args(["--file-selection", &format!("--file-filter={description} | {filter}")]).output().ok()
                .or_else(|| Command::new("kdialog").args(["--getopenfilename", ".", filter]).output().ok())?
        };
        if !output.status.success() { return None; }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (!value.is_empty()).then(|| PathBuf::from(value))
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Settings", "Make Lunar Launcher feel like yours.");
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                Self::svg_icon(ui, "bytes://lunar-image.svg", include_bytes!("../assets/icons/image.svg"), 20.0);
                ui.label(RichText::new("ADD BACKGROUND GIF").strong());
            });
            ui.label(RichText::new("Choose any local GIF. Its frame timing is preserved so the background animation stays in sync, behind the transparent panels.").size(12.0).color(Color32::from_rgb(166, 176, 197)));
            ui.add_space(8.0);
            if !self.background_gif.is_empty() {
                let selected_name = PathBuf::from(&self.background_gif).file_name().and_then(|name| name.to_str()).map(str::to_owned).unwrap_or_else(|| "Selected GIF".to_owned());
                ui.label(RichText::new(format!("Selected: {selected_name}")).size(11.0).color(Color32::from_rgb(183, 196, 225)));
            }
            if !self.background_frames.is_empty() {
                ui.label(RichText::new(format!("{} animation frames loaded", self.background_frames.len())).size(10.0).color(Color32::GRAY));
            }
            ui.horizontal(|ui| {
                let field_width = (ui.available_width() - 116.0).max(120.0);
                ui.add_sized([field_width, 34.0], egui::TextEdit::singleline(&mut self.background_gif).hint_text("Path to a .gif file"));
                if ui.button("Choose GIF").clicked() && let Some(path) = Self::choose_gif_file() {
                    self.background_gif = path;
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Apply background").clicked() && !self.background_gif.trim().is_empty() {
                    self.load_background_gif(self.background_gif.trim().to_owned());
                }
                if ui.button("Remove background").clicked() {
                    self.background_gif.clear();
                    self.background_frames.clear();
                    self.background_texture = None;
                    self.background_rx = None;
                    LauncherPreferences { memory_mb: self.memory_mb, snapshots: self.snapshots, background_gif: String::new(), performance_mode: self.performance_mode.clone(), auto_sodium: self.auto_sodium, animations_enabled: self.animations_enabled, accent_rgb: [self.accent.r(), self.accent.g(), self.accent.b()] }.save();
                    self.status = "Animated background removed".into();
                }
            });
            if self.background_rx.is_some() {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Preparing blurred animation…"); });
            }
        });
        ui.add_space(10.0);
        ui.group(|ui| {
            ui.label(RichText::new("LAUNCHER APPEARANCE").strong());
            ui.label("Accent color");
            let presets = [
                ("Moonlight", Color32::from_rgb(151, 169, 255)),
                ("Ice", Color32::from_rgb(105, 207, 255)),
                ("Violet", Color32::from_rgb(183, 132, 255)),
                ("Rose", Color32::from_rgb(255, 133, 173)),
                ("Mint", Color32::from_rgb(112, 224, 185)),
                ("Amber", Color32::from_rgb(255, 190, 105)),
            ];
            ui.horizontal_wrapped(|ui| {
                for (name, color) in presets {
                    if ui.add(egui::Button::new(name).fill(color).stroke(Stroke::new(1.0_f32, Color32::WHITE))).clicked() {
                        self.accent = color;
                        LauncherPreferences { memory_mb: self.memory_mb, snapshots: self.snapshots, background_gif: self.background_gif.clone(), performance_mode: self.performance_mode.clone(), auto_sodium: self.auto_sodium, animations_enabled: self.animations_enabled, accent_rgb: [color.r(), color.g(), color.b()] }.save();
                        self.status = format!("{name} accent applied");
                    }
                }
            });
            ui.label("Animated GIF wallpaper, motion effects and accent colors are applied locally.");
        });
        ui.add_space(10.0);
        let previous_memory = self.memory_mb;
        let previous_performance = self.performance_mode.clone();
        ui.group(|ui| {
            ui.horizontal(|ui| {
                Self::svg_icon(ui, "bytes://lunar-shield.svg", include_bytes!("../assets/icons/shield.svg"), 19.0);
                ui.label(RichText::new("IDENTITY").strong());
            });
            ui.label("Authentication mode: Offline profiles only");
            ui.label("Online account authentication is not included.");
        });
        ui.add_space(10.0);
        ui.group(|ui| {
            ui.label(RichText::new("JAVA RUNTIME").strong());
            ui.label(&self.java_status);
            ui.label(RichText::new("You do not need to install Java separately. Lunar manages the correct runtime for each Minecraft version when you launch.").size(11.0).color(Color32::from_rgb(166, 176, 197)));
            ui.horizontal(|ui| {
                for major in [8_u32, 17, 21] {
                    if ui.add_enabled(self.java_install_rx.is_none(), egui::Button::new(format!("Install Java {major}"))).clicked() {
                        let root = self.game_root.clone();
                        let (tx, rx) = mpsc::channel();
                        self.java_install_rx = Some(rx);
                        self.status = format!("Installing Java {major}…");
                        std::thread::spawn(move || { let result = crate::runtime::install_java(major, &root).map(|path| path.display().to_string()).map_err(|error| format!("{error:#}")); let _ = tx.send(result); });
                    }
                }
            });
            if ui.button("Detect Java again").clicked() {
                self.java_status = launcher::detect_java().unwrap_or_else(|error| error.to_string());
            }
        });
        ui.add_space(10.0);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                Self::svg_icon(ui, "bytes://lunar-memory.svg", include_bytes!("../assets/icons/memory.svg"), 19.0);
                ui.label(RichText::new("MEMORY ALLOCATION").strong());
            });
            ui.add(egui::Slider::new(&mut self.memory_mb, 2048..=8192).step_by(512.0).suffix(" MB"));
            ui.label(RichText::new("4 GB is a good starting point for vanilla Minecraft. Avoid allocating all system RAM.").size(11.0).color(Color32::GRAY));
        });
        ui.add_space(10.0);
        if previous_memory != self.memory_mb {
            LauncherPreferences { memory_mb: self.memory_mb, snapshots: self.snapshots, background_gif: self.background_gif.clone(), performance_mode: self.performance_mode.clone(), auto_sodium: self.auto_sodium, animations_enabled: self.animations_enabled, accent_rgb: [self.accent.r(), self.accent.g(), self.accent.b()] }.save();
        }
        ui.add_space(10.0);
        if ui.checkbox(&mut self.animations_enabled, "Enable animated interface and ambient effects").changed() {
            LauncherPreferences { memory_mb: self.memory_mb, snapshots: self.snapshots, background_gif: self.background_gif.clone(), performance_mode: self.performance_mode.clone(), auto_sodium: self.auto_sodium, animations_enabled: self.animations_enabled, accent_rgb: [self.accent.r(), self.accent.g(), self.accent.b()] }.save();
        }
        ui.group(|ui| {
            ui.label(RichText::new("MINECRAFT PERFORMANCE").strong());
            egui::ComboBox::from_id_salt("lunar_performance_mode")
                .selected_text(&self.performance_mode)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.performance_mode, "Maximum FPS".into(), "Maximum FPS");
                    ui.selectable_value(&mut self.performance_mode, "Performance".into(), "Performance");
                    ui.selectable_value(&mut self.performance_mode, "Balanced".into(), "Balanced");
                    ui.selectable_value(&mut self.performance_mode, "Custom".into(), "Keep my in-game settings");
                });
            let description = match self.performance_mode.as_str() {
                "Maximum FPS" => "2-chunk render distance, 2 simulation distance, fast graphics and a high FPS ceiling",
                "Performance" => "4-chunk render distance, reduced effects and no VSync",
                "Balanced" => "8-chunk render distance with moderate visual settings",
                _ => "Lunar Launcher will not overwrite your Minecraft video settings",
            };
            ui.label(RichText::new(description).size(11.0).color(Color32::GRAY));
            if ui.checkbox(&mut self.auto_sodium, "Automatically install Fabric + Sodium for vanilla launches").changed() {
                LauncherPreferences { memory_mb: self.memory_mb, snapshots: self.snapshots, background_gif: self.background_gif.clone(), performance_mode: self.performance_mode.clone(), auto_sodium: self.auto_sodium, animations_enabled: self.animations_enabled, accent_rgb: [self.accent.r(), self.accent.g(), self.accent.b()] }.save();
            }
        });
        if previous_performance != self.performance_mode {
            LauncherPreferences { memory_mb: self.memory_mb, snapshots: self.snapshots, background_gif: self.background_gif.clone(), performance_mode: self.performance_mode.clone(), auto_sodium: self.auto_sodium, animations_enabled: self.animations_enabled, accent_rgb: [self.accent.r(), self.accent.g(), self.accent.b()] }.save();
        }
        ui.group(|ui| {
            ui.horizontal(|ui| {
                Self::svg_icon(ui, "bytes://lunar-monitor.svg", include_bytes!("../assets/icons/monitor.svg"), 19.0);
                ui.label(RichText::new("GAME DIRECTORY").strong());
            });
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
        ui.add_space(12.0);
    }
}

impl eframe::App for LunarApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_versions(ctx);
        self.poll_launcher(ctx);
        if self.last_metrics_refresh.elapsed() >= Duration::from_secs(1) {
            self.system_monitor.refresh_all();
            self.last_metrics_refresh = Instant::now();
        }
        self.download_manager.poll();
        self.poll_background(ctx);
        if let Some(rx) = &self.content_search_rx
            && let Ok(result) = rx.try_recv()
        {
            if self.content_search_kind == self.content_kind {
                match result {
                    Ok(items) => {
                        self.content_images.clear();
                        for project in &items {
                            if let Some((width, height, pixels)) = &project.icon_rgba {
                                let image = egui::ColorImage::from_rgba_unmultiplied([*width, *height], pixels);
                                self.content_images.insert(project.project_id.clone(), ctx.load_texture(
                                    format!("modrinth-{}", project.project_id),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                        }
                        self.content_results = items;
                        self.content_results_kind = self.content_kind.clone();
                        self.status = format!("Found {} projects", self.content_results.len());
                    }
                    Err(error) => self.status = error,
                }
            }
            self.content_search_rx = None;
            self.content_busy = false;
        }
        if let Some(rx) = &self.content_install_rx
            && let Ok(result) = rx.try_recv()
        {
            match result {
                Ok(path) => {
                    self.status = format!("Installed to {path}");
                    self.installed_packs = crate::content::installed_modpacks(&self.game_root).unwrap_or_default();
                    self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default();
                }
                Err(error) => self.status = error,
            }
            self.content_install_rx = None;
            self.content_busy = false;
        }
        if let Some(rx) = &self.java_install_rx
            && let Ok(result) = rx.try_recv()
        {
            self.java_install_rx = None;
            match result {
                Ok(path) => {
                    self.java_status = format!("Managed runtime: {path}");
                    self.status = "Java installation complete".into();
                }
                Err(error) => self.status = error,
            }
        }
        if let Some(rx) = &self.modpack_launch_rx
            && let Ok(result) = rx.try_recv()
        {
            self.modpack_launch_rx = None;
            match result {
                Ok(installed) => {
                    if let Some(profile) = self.profiles.profiles.iter().find(|profile| {
                        Some(profile.name.as_str()) == self.profiles.selected.as_deref()
                    }) {
                        match launcher::launch(&installed, &profile.name, &profile.uuid, self.memory_mb, &self.performance_mode) {
                            Ok(process) => {
                                self.game_process = Some(process);
                                self.status = format!("Launching {}", installed.id);
                            }
                            Err(error) => self.status = format!("{error:#}"),
                        }
                    } else {
                        self.status = "Select an offline profile first".into();
                    }
                }
                Err(error) => self.status = error,
            }
        }
        if let Some(rx) = &self.server_rx
            && let Ok(results) = rx.try_recv()
        {
            for (address, result) in results {
                self.server_statuses.insert(address, result.unwrap_or_else(|_| crate::server::ServerStatus::default()));
            }
            self.server_rx = None;
        }
        if self.content_search_rx.is_some()
            || self.content_install_rx.is_some()
            || self.modpack_launch_rx.is_some()
            || self.java_install_rx.is_some()
            || self.download_manager.has_active()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        self.paint_background(ctx);
        self.sidebar(ctx);
        egui::TopBottomPanel::bottom("status_bar")
            .frame(egui::Frame::new().fill(Color32::from_rgba_unmultiplied(7, 10, 19, 178)).stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(180, 195, 235, 24))).inner_margin(egui::Margin::symmetric(18, 8)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(self.accent).size(9.0));
                    ui.label(RichText::new(&self.status).size(11.0).color(Color32::from_rgb(157, 166, 185)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new("Lunar Launcher 0.1.0").size(10.0).color(Color32::GRAY));
                    });
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(egui::Margin::same(30)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("lunar_main_content_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match self.screen {
                            Screen::Home => self.home(ui),
                            Screen::Play => self.play(ui),
                            Screen::Versions => self.versions(ui),
                            Screen::Profiles => self.profiles(ui),
                            Screen::Content => self.content(ui),
                            Screen::Servers => self.servers(ui),
                            Screen::Workspace => self.workspace(ui),
                            Screen::Settings => self.settings(ui),
                        }
                    });
            });
        self.paint_splash(ctx);
    }
}

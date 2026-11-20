use anyhow::Context;
use crate::{launcher::{self, InstalledVersion, InstallProgress}, minecraft::{self, VersionEntry, VersionManifest}, profiles::ProfileStore};
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Stroke, Vec2};
use image::{AnimationDecoder, codecs::gif::GifDecoder, imageops::FilterType};
use std::{fs, io::BufReader, path::PathBuf, process::{Child, Command}, sync::{mpsc::{self, Receiver, TryRecvError}}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

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
        Self { memory_mb: 4096, snapshots: false, background_gif: String::new(), performance_mode: "Performance".into(), auto_sodium: false, animations_enabled: true, accent_rgb: [151, 169, 255] }
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
    LivePerformance,
    PlayStats,
    SkinManager,
    Downloads,
    Instances,
    CrashAnalyzer,
}

type ServerPingResults = Vec<(String, Result<crate::server::ServerStatus, String>)>;
type BackgroundFrames = Vec<(egui::ColorImage, u64)>;
type BackgroundLoadResult = Result<BackgroundFrames, String>;
type NavigationItem = (&'static str, Screen, &'static str, &'static [u8]);
type SkinTexturedFace = (f32, [egui::Pos2; 4], (u32, u32, u32, u32));
fn skin_uv_regions(part: &str, outer: bool) -> &'static [(u32, u32, u32, u32)] {
    const ALL: &[(u32, u32, u32, u32)] = &[(0, 0, 64, 64)];
    const HEAD: &[(u32, u32, u32, u32)] = &[(0, 0, 32, 16)];
    const BODY: &[(u32, u32, u32, u32)] = &[(16, 16, 24, 16)];
    const ARMS: &[(u32, u32, u32, u32)] = &[(40, 16, 16, 16), (32, 48, 16, 16)];
    const LEGS: &[(u32, u32, u32, u32)] = &[(0, 16, 16, 16), (16, 48, 16, 16)];
    const HEAD_OUTER: &[(u32, u32, u32, u32)] = &[(32, 0, 32, 16)];
    const BODY_OUTER: &[(u32, u32, u32, u32)] = &[(16, 32, 24, 16)];
    const ARMS_OUTER: &[(u32, u32, u32, u32)] = &[(40, 32, 16, 16), (48, 48, 16, 16)];
    const LEGS_OUTER: &[(u32, u32, u32, u32)] = &[(0, 32, 16, 16), (0, 48, 16, 16)];
    if !outer { match part { "Head" => HEAD, "Body" => BODY, "Arms" => ARMS, "Legs" => LEGS, _ => ALL } }
    else { match part { "Head" => HEAD_OUTER, "Body" => BODY_OUTER, "Arms" => ARMS_OUTER, "Legs" => LEGS_OUTER, _ => ALL } }
}


enum MicrosoftLoginEvent {
    DeviceCode { code: String, url: String },
    Complete(crust_core::authenticator::Account),
    Restored(crust_core::authenticator::Account),
    RestoreFailed(String),
    Failed(String),
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
    performance_mode: String,
    auto_sodium: bool,
    animations_enabled: bool,
    startup_at: Instant,
    splash_complete: bool,
    game_process: Option<Child>,
    content_kind: String,
    content_tab: String,
    loader_kind: String,
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
    pending_instance_launch: Option<crate::workspace::Instance>,
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
    new_instance_name: String,
    crash_analysis: Option<crate::crash::CrashAnalysis>,
    play_stats: crate::stats::PlayStats,
    session_started_at: Option<Instant>,
    session_started_epoch: u64,
    session_version: String,
    session_instance: Option<String>,
    system_monitor: sysinfo::System,
    last_metrics_refresh: Instant,
    cpu_history: Vec<f32>,
    memory_history: Vec<f32>,
    game_cpu_history: Vec<f32>,
    game_memory_history: Vec<f32>,
    microsoft_rx: Option<Receiver<MicrosoftLoginEvent>>,
    microsoft_account: Option<crust_core::authenticator::Account>,
    microsoft_code: Option<(String, String)>,
    skin_editor: Option<image::RgbaImage>,
    skin_editor_profile: String,
    skin_color: Color32,
    skin_rotation: f32,
    skin_preview_texture: Option<egui::TextureHandle>,
    skin_overlay_layer: bool,
    skin_selected_part: String,
    downloads: crate::downloads::DownloadManager,
    download_url: String,
    active_install_task: Option<u64>,
    active_content_task: Option<u64>,
    active_java_task: Option<u64>,
    active_modpack_task: Option<u64>,
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
            auto_sodium: false,
            animations_enabled: preferences.animations_enabled,
            startup_at: Instant::now(),
            splash_complete: false,
            game_process: None,
            content_kind: "modpack".into(),
            content_tab: "browse".into(),
            loader_kind: "fabric".into(),
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
            pending_instance_launch: None,
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
            new_instance_name: String::new(),
            crash_analysis: None,
            play_stats: crate::stats::PlayStats::load(),
            session_started_at: None,
            session_started_epoch: 0,
            session_version: String::new(),
            session_instance: None,
            system_monitor: sysinfo::System::new_all(),
            last_metrics_refresh: Instant::now(),
            cpu_history: Vec::with_capacity(120),
            memory_history: Vec::with_capacity(120),
            game_cpu_history: Vec::with_capacity(120),
            game_memory_history: Vec::with_capacity(120),
            microsoft_rx: None,
            microsoft_account: Self::load_microsoft_account(),
            microsoft_code: None,
            skin_editor: None,
            skin_editor_profile: String::new(),
            skin_color: Color32::from_rgb(151, 169, 255),
            skin_rotation: 0.45,
            skin_preview_texture: None,
            skin_overlay_layer: false,
            skin_selected_part: "All".into(),
            downloads: crate::downloads::DownloadManager::default(),
            download_url: String::new(),
            active_install_task: None,
            active_content_task: None,
            active_java_task: None,
            active_modpack_task: None,
        };
        if let Some(saved_account) = app.microsoft_account.clone() {
            let (tx, rx) = mpsc::channel();
            app.microsoft_rx = Some(rx);
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<crust_core::authenticator::Account> {
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .enable_time()
                        .build()?;
                    runtime.block_on(async {
                        let auth = crust_core::authenticator::Authenticator::new()
                            .map_err(|error| anyhow::anyhow!("{error}"))?;
                        auth.refresh(&saved_account).await
                            .map_err(|error| anyhow::anyhow!("{error}"))
                    })
                })();
                match result {
                    Ok(account) => { let _ = tx.send(MicrosoftLoginEvent::Restored(account)); }
                    Err(error) => { let _ = tx.send(MicrosoftLoginEvent::RestoreFailed(format!("{error:#}"))); }
                }
            });
        }
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
                if let Some(id) = self.active_install_task.take() { self.downloads.finish_operation(id, Ok(())); }
                self.status = "Minecraft installation complete".into();
            }
            Some(Ok(Err(error))) => {
                if let Some(id) = self.active_install_task.take() { self.downloads.finish_operation(id, Err(error.clone())); }
                self.status = error;
                self.install_rx = None;
                self.progress_rx = None;
                self.install_progress = None;
                self.content_busy = false;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                if let Some(id) = self.active_install_task.take() { self.downloads.finish_operation(id, Err("Installation worker disconnected".into())); }
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
                self.finish_play_session();
            }
            Some(Ok(None)) => self.status = "Minecraft is running".into(),
            Some(Err(error)) => {
                self.status = format!("Could not read Minecraft process: {error}");
                self.game_process = None;
                self.finish_play_session();
            }
            None => {}
        }
        if self.install_rx.is_some() || self.game_process.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn begin_play_session(&mut self, version: String, instance: Option<String>) {
        if let Err(error) = self.play_stats.begin_session(version.clone(), instance.clone()) {
            self.status = format!("Could not checkpoint play session: {error}");
        }
        self.session_started_at = Some(Instant::now());
        self.session_started_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        self.session_version = version;
        self.session_instance = instance;
    }

    fn finish_play_session(&mut self) {
        if let Some(started) = self.session_started_at.take() {
            if let Some(instance_name) = self.session_instance.as_deref()
                && let Some(instance) = self.workspace_instances.iter().find(|entry| entry.name == instance_name)
            {
                let instance_path = crate::workspace::instance_path(&self.game_root, instance);
                if instance.backup_on_exit {
                match crate::workspace::backup_all_worlds(&instance_path) {
                    Ok(backups) if !backups.is_empty() => self.status = format!("Created {} automatic world backup(s)", backups.len()),
                    Ok(_) => {},
                    Err(error) => self.status = format!("Automatic world backup failed: {error}"),
                }
                }
            }
            if let Err(error) = self.play_stats.record(
                self.session_version.clone(),
                self.session_instance.take(),
                self.session_started_epoch,
                started.elapsed().as_secs(),
            ) {
                self.status = format!("Could not save play stats: {error}");
            }
        }
        self.session_version.clear();
        self.session_started_epoch = 0;
    }

    fn stop_game(&mut self) {
        if let Some(mut process) = self.game_process.take() {
            let _ = process.kill();
            let _ = process.wait();
            self.finish_play_session();
            self.status = "Minecraft stopped".into();
        }
    }

    fn launch_selected(&mut self) {
        let Some(microsoft_account) = self.microsoft_account.clone() else {
            self.status = "Sign in with a Microsoft account that owns Minecraft Java Edition to launch".into();
            self.screen = Screen::Profiles;
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
            self.active_modpack_task = Some(self.downloads.begin_operation(&format!("Prepare Fabric + Sodium · {minecraft}"), instance.clone()));
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
        let launch_result = launcher::launch_with_account(&installed, &microsoft_account, self.memory_mb, &self.performance_mode);
        match launch_result {
            Ok(process) => {
                self.installed_version = Some(installed.clone());
                self.game_process = Some(process);
                self.begin_play_session(installed.id.clone(), None);
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
        ui.add_space(9.0);
        let rect = ui.available_rect_before_wrap();
        ui.painter().line_segment([rect.left_top(), egui::pos2(rect.left() + 74.0, rect.top())], Stroke::new(2.0_f32, Color32::from_rgba_unmultiplied(151, 169, 255, 170)));
        ui.add_space(11.0);
    }

    fn nav_button(ui: &mut egui::Ui, label: &str, active: bool, uri: &'static str, icon: &'static [u8]) -> egui::Response {
        let text = if active {
            RichText::new(label).color(Color32::WHITE).strong()
        } else {
            RichText::new(label).color(Color32::from_rgb(174, 183, 204))
        };
        let image = egui::Image::from_bytes(uri, icon.to_vec())
            .fit_to_exact_size(Vec2::splat(18.0));
        let response = ui.add_sized([178.0, 42.0], egui::Button::image_and_text(image, text).fill(
            if active { Color32::from_rgba_unmultiplied(83, 105, 160, 150) } else { Color32::TRANSPARENT }
        ).stroke(Stroke::NONE));
        if active {
            let marker = egui::Rect::from_min_size(response.rect.left_top() + Vec2::new(1.0, 8.0), Vec2::new(3.0, response.rect.height() - 16.0));
            ui.painter().rect_filled(marker, egui::CornerRadius::same(2), Color32::from_rgb(174, 190, 255));
        }
        response
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
                    let navigation: [NavigationItem; 17] = [
                        ("Home", Screen::Home, "bytes://lunar-home.svg", include_bytes!("../assets/icons/home.svg")),
                        ("Play", Screen::Play, "bytes://lunar-play.svg", include_bytes!("../assets/icons/play.svg")),
                        ("Play Stats", Screen::PlayStats, "bytes://lunar-monitor.svg", include_bytes!("../assets/icons/monitor.svg")),
                        ("Versions", Screen::Versions, "bytes://lunar-versions.svg", include_bytes!("../assets/icons/versions.svg")),
                        ("Profiles", Screen::Profiles, "bytes://lunar-profiles.svg", include_bytes!("../assets/icons/profiles.svg")),
                        ("Content", Screen::Content, "bytes://lunar-content.svg", include_bytes!("../assets/icons/content.svg")),
                        ("Live Performance Manager", Screen::LivePerformance, "bytes://lunar-monitor.svg", include_bytes!("../assets/icons/monitor.svg")),
                        ("Mod Manager", Screen::Content, "bytes://lunar-mods.svg", include_bytes!("../assets/icons/mods.svg")),
                        ("Resource Packs", Screen::Content, "bytes://lunar-packs.svg", include_bytes!("../assets/icons/resource-packs.svg")),
                        ("Shader Manager", Screen::Content, "bytes://lunar-shaders.svg", include_bytes!("../assets/icons/shaders.svg")),
                        ("Launcher Customization", Screen::Settings, "bytes://lunar-settings.svg", include_bytes!("../assets/icons/settings.svg")),
                        ("Servers", Screen::Servers, "bytes://lunar-servers.svg", include_bytes!("../assets/icons/servers.svg")),
                        ("Skin Manager", Screen::SkinManager, "bytes://lunar-profiles.svg", include_bytes!("../assets/icons/profiles.svg")),
                        ("Download Center", Screen::Downloads, "bytes://lunar-download.svg", include_bytes!("../assets/icons/download.svg")),
                        ("Instance Manager", Screen::Instances, "bytes://lunar-versions.svg", include_bytes!("../assets/icons/versions.svg")),
                        ("Crash Analyzer", Screen::CrashAnalyzer, "bytes://lunar-monitor.svg", include_bytes!("../assets/icons/monitor.svg")),
                        ("Settings", Screen::Settings, "bytes://lunar-settings.svg", include_bytes!("../assets/icons/settings.svg")),
                    ];
                    for (label, screen, uri, icon) in navigation {
                        let category_active = match label {
                            "Mod Manager" => self.content_kind == "mod",
                            "Resource Packs" => self.content_kind == "resourcepack",
                            "Shader Manager" => self.content_kind == "shader",
                            "Content" => self.content_kind == "modpack",
                            _ => true,
                        };
                        let active = self.screen == screen && category_active;
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
        Self::heading(ui, "Play", "Sign in with Microsoft, choose a Minecraft version and launch.");
        ui.group(|ui| {
            ui.label(RichText::new("MICROSOFT PLAYER").size(10.0).color(Color32::GRAY).strong());
            ui.add_space(6.0);
            if let Some(account_name) = self.microsoft_account.as_ref().map(|account| account.name.clone()) {
                ui.label(RichText::new(account_name).size(18.0).strong());
                ui.label(RichText::new("Authenticated Minecraft account").size(11.0).color(Color32::GRAY));
            } else {
                ui.label("No Microsoft account connected");
                if ui.button("Connect Microsoft account").clicked() {
                    self.screen = Screen::Profiles;
                }
            }
            if ui.button("Manage accounts").clicked() {
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
        ui.group(|ui| {
            ui.label(RichText::new("LOADER INSTALLER").size(10.0).strong().color(self.accent));
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("version_loader_kind")
                    .selected_text(self.loader_kind.to_uppercase())
                    .show_ui(ui, |ui| {
                        for loader in ["vanilla", "fabric", "forge", "neoforge", "quilt"] {
                            ui.selectable_value(&mut self.loader_kind, loader.to_owned(), loader.to_uppercase());
                        }
                    });
                let ready = !self.selected_version.is_empty() && self.install_rx.is_none() && !self.content_busy;
                if ui.add_enabled(ready, egui::Button::new(format!("Install {} profile", self.loader_kind.to_uppercase()))).clicked() {
                    let minecraft = self.selected_version.clone();
                    let loader = self.loader_kind.clone();
                    let root = self.game_root.clone();
                    let instance = root.join("loader-profiles").join(format!("{minecraft}-{loader}"));
                    let (tx, rx) = mpsc::channel();
                    self.install_rx = Some(rx);
                    self.active_install_task = Some(self.downloads.begin_operation(&format!("Install {loader} · Minecraft {minecraft}"), instance.clone()));
                    self.content_busy = true;
                    self.status = format!("Installing {loader} for Minecraft {minecraft}…");
                    std::thread::spawn(move || {
                        let result = crate::loaders::prepare_instance(&minecraft, Some(&loader), &root, &instance)
                            .map_err(|error| format!("{error:#}"));
                        let _ = tx.send(result);
                    });
                }
            });
            ui.label(RichText::new("Installs a compatible game profile and its required loader libraries. Select a Minecraft version below first.").size(10.0).color(Color32::GRAY));
        });
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

    fn microsoft_account_entry() -> anyhow::Result<keyring::Entry> {
        keyring::Entry::new("Lunar Launcher", "microsoft-account")
            .map_err(|error| anyhow::anyhow!("Could not access the system credential store: {error}"))
    }

    fn load_microsoft_account() -> Option<crust_core::authenticator::Account> {
        Self::microsoft_account_entry().ok()?
            .get_password().ok()
            .and_then(|value| serde_json::from_str(&value).ok())
    }

    fn save_microsoft_account(account: &crust_core::authenticator::Account) -> anyhow::Result<()> {
        let serialized = serde_json::to_string(account)?;
        Self::microsoft_account_entry()?.set_password(&serialized)
            .map_err(|error| anyhow::anyhow!("Could not save Microsoft sign-in to the system credential store: {error}"))
    }

    fn delete_microsoft_account() -> anyhow::Result<()> {
        let entry = Self::microsoft_account_entry()?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(anyhow::anyhow!("Could not remove saved Microsoft sign-in: {error}")),
        }
    }

    fn start_microsoft_login(&mut self) {
        if self.microsoft_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.microsoft_rx = Some(rx);
        self.microsoft_code = None;
        self.status = "Starting Microsoft sign-in…".into();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_time()
                    .build()?;
                runtime.block_on(async {
                    let auth = crust_core::authenticator::Authenticator::new()
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    let flow = auth.device_code().await
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    let code = flow.user_code().to_owned();
                    let url = flow.verification_uri_complete();
                    tx.send(MicrosoftLoginEvent::DeviceCode { code, url })
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    let account = flow.wait().await
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    tx.send(MicrosoftLoginEvent::Complete(account))
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    Ok::<(), anyhow::Error>(())
                })?;
                Ok(())
            })();
            if let Err(error) = result {
                let _ = tx.send(MicrosoftLoginEvent::Failed(format!("{error:#}")));
            }
        });
    }

    fn poll_microsoft_login(&mut self, ctx: &egui::Context) {
        let mut keep_polling = false;
        if let Some(rx) = &self.microsoft_rx {
            loop {
                match rx.try_recv() {
                    Ok(MicrosoftLoginEvent::DeviceCode { code, url }) => {
                        self.microsoft_code = Some((code, url));
                        self.status = "Enter the displayed code on Microsoft's sign-in page".into();
                    }
                    Ok(MicrosoftLoginEvent::Complete(account)) => {
                        let account_name = account.name.clone();
                        self.microsoft_account = Some(account.clone());
                        self.status = match Self::save_microsoft_account(&account) {
                            Ok(()) => format!("Microsoft account connected and securely saved: {account_name}"),
                            Err(error) => format!("Microsoft account connected for this session, but could not be saved: {error}"),
                        };
                        self.microsoft_code = None;
                        self.microsoft_rx = None;
                        return;
                    }
                    Ok(MicrosoftLoginEvent::Restored(account)) => {
                        let account_name = account.name.clone();
                        self.microsoft_account = Some(account.clone());
                        self.status = match Self::save_microsoft_account(&account) {
                            Ok(()) => format!("Saved Microsoft account restored: {account_name}"),
                            Err(error) => format!("Microsoft account restored, but token refresh could not be saved: {error}"),
                        };
                        self.microsoft_rx = None;
                        return;
                    }
                    Ok(MicrosoftLoginEvent::RestoreFailed(error)) => {
                        self.status = format!("Saved Microsoft session could not refresh: {error}");
                        self.microsoft_rx = None;
                        return;
                    }
                    Ok(MicrosoftLoginEvent::Failed(error)) => {
                        self.status = format!("Microsoft sign-in failed: {error}");
                        self.microsoft_code = None;
                        self.microsoft_rx = None;
                        return;
                    }
                    Err(TryRecvError::Empty) => {
                        keep_polling = true;
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.status = "Microsoft sign-in stopped unexpectedly".into();
                        self.microsoft_code = None;
                        self.microsoft_rx = None;
                        return;
                    }
                }
            }
        }
        if keep_polling {
            ctx.request_repaint_after(Duration::from_millis(300));
        }
    }

    fn profiles(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Profiles", "Connect a Microsoft account to play Minecraft Java Edition.");
        ui.group(|ui| {
            ui.label(RichText::new("MICROSOFT ACCOUNT").size(10.0).color(self.accent).strong());
            if let Some(account_name) = self.microsoft_account.as_ref().map(|account| account.name.clone()) {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(account_name).size(17.0).strong());
                        ui.label(RichText::new("Microsoft authenticated · Minecraft Java profile").size(11.0).color(Color32::GRAY));
                    });
                    if ui.button("Sign out").clicked() {
                        self.microsoft_account = None;
                        self.microsoft_rx = None;
                        self.microsoft_code = None;
                        self.status = match Self::delete_microsoft_account() {
                            Ok(()) => "Microsoft account signed out".into(),
                            Err(error) => error.to_string(),
                        };
                    }
                });
            } else if self.microsoft_rx.is_some() {
                ui.label("Waiting for Microsoft authorization…");
                if let Some((code, url)) = &self.microsoft_code {
                    ui.label(RichText::new(format!("CODE  {code}")).size(22.0).strong().monospace().color(self.accent));
                    ui.hyperlink_to("Open Microsoft device sign-in", url);
                    ui.label(RichText::new("Complete sign-in in your browser. Lunar never asks for your Microsoft password.").size(11.0).color(Color32::GRAY));
                }
            } else if ui.button("Sign in with Microsoft").clicked() {
                self.start_microsoft_login();
            }
        });
        ui.add_space(12.0);
        ui.label(RichText::new("LOCAL PROFILE LABELS").size(10.0).color(Color32::GRAY).strong());
        ui.label(RichText::new("Local profile labels do not authenticate or grant game ownership. A Microsoft account with Minecraft Java Edition is required to launch.").size(11.0).color(Color32::GRAY));
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
                    self.active_install_task = Some(self.downloads.begin_operation(&format!("Install Fabric + Sodium · {minecraft}"), instance.clone()));
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
        if self.content_kind == "modpack" {
            if ui.button("Import local .mrpack").clicked()
                && let Some(source) = Self::choose_content_file("modpack")
            {
                let root = self.game_root.clone();
                let (tx, rx) = mpsc::channel();
                self.content_install_rx = Some(rx);
                self.active_content_task = Some(self.downloads.begin_operation("Import Modrinth modpack", root.clone()));
                self.content_busy = true;
                self.status = format!("Importing {}…", source.file_name().unwrap_or_default().to_string_lossy());
                std::thread::spawn(move || {
                    let result = (|| -> anyhow::Result<String> {
                        let bytes = fs::read(&source)?;
                        crate::content::install_modpack_bytes(&bytes, &root)
                    })().map_err(|error| format!("{error:#}"));
                    let _ = tx.send(result);
                });
            }
        } else {
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
        if self.content_kind != "modpack" {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.content_tab, "browse".into(), "Browse");
                ui.selectable_value(&mut self.content_tab, "installed".into(), "Installed");
            });
            if self.content_tab == "installed" {
                let folder = match self.content_kind.as_str() {
                    "mod" => "mods",
                    "resourcepack" => "resourcepacks",
                    _ => "shaderpacks",
                };
                let files = crate::content::list_files(&content_root, folder).unwrap_or_default();
                ui.label(RichText::new(format!("{} installed item(s)", files.len())).size(11.0).color(Color32::GRAY));
                if files.is_empty() {
                    ui.group(|ui| {
                        ui.label(RichText::new("Nothing installed yet").strong());
                        ui.label("Packages installed for this target will appear here.");
                    });
                }
                egui::ScrollArea::vertical().max_height(430.0).show(ui, |ui| {
                    for path in files {
                        let filename = path.file_name().and_then(|value| value.to_str()).unwrap_or("Unknown package").to_owned();
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&filename).strong());
                                    ui.label(RichText::new(path.display().to_string()).size(9.0).color(Color32::GRAY));
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.button("Uninstall").clicked() {
                                        match crate::content::remove_file(&path, &content_root, folder) {
                                            Ok(()) => self.status = format!("Uninstalled {filename}"),
                                            Err(error) => self.status = format!("Could not uninstall {filename}: {error}"),
                                        }
                                    }
                                });
                            });
                        });
                    }
                });
                return;
            }
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
                            let install_root = if self.content_kind == "modpack" { &self.game_root } else { &content_root };
                            let already_installed = crate::content::project_is_installed(install_root, &self.content_kind, &project.project_id)
                                || (self.content_kind == "modpack" && self.installed_packs.iter().any(|pack| pack.name.eq_ignore_ascii_case(&project.title)));
                            let install_ready = !self.content_busy
                                && !already_installed
                                && (matches!(self.content_kind.as_str(), "modpack" | "resourcepack" | "shader")
                                    || !target_minecraft.is_empty());
                            let action_label = if already_installed { "Installed" } else { "Install" };
                            if ui.add_enabled(install_ready, egui::Button::image_and_text(install_icon, action_label)).clicked() {
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
                                self.active_content_task = Some(self.downloads.begin_operation("Install Modrinth content", root.clone()));
                                self.content_busy = true;
                                self.status = format!("Installing {}…", project.title);
                                std::thread::spawn(move || {
                                    let result = (|| -> anyhow::Result<String> {
                                        let target = instance_target.as_deref().unwrap_or(&root);
                                        let installed = match kind.as_str() {
                                            "resourcepack" => crate::content::install_resource_pack(&id, target, &minecraft)?,
                                            "shader" => crate::content::install_shader_pack(&id, target, &minecraft)?,
                                            "mod" => {
                                                crate::content::install_mod(&id, target, &minecraft, &loader)?;
                                                if let Some(instance) = selected_instance {
                                                    let mut updated = instance;
                                                    updated.loader = Some(loader.clone());
                                                    crate::workspace::save_instance(&root, &updated)?;
                                                    crate::loaders::prepare_instance(&minecraft, Some(&loader), &root, target)?;
                                                }
                                                format!("Installed mod and required dependencies to {}", target.display())
                                            }
                                            _ => crate::content::install_modpack(&id, &root)?,
                                        };
                                        let registry_root = if kind == "modpack" { root.as_path() } else { target };
                                        crate::content::mark_project_installed(registry_root, &kind, &id, &installed)?;
                                        Ok(installed)
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
                        if ui.add_enabled(!self.content_busy, egui::Button::new("Resolve dependencies")).clicked() {
                            let instance = pack.instance.clone();
                            let minecraft = pack.minecraft.clone();
                            let loader = pack.loaders.keys().find(|key| key.as_str() != "minecraft")
                                .map(|key| key.trim_end_matches("-loader").to_owned())
                                .unwrap_or_else(|| "fabric".to_owned());
                            let (tx, rx) = mpsc::channel();
                            self.content_install_rx = Some(rx);
                            self.active_content_task = Some(self.downloads.begin_operation("Resolve required mod dependencies", instance.clone()));
                            self.content_busy = true;
                            self.status = format!("Checking required dependencies for {}…", pack.name);
                            std::thread::spawn(move || {
                                let result = crate::content::resolve_instance_dependencies(&instance, &minecraft, &loader)
                                    .map(|installed| if installed.is_empty() {
                                        "Dependency check complete: no missing required dependencies found".to_owned()
                                    } else {
                                        format!("Resolved {} required dependency file(s)", installed.len())
                                    })
                                    .map_err(|error| format!("{error:#}"));
                                let _ = tx.send(result);
                            });
                        }
                        if ui.add_enabled(!self.content_busy, egui::Button::new("Scan compatibility")).clicked() {
                            let instance = pack.instance.clone();
                            let minecraft = pack.minecraft.clone();
                            let loader = pack.loaders.keys().find(|key| key.as_str() != "minecraft")
                                .map(|key| key.trim_end_matches("-loader").to_owned())
                                .unwrap_or_else(|| "fabric".to_owned());
                            let (tx, rx) = mpsc::channel();
                            self.content_install_rx = Some(rx);
                            self.active_content_task = Some(self.downloads.begin_operation("Scan mod compatibility", instance.clone()));
                            self.content_busy = true;
                            self.status = format!("Inspecting installed mod metadata for {}…", pack.name);
                            std::thread::spawn(move || {
                                let result = crate::content::analyze_instance_compatibility(&instance, &minecraft, &loader)
                                    .map(|issues| issues.join("\n"))
                                    .map_err(|error| format!("{error:#}"));
                                let _ = tx.send(result);
                            });
                        }
                        if ui.add_enabled(self.game_process.is_none() && self.modpack_launch_rx.is_none() && !self.content_busy, egui::Button::new("Launch")).clicked() {
                            if self.microsoft_account.is_some() {
                                let root = self.game_root.clone();
                                let (tx, rx) = mpsc::channel();
                                self.modpack_launch_rx = Some(rx);
                                self.active_modpack_task = Some(self.downloads.begin_operation("Prepare modpack instance", root.clone()));
                                self.status = format!("Preparing {}…", pack.name);
                                std::thread::spawn(move || { let _ = tx.send(crate::loaders::prepare_modpack(&pack, &root).map_err(|error| format!("{error:#}"))); });
                            } else { self.status = "Sign in with Microsoft to launch a modpack".into(); }
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

    fn performance_history_chart(ui: &mut egui::Ui, title: &str, history: &[f32], accent: Color32) {
        ui.group(|ui| {
            ui.label(RichText::new(title).size(10.0).strong().color(accent));
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 112.0), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            for step in 0..=4 {
                let y = rect.top() + rect.height() * step as f32 / 4.0;
                painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(160, 175, 205, 28)));
            }
            if history.len() > 1 {
                let points: Vec<_> = history.iter().enumerate().map(|(index, value)| {
                    let x = rect.left() + rect.width() * index as f32 / (history.len() - 1) as f32;
                    let y = rect.bottom() - rect.height() * value.clamp(0.0, 100.0) / 100.0;
                    egui::pos2(x, y)
                }).collect();
                painter.add(egui::Shape::line(points, Stroke::new(2.2_f32, accent)));
            }
            ui.label(RichText::new("0%").size(9.0).color(Color32::GRAY));
        });
    }

    fn live_performance_manager(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Live Performance Manager", "A live view of what Minecraft and your system are actually using.");

        let cpu = self.system_monitor.global_cpu_usage().clamp(0.0, 100.0);
        let total_memory = self.system_monitor.total_memory() / 1024 / 1024;
        let used_memory = self.system_monitor.used_memory() / 1024 / 1024;
        let memory_percent = if total_memory > 0 { used_memory as f32 / total_memory as f32 * 100.0 } else { 0.0 };
        let game_metrics = self.game_process.as_ref().and_then(|process| {
            self.system_monitor.process(sysinfo::Pid::from_u32(process.id())).map(|game| (
                game.cpu_usage().clamp(0.0, 100.0),
                game.memory() / 1024 / 1024,
                process.id(),
            ))
        });

        ui.horizontal_wrapped(|ui| {
            for (label, value, detail) in [
                ("SYSTEM CPU", format!("{cpu:.0}%"), format!("{} logical threads", self.system_monitor.cpus().len())),
                ("SYSTEM RAM", format!("{used_memory} MB"), format!("{memory_percent:.0}% of {total_memory} MB")),
                ("MINECRAFT RAM", game_metrics.as_ref().map(|m| format!("{} MB", m.1)).unwrap_or_else(|| "Offline".into()), game_metrics.as_ref().map(|m| format!("Java PID {}", m.2)).unwrap_or_else(|| "Launch Minecraft to monitor".into())),
            ] {
                egui::Frame::group(ui.style())
                    .fill(Color32::from_rgba_unmultiplied(22, 30, 49, 205))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(self.accent.r(), self.accent.g(), self.accent.b(), 90)))
                    .corner_radius(egui::CornerRadius::same(14))
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_min_width(170.0);
                        ui.label(RichText::new(label).size(10.0).strong().color(self.accent));
                        ui.label(RichText::new(value).size(25.0).strong());
                        ui.label(RichText::new(detail).size(10.0).color(Color32::GRAY));
                    });
            }
        });

        ui.add_space(12.0);
        ui.columns(2, |columns| {
            columns[0].group(|ui| {
                ui.label(RichText::new("SYSTEM LOAD").strong().color(self.accent));
                ui.add(egui::ProgressBar::new(cpu / 100.0).text(format!("CPU · {cpu:.0}%")));
                ui.add(egui::ProgressBar::new(memory_percent / 100.0).text(format!("RAM · {memory_percent:.0}%")));
                ui.label(RichText::new(format!("{} logical CPU threads · {} MB total memory", self.system_monitor.cpus().len(), total_memory)).size(10.0).color(Color32::GRAY));
            });
            columns[1].group(|ui| {
                ui.label(RichText::new("MINECRAFT PROCESS").strong().color(self.accent));
                if let Some((game_cpu, game_memory, pid)) = game_metrics {
                    ui.label(RichText::new(format!("{game_cpu:.1}% CPU")).size(22.0).strong());
                    ui.label(format!("{game_memory} MB working memory"));
                    ui.label(RichText::new(format!("Process ID {pid}")).size(10.0).color(Color32::GRAY));
                    ui.add(egui::ProgressBar::new((game_cpu / 100.0).clamp(0.0, 1.0)).text("Process CPU"));
                } else {
                    ui.label("Minecraft is not running through Lunar right now.");
                }
            });
        });

        ui.add_space(12.0);
        ui.group(|ui| {
            ui.label(RichText::new("LAUNCH PROFILE").strong().color(self.accent));
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("live_performance_profile")
                    .selected_text(&self.performance_mode)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.performance_mode, "Maximum FPS".into(), "Maximum FPS");
                        ui.selectable_value(&mut self.performance_mode, "Performance".into(), "Performance");
                        ui.selectable_value(&mut self.performance_mode, "Balanced".into(), "Balanced");
                        ui.selectable_value(&mut self.performance_mode, "Custom".into(), "Custom");
                    });
                if ui.button("Use recommended").clicked() {
                    self.performance_mode = "Performance".into();
                    self.memory_mb = self.memory_mb.clamp(2048, 8192);
                    self.status = "Recommended launch profile selected for the next session".into();
                }
            });
            ui.add(egui::Slider::new(&mut self.memory_mb, 2048..=8192).step_by(512.0).suffix(" MB"));
            ui.horizontal(|ui| {
                if ui.button("Save changes").clicked() {
                    LauncherPreferences {
                        memory_mb: self.memory_mb,
                        snapshots: self.snapshots,
                        background_gif: self.background_gif.clone(),
                        performance_mode: self.performance_mode.clone(),
                        auto_sodium: self.auto_sodium,
                        animations_enabled: self.animations_enabled,
                        accent_rgb: [self.accent.r(), self.accent.g(), self.accent.b()],
                    }.save();
                    self.status = "Performance profile saved".into();
                }
            });
            ui.label(RichText::new("Changes apply on the next launch. Memory allocation is a maximum heap limit, not a direct FPS setting.").size(10.0).color(Color32::GRAY));
        });

        ui.add_space(10.0);
        egui::CollapsingHeader::new("Recent system and Minecraft history")
            .default_open(false)
            .show(ui, |ui| {
                ui.columns(2, |columns| {
                    Self::performance_history_chart(&mut columns[0], "SYSTEM CPU", &self.cpu_history, self.accent);
                    Self::performance_history_chart(&mut columns[1], "SYSTEM RAM", &self.memory_history, self.accent);
                });
                ui.columns(2, |columns| {
                    Self::performance_history_chart(&mut columns[0], "MINECRAFT CPU", &self.game_cpu_history, self.accent);
                    Self::performance_history_chart(&mut columns[1], "MINECRAFT RAM", &self.game_memory_history, self.accent);
                });
            });
        ui.label(RichText::new("Lunar measures CPU and memory locally. FPS, GPU utilization, frame-time and server ping are not reported unless a game-side or hardware telemetry source is available.").size(10.0).color(Color32::GRAY));
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }

    fn download_center_ui(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Download Center", "Track direct downloads and Lunar installation tasks in one persistent history.");
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(RichText::new("ADD DOWNLOAD").strong().size(11.0).color(self.accent));
            ui.horizontal(|ui| {
                let url_width = (ui.available_width() - 116.0).max(120.0);
                ui.add_sized([url_width, 34.0], egui::TextEdit::singleline(&mut self.download_url).hint_text("Paste an HTTPS download URL"));
                if ui.add_enabled(!self.download_url.trim().is_empty(), egui::Button::new("Download")).clicked() {
                    match self.downloads.enqueue(self.download_url.trim(), self.game_root.join("downloads")) {
                        Ok(id) => {
                            self.status = format!("Download #{id} queued");
                            self.download_url.clear();
                        }
                        Err(error) => self.status = format!("Could not queue download: {error}"),
                    }
                }
            });
            ui.label(RichText::new("Files are saved in Lunar's downloads folder. Only HTTP and HTTPS links are accepted.").size(10.0).color(Color32::GRAY));
        });
        ui.add_space(12.0);
        let tasks = self.downloads.tasks().to_vec();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("DOWNLOAD HISTORY · {}", tasks.len())).strong().color(self.accent));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(tasks.iter().any(|task| matches!(&task.state, crate::downloads::DownloadState::Completed | crate::downloads::DownloadState::Failed(_) | crate::downloads::DownloadState::Cancelled)), egui::Button::new("Clear finished")).clicked() {
                    self.downloads.clear_finished();
                }
            });
        });
        if tasks.is_empty() {
            ui.group(|ui| {
                ui.label(RichText::new("No downloads yet").strong());
                ui.label("Downloads you add here will appear with live progress, pause and retry controls.");
            });
        }
        egui::ScrollArea::vertical().max_height(500.0).show(ui, |ui| {
            for task in tasks.iter().rev() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&task.filename).strong());
                            ui.label(RichText::new(task.destination.display().to_string()).size(9.0).color(Color32::GRAY));
                            ui.label(RichText::new(task.state.label()).size(10.0).color(match &task.state {
                                crate::downloads::DownloadState::Completed => Color32::from_rgb(112, 215, 157),
                                crate::downloads::DownloadState::Failed(_) => Color32::from_rgb(230, 130, 130),
                                crate::downloads::DownloadState::Cancelled => Color32::GRAY,
                                _ => self.accent,
                            }));
                        });
                        if task.url.starts_with("lunar://") {
                            ui.label(RichText::new("LUNAR TASK").size(9.0).strong().color(self.accent));
                        } else {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if matches!(&task.state, crate::downloads::DownloadState::Queued | crate::downloads::DownloadState::Downloading | crate::downloads::DownloadState::Paused)
                                && ui.small_button("Cancel").clicked()
                            {
                                self.downloads.cancel(task.id);
                            }
                            match &task.state {
                                crate::downloads::DownloadState::Queued | crate::downloads::DownloadState::Downloading => {
                                    if ui.small_button("Pause").clicked() { self.downloads.pause(task.id); }
                                }
                                crate::downloads::DownloadState::Paused => {
                                    if ui.small_button("Resume").clicked() { self.downloads.resume(task.id); }
                                }
                                crate::downloads::DownloadState::Failed(_) | crate::downloads::DownloadState::Cancelled => {
                                    if ui.small_button("Retry").clicked() {
                                        match self.downloads.retry(task.id) {
                                            Ok(new_id) => self.status = format!("Retry #{new_id} queued"),
                                            Err(error) => self.status = error.to_string(),
                                        }
                                    }
                                }
                                crate::downloads::DownloadState::Completed => {}
                            }
                            });
                        }
                    });
                    if let Some(total) = task.total {
                        let fraction = if total == 0 { 0.0 } else { (task.downloaded as f32 / total as f32).clamp(0.0, 1.0) };
                        ui.add(egui::ProgressBar::new(fraction).text(format!("{:.1} / {:.1} MB", task.downloaded as f64 / 1_048_576.0, total as f64 / 1_048_576.0)));
                    } else if !task.url.starts_with("lunar://") && matches!(&task.state, crate::downloads::DownloadState::Downloading | crate::downloads::DownloadState::Paused) {
                        ui.add(egui::ProgressBar::new(0.0).text(format!("{:.1} MB downloaded", task.downloaded as f64 / 1_048_576.0)));
                    }
                    egui::CollapsingHeader::new(format!("Activity log · {} entries", task.logs.len()))
                        .id_salt(("download-log", task.id))
                        .show(ui, |ui| {
                            for entry in task.logs.iter().rev().take(8).rev() {
                                ui.label(RichText::new(entry).size(10.0).color(Color32::GRAY));
                            }
                        });
                });
            }
        });
    }

    fn skin_preview(&mut self, ui: &mut egui::Ui, skin: &image::RgbaImage) {
        ui.label(RichText::new("3D CHARACTER PREVIEW").size(10.0).strong().color(self.accent));
        let (rect, response) = ui.allocate_exact_size(Vec2::new(300.0, 310.0), egui::Sense::drag());
        if response.dragged() {
            self.skin_rotation += response.drag_delta().x * 0.012;
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, egui::CornerRadius::same(16), Color32::from_rgba_unmultiplied(16, 22, 37, 235));
        painter.circle_filled(egui::pos2(rect.center().x, rect.bottom() - 35.0), 76.0, Color32::from_rgba_unmultiplied(90, 112, 170, 18));
        let sample = |x: u32, y: u32, w: u32, h: u32| {
            let scale_x = (skin.width() / 64).max(1);
            let scale_y = (skin.height() / 64).max(1);
            let x0 = x.saturating_mul(scale_x).min(skin.width().saturating_sub(1));
            let y0 = y.saturating_mul(scale_y).min(skin.height().saturating_sub(1));
            let x1 = (x + w).saturating_mul(scale_x).min(skin.width());
            let y1 = (y + h).saturating_mul(scale_y).min(skin.height());
            let mut total = [0_u64; 4];
            let mut count = 0_u64;
            for py in y0..y1.max(y0 + 1) {
                for px in x0..x1.max(x0 + 1) {
                    let pixel = skin.get_pixel(px.min(skin.width()-1), py.min(skin.height()-1));
                    for channel in 0..4 { total[channel] += pixel[channel] as u64; }
                    count += 1;
                }
            }
            Color32::from_rgba_unmultiplied((total[0]/count) as u8, (total[1]/count) as u8, (total[2]/count) as u8, (total[3]/count) as u8)
        };
        let parts = [
            ((0.0, 2.65, 0.0), (1.12, 1.12, 1.12), sample(8, 8, 8, 8), [
                (8, 8, 8, 8), (0, 8, 8, 8), (16, 8, 8, 8),
                (8, 0, 8, 8), (16, 0, 8, 8), (24, 8, 8, 8),
            ]),
            ((0.0, 1.35, 0.0), (1.0, 1.42, 0.55), sample(20, 20, 8, 12), [
                (20, 20, 8, 12), (16, 20, 4, 12), (28, 20, 4, 12),
                (20, 16, 8, 4), (28, 16, 8, 4), (32, 20, 8, 12),
            ]),
            ((-0.82, 1.35, 0.0), (0.42, 1.35, 0.5), sample(44, 20, 4, 12), [
                (44, 20, 4, 12), (40, 20, 4, 12), (48, 20, 4, 12),
                (44, 16, 4, 4), (48, 16, 4, 4), (52, 20, 4, 12),
            ]),
            ((0.82, 1.35, 0.0), (0.42, 1.35, 0.5), sample(36, 52, 4, 12), [
                (36, 52, 4, 12), (32, 52, 4, 12), (40, 52, 4, 12),
                (36, 48, 4, 4), (40, 48, 4, 4), (44, 52, 4, 12),
            ]),
            ((-0.28, 0.32, 0.0), (0.46, 1.35, 0.52), sample(4, 20, 4, 12), [
                (4, 20, 4, 12), (0, 20, 4, 12), (8, 20, 4, 12),
                (4, 16, 4, 4), (8, 16, 4, 4), (12, 20, 4, 12),
            ]),
            ((0.28, 0.32, 0.0), (0.46, 1.35, 0.52), sample(20, 52, 4, 12), [
                (20, 52, 4, 12), (16, 52, 4, 12), (24, 52, 4, 12),
                (20, 48, 4, 4), (24, 48, 4, 4), (28, 52, 4, 12),
            ]),
        ];
        let texture_image = egui::ColorImage::from_rgba_unmultiplied(
            [skin.width() as usize, skin.height() as usize],
            skin.as_raw(),
        );
        if let Some(texture) = self.skin_preview_texture.as_mut() {
            texture.set(texture_image, egui::TextureOptions::NEAREST);
        } else {
            self.skin_preview_texture = Some(ui.ctx().load_texture("lunar-skin-preview", texture_image, egui::TextureOptions::NEAREST));
        }
        let skin_texture_id = self.skin_preview_texture.as_ref().map(egui::TextureHandle::id);
        let yaw = self.skin_rotation;
        let (sin, cos) = yaw.sin_cos();
        let project = |point: (f32, f32, f32)| {
            let rx = point.0 * cos + point.2 * sin;
            let rz = -point.0 * sin + point.2 * cos;
            (egui::pos2(rect.center().x + rx * 45.0, rect.center().y - point.1 * 45.0 + rz * 9.0), rz)
        };
        let mut faces: Vec<(f32, Vec<egui::Pos2>, Color32)> = Vec::new();
        let mut textured_faces: Vec<SkinTexturedFace> = Vec::new();
        for (center, size, base, uv_faces) in parts {
            let (cx, cy, cz) = center;
            let (sx, sy, sz) = (size.0/2.0, size.1/2.0, size.2/2.0);
            let vertices = [
                (cx-sx,cy-sy,cz-sz),(cx+sx,cy-sy,cz-sz),(cx+sx,cy+sy,cz-sz),(cx-sx,cy+sy,cz-sz),
                (cx-sx,cy-sy,cz+sz),(cx+sx,cy-sy,cz+sz),(cx+sx,cy+sy,cz+sz),(cx-sx,cy+sy,cz+sz),
            ];
            let indices = [[4,5,6,7],[0,4,7,3],[5,1,2,6],[3,7,6,2],[0,1,5,4],[1,0,3,2]];
            for (face_index, face) in indices.iter().enumerate() {
                let projected = face.iter().map(|index| project(vertices[*index])).collect::<Vec<_>>();
                let depth = projected.iter().map(|point| point.1).sum::<f32>() / 4.0;
                let factor = [1.0_f32, 0.62, 0.82, 1.16, 0.48, 0.74][face_index];
                if face_index < uv_faces.len() {
                    textured_faces.push((depth, [projected[0].0, projected[1].0, projected[2].0, projected[3].0], uv_faces[face_index]));
                } else {
                    let color = Color32::from_rgba_unmultiplied(
                        (base.r() as f32 * factor).clamp(0.0,255.0) as u8,
                        (base.g() as f32 * factor).clamp(0.0,255.0) as u8,
                        (base.b() as f32 * factor).clamp(0.0,255.0) as u8,
                        base.a(),
                    );
                    faces.push((depth, projected.into_iter().map(|point| point.0).collect(), color));
                }
            }
        }
        faces.sort_by(|a,b| a.0.total_cmp(&b.0));
        for (_, points, color) in faces {
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(5, 8, 16, 190))));
        }
        if let Some(texture_id) = skin_texture_id {
            textured_faces.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut mesh = egui::Mesh::with_texture(texture_id);
            for (_, points, (x, y, width, height)) in textured_faces {
                let sx = (skin.width() / 64).max(1);
                let sy = (skin.height() / 64).max(1);
                let u0 = x * sx;
                let v0 = y * sy;
                let u1 = (x + width) * sx;
                let v1 = (y + height) * sy;
                let uv = [
                    egui::pos2(u0 as f32 / skin.width() as f32, v1 as f32 / skin.height() as f32),
                    egui::pos2(u1 as f32 / skin.width() as f32, v1 as f32 / skin.height() as f32),
                    egui::pos2(u1 as f32 / skin.width() as f32, v0 as f32 / skin.height() as f32),
                    egui::pos2(u0 as f32 / skin.width() as f32, v0 as f32 / skin.height() as f32),
                ];
                let start = mesh.vertices.len() as u32;
                for index in 0..4 {
                    mesh.vertices.push(egui::epaint::Vertex { pos: points[index], uv: uv[index], color: Color32::WHITE });
                }
                mesh.add_triangle(start, start + 1, start + 2);
                mesh.add_triangle(start, start + 2, start + 3);
            }
            painter.add(egui::Shape::mesh(mesh));
        }
        painter.text(egui::pos2(rect.center().x, rect.bottom()-16.0), egui::Align2::CENTER_CENTER, "DRAG TO ROTATE", FontId::proportional(9.0), Color32::from_gray(145));
    }

    fn skin_manager_ui(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Skin Manager", "Create, paint, preview and apply a custom Minecraft skin.");
        let selected = self.profiles.selected.clone().unwrap_or_default();
        if selected.is_empty() {
            ui.label(RichText::new("Create or select an offline profile before editing a skin.").color(Color32::LIGHT_RED));
            return;
        }
        let skin_dir = self.game_root.join("CustomSkinLoader").join("LocalSkin").join("skins");
        let skin_path = skin_dir.join(format!("{selected}.png"));
        if self.skin_editor_profile != selected {
            self.skin_editor = image::open(&skin_path).ok().map(|image| image.to_rgba8());
            self.skin_editor_profile = selected.clone();
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("ACTIVE PROFILE").size(10.0).strong().color(self.accent));
            ui.label(RichText::new(&selected).size(18.0).strong());
            if skin_path.exists() {
                ui.label(RichText::new("● Skin installed").color(Color32::from_rgb(112, 224, 185)));
            } else {
                ui.label(RichText::new("No saved skin").color(Color32::GRAY));
            }
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.skin_overlay_layer, false, "Base layer");
            ui.add_enabled_ui(self.skin_editor.as_ref().is_some_and(|skin| skin.height() >= 64), |ui| {
                ui.selectable_value(&mut self.skin_overlay_layer, true, "Outer layer");
            });
            egui::ComboBox::from_id_salt("skin_body_part").selected_text(format!("Paint: {}", self.skin_selected_part)).show_ui(ui, |ui| {
                for part in ["All", "Head", "Body", "Arms", "Legs"] { ui.selectable_value(&mut self.skin_selected_part, part.to_owned(), part); }
            });
            if ui.button("Import PNG").clicked()
                && let Some(source) = Self::choose_content_file("skin")
            {
                let result = (|| -> anyhow::Result<image::RgbaImage> {
                    let image = image::open(&source)?.to_rgba8();
                    let (width, height) = image.dimensions();
                    let valid = (width == height && width >= 64 && width % 64 == 0)
                        || (width == height * 2 && width % 64 == 0 && height % 32 == 0);
                    if !valid {
                        anyhow::bail!("Use a Minecraft skin layout such as 64×64 or 64×32 (HD multiples are supported)");
                    }
                    Ok(image)
                })();
                match result {
                    Ok(image) => { self.skin_editor = Some(image); self.status = "Skin loaded into the editor".into(); }
                    Err(error) => self.status = format!("Skin import failed: {error:#}"),
                }
            }
            if ui.button("New blank skin").clicked() {
                self.skin_editor = Some(image::RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 0, 0])));
            }
            ui.color_edit_button_srgba(&mut self.skin_color);
            ui.label("Brush colour");
        });
        ui.add_space(8.0);
        if let Some(preview_skin) = self.skin_editor.clone() {
            self.skin_preview(ui, &preview_skin);
            ui.add_space(10.0);
        }
        let mut save_requested = false;
        let mut reload_requested = false;
        let mut remove_requested = false;
        if let Some(skin) = self.skin_editor.as_mut() {
            let (width, height) = skin.dimensions();
            let canvas_size = 384.0_f32;
            let cell = canvas_size / width.max(height) as f32;
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(canvas_size), egui::Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 4.0, Color32::from_rgb(25, 30, 43));
            for y in 0..height {
                for x in 0..width {
                    let pixel = skin.get_pixel(x, y);
                    let color = Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3]);
                    let tile = egui::Rect::from_min_size(rect.min + Vec2::new(x as f32 * cell, y as f32 * cell), Vec2::splat(cell));
                    painter.rect_filled(tile, 0.0, color);
                    painter.rect_stroke(tile, 0.0, Stroke::new(0.35_f32, Color32::from_rgba_unmultiplied(120, 130, 155, 70)), egui::StrokeKind::Inside);
                }
            }
            if (response.clicked() || response.dragged())
                && let Some(pointer) = response.interact_pointer_pos()
            {
                let x = ((pointer.x - rect.left()) / cell).floor() as u32;
                let y = ((pointer.y - rect.top()) / cell).floor() as u32;
                if x < width && y < height {
                    let scale_x = (width / 64).max(1);
                    let scale_y = (height / 64).max(1);
                    let base_x = x / scale_x;
                    let base_y = y / scale_y;
                    let allowed_regions = skin_uv_regions(&self.skin_selected_part, self.skin_overlay_layer);
                    let in_selected_region = allowed_regions.iter().any(|(rx, ry, rw, rh)| {
                        base_x >= *rx && base_x < *rx + *rw && base_y >= *ry && base_y < *ry + *rh
                    });
                    if in_selected_region {
                        skin.put_pixel(x, y, image::Rgba([self.skin_color.r(), self.skin_color.g(), self.skin_color.b(), self.skin_color.a()]));
                    }
                }
            }
            ui.label(RichText::new(format!("{} × {} skin texture · {} · {} · tap or drag to paint", width, height, if self.skin_overlay_layer { "outer layer" } else { "base layer" }, self.skin_selected_part)).size(11.0).color(Color32::GRAY));
            ui.horizontal_wrapped(|ui| {
                save_requested = ui.button("Save skin").clicked();
                reload_requested = ui.button("Undo changes / reload").clicked();
                if ui.button("Clear editor").clicked() {
                    *skin = image::RgbaImage::from_pixel(width, height, image::Rgba([0, 0, 0, 0]));
                }
                remove_requested = ui.button("Remove saved skin").clicked();
            });
        } else {
            ui.group(|ui| {
                ui.label(RichText::new("YOUR SKIN WORKSPACE").strong().color(self.accent));
                ui.label("Import a PNG skin or start with a blank 64×64 canvas. Paint directly on the texture, then save it to this profile.");
            });
        }
        if save_requested {
            let result = (|| -> anyhow::Result<()> {
                fs::create_dir_all(&skin_dir)?;
                self.skin_editor.as_ref().context("No skin is open in the editor")?.save(&skin_path)?;
                Ok(())
            })();
            self.status = result.map(|_| format!("Custom skin saved for {selected}"))
                .unwrap_or_else(|error| format!("Could not save skin: {error:#}"));
        }
        if reload_requested {
            self.skin_editor = image::open(&skin_path).ok().map(|image| image.to_rgba8());
        }
        if remove_requested {
            match fs::remove_file(&skin_path) {
                Ok(()) => { self.skin_editor = None; self.status = "Saved skin removed".into(); }
                Err(error) => self.status = format!("Could not remove skin: {error}"),
            }
        }
        ui.add_space(12.0);
        ui.group(|ui| {
            ui.label(RichText::new("HOW IT APPEARS IN GAME").strong().color(self.accent));
            ui.label("Offline profile skins are stored locally. Install the client-side CustomSkinLoader mod in the Minecraft instance to load this skin. Lunar does not upload offline skins to Mojang, and other players may not see it unless their setup uses the same local skin source.");
        });
    }

    fn play_stats_ui(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Play Stats", "Your Minecraft play history across Lunar Launcher.");
        let total = self.play_stats.total_playtime_seconds;
        let average = total.checked_div(self.play_stats.total_launches).unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            for (label, value) in [
                ("TOTAL PLAYTIME", crate::stats::format_duration(total)),
                ("SESSIONS", self.play_stats.total_launches.to_string()),
                ("AVERAGE SESSION", crate::stats::format_duration(average)),
                ("LAST PLAYED", self.play_stats.last_played.map(crate::stats::format_timestamp).unwrap_or_else(|| "Never".into())),
            ] {
                ui.group(|ui| {
                    ui.label(RichText::new(label).size(10.0).color(Color32::GRAY));
                    ui.label(RichText::new(value).size(22.0).strong());
                });
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("PLAYTIME BY VERSION").strong());
            if ui.button("Refresh").clicked() { self.play_stats = crate::stats::PlayStats::load(); }
        });
        let mut versions = self.play_stats.version_playtime.iter().collect::<Vec<_>>();
        versions.sort_by(|a, b| b.1.cmp(a.1));
        if versions.is_empty() {
            ui.label(RichText::new("No completed play sessions yet. Launch Minecraft through Lunar and your stats will appear here.").color(Color32::GRAY));
        } else {
            let max = versions.first().map(|(_, seconds)| **seconds).unwrap_or(1).max(1) as f32;
            for (version, seconds) in versions {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(version).strong().size(12.0));
                    ui.add(egui::ProgressBar::new(*seconds as f32 / max).desired_width(220.0).text(crate::stats::format_duration(*seconds)));
                });
            }
        }
        ui.add_space(14.0);
        ui.label(RichText::new("RECENT SESSIONS").strong());
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for session in &self.play_stats.recent_sessions {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&session.version).strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(crate::stats::format_duration(session.duration_seconds)).strong().color(self.accent));
                        });
                    });
                    ui.label(RichText::new(format!("{} · {}", session.instance.as_deref().unwrap_or("Global game folder"), crate::stats::format_timestamp(session.ended_at))).size(10.0).color(Color32::GRAY));
                });
            }
        });
        ui.add_space(8.0);
        if ui.button("Clear play history").clicked() {
            self.play_stats = crate::stats::PlayStats::default();
            match self.play_stats.save() { Ok(()) => self.status = "Play history cleared".into(), Err(error) => self.status = error.to_string() }
        }
    }

    fn open_local_path(&mut self, path: PathBuf) {
        let result = if cfg!(target_os = "windows") { Command::new("explorer").arg(&path).spawn() }
            else if cfg!(target_os = "macos") { Command::new("open").arg(&path).spawn() }
            else { Command::new("xdg-open").arg(&path).spawn() };
        self.status = result.map(|_| format!("Opened {}", path.display())).unwrap_or_else(|error| format!("Could not open path: {error}"));
    }

    fn choose_content_file(kind: &str) -> Option<PathBuf> {
        let (filter, description) = match kind {
            "mod" => ("*.jar", "Minecraft mod (*.jar)"),
            "modpack" => ("*.mrpack", "Modrinth modpack (*.mrpack)"),
            "resourcepack" | "shader" | "archive" => ("*.zip", "ZIP archive (*.zip)"),
            "skin" => ("*.png", "Minecraft skin PNG (*.png)"),
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

    fn instance_manager_ui(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Instance Manager", "Separate game versions, mods, settings, Java runtimes and saves.");
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(RichText::new("CREATE INSTANCE").strong().size(11.0).color(self.accent));
            ui.horizontal(|ui| {
                ui.add_sized([250.0, 34.0], egui::TextEdit::singleline(&mut self.new_instance_name).hint_text("Instance name"));
                if ui.add_enabled(!self.new_instance_name.trim().is_empty() && self.installed_ids.contains(&self.selected_version), egui::Button::new("Create")).clicked() {
                    match crate::workspace::create_instance(&self.game_root, &self.new_instance_name, &self.selected_version) {
                        Ok(instance) => {
                            self.workspace_selected = Some(instance.id.clone());
                            self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default();
                            self.new_instance_name.clear();
                            self.status = format!("Created instance {}", instance.name);
                        }
                        Err(error) => self.status = error.to_string(),
                    }
                }
            });
            ui.label(RichText::new(format!("Base version: {}", self.selected_version)).size(10.0).color(Color32::GRAY));
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} INSTANCE(S)", self.workspace_instances.len())).strong().color(self.accent));
            if ui.button("Refresh").clicked() {
                self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default();
            }
        });
        if self.workspace_instances.is_empty() {
            ui.label("No isolated instances yet. Create one above.");
            return;
        }
        let instances = self.workspace_instances.clone();
        for instance in &instances {
            let selected = self.workspace_selected.as_deref() == Some(instance.id.as_str());
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&instance.name).strong());
                        ui.label(RichText::new(format!("Minecraft {} · {} · {} launches", instance.minecraft, instance.loader.as_deref().unwrap_or("Vanilla"), instance.play_count)).size(10.0).color(Color32::GRAY));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(if selected { "Selected" } else { "Edit settings" }).clicked() {
                            self.workspace_selected = Some(instance.id.clone());
                        }
                    });
                });
            });
        }
        let Some(mut instance) = self.workspace_instances.iter().find(|item| Some(item.id.as_str()) == self.workspace_selected.as_deref()).cloned() else { return; };
        let path = crate::workspace::instance_path(&self.game_root, &instance);
        ui.add_space(12.0);
        Self::heading(ui, &instance.name, "INSTANCE CONFIGURATION");
        ui.horizontal(|ui| {
            ui.label("Memory allocation");
            ui.add(egui::Slider::new(&mut instance.memory_mb, 1024..=16384).step_by(512.0).suffix(" MB"));
        });
        let mut java = instance.java_major.unwrap_or(0);
        let java_label = if java == 0 { "Automatic Java".to_owned() } else { format!("Java {java}") };
        egui::ComboBox::from_id_salt("instance_java_runtime").selected_text(java_label).show_ui(ui, |ui| {
            ui.selectable_value(&mut java, 0, "Automatic Java");
            for major in [8, 17, 21] { ui.selectable_value(&mut java, major, format!("Java {major}")); }
        });
        instance.java_major = (java != 0).then_some(java);
        egui::ComboBox::from_id_salt("instance_performance").selected_text(&instance.performance_mode).show_ui(ui, |ui| {
            for mode in ["Maximum FPS", "Performance", "Balanced", "Custom"] { ui.selectable_value(&mut instance.performance_mode, mode.to_owned(), mode); }
        });
        egui::ComboBox::from_id_salt("instance_loader").selected_text(instance.loader.as_deref().unwrap_or("Vanilla")).show_ui(ui, |ui| {
            ui.selectable_value(&mut instance.loader, None, "Vanilla");
            for loader in ["fabric", "forge", "neoforge", "quilt"] { ui.selectable_value(&mut instance.loader, Some(loader.to_owned()), loader); }
        });
        ui.label("Additional JVM arguments");
        ui.add_sized([ui.available_width(), 58.0], egui::TextEdit::multiline(&mut instance.jvm_arguments).hint_text("-XX:+UseZGC"));
        ui.checkbox(&mut instance.backup_on_exit, "Back up every world automatically when Minecraft closes");
        ui.horizontal_wrapped(|ui| {
            if ui.button("Save instance settings").clicked() {
                match crate::workspace::save_instance(&self.game_root, &instance) {
                    Ok(()) => {
                        self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default();
                        self.status = format!("Saved settings for {}", instance.name);
                    }
                    Err(error) => self.status = error.to_string(),
                }
            }
            if ui.button("Open instance folder").clicked() { self.open_local_path(path.clone()); }
            if ui.button("Analyze instance logs").clicked() {
                self.crash_analysis = crate::crash::analyze(&self.game_root, Some(&path)).ok();
                self.screen = Screen::CrashAnalyzer;
            }
        });
        ui.add_space(8.0);
        if ui.button("Launch this instance").clicked() {
            if let Some(account) = self.microsoft_account.clone() {
                match launcher::load_installed(&instance.minecraft, &self.game_root) {
                    Ok(mut installed) => {
                        installed.game_directory = Some(path);
                        match launcher::launch_with_account_settings(&installed, &account, instance.memory_mb, &instance.performance_mode, instance.java_major, &instance.jvm_arguments) {
                            Ok(process) => {
                                self.game_process = Some(process);
                                self.begin_play_session(installed.id.clone(), Some(instance.name.clone()));
                                if let Err(error) = crate::workspace::record_launch(&self.game_root, &mut instance) { self.status = error.to_string(); }
                                self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default();
                                self.status = format!("Launching {}", instance.name);
                            }
                            Err(error) => self.status = format!("{error:#}"),
                        }
                    }
                    Err(error) => self.status = error.to_string(),
                }
            } else {
                self.status = "Sign in with Microsoft to launch Minecraft Java Edition".into();
                self.screen = Screen::Profiles;
            }
        }
        ui.label(RichText::new("Instance settings are saved independently. JVM arguments are split safely and passed as separate process arguments.").size(10.0).color(Color32::GRAY));
    }

    fn crash_analyzer_ui(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "Crash Analyzer", "Inspect the latest crash report or game log and identify common failure signatures.");
        let selected = self.workspace_instances.iter().find(|item| Some(item.id.as_str()) == self.workspace_selected.as_deref()).cloned();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("TARGET").strong().color(self.accent));
            ui.label(selected.as_ref().map(|item| item.name.as_str()).unwrap_or("Global Minecraft folder"));
            if ui.button("Analyze latest report").clicked() {
                let path = selected.as_ref().map(|item| crate::workspace::instance_path(&self.game_root, item));
                match crate::crash::analyze(&self.game_root, path.as_deref()) {
                    Ok(result) => { self.status = result.summary.clone(); self.crash_analysis = Some(result); }
                    Err(error) => { self.status = error.to_string(); self.crash_analysis = None; }
                }
            }
        });
        if let Some(mut analysis) = self.crash_analysis.clone() {
            ui.add_space(10.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(RichText::new(&analysis.summary).strong().size(16.0).color(self.accent));
                ui.label(RichText::new(analysis.source.display().to_string()).size(10.0).color(Color32::GRAY));
                for (index, finding) in analysis.findings.iter().enumerate() {
                    ui.label(RichText::new(format!("{}. {}", index + 1, finding)).size(12.0));
                }
                if ui.button("Open source log").clicked() { self.open_local_path(analysis.source.clone()); }
            });
            egui::CollapsingHeader::new("Last 45 log lines").default_open(true).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut analysis.excerpt).desired_rows(18).code_editor().interactive(false));
            });
        } else {
            ui.group(|ui| {
                ui.label(RichText::new("No analysis loaded").strong());
                ui.label("Run the analyzer to inspect the newest crash report or log for this target.");
            });
        }
        ui.label(RichText::new("The analyzer uses local log signatures and gives diagnostic leads; it does not modify mods or delete files.").size(10.0).color(Color32::GRAY));
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
                        self.active_java_task = Some(self.downloads.begin_operation(&format!("Install Java {major}"), root.clone()));
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
        self.poll_microsoft_login(ctx);
        self.downloads.poll();
        if self.last_metrics_refresh.elapsed() >= Duration::from_secs(1) {
            self.system_monitor.refresh_all();
            let cpu = self.system_monitor.global_cpu_usage().clamp(0.0, 100.0);
            let total = self.system_monitor.total_memory().max(1) as f32;
            let memory = (self.system_monitor.used_memory() as f32 / total * 100.0).clamp(0.0, 100.0);
            self.cpu_history.push(cpu);
            self.memory_history.push(memory);
            let game_metrics = self.game_process.as_ref().and_then(|process| {
                self.system_monitor.process(sysinfo::Pid::from_u32(process.id())).map(|game| {
                    let process_memory = game.memory() as f32;
                    (game.cpu_usage().clamp(0.0, 100.0), (process_memory / total * 100.0).clamp(0.0, 100.0))
                })
            });
            let (game_cpu, game_memory) = game_metrics.unwrap_or((0.0, 0.0));
            self.game_cpu_history.push(game_cpu);
            self.game_memory_history.push(game_memory);
            for history in [&mut self.cpu_history, &mut self.memory_history, &mut self.game_cpu_history, &mut self.game_memory_history] {
                if history.len() > 120 { history.remove(0); }
            }
            self.last_metrics_refresh = Instant::now();
        }
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
            let operation_result = result.as_ref().map(|_| ()).map_err(Clone::clone);
            if let Some(id) = self.active_content_task.take() { self.downloads.finish_operation(id, operation_result); }
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
            let operation_result = result.as_ref().map(|_| ()).map_err(Clone::clone);
            if let Some(id) = self.active_java_task.take() { self.downloads.finish_operation(id, operation_result); }
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
            let operation_result = result.as_ref().map(|_| ()).map_err(Clone::clone);
            if let Some(id) = self.active_modpack_task.take() { self.downloads.finish_operation(id, operation_result); }
            match result {
                Ok(mut installed) => {
                    if let Some(account) = self.microsoft_account.clone() {
                        if let Some(mut instance) = self.pending_instance_launch.take() {
                            let instance_path = crate::workspace::instance_path(&self.game_root, &instance);
                            installed.game_directory = Some(instance_path);
                            match launcher::launch_with_account(&installed, &account, instance.memory_mb, &self.performance_mode) {
                                Ok(process) => {
                                    self.game_process = Some(process);
                                    self.begin_play_session(installed.id.clone(), Some(instance.name.clone()));
                                    if let Err(error) = crate::workspace::record_launch(&self.game_root, &mut instance) { self.status = error.to_string(); }
                                    self.workspace_instances = crate::workspace::list_instances(&self.game_root).unwrap_or_default();
                                    self.status = format!("Launching {}", instance.name);
                                }
                                Err(error) => self.status = format!("{error:#}"),
                            }
                        } else {
                            match launcher::launch_with_account(&installed, &account, self.memory_mb, &self.performance_mode) {
                                Ok(process) => {
                                    self.game_process = Some(process);
                                    self.begin_play_session(installed.id.clone(), None);
                                    self.status = format!("Launching {}", installed.id);
                                }
                                Err(error) => self.status = format!("{error:#}"),
                            }
                        }
                    } else {
                        self.pending_instance_launch = None;
                        self.status = "Sign in with Microsoft to launch a modpack".into();
                    }
                }
                Err(error) => {
                    self.pending_instance_launch = None;
                    self.status = error;
                }
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
            || self.downloads.has_active()
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
                            Screen::PlayStats => self.play_stats_ui(ui),
                            Screen::SkinManager => self.skin_manager_ui(ui),
                            Screen::Downloads => self.download_center_ui(ui),
                            Screen::Instances => self.instance_manager_ui(ui),
                            Screen::CrashAnalyzer => self.crash_analyzer_ui(ui),
                            Screen::LivePerformance => self.live_performance_manager(ui),
                            Screen::Settings => self.settings(ui),
                        }
                    });
            });
        if self.content_install_rx.is_some() || self.install_rx.is_some() || self.modpack_launch_rx.is_some() || self.java_install_rx.is_some() || self.install_progress.is_some() || self.downloads.has_active() {
            egui::Area::new(egui::Id::new("lunar_download_toast"))
                .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-22.0, -62.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(17, 23, 39, 238))
                        .stroke(Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(151, 169, 255, 100)))
                        .corner_radius(egui::CornerRadius::same(14))
                        .inner_margin(egui::Margin::same(14))
                        .show(ui, |ui| {
                            ui.set_min_width(250.0);
                            let active_download = self.downloads.tasks().iter().find(|task| matches!(&task.state, crate::downloads::DownloadState::Queued | crate::downloads::DownloadState::Downloading | crate::downloads::DownloadState::Paused));
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(active_download.map(|task| task.filename.as_str()).unwrap_or("Lunar is working")).strong());
                                    ui.label(RichText::new(active_download.map(|task| task.state.label()).unwrap_or_else(|| self.status.clone())).size(10.0).color(Color32::from_rgb(180, 190, 210)));
                                });
                            });
                            if let Some(task) = active_download
                                && let Some(total) = task.total
                                && total > 0
                            {
                                let fraction = (task.downloaded as f32 / total as f32).clamp(0.0, 1.0);
                                ui.add(egui::ProgressBar::new(fraction).text(format!("{:.1} / {:.1} MB", task.downloaded as f64 / 1_048_576.0, total as f64 / 1_048_576.0)));
                            } else if let Some(progress) = &self.install_progress {
                                let fraction = if progress.total == 0 { 0.0 } else { progress.completed as f32 / progress.total as f32 };
                                ui.add(egui::ProgressBar::new(fraction).text(format!("{} · {}/{}", progress.phase, progress.completed, progress.total)));
                            }
                        });
                });
        }
        self.paint_splash(ctx);
    }
}

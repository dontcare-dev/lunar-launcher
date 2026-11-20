#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, io::{Read, Write}, path::{Path, PathBuf}, sync::{Arc, Condvar, Mutex, mpsc::{self, Receiver, Sender}}};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadState {
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed(String),
    Cancelled,
}

impl DownloadState {
    pub fn label(&self) -> String {
        match self {
            Self::Queued => "Queued".into(),
            Self::Downloading => "Downloading".into(),
            Self::Paused => "Paused".into(),
            Self::Completed => "Completed".into(),
            Self::Failed(error) => format!("Failed: {error}"),
            Self::Cancelled => "Cancelled".into(),
        }
    }

    fn terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed(_) | Self::Cancelled)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadTask {
    pub id: u64,
    pub url: String,
    pub filename: String,
    pub destination: PathBuf,
    pub state: DownloadState,
    pub downloaded: u64,
    pub total: Option<u64>,
    #[serde(default)]
    pub logs: Vec<String>,
}

#[derive(Default)]
struct ControlState {
    paused: bool,
    cancelled: bool,
}

struct Update {
    id: u64,
    state: DownloadState,
    downloaded: u64,
    total: Option<u64>,
}

pub struct DownloadManager {
    tasks: Vec<DownloadTask>,
    controls: HashMap<u64, Arc<(Mutex<ControlState>, Condvar)>>,
    sender: Sender<Update>,
    receiver: Receiver<Update>,
    next_id: u64,
    history_path: Option<PathBuf>,
    queue_lock: Arc<Mutex<()>>,
}

impl Default for DownloadManager {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        let history_path = directories::ProjectDirs::from("dev", "SilverDevStudios", "LUNAR")
            .map(|dirs| dirs.data_dir().join("downloads.json"));
        let mut tasks = history_path.as_ref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Vec<DownloadTask>>(&bytes).ok())
            .unwrap_or_default();
        for task in &mut tasks {
            if !task.state.terminal() {
                task.state = DownloadState::Failed("Interrupted when Lunar closed".into());
            }
        }
        let next_id = tasks.iter().map(|task| task.id).max().unwrap_or(0).saturating_add(1);
        let manager = Self { tasks, controls: HashMap::new(), sender, receiver, next_id, history_path, queue_lock: Arc::new(Mutex::new(())) };
        manager.persist();
        manager
    }
}

impl DownloadManager {
    fn persist(&self) {
        let Some(path) = &self.history_path else { return };
        if let Some(parent) = path.parent() { let _ = fs::create_dir_all(parent); }
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.tasks) {
            let temporary = path.with_extension("tmp");
            if fs::write(&temporary, bytes).is_ok() && fs::rename(&temporary, path).is_err() {
                let _ = fs::remove_file(temporary);
            }
        }
    }

    pub fn tasks(&self) -> &[DownloadTask] {
        &self.tasks
    }

    pub fn begin_operation(&mut self, label: &str, destination: PathBuf) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.tasks.push(DownloadTask {
            id,
            url: "lunar://internal-operation".into(),
            filename: label.to_owned(),
            destination,
            state: DownloadState::Downloading,
            downloaded: 0,
            total: None,
            logs: vec!["Started by Lunar".into()],
        });
        self.persist();
        id
    }

    pub fn finish_operation(&mut self, id: u64, result: Result<(), String>) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            let state = match result {
                Ok(()) => DownloadState::Completed,
                Err(error) => DownloadState::Failed(error),
            };
            task.logs.push(state.label());
            task.state = state;
            self.persist();
        }
    }

    pub fn enqueue(&mut self, url: &str, directory: PathBuf) -> anyhow::Result<u64> {
        let parsed = reqwest::Url::parse(url)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            anyhow::bail!("Only HTTP and HTTPS download links are supported");
        }
        let raw_name = parsed.path_segments().and_then(|mut segments| segments.next_back()).unwrap_or("download.bin");
        let filename = raw_name.split('?').next().unwrap_or("download.bin");
        let filename = filename.chars().map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') { ch } else { '_' }).collect::<String>();
        if filename.is_empty() || filename == "." || filename == ".." {
            anyhow::bail!("The download URL does not contain a valid filename");
        }
        fs::create_dir_all(&directory)?;
        let mut destination = directory.join(&filename);
        let mut suffix = 2_u32;
        while destination.exists() {
            let stem = destination.file_stem().and_then(|value| value.to_str()).unwrap_or("download");
            let extension = destination.extension().and_then(|value| value.to_str()).map(|value| format!(".{value}")).unwrap_or_default();
            destination = directory.join(format!("{stem}-{suffix}{extension}"));
            suffix = suffix.saturating_add(1);
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let url = url.to_owned();
        let control = Arc::new((Mutex::new(ControlState::default()), Condvar::new()));
        self.controls.insert(id, Arc::clone(&control));
        let task = DownloadTask { id, url: url.clone(), filename: destination.file_name().and_then(|value| value.to_str()).unwrap_or(&filename).to_owned(), destination: destination.clone(), state: DownloadState::Queued, downloaded: 0, total: None, logs: vec!["Added to download history".into()] };
        self.tasks.push(task);
        self.persist();
        let sender = self.sender.clone();
        let queue_lock = Arc::clone(&self.queue_lock);
        std::thread::spawn(move || {
            let temporary = destination.with_extension("lunar-part");
            let result = (|| -> anyhow::Result<()> {
                let _queue_guard = queue_lock.lock().map_err(|_| anyhow::anyhow!("Download queue was interrupted"))?;
                {
                    let (lock, _) = &*control;
                    if lock.lock().map_err(|_| anyhow::anyhow!("Download control was interrupted"))?.cancelled {
                        let _ = sender.send(Update { id, state: DownloadState::Cancelled, downloaded: 0, total: None });
                        return Ok(());
                    }
                }
                let client = reqwest::blocking::Client::builder().user_agent("Lunar-Launcher").timeout(std::time::Duration::from_secs(600)).build()?;
                let mut response = client.get(url).send()?.error_for_status()?;
                let total = response.content_length();
                let _ = sender.send(Update { id, state: DownloadState::Downloading, downloaded: 0, total });
                let mut output = fs::File::create(&temporary)?;
                let mut downloaded = 0_u64;
                let mut buffer = [0_u8; 65536];
                loop {
                    {
                        let (lock, condition) = &*control;
                        let mut state = lock.lock().map_err(|_| anyhow::anyhow!("Download control was interrupted"))?;
                        while state.paused && !state.cancelled {
                            let _ = sender.send(Update { id, state: DownloadState::Paused, downloaded, total });
                            state = condition.wait(state).map_err(|_| anyhow::anyhow!("Download control was interrupted"))?;
                        }
                        if state.cancelled {
                            drop(state);
                            let _ = fs::remove_file(&temporary);
                            let _ = sender.send(Update { id, state: DownloadState::Cancelled, downloaded, total });
                            return Ok(());
                        }
                    }
                    let count = response.read(&mut buffer)?;
                    if count == 0 { break; }
                    output.write_all(&buffer[..count])?;
                    downloaded = downloaded.saturating_add(count as u64);
                    let _ = sender.send(Update { id, state: DownloadState::Downloading, downloaded, total });
                }
                output.flush()?;
                drop(output);
                if destination.exists() { fs::remove_file(&destination)?; }
                fs::rename(&temporary, &destination)?;
                let _ = sender.send(Update { id, state: DownloadState::Completed, downloaded, total });
                Ok(())
            })();
            if let Err(error) = result {
                let _ = fs::remove_file(&temporary);
                let _ = sender.send(Update { id, state: DownloadState::Failed(error.to_string()), downloaded: 0, total: None });
            }
        });
        Ok(id)
    }

    pub fn pause(&mut self, id: u64) {
        if let Some(control) = self.controls.get(&id)
            && let Ok(mut state) = control.0.lock()
        {
            state.paused = true;
            if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) { task.state = DownloadState::Paused; }
            self.persist();
        }
    }

    pub fn resume(&mut self, id: u64) {
        if let Some(control) = self.controls.get(&id)
            && let Ok(mut state) = control.0.lock()
        {
            state.paused = false;
            control.1.notify_all();
            if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) { task.state = DownloadState::Downloading; }
            self.persist();
        }
    }

    pub fn cancel(&mut self, id: u64) {
        if let Some(control) = self.controls.get(&id)
            && let Ok(mut state) = control.0.lock()
        {
            state.cancelled = true;
            state.paused = false;
            control.1.notify_all();
            self.persist();
        }
    }

    pub fn retry(&mut self, id: u64) -> anyhow::Result<u64> {
        let task = self.tasks.iter().find(|task| task.id == id).cloned()
            .ok_or_else(|| anyhow::anyhow!("Download task was not found"))?;
        if !matches!(task.state, DownloadState::Failed(_) | DownloadState::Cancelled) {
            anyhow::bail!("Only failed or cancelled downloads can be retried");
        }
        let directory = task.destination.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        self.enqueue(&task.url, directory)
    }

    pub fn poll(&mut self) {
        let mut changed = false;
        while let Ok(update) = self.receiver.try_recv() {
            changed = true;
            if let Some(task) = self.tasks.iter_mut().find(|task| task.id == update.id) {
                if task.state != update.state {
                    task.logs.push(update.state.label());
                    if task.logs.len() > 24 { task.logs.remove(0); }
                }
                task.state = update.state.clone();
                task.downloaded = update.downloaded;
                task.total = update.total;
            }
            if update.state.terminal() {
                self.controls.remove(&update.id);
            }
        }
        if changed { self.persist(); }
    }

    pub fn has_active(&self) -> bool {
        self.tasks.iter().any(|task| !task.state.terminal())
    }

    pub fn clear_finished(&mut self) {
        self.tasks.retain(|task| !task.state.terminal());
        self.persist();
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_state_labels_are_stable() {
        assert_eq!(DownloadState::Queued.label(), "Queued");
        assert_eq!(DownloadState::Downloading.label(), "Downloading");
        assert_eq!(DownloadState::Paused.label(), "Paused");
        assert_eq!(DownloadState::Completed.label(), "Completed");
        assert_eq!(DownloadState::Cancelled.label(), "Cancelled");
        assert!(DownloadState::Failed("network".into()).label().contains("network"));
    }

    #[test]
    fn terminal_states_are_recognized() {
        assert!(DownloadState::Completed.terminal());
        assert!(DownloadState::Cancelled.terminal());
        assert!(DownloadState::Failed("failed".into()).terminal());
        assert!(!DownloadState::Queued.terminal());
        assert!(!DownloadState::Downloading.terminal());
        assert!(!DownloadState::Paused.terminal());
    }

    #[test]
    fn rejects_non_http_download_urls() {
        let mut manager = DownloadManager::default();
        let root = std::env::temp_dir().join(format!("lunar-download-test-{}", std::process::id()));
        assert!(manager.enqueue("file:///etc/passwd", root.clone()).is_err());
        assert!(manager.enqueue("ftp://example.com/file.jar", root.clone()).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_malformed_download_urls() {
        let mut manager = DownloadManager::default();
        assert!(manager.enqueue("not a url", std::env::temp_dir()).is_err());
    }

    #[test]
    fn manager_starts_with_empty_queue() {
        let manager = DownloadManager::default();
        assert!(manager.tasks().is_empty());
        assert!(!manager.has_active());
    }
}

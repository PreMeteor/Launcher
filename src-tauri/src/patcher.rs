//! Brings an install up to the manifest's version: downloads the patches it is missing and
//! applies them in order, downloading the next while the current one is applied.
//!
//! game.ver / boot.ver are rewritten after each patch, so an interrupted update resumes at the
//! patch it stopped in.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::manifest::{url_join, Manifest, Patch, Repo};
use crate::{download, zipatch};

#[derive(Debug, Clone, Serialize)]
pub struct InstallState {
    pub game_version: Option<String>,
    pub boot_version: Option<String>,
    pub has_game_exe: bool,
    /// A FINAL FANTASY XIV install, patched or not: the disc installs ffxivboot.exe and the
    /// version files, and the patches add ffxivgame.exe.
    pub is_install: bool,
}

fn read_version(dir: &Path, repo: Repo) -> Option<String> {
    let s = std::fs::read_to_string(dir.join(repo.version_file())).ok()?;
    let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_string();
    (!s.is_empty()).then_some(s)
}

pub fn install_state(dir: &Path) -> InstallState {
    InstallState {
        game_version: read_version(dir, Repo::Game),
        boot_version: read_version(dir, Repo::Boot),
        has_game_exe: dir.join("ffxivgame.exe").is_file(),
        is_install: dir.join("ffxivboot.exe").is_file() || dir.join("game.ver").is_file(),
    }
}

/// The manifest's patches this install has not had yet.
pub fn needed(manifest: &Manifest, dir: &Path) -> Vec<Patch> {
    let game = read_version(dir, Repo::Game).unwrap_or_default();
    let boot = read_version(dir, Repo::Boot).unwrap_or_default();
    manifest
        .patches
        .iter()
        .filter(|p| p.version.as_str() > if p.repo == Repo::Game { game.as_str() } else { boot.as_str() })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub count: usize,
    pub total_bytes: u64,
    /// Bytes of the plan downloaded so far (finished files plus the current one).
    pub downloaded_bytes: u64,
    pub downloading: Option<String>,
    pub applied: usize,
    pub applying: Option<String>,
    /// How far into the patch being applied, 0..1.
    pub apply_fraction: f64,
}

pub struct Job {
    pub client: reqwest::Client,
    pub base_url: String,
    pub game_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub keep_patches: bool,
    pub patches: Vec<Patch>,
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<Mutex<Progress>>,
}

fn short(p: &Patch) -> String {
    p.path.rsplit('/').next().unwrap_or(&p.path).to_string()
}

pub async fn run(job: Job) -> Result<(), String> {
    {
        let mut pr = job.progress.lock().unwrap();
        *pr = Progress { count: job.patches.len(), total_bytes: job.patches.iter().map(|p| p.size).sum(), ..Default::default() };
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Patch, PathBuf)>(1);

    let downloader = {
        let client = job.client.clone();
        let base = job.base_url.clone();
        let cache = job.cache_dir.clone();
        let cancel = job.cancel.clone();
        let progress = job.progress.clone();
        let patches = job.patches.clone();
        tokio::spawn(async move {
            let mut before = 0u64;
            for p in patches {
                let dest = cache.join(&p.path);
                progress.lock().unwrap().downloading = Some(short(&p));
                let url = url_join(&base, &p.path);
                let pr = progress.clone();
                download::fetch(&client, &url, &dest, p.size, &p.sha256, &cancel, |n| {
                    pr.lock().unwrap().downloaded_bytes = before + n;
                })
                .await?;
                before += p.size;
                progress.lock().unwrap().downloading = None;
                if tx.send((p, dest)).await.is_err() {
                    break;
                }
            }
            Ok::<(), String>(())
        })
    };

    let applied = async {
        while let Some((p, file)) = rx.recv().await {
            if job.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }
            {
                let mut pr = job.progress.lock().unwrap();
                pr.applying = Some(short(&p));
                pr.apply_fraction = 0.0;
            }
            let game = job.game_dir.clone();
            let cancel = job.cancel.clone();
            let pr = job.progress.clone();
            let f = file.clone();
            let size = p.size.max(1);
            tokio::task::spawn_blocking(move || {
                zipatch::apply(&f, &game, &cancel, |n| pr.lock().unwrap().apply_fraction = n as f64 / size as f64)
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{}: {e}", short(&p)))?;

            std::fs::write(job.game_dir.join(p.repo.version_file()), &p.version)
                .map_err(|e| format!("cannot record the game version: {e}"))?;
            if !job.keep_patches {
                let _ = std::fs::remove_file(&file);
            }
            let mut pr = job.progress.lock().unwrap();
            pr.applied += 1;
            pr.applying = None;
        }
        Ok::<(), String>(())
    }
    .await;

    if applied.is_err() {
        job.cancel.store(true, Ordering::Relaxed);
    }
    // Unblocks a downloader waiting to hand over its next patch.
    drop(rx);
    let downloaded = downloader.await.map_err(|e| e.to_string())?;
    applied?;
    downloaded
}

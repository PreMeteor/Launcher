//! Resumable patch downloads: bytes go to `<file>.part` and a dropped connection picks up where
//! it stopped (HTTP Range). The finished file must match the manifest's size and SHA-256.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

pub async fn sha256_file(path: PathBuf) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(hex::encode(h.finalize()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Is `path` already the file the manifest describes?
pub async fn is_complete(path: &Path, size: u64, sha256: &str) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.len() == size => sha256_file(path.to_path_buf()).await.map(|h| h.eq_ignore_ascii_case(sha256)).unwrap_or(false),
        _ => false,
    }
}

/// Downloads `url` to `dest`. `progress` gets the bytes of this file on disk so far.
pub async fn fetch(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    size: u64,
    sha256: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    if is_complete(dest, size, sha256).await {
        progress(size);
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let mut part_name = dest.file_name().unwrap_or_default().to_os_string();
    part_name.push(".part");
    let part = dest.with_file_name(part_name);

    let mut attempts = 0;
    loop {
        match fetch_once(client, url, &part, size, cancel, &mut progress).await {
            Ok(()) => break,
            Err(e) if cancel.load(Ordering::Relaxed) => return Err(e),
            Err(_) if attempts < 4 => {
                attempts += 1;
                tokio::time::sleep(std::time::Duration::from_secs(2 * attempts)).await;
            }
            Err(e) => return Err(e),
        }
    }

    let got = sha256_file(part.clone()).await?;
    if !got.eq_ignore_ascii_case(sha256) {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!("{} was corrupted in transit; try again", file_name(dest)));
    }
    tokio::fs::rename(&part, dest).await.map_err(|e| e.to_string())
}

async fn fetch_once(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    size: u64,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(u64),
) -> Result<(), String> {
    let mut have = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
    if have > size {
        have = 0;
    }
    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req.send().await.map_err(|e| format!("download failed: {e}"))?;
    let status = resp.status();
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    if !status.is_success() && !(have == size && status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE) {
        return Err(format!("the patch server answered {status} for {url}"));
    }
    if have == size && !status.is_success() {
        return Ok(());
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(part)
        .await
        .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
    let mut done = if resumed { have } else { 0 };
    progress(done);

    let mut body = resp.bytes_stream();
    while let Some(piece) = body.next().await {
        if cancel.load(Ordering::Relaxed) {
            file.flush().await.ok();
            return Err("cancelled".into());
        }
        let piece = piece.map_err(|e| format!("download interrupted: {e}"))?;
        file.write_all(&piece).await.map_err(|e| format!("cannot write {}: {e}", part.display()))?;
        done += piece.len() as u64;
        progress(done);
    }
    file.flush().await.map_err(|e| e.to_string())?;
    if done != size {
        return Err(format!("download stopped at {done} of {size} bytes"));
    }
    Ok(())
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

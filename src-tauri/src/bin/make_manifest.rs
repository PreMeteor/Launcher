//! Builds manifest.json for a folder of retail patches laid out the way the patch server serves
//! them (`<repo id>/patch/D<version>.patch`).
//!
//!   make-manifest <patch folder> [--servers servers.json] [--out manifest.json]
//!
//! `servers.json` is a list of `{ "name", "login_url", "signup_url"?, "lobby_host" }`.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use xiv_launcher_lib::manifest::{version_from_file_name, Manifest, Patch, Repo, Server};

/// The retail patch repositories, by the folder name the patch server used for them.
fn repo_for(id: &str) -> Option<Repo> {
    match id {
        "2d2a390f" => Some(Repo::Boot),
        "48eca647" => Some(Repo::Game),
        _ => None,
    }
}

fn sha256(path: &Path) -> std::io::Result<String> {
    let mut r = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    let mut out = PathBuf::from("manifest.json");
    let mut servers: Vec<Server> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = PathBuf::from(args.next().expect("--out needs a path")),
            "--servers" => {
                let p = args.next().expect("--servers needs a path");
                servers = serde_json::from_reader(File::open(&p).expect("cannot open servers file")).expect("bad servers file");
            }
            _ if root.is_none() => root = Some(PathBuf::from(a)),
            _ => panic!("unexpected argument {a}"),
        }
    }
    let root = root.unwrap_or_else(|| {
        eprintln!("usage: make-manifest <patch folder> [--servers servers.json] [--out manifest.json]");
        std::process::exit(2)
    });

    let mut patches = Vec::new();
    for repo_dir in std::fs::read_dir(&root).expect("cannot read the patch folder").flatten() {
        let id = repo_dir.file_name().to_string_lossy().to_string();
        let Some(repo) = repo_for(&id) else { continue };
        let Ok(files) = std::fs::read_dir(repo_dir.path().join("patch")) else { continue };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            let Some(version) = version_from_file_name(&name) else { continue };
            eprintln!("hashing {id}/patch/{name}");
            patches.push(Patch {
                repo,
                version,
                path: format!("{id}/patch/{name}"),
                size: std::fs::metadata(f.path()).expect("cannot read patch").len(),
                sha256: sha256(&f.path()).expect("cannot read patch"),
            });
        }
    }
    // Retail applied them in date order, boot and game interleaved.
    patches.sort_by(|a, b| a.version.cmp(&b.version));

    let latest = |r: Repo| patches.iter().filter(|p| p.repo == r).map(|p| p.version.clone()).max().unwrap_or_default();
    let manifest = Manifest { game_version: latest(Repo::Game), boot_version: latest(Repo::Boot), patches, servers };
    let total: u64 = manifest.patches.iter().map(|p| p.size).sum();
    std::fs::write(&out, serde_json::to_string_pretty(&manifest).unwrap()).expect("cannot write manifest");
    eprintln!(
        "{} patches, {:.2} GB, game {} boot {} -> {}",
        manifest.patches.len(),
        total as f64 / 1e9,
        manifest.game_version,
        manifest.boot_version,
        out.display()
    );
}

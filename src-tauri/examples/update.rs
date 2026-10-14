//! Runs an update without the window: `cargo run --example update -- <patch server> <game dir> <cache dir>`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use xiv_launcher_lib::{manifest, patcher};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (base, game, cache) = (a[0].clone(), PathBuf::from(&a[1]), PathBuf::from(&a[2]));
    let client = reqwest::Client::new();
    let m = manifest::fetch(&client, &base).await.unwrap();
    let patches = patcher::needed(&m, &game);
    println!("{} of {} patches needed", patches.len(), m.patches.len());
    let progress = Arc::new(Mutex::new(patcher::Progress::default()));
    let p2 = progress.clone();
    let ticker = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let p = p2.lock().unwrap().clone();
            println!("dl {}/{} MB  applied {}/{} {:?}", p.downloaded_bytes >> 20, p.total_bytes >> 20, p.applied, p.count, p.applying);
        }
    });
    let t = std::time::Instant::now();
    let r = patcher::run(patcher::Job {
        client,
        base_url: base,
        game_dir: game.clone(),
        cache_dir: cache,
        keep_patches: false,
        patches,
        cancel: Arc::new(AtomicBool::new(false)),
        progress,
    })
    .await;
    ticker.abort();
    println!("{r:?} in {:.1}s; state {:?}", t.elapsed().as_secs_f64(), patcher::install_state(&game));
}

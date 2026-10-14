pub mod download;
pub mod launch;
pub mod login;
pub mod manifest;
pub mod news;
pub mod patcher;
pub mod settings;
pub mod zipatch;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use manifest::{Manifest, Server};
use patcher::{InstallState, Progress};
use settings::Settings;

/// A sign-in the server's login page reported.
#[derive(Clone)]
struct Session {
    server: String,
    id: String,
}

struct AppState {
    settings: Mutex<Settings>,
    session: Mutex<Option<Session>>,
    manifest: Mutex<Result<Manifest, String>>,
    update: Mutex<Option<Arc<AtomicBool>>>,
    progress: Arc<Mutex<Progress>>,
    client: reqwest::Client,
}

#[derive(Serialize)]
struct UpdateInfo {
    count: usize,
    bytes: u64,
}

#[derive(Serialize)]
struct Status {
    settings: Settings,
    platform: &'static str,
    install: Option<InstallState>,
    servers: Vec<Server>,
    latest_version: Option<String>,
    manifest_error: Option<String>,
    update: Option<UpdateInfo>,
    updating: bool,
    /// The server signed in to, if any.
    signed_in: Option<String>,
}

#[derive(Clone, Serialize)]
struct Finished {
    ok: bool,
    error: Option<String>,
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_config_dir().map_err(|e| e.to_string())?.join("settings.json"))
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    }
}

fn status(state: &AppState) -> Status {
    let settings = state.settings.lock().unwrap().clone();
    let manifest = state.manifest.lock().unwrap();
    let install = settings.game_dir.as_deref().map(patcher::install_state);

    let mut servers: Vec<Server> = manifest.as_ref().map(|m| m.servers.clone()).unwrap_or_default();
    servers.iter_mut().for_each(Server::fill_signup);
    for s in &settings.servers {
        if !servers.iter().any(|x| x.name == s.name) {
            servers.push(s.clone());
        }
    }
    let update = match (&*manifest, settings.game_dir.as_deref()) {
        (Ok(m), Some(dir)) if patcher::install_state(dir).is_install => {
            let need = patcher::needed(m, dir);
            Some(UpdateInfo { count: need.len(), bytes: need.iter().map(|p| p.size).sum() })
        }
        _ => None,
    };
    Status {
        platform: platform(),
        install,
        servers,
        latest_version: manifest.as_ref().ok().map(|m| m.game_version.clone()),
        manifest_error: manifest.as_ref().err().cloned(),
        update,
        updating: state.update.lock().unwrap().is_some(),
        signed_in: state.session.lock().unwrap().as_ref().map(|s| s.server.clone()),
        settings,
    }
}

/// The bucket holding manifest.json, news.json and the patches. Built in; `XIV_PATCH_SERVER`
/// (at build or run time) points a launcher somewhere else for testing.
fn patch_server() -> String {
    std::env::var("XIV_PATCH_SERVER")
        .ok()
        .or_else(|| option_env!("XIV_PATCH_SERVER").map(str::to_string))
        .unwrap_or_else(|| "https://axi.sfo3.digitaloceanspaces.com/ffxiv".to_string())
}

async fn refresh_manifest(state: &AppState) {
    let result = manifest::fetch(&state.client, &patch_server()).await;
    *state.manifest.lock().unwrap() = result;
}

#[tauri::command]
async fn get_status(state: State<'_, AppState>, refresh: bool) -> Result<Status, String> {
    // The manifest is public (it also lists the servers); patching itself waits for a sign-in.
    if refresh && state.update.lock().unwrap().is_none() {
        refresh_manifest(&state).await;
    }
    Ok(status(&state))
}

#[tauri::command]
async fn save_settings(app: AppHandle, state: State<'_, AppState>, settings: Settings) -> Result<Status, String> {
    for s in &settings.servers {
        if s.lobby_host.len() > launch::LOBBY_HOST_MAX {
            return Err(format!("{}: the lobby host name can be at most {} characters", s.name, launch::LOBBY_HOST_MAX));
        }
    }
    settings.save(&settings_path(&app)?)?;
    *state.settings.lock().unwrap() = settings;
    Ok(status(&state))
}

#[tauri::command]
async fn start_update(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if state.session.lock().unwrap().is_none() {
        return Err("sign in first".into());
    }
    let settings = state.settings.lock().unwrap().clone();
    let game_dir = settings.game_dir.clone().ok_or("choose the game folder in Settings first")?;
    if !patcher::install_state(&game_dir).is_install {
        return Err("That folder isn't a FINAL FANTASY XIV install (no ffxivboot.exe or game.ver).".into());
    }
    let manifest = state.manifest.lock().unwrap().clone()?;
    let patches = patcher::needed(&manifest, &game_dir);
    if patches.is_empty() {
        return Ok(());
    }
    let cache_dir = match settings.patch_cache_dir.clone() {
        Some(d) => d,
        None => app.path().app_cache_dir().map_err(|e| e.to_string())?.join("patches"),
    };

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut slot = state.update.lock().unwrap();
        if slot.is_some() {
            return Err("an update is already running".into());
        }
        *slot = Some(cancel.clone());
    }
    let job = patcher::Job {
        client: state.client.clone(),
        base_url: patch_server(),
        game_dir,
        cache_dir,
        keep_patches: settings.keep_patches,
        patches,
        cancel: cancel.clone(),
        progress: state.progress.clone(),
    };

    let progress = state.progress.clone();
    let ticker_app = app.clone();
    let done = Arc::new(AtomicBool::new(false));
    let ticker_done = done.clone();
    tauri::async_runtime::spawn(async move {
        while !ticker_done.load(Ordering::Relaxed) {
            let snapshot = progress.lock().unwrap().clone();
            let _ = ticker_app.emit("update-progress", snapshot);
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });

    tauri::async_runtime::spawn(async move {
        let result = patcher::run(job).await;
        done.store(true, Ordering::Relaxed);
        let state = app.state::<AppState>();
        *state.update.lock().unwrap() = None;
        let cancelled = cancel.load(Ordering::Relaxed);
        let error = match result {
            Ok(()) => None,
            Err(_) if cancelled => Some("Update cancelled.".to_string()),
            Err(e) => Some(e),
        };
        let _ = app.emit("update-progress", state.progress.lock().unwrap().clone());
        let _ = app.emit("update-finished", Finished { ok: error.is_none(), error });
    });
    Ok(())
}

#[tauri::command]
async fn get_news(state: State<'_, AppState>) -> Result<news::News, String> {
    news::fetch(&state.client, &patch_server()).await
}

#[tauri::command]
fn cancel_update(state: State<'_, AppState>) {
    if let Some(c) = state.update.lock().unwrap().as_ref() {
        c.store(true, Ordering::Relaxed);
    }
}

async fn start_session(app: &AppHandle, state: &AppState, server: String, id: String, remember: Option<String>) -> Result<Status, String> {
    *state.session.lock().unwrap() = Some(Session { server: server.clone(), id });
    {
        let mut settings = state.settings.lock().unwrap();
        settings.last_server = Some(server);
        settings.last_user = remember;
        let _ = settings.save(&settings_path(app)?);
    }
    refresh_manifest(state).await;
    Ok(status(state))
}

fn server_named(state: &AppState, name: &str) -> Result<Server, String> {
    status(state).servers.into_iter().find(|s| s.name == name).ok_or_else(|| "Choose a server.".to_string())
}

#[tauri::command]
async fn sign_in(
    app: AppHandle,
    state: State<'_, AppState>,
    server: String,
    user: String,
    password: String,
    remember: bool,
) -> Result<Status, String> {
    let srv = server_named(&state, &server)?;
    let user = user.trim().to_string();
    if user.is_empty() || password.is_empty() {
        return Err("Enter your account name and password.".into());
    }
    let id = login::sign_in(&srv.login_url, &user, &password).await?;
    start_session(&app, &state, server, id, remember.then_some(user)).await
}

#[tauri::command]
async fn sign_up(
    app: AppHandle,
    state: State<'_, AppState>,
    server: String,
    user: String,
    password: String,
    email: String,
) -> Result<Status, String> {
    let srv = server_named(&state, &server)?;
    let url = srv.signup_url.clone().ok_or("This server doesn't take new accounts here.")?;
    let user = user.trim().to_string();
    let id = login::sign_up(&url, &user, &password, email.trim()).await?;
    start_session(&app, &state, server, id, Some(user)).await
}

#[tauri::command]
fn sign_out(state: State<'_, AppState>) -> Result<Status, String> {
    if state.update.lock().unwrap().is_some() {
        return Err("wait for the update to finish".into());
    }
    *state.session.lock().unwrap() = None;
    Ok(status(&state))
}

#[tauri::command]
async fn play(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let session = state.session.lock().unwrap().clone().ok_or("sign in first")?;
    let st = status(&state);
    if st.updating {
        return Err("wait for the update to finish".into());
    }
    if st.update.as_ref().is_some_and(|u| u.count > 0) {
        return Err("the game needs updating first".into());
    }
    let srv = st.servers.iter().find(|s| s.name == session.server).cloned().ok_or("that server is gone")?;
    let game_dir = st.settings.game_dir.clone().ok_or("choose the game folder in Settings first")?;
    let log_dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    let wait = launch::start(&launch::Launch {
        game_dir: &game_dir,
        session: &session.id,
        lobby_host: &srv.lobby_host,
        xivboot: st.settings.xivboot.as_deref(),
        log_dir: &log_dir,
    })?;

    // Out of the way while the game runs; back when it quits.
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    std::thread::spawn(move || {
        wait();
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
        let _ = app.emit("game-exited", ());
    });
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let settings = Settings::load(&settings_path(app.handle())?);
            app.manage(AppState {
                settings: Mutex::new(settings),
                session: Mutex::new(None),
                manifest: Mutex::new(Err("not checked yet".into())),
                update: Mutex::new(None),
                progress: Arc::new(Mutex::new(Progress::default())),
                client: reqwest::Client::builder()
                    .user_agent(concat!("xiv-launcher/", env!("CARGO_PKG_VERSION")))
                    .connect_timeout(std::time::Duration::from_secs(15))
                    .build()?,
            });
            WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("FINAL FANTASY XIV Launcher")
                // The retail launcher's size.
                .inner_size(1280.0, 720.0)
                .resizable(false)
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_status, get_news, save_settings, start_update, cancel_update, sign_in, sign_up, sign_out, play])
        .run(tauri::generate_context!())
        .expect("error while running the launcher");
}

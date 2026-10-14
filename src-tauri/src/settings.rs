//! What the launcher remembers between runs (settings.json in the app's config folder).
//! Passwords are never stored.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::manifest::Server;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The FINAL FANTASY XIV folder (the one with ffxivboot.exe and game.ver).
    pub game_dir: Option<PathBuf>,
    /// Where patches are downloaded to; the app's cache folder when unset.
    pub patch_cache_dir: Option<PathBuf>,
    /// Keep downloaded patches after they are applied.
    pub keep_patches: bool,
    /// macOS: the recompiled client, `xivboot` or the .app holding it.
    pub xivboot: Option<PathBuf>,
    /// Servers added by hand, alongside the ones the patch server lists.
    pub servers: Vec<Server>,
    pub last_server: Option<String>,
    /// The account name to fill in, when the player asked to be remembered.
    pub last_user: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            game_dir: default_game_dir(),
            patch_cache_dir: None,
            keep_patches: false,
            xivboot: None,
            servers: vec![Server {
                name: "Local Project Meteor".into(),
                login_url: "http://127.0.0.1:8000/login/".into(),
                signup_url: Some("http://127.0.0.1:8000/login_su/create_user.php".into()),
                lobby_host: "127.0.0.1".into(),
            }],
            last_server: None,
            last_user: None,
        }
    }
}

fn default_game_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        let p = PathBuf::from(r"C:\Program Files (x86)\SquareEnix\FINAL FANTASY XIV");
        p.is_dir().then_some(p)
    } else {
        None
    }
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        let mut settings: Settings = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        for s in &mut settings.servers {
            s.fill_signup();
        }
        settings
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self).unwrap()).map_err(|e| e.to_string())
    }
}

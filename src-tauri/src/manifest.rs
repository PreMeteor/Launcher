//! `manifest.json` at the root of the patch server: what 1.23b is made of and where to sign in.
//!
//! ```json
//! { "game_version": "2012.09.19.0001", "boot_version": "2010.09.18.0000",
//!   "patches": [ { "repo": "game", "version": "2010.09.19.0000",
//!                  "path": "48eca647/patch/D2010.09.19.0000.patch", "size": 444398866,
//!                  "sha256": "…" } ],
//!   "servers": [ { "name": "…", "login_url": "https://…/login/", "lobby_host": "…" } ] }
//! ```
//! Patch paths are relative to the manifest's folder.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub game_version: String,
    pub boot_version: String,
    pub patches: Vec<Patch>,
    #[serde(default)]
    pub servers: Vec<Server>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repo {
    /// ffxivboot and friends; its version is in boot.ver.
    Boot,
    /// The game itself; its version is in game.ver.
    Game,
}

impl Repo {
    pub fn version_file(self) -> &'static str {
        match self {
            Repo::Boot => "boot.ver",
            Repo::Game => "game.ver",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    pub repo: Repo,
    pub version: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Server {
    pub name: String,
    /// Where accounts sign in (Project Meteor: `http://host:8000/login/`).
    pub login_url: String,
    /// Where accounts are created (Project Meteor: `http://host:8000/login_su/create_user.php`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signup_url: Option<String>,
    /// Where the game finds the lobby server; the client has room for 19 characters.
    pub lobby_host: String,
}

impl Server {
    /// Project Meteor keeps account creation beside its sign-in page; fill that in when a server
    /// doesn't say.
    pub fn fill_signup(&mut self) {
        if self.signup_url.is_none() {
            if let Some(base) = self.login_url.strip_suffix("login/") {
                self.signup_url = Some(format!("{base}login_su/create_user.php"));
            }
        }
    }
}

/// The retail patch file names carry their version: `D2010.09.19.0000.patch`.
pub fn version_from_file_name(name: &str) -> Option<String> {
    let v = name.strip_prefix('D')?.strip_suffix(".patch")?;
    let ok = v.len() == 15 && v.chars().enumerate().all(|(i, c)| if [4, 7, 10].contains(&i) { c == '.' } else { c.is_ascii_digit() });
    ok.then(|| v.to_string())
}

pub fn url_join(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

pub async fn fetch(client: &reqwest::Client, base: &str) -> Result<Manifest, String> {
    let url = url_join(base, "manifest.json");
    let resp = client.get(&url).send().await.map_err(|e| format!("could not reach the patch server: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("the patch server answered {} for {url}", resp.status()));
    }
    resp.json().await.map_err(|e| format!("the patch server's manifest is unreadable: {e}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert_eq!(super::version_from_file_name("D2012.09.19.0001.patch").as_deref(), Some("2012.09.19.0001"));
        assert_eq!(super::version_from_file_name("D2012.09.19.patch"), None);
    }
}

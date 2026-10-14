//! `news.json` beside the manifest on the patch server: banners for the carousel and dated
//! headlines. Image paths may be relative to the patch server.
//!
//! ```json
//! { "banners": [ { "image": "news/maintenance.webp", "title": "…", "link": "https://…" } ],
//!   "items":   [ { "date": "2026-10-14", "category": "Maintenance", "title": "…", "link": "https://…" } ] }
//! ```

use serde::{Deserialize, Serialize};

use crate::manifest::url_join;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct News {
    #[serde(default)]
    pub banners: Vec<Banner>,
    #[serde(default)]
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Banner {
    pub image: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

fn absolute(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else {
        url_join(base, path)
    }
}

pub async fn fetch(client: &reqwest::Client, base: &str) -> Result<News, String> {
    let url = url_join(base, "news.json");
    let resp = client.get(&url).send().await.map_err(|e| format!("could not reach the patch server: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("no news ({})", resp.status()));
    }
    let mut news: News = resp.json().await.map_err(|e| format!("the news file is unreadable: {e}"))?;
    for b in &mut news.banners {
        b.image = absolute(base, &b.image);
    }
    // Only web links leave the launcher.
    let web = |l: &Option<String>| l.as_ref().filter(|u| u.starts_with("https://") || u.starts_with("http://")).cloned();
    for b in &mut news.banners {
        b.link = web(&b.link);
    }
    for i in &mut news.items {
        i.link = web(&i.link);
    }
    Ok(news)
}

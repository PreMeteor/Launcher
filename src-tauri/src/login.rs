//! Talking to the server's account pages. Project Meteor signs in through `login/` and creates
//! accounts through `login_su/create_user.php`; both answer a success with the retail login page's
//! `<x-sqexauth sid="…">` tag holding the 56-hex-digit session id the game is started with. Servers
//! that redirect to `ffxiv://login_success?sessionId=…` instead are understood too.

fn find_session(text: &str) -> Option<String> {
    for key in ["sessionId=", "sid=\""] {
        let mut rest = text;
        while let Some(i) = rest.find(key) {
            let tail = &rest[i + key.len()..];
            let id: String = tail.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if id.len() == 56 {
                return Some(id);
            }
            rest = tail;
        }
    }
    None
}

fn find_error(location: &str, body: &str) -> Option<String> {
    if let Some(i) = location.find("error=") {
        let raw = location[i + 6..].split('&').next().unwrap_or("");
        return Some(percent_decode(raw));
    }
    for marker in ["class=errorText>", "class=\"errorText\">", "class=\"errorMessage\">"] {
        if let Some(i) = body.find(marker) {
            let msg = body[i + marker.len()..].split('<').next().unwrap_or("").trim();
            if !msg.is_empty() {
                return Some(msg.to_string());
            }
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn post(url: &str, form: &[(&str, &str)], what: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(|e| format!("Could not reach the server: {e}"))?;
    let status = resp.status();
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp.text().await.unwrap_or_default();

    if let Some(id) = find_session(&location).or_else(|| find_session(&body)) {
        return Ok(id);
    }
    Err(find_error(&location, &body).unwrap_or_else(|| format!("{what} failed ({status}).")))
}

pub async fn sign_in(login_url: &str, user: &str, password: &str) -> Result<String, String> {
    post(login_url, &[("username", user), ("password", password), ("login", "1")], "Sign-in").await
}

pub async fn sign_up(signup_url: &str, user: &str, password: &str, email: &str) -> Result<String, String> {
    post(
        signup_url,
        &[("username", user), ("password", password), ("repeatPassword", password), ("email", email), ("createUser", "1")],
        "Account creation",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn finds_the_session() {
        assert_eq!(find_session(&format!("ffxiv://login_success?sessionId={ID}")).as_deref(), Some(ID));
        assert_eq!(find_session(&format!("<html><x-sqexauth sid=\"{ID}\" lang=\"en-us\">")).as_deref(), Some(ID));
        assert_eq!(find_session("sessionId=abc"), None);
    }

    #[test]
    fn finds_errors() {
        assert_eq!(find_error("/login?error=Wrong+password%21", "").as_deref(), Some("Wrong password!"));
        assert_eq!(find_error("", "<p class=errorText>Bad login</p>").as_deref(), Some("Bad login"));
    }
}

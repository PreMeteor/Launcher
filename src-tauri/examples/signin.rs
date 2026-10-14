//! Signs in without the window: `XIV_USER=… XIV_PASSWORD=… cargo run --example signin -- <login url>`.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url = std::env::args().nth(1).expect("login url");
    let user = std::env::var("XIV_USER").expect("XIV_USER");
    let password = std::env::var("XIV_PASSWORD").expect("XIV_PASSWORD");
    match xiv_launcher_lib::login::sign_in(&url, &user, &password).await {
        Ok(id) => println!("signed in: session {}… ({} chars)", &id[..8], id.len()),
        Err(e) => println!("refused: {e}"),
    }
    match xiv_launcher_lib::login::sign_in(&url, &user, "definitely-wrong").await {
        Ok(_) => println!("wrong password accepted?!"),
        Err(e) => println!("wrong password refused: {e}"),
    }
}

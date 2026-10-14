# XIV 1.0 Launcher

Launcher and patcher for FINAL FANTASY XIV 1.23b private servers. Tauri 2 (Rust) with a plain
HTML/JS front end.

1. **Sign in** — the launcher's own form posts to the server's account pages (Project Meteor:
   `login/` and `login_su/create_user.php`) and takes the session they answer with
   (`<x-sqexauth sid="…">`). Accounts can be created from the launcher too.
2. **Update** — only once signed in: the launcher reads `manifest.json` from the patch server,
   downloads the retail patches the install is missing and applies them.
3. **Play** — Windows starts `ffxivgame.exe`; macOS starts the recompiled client (`xivboot`).

## Run

```sh
cd src-tauri && cargo tauri dev      # build and open the launcher
cargo tauri build                    # installer / .app in src-tauri/target/release/bundle
cargo test                           # format, launch-argument and sign-in tests
```

The patch server (manifest, news, patches) is built in: `https://axi.sfo3.digitaloceanspaces.com/ffxiv`.
`XIV_PATCH_SERVER=…` at build or run time points a launcher elsewhere for testing.

## Patch server

Any static host that serves files over HTTP(S) with Range support (DigitalOcean Spaces works). Lay
the retail patches out the way the original patch server did and add a manifest:

```
<root>/manifest.json
<root>/2d2a390f/patch/D2010.09.18.0000.patch          boot
<root>/48eca647/patch/D2010.09.19.0000.patch … D2012.09.19.0001.patch   game
```

```sh
cd src-tauri
cargo run --release --bin make-manifest -- ../../patches/ffxiv_patches --servers servers.json --out manifest.json
```

`servers.json` lists the servers every launcher should offer:

```json
[{ "name": "My Server", "login_url": "https://example.org/login/", "lobby_host": "lobby.example.org" }]
```

Uploading to Spaces: copy `upload.conf.example` to `upload.conf` (gitignored), add a Spaces
access key and secret, then

```sh
./upload-patches.sh            # builds manifest.json, syncs new patches, uploads the manifest
./upload-patches.sh --dry-run  # show what would be uploaded
```

Bucket `axi` in `sfo3` under `ffxiv/` by default; override in `upload.conf`.


## News

`news.json` sits beside `manifest.json` and fills the banner carousel and headline list (shown
before sign-in). Image paths may be relative to the patch server; links open in the browser.

```json
{
  "banners": [{ "image": "news/maintenance.webp", "title": "", "link": "https://example.org/…" }],
  "items": [{ "date": "10/14/2026", "category": "Maintenance", "title": "All Worlds Maintenance", "link": "https://…" }]
}
```

Banners fill retail's 800x312 frame like a centred `background-size: cover`: any size works, the middle is kept. `publish/` holds the live
`news.json` and its images; `docs/news-example/` is a second example.

```sh
s3cmd put --acl-public publish/news.json s3://<bucket>/ffxiv/news.json
s3cmd put --acl-public publish/news/*.webp s3://<bucket>/ffxiv/news/
```

## How it works

- **Patches** (`zipatch.rs`): ZiPatch is a 12-byte magic and then chunks
  `[u32 BE size][tag][body][u32 BE crc32(tag+body)]`. `ETRY` writes or deletes whole files (`A`/`M`
  with `N`one or `Z`lib data, `D` without data), `ADIR`/`DELD` make and remove folders. Every chunk
  CRC and every written file's SHA1 (the entry's target hash) is checked; files are written beside
  their target and renamed over it. `game.ver`/`boot.ver` are updated after each patch, so an
  interrupted update resumes where it stopped. Downloads resume with HTTP Range and are checked
  against the manifest's SHA-256.
- **Launch argument** (`launch.rs`): `sqex0002` + base64url of Blowfish (little-endian words, key =
  8 hex digits of `tick & ~0xFFFF`) over ` T =<tick> /LANG =en-us /REGION =2 /SERVER_UTC =1356916742
  /SESSION_ID =<sid>` + `!////`.
- **Windows**: the game is started suspended; the lobby host name is written into its 20-byte slot
  and the lobby handshake's `time()` call is replaced by a fixed value, then it is resumed. The
  install under Program Files needs the launcher to run as administrator to be patched.
- **macOS**: `xivboot <game>/ffxivgame.exe <game>` with `XIV_ARGS`, `XIV_TICK`, `XIV_LOBBY`; its
  output goes to the launcher's log folder (`game.log`).
- The background and logo (`ui/assets/`) and the app icon (`src-tauri/icons/`, made from
  `icons/source.png` with `cargo tauri icon`) are Square Enix artwork kept out of git; put
  `background.webp` (1.0 key art) and `logo.png` there before building.

Format details were learned from Seventh Umbral's launcher (BSD-2, Jean-Philip Desjardins) and
checked against the retail patches; no code was taken from it.

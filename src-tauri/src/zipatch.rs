//! ZiPatch, the retail 1.x `.patch` format.
//!
//! A patch is a 12-byte magic followed by chunks: `[u32 BE size][4-byte tag][size bytes][u32 BE
//! crc32(tag + body)]`. Only `ETRY` (whole-file writes and deletes), `ADIR` and `DELD` change the
//! install; `FHDR`, `APLY` and `HIST` carry nothing we need.
//!
//! An `ETRY` holds a path and one or more entries, each `[mode u8 ×4][src sha1][dst sha1]
//! [compression u8 ×4][u32 BE stored][u32 BE old size][u32 BE new size][stored bytes]`. Mode is
//! `A` (add), `M` (modify) or `D` (delete, never with data); compression is `N` (none) or `Z`
//! (zlib). Only the last entry carries data, and the SHA1 of what it inflates to is its dst hash,
//! so every file written here is checked against it.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::read::ZlibDecoder;
use sha1::{Digest, Sha1};

const MAGIC: &[u8; 12] = b"\x91ZIPATCH\r\n\x1a\n";
const ENTRY_HEADER: usize = 60;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Format(String),
    Cancelled,
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Format(m) => write!(f, "{m}"),
            Error::Cancelled => write!(f, "cancelled"),
        }
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error::Format(msg.into())
}

/// Counts what has been read from the patch file, for progress.
struct Counting<R> {
    inner: R,
    read: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        Ok(n)
    }
}

/// One chunk's body: stops at its size and feeds everything read into the chunk's CRC.
struct Chunk<'a, R> {
    inner: &'a mut R,
    left: u64,
    crc: crc32fast::Hasher,
}

impl<R: Read> Read for Chunk<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let max = buf.len().min(self.left as usize);
        let n = self.inner.read(&mut buf[..max])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.crc.update(&buf[..n]);
        self.left -= n as u64;
        Ok(n)
    }
}

impl<R: Read> Chunk<'_, R> {
    fn u32(&mut self) -> io::Result<u32> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b)?;
        Ok(u32::from_be_bytes(b))
    }

    fn bytes(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.read_exact(&mut v)?;
        Ok(v)
    }

    fn skip_rest(&mut self) -> io::Result<()> {
        io::copy(self, &mut io::sink())?;
        Ok(())
    }

    fn path(&mut self) -> Result<String, Error> {
        let len = self.u32()? as usize;
        if len > 4096 {
            return Err(bad("path too long"));
        }
        let raw = self.bytes(len)?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        String::from_utf8(raw[..end].to_vec()).map_err(|_| bad("path is not text"))
    }
}

/// Joins a patch's path onto the game folder, refusing anything that would land outside it.
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, Error> {
    let rel = rel.replace('\\', "/");
    let mut out = root.to_path_buf();
    let mut any = false;
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        match Path::new(part).components().next() {
            Some(Component::Normal(_)) if !part.contains(':') => {
                out.push(part);
                any = true;
            }
            _ => return Err(bad(format!("unsafe path in patch: {rel}"))),
        }
    }
    if !any {
        return Err(bad("empty path in patch"));
    }
    Ok(out)
}

/// Applies one patch file to `game_dir`. `progress` gets the number of patch bytes consumed so
/// far. Re-applying a patch (after an interruption) is harmless: every write is a whole file.
pub fn apply(
    patch: &Path,
    game_dir: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<(), Error> {
    let mut r = Counting { inner: BufReader::with_capacity(1 << 20, File::open(patch)?), read: 0 };

    let mut magic = [0u8; 12];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(bad("not a ZiPatch file"));
    }

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        progress(r.read);

        let mut head = [0u8; 8];
        if !read_exact_or_eof(&mut r, &mut head)? {
            break;
        }
        let size = u32::from_be_bytes(head[..4].try_into().unwrap()) as u64;
        let tag = [head[4], head[5], head[6], head[7]];

        let mut crc = crc32fast::Hasher::new();
        crc.update(&tag);
        let mut chunk = Chunk { inner: &mut r, left: size, crc };

        match &tag {
            b"ETRY" => entry(&mut chunk, game_dir)?,
            b"ADIR" => {
                let dir = safe_join(game_dir, &chunk.path()?)?;
                fs::create_dir_all(dir)?;
            }
            b"DELD" => {
                let dir = safe_join(game_dir, &chunk.path()?)?;
                if dir.is_dir() {
                    fs::remove_dir_all(dir)?;
                }
            }
            b"FHDR" | b"APLY" | b"HIST" | b"DIFF" => {}
            _ => return Err(bad(format!("unknown chunk {}", String::from_utf8_lossy(&tag)))),
        }
        chunk.skip_rest()?;
        let want = chunk.crc.finalize();

        let mut stored = [0u8; 4];
        r.read_exact(&mut stored)?;
        if u32::from_be_bytes(stored) != want {
            return Err(bad(format!("chunk {} failed its CRC check", String::from_utf8_lossy(&tag))));
        }
    }
    progress(r.read);
    Ok(())
}

fn read_exact_or_eof(r: &mut impl Read, buf: &mut [u8]) -> Result<bool, Error> {
    let mut got = 0;
    while got < buf.len() {
        let n = r.read(&mut buf[got..])?;
        if n == 0 {
            return if got == 0 { Ok(false) } else { Err(bad("patch ends mid-chunk")) };
        }
        got += n;
    }
    Ok(true)
}

fn entry<R: Read>(c: &mut Chunk<'_, R>, game_dir: &Path) -> Result<(), Error> {
    let rel = c.path()?;
    let target = safe_join(game_dir, &rel)?;
    let count = c.u32()?;

    for i in 0..count {
        let h = c.bytes(ENTRY_HEADER)?;
        let mode = h[0];
        let dst_sha1: [u8; 20] = h[24..44].try_into().unwrap();
        let compression = h[44];
        let stored = u32::from_be_bytes(h[48..52].try_into().unwrap()) as u64;
        let new_size = u32::from_be_bytes(h[56..60].try_into().unwrap()) as u64;
        let last = i + 1 == count;

        if stored > 0 {
            let mut data = (&mut *c).take(stored);
            match compression {
                b'N' => write_file(&target, &mut data, new_size, &dst_sha1, &rel)?,
                b'Z' => write_file(&target, ZlibDecoder::new(&mut data), new_size, &dst_sha1, &rel)?,
                other => return Err(bad(format!("unknown compression {:?} for {rel}", other as char))),
            }
            // Whatever the zlib stream left unread, so the entries that follow stay in step.
            io::copy(&mut data, &mut io::sink())?;
        } else if last && mode == b'D' {
            match fs::remove_file(&target) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Writes next to the target and renames over it once the content checks out, so an interrupted
/// patch never leaves a half-written game file.
fn write_file(target: &Path, mut data: impl Read, size: u64, sha1: &[u8; 20], rel: &str) -> Result<(), Error> {
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp_name = target.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".patching");
    let tmp = target.with_file_name(tmp_name);

    let result = (|| -> Result<(), Error> {
        let mut out = io::BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        let mut hash = Sha1::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut written = 0u64;
        loop {
            let n = data.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            written += n as u64;
        }
        out.flush()?;
        drop(out);
        if written != size || hash.finalize().as_slice() != sha1 {
            return Err(bad(format!("{rel} does not match the patch's checksum")));
        }
        replace(&tmp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Windows Explorer can hold the game's .exe files open for their icons; retry briefly.
fn replace(tmp: &Path, target: &Path) -> Result<(), Error> {
    let mut tries = 0;
    loop {
        match fs::rename(tmp, target) {
            Ok(()) => return Ok(()),
            Err(_) if tries < 5 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(tag);
        v.extend_from_slice(body);
        let mut crc = crc32fast::Hasher::new();
        crc.update(tag);
        crc.update(body);
        v.extend_from_slice(&crc.finalize().to_be_bytes());
        v
    }

    fn etry(path: &str, mode: u8, data: &[u8], compress: bool) -> Vec<u8> {
        let mut p = path.as_bytes().to_vec();
        p.push(0);
        let mut b = (p.len() as u32).to_be_bytes().to_vec();
        b.extend_from_slice(&p);
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&[mode, 0, 0, 0]);
        b.extend_from_slice(&[0u8; 20]);
        b.extend_from_slice(&Sha1::digest(data));
        let stored = if compress {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        } else {
            data.to_vec()
        };
        b.extend_from_slice(&[if compress { b'Z' } else { b'N' }, 0, 0, 0]);
        b.extend_from_slice(&(stored.len() as u32).to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&(data.len() as u32).to_be_bytes());
        b.extend_from_slice(&stored);
        chunk(b"ETRY", &b)
    }

    fn tempdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xivlauncher-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn applies_writes_and_deletes() {
        let dir = tempdir("apply");
        let game = dir.join("game");
        fs::create_dir_all(game.join("old/sub")).unwrap();
        fs::write(game.join("gone.txt"), b"x").unwrap();

        let mut p = MAGIC.to_vec();
        p.extend(chunk(b"FHDR", &[0, 0, 2, 0]));
        p.extend(chunk(b"ADIR", b"\0\0\0\x04new\0................"));
        p.extend(etry("new/a.bin", b'A', b"hello world", true));
        p.extend(etry("b.bin", b'M', b"plain", false));
        p.extend(etry("gone.txt", b'D', b"", false));
        p.extend(chunk(b"DELD", b"\0\0\0\x04old\0................"));
        let patch = dir.join("t.patch");
        fs::write(&patch, p).unwrap();

        apply(&patch, &game, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(fs::read(game.join("new/a.bin")).unwrap(), b"hello world");
        assert_eq!(fs::read(game.join("b.bin")).unwrap(), b"plain");
        assert!(!game.join("gone.txt").exists());
        assert!(!game.join("old").exists());
    }

    #[test]
    fn rejects_bad_crc_and_escaping_paths() {
        let dir = tempdir("bad");
        let mut p = MAGIC.to_vec();
        let mut c = chunk(b"FHDR", &[0, 0, 2, 0]);
        *c.last_mut().unwrap() ^= 1;
        p.extend(c);
        fs::write(dir.join("crc.patch"), p).unwrap();
        assert!(apply(&dir.join("crc.patch"), &dir, &AtomicBool::new(false), |_| {}).is_err());

        let mut p = MAGIC.to_vec();
        p.extend(etry("../escape.txt", b'A', b"no", false));
        fs::write(dir.join("esc.patch"), p).unwrap();
        assert!(apply(&dir.join("esc.patch"), &dir.join("g"), &AtomicBool::new(false), |_| {}).is_err());
        assert!(!dir.join("escape.txt").exists());
    }
}

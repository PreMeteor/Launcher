//! Starting the game with a session.
//!
//! The game takes one argument, `sqex0002<blob>!////`: the launch parameters, Blowfish-enciphered
//! (little-endian words) with the 8 hex digits of `tick & ~0xFFFF` as key, then base64url. The
//! game deciphers it with its own GetTickCount(), so the tick used here must still be in the
//! same 65536 ms window when the game reads it.

use std::path::Path;

use base64::Engine;
use blowfish::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};

pub fn launch_arg(session: &str, tick: u32) -> String {
    let plain = format!(" T ={tick} /LANG =en-us /REGION =2 /SERVER_UTC =1356916742 /SESSION_ID ={session}");
    let mut buf = plain.into_bytes();
    buf.push(0);
    let key = format!("{:08x}", tick & !0xFFFF);
    let cipher = blowfish::BlowfishLE::new_from_slice(key.as_bytes()).expect("8-byte key");
    // Only whole blocks are enciphered; the tail stays as it is, as the game expects.
    let whole = buf.len() & !7;
    for block in buf[..whole].chunks_exact_mut(8) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    let enc = base64::engine::general_purpose::STANDARD.encode(&buf).replace('+', "-").replace('/', "_");
    format!("sqex0002{enc}!////")
}

/// The client keeps the lobby server's host name in a 20-byte slot.
pub const LOBBY_HOST_MAX: usize = 19;

pub struct Launch<'a> {
    pub game_dir: &'a Path,
    pub session: &'a str,
    pub lobby_host: &'a str,
    /// macOS: the recompiled client (`xivboot`, or the .app around it).
    pub xivboot: Option<&'a Path>,
    pub log_dir: &'a Path,
}

/// Waits until the started game has exited.
pub type Wait = Box<dyn FnOnce() + Send>;

pub fn start(l: &Launch) -> Result<Wait, String> {
    if l.lobby_host.is_empty() || l.lobby_host.len() > LOBBY_HOST_MAX {
        return Err(format!("the lobby host name must be 1-{LOBBY_HOST_MAX} characters"));
    }
    if !l.game_dir.join("ffxivgame.exe").is_file() {
        return Err("ffxivgame.exe is not in the game folder".into());
    }
    platform::start(l)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    /// xivboot starts the guest's tick count here, so the argument can be keyed to it.
    const TICK: u32 = 0x10000;

    fn resolve(p: &Path) -> PathBuf {
        if p.extension().is_some_and(|e| e == "app") {
            p.join("Contents/MacOS/xivboot")
        } else {
            p.to_path_buf()
        }
    }

    pub fn start(l: &Launch) -> Result<Wait, String> {
        let boot = resolve(l.xivboot.ok_or("choose the recompiled client (xivboot) in Settings")?);
        if !boot.is_file() {
            return Err(format!("{} does not exist", boot.display()));
        }
        std::fs::create_dir_all(l.log_dir).map_err(|e| e.to_string())?;
        let log = std::fs::File::create(l.log_dir.join("game.log")).map_err(|e| e.to_string())?;
        let mut child = Command::new(&boot)
            .arg(l.game_dir.join("ffxivgame.exe"))
            .arg(l.game_dir)
            .env("XIV_ARGS", launch_arg(l.session, TICK))
            .env("XIV_TICK", format!("{TICK:#x}"))
            .env("XIV_LOBBY", l.lobby_host)
            .current_dir(boot.parent().unwrap_or(Path::new("/")))
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", boot.display()))?;
        Ok(Box::new(move || {
            let _ = child.wait();
        }))
    }
}

#[cfg(windows)]
mod platform {
    //! ffxivgame.exe is started suspended and two things are written into it before it runs:
    //! the lobby host name, and a fixed value in place of the `time(NULL)` call whose result
    //! seeds the lobby handshake (the same patches every 1.x launcher makes).
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows_sys::Win32::System::Memory::{VirtualProtectEx, PAGE_EXECUTE_READWRITE};
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, ResumeThread, TerminateProcess, WaitForSingleObject, CREATE_SUSPENDED, INFINITE,
        PROCESS_INFORMATION, STARTUPINFOW,
    };

    /// ffxivgame.exe has no relocations, so it always loads at its preferred base.
    const IMAGE_BASE: usize = 0x0040_0000;
    /// The lobby host name slot.
    const LOBBY_HOST: usize = 0x00B9_0110;
    /// `call time` in the lobby handshake builder, replaced by `mov eax, 0x50E0E812`.
    const HANDSHAKE_TIME: usize = 0x009A_15E3;
    const HANDSHAKE_TIME_PATCH: [u8; 5] = [0xB8, 0x12, 0xE8, 0xE0, 0x50];

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    unsafe fn write(process: *mut c_void, rva: usize, bytes: &[u8]) -> Result<(), String> {
        let addr = (IMAGE_BASE + rva) as *mut c_void;
        let mut old = 0u32;
        if VirtualProtectEx(process, addr, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return Err("could not unprotect the game's memory".into());
        }
        let mut written = 0usize;
        let ok = WriteProcessMemory(process, addr, bytes.as_ptr() as *const c_void, bytes.len(), &mut written);
        VirtualProtectEx(process, addr, bytes.len(), old, &mut old);
        if ok == 0 || written != bytes.len() {
            return Err("could not patch the game's memory".into());
        }
        Ok(())
    }

    /// The game's process handle, kept to wait on it.
    struct Process(*mut c_void);
    // A process handle may be waited on and closed from any thread.
    unsafe impl Send for Process {}

    pub fn start(l: &Launch) -> Result<Wait, String> {
        let exe = l.game_dir.join("ffxivgame.exe");
        let tick = unsafe { GetTickCount() };
        let mut cmd = wide(format!("\"{}\" {}", exe.display(), launch_arg(l.session, tick)).as_ref());
        let cwd = wide(l.game_dir.as_os_str());

        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_SUSPENDED,
                std::ptr::null(),
                cwd.as_ptr(),
                &si,
                &mut pi,
            )
        };
        if ok == 0 {
            return Err(format!("could not start the game: {}", std::io::Error::last_os_error()));
        }

        let mut host = l.lobby_host.as_bytes().to_vec();
        host.push(0);
        let patched = unsafe {
            write(pi.hProcess, HANDSHAKE_TIME, &HANDSHAKE_TIME_PATCH).and_then(|_| write(pi.hProcess, LOBBY_HOST, &host))
        };
        unsafe {
            if patched.is_err() {
                TerminateProcess(pi.hProcess, 1);
            } else {
                ResumeThread(pi.hThread);
            }
            CloseHandle(pi.hThread);
        }
        if let Err(e) = patched {
            unsafe { CloseHandle(pi.hProcess) };
            return Err(e);
        }
        let process = Process(pi.hProcess);
        Ok(Box::new(move || {
            let process = process;
            unsafe {
                WaitForSingleObject(process.0, INFINITE);
                CloseHandle(process.0);
            }
        }))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::*;
    pub fn start(_: &Launch) -> Result<Wait, String> {
        Err("the game can only be started on Windows or macOS".into())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_the_reference_argument() {
        // tools/xiv_args.py in recomp, for the same session and tick.
        let session = "0123456789abcdef0123456789abcdef0123456789abcdef01234567";
        assert_eq!(super::launch_arg(session, 0x10000), include_str!("../tests/launch_arg.txt").trim());
    }
}

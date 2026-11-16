//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The lock of a folder of tiles: one process writes it at a time (`tiles`, `update`).
//!
//! A file [`FILE`] in the folder, made with `create_new` (so two processes never both make it),
//! holds who writes: process number, program, command, since when. It goes when the writer ends
//! ([`Lock`] dropped). A writer stopped from outside (the interface stops its jobs by their handle,
//! `progress::watch_stdin` ends the process with `exit`) leaves it behind: a lock whose process is no
//! longer running, or whose number now belongs to another program, is stale and taken over.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// The name of the lock in a folder of tiles.
pub const FILE: &str = "write.lock";

/// A folder held by this process; let go when dropped.
pub struct Lock {
    path: PathBuf,
}

/// What a lock says of its writer.
#[derive(Debug, PartialEq)]
struct Holder {
    pid: u32,
    program: String,
    command: String,
    since: String,
}

fn holder(text: &str) -> Option<Holder> {
    let v: Value = serde_json::from_str(text).ok()?;
    Some(Holder {
        pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
        program: v.get("program").and_then(Value::as_str).unwrap_or_default().to_string(),
        command: v.get("command").and_then(Value::as_str).unwrap_or_default().to_string(),
        since: v.get("since").and_then(Value::as_str).unwrap_or_default().to_string(),
    })
}

/// This program's file name (`btms.exe`).
fn own_program() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

impl Lock {
    /// Takes the lock of `dir` for `command` (`tiles`, `update`), or says who holds it. A stale
    /// lock is taken over.
    pub fn take(dir: &Path, command: &str) -> Result<Lock> {
        let path = dir.join(FILE);
        let content = serde_json::to_string_pretty(&json!({
            "pid": std::process::id(),
            "program": own_program(),
            "command": command,
            "since": super::record::now(),
        }))?;
        // a few rounds: a lock being written (empty for an instant), a stale one removed by another
        // process at the same time
        for round in 0..20 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(content.as_bytes()).with_context(|| format!("writing {}", path.display()))?;
                    return Ok(Lock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    match holder(&text) {
                        Some(h) if alive(h.pid, &h.program) => bail!(
                            "{} is being written by another job ({} by process {}{}): wait for it to end (lock {})",
                            dir.display(),
                            if h.command.is_empty() { "?".to_string() } else { h.command.clone() },
                            h.pid,
                            if h.since.is_empty() { String::new() } else { format!(", since {}", h.since) },
                            path.display()
                        ),
                        Some(h) => {
                            println!(
                                "lock: the lock of process {} ({}), which is no longer running, is taken over",
                                h.pid, h.command
                            );
                            let _ = std::fs::remove_file(&path);
                        }
                        // being written by its maker: looked at again in a moment; still unreadable
                        // after a second, it is a broken file
                        None if round < 10 => std::thread::sleep(std::time::Duration::from_millis(100)),
                        None => {
                            println!("lock: {} is unreadable, taken over", path.display());
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                }
                Err(e) => return Err(e).with_context(|| format!("creating the lock {}", path.display())),
            }
        }
        bail!("{}: the lock {} cannot be taken", dir.display(), path.display())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // only our own: a lock taken over meanwhile is another's
        let ours = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| holder(&t))
            .is_some_and(|h| h.pid == std::process::id());
        if ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Whether the process `pid` runs, as the program `program` (a number taken again by another
/// program is not the writer). A program name left empty is not checked.
#[cfg(windows)]
pub fn alive(pid: u32, program: &str) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => h,
        // a process of another user, or a protected one: it runs, and cannot be ours
        Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => return true,
        Err(_) => return false,
    };
    let mut code = 0u32;
    let running = unsafe { GetExitCodeProcess(handle, &mut code) }.is_ok() && code == STILL_ACTIVE.0 as u32;
    let mut name = [0u16; 1024];
    let mut size = name.len() as u32;
    let image = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, windows::core::PWSTR(name.as_mut_ptr()), &mut size)
    }
    .ok()
    .map(|_| String::from_utf16_lossy(&name[..size as usize]));
    let _ = unsafe { CloseHandle(handle) };
    let same = match image {
        Some(path) if !program.is_empty() => {
            Path::new(&path).file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(program))
        }
        _ => true,
    };
    running && same
}

#[cfg(not(windows))]
pub fn alive(pid: u32, _program: &str) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_lock_is_taken_refused_and_let_go() {
        let dir = scratch("lock");
        let lock = Lock::take(&dir, "update").unwrap();
        let h = holder(&std::fs::read_to_string(dir.join(FILE)).unwrap()).unwrap();
        assert_eq!((h.pid, h.command.as_str()), (std::process::id(), "update"));
        assert!(!h.program.is_empty() && !h.since.is_empty());
        // held by a running process (this one): refused, the holder named
        let e = format!("{:#}", Lock::take(&dir, "tiles").err().unwrap());
        assert!(e.contains("is being written by another job (update by process"), "{e}");
        drop(lock);
        assert!(!dir.join(FILE).exists());
        // a lock left by a process that ended: taken over
        std::fs::write(dir.join(FILE), r#"{"pid": 4294967290, "program": "btms.exe", "command": "tiles"}"#).unwrap();
        let lock = Lock::take(&dir, "tiles").unwrap();
        assert_eq!(holder(&std::fs::read_to_string(dir.join(FILE)).unwrap()).unwrap().pid, std::process::id());
        drop(lock);
        // a broken file: taken over after a second
        std::fs::write(dir.join(FILE), b"not JSON").unwrap();
        drop(Lock::take(&dir, "tiles").unwrap());
        // a lock taken over by another meanwhile is not removed by the one that let go
        let lock = Lock::take(&dir, "tiles").unwrap();
        std::fs::write(dir.join(FILE), r#"{"pid": 4294967291, "program": "x.exe"}"#).unwrap();
        drop(lock);
        assert!(dir.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A lock held by another process that runs is refused; once it ended, taken over. (A program
    /// of the system that waits: `ping`, launched and stopped here.)
    #[cfg(windows)]
    #[test]
    fn the_lock_of_another_running_process_is_refused_until_it_ends() {
        let dir = scratch("lock-other");
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert!(alive(child.id(), "PING.EXE") && alive(child.id(), ""));
        // the same number under another program's name: a number taken again, stale
        assert!(!alive(child.id(), "btms.exe"));
        std::fs::write(
            dir.join(FILE),
            format!(
                r#"{{"pid": {}, "program": "PING.EXE", "command": "tiles", "since": "2026-11-15T10:00:00Z"}}"#,
                child.id()
            ),
        )
        .unwrap();
        let e = format!("{:#}", Lock::take(&dir, "update").err().unwrap());
        assert!(e.contains(&format!("(tiles by process {}, since 2026-11-15T10:00:00Z)", child.id())), "{e}");
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!alive(child.id(), "PING.EXE"));
        let lock = Lock::take(&dir, "update").unwrap();
        drop(lock);
        assert!(!dir.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

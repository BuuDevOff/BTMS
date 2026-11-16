//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! One interface at a time for one data folder. A second server would start on the next port
//! without the job of the first: its page would show no render while one runs, could launch
//! another into the same folders, and both would write the same settings and the same derived
//! configuration. So the server that starts first keeps a file locked as long as it lives (the
//! system frees the lock when the process ends, however it ends) and tells its port in a file
//! beside it; an interface started meanwhile reads that port and shows the page of the first.
//!
//! Two files, because a locked file cannot be read by another process on Windows:
//! `interface/server.lock` (locked, empty) and `interface/server.json` (`{"port": …}`).

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Under the data folder: the folder of the two files, the locked one, the one that tells the port.
const FOLDER: &str = "interface";
const LOCK: &str = "server.lock";
const PORT: &str = "server.json";
/// How long an interface that finds the lock held waits for the port of its holder (which
/// writes it once it listens).
pub const PATIENCE: Duration = Duration::from_secs(3);

/// What an interface that starts finds.
pub enum Instance {
    /// No other interface on this data folder: this one is the server, as long as the guard lives.
    First(Guard),
    /// Another one is running: the port it listens on.
    Running(u16),
    /// Another one holds the lock and did not tell its port in time.
    Silent,
}

/// The lock of the server that runs. Kept as long as the server lives.
pub struct Guard {
    /// None when the lock could not be asked for at all (no data folder, a file system without
    /// locks): the interface starts all the same, unguarded.
    _lock: Option<File>,
    port_file: PathBuf,
}

impl Guard {
    /// Tells the port listened on to the interfaces started after this one.
    pub fn publish(&self, port: u16) {
        let text = json!({ "port": port, "pid": std::process::id() }).to_string();
        if let Err(e) = std::fs::write(&self.port_file, text) {
            eprintln!(
                "interface: cannot write {} ({e}): a second interface will not find this one",
                self.port_file.display()
            );
        }
    }
}

/// The port a running server wrote, None while the file is not there or not whole.
fn port_told(file: &Path) -> Option<u16> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    value.get("port")?.as_u64().and_then(|p| u16::try_from(p).ok()).filter(|p| *p > 0)
}

/// Takes the lock of the data folder `data`, or finds who holds it. Waits at most `patience` for
/// the port of a holder that is still starting.
pub fn claim(data: &Path, patience: Duration) -> Instance {
    let dir = data.join(FOLDER);
    let port_file = dir.join(PORT);
    let unguarded = |port_file: PathBuf| Instance::First(Guard { _lock: None, port_file });
    if std::fs::create_dir_all(&dir).is_err() {
        return unguarded(port_file);
    }
    let Ok(lock) = File::options().read(true).write(true).create(true).truncate(false).open(dir.join(LOCK)) else {
        return unguarded(port_file);
    };
    let from = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => {
                // the port of a server that ended is not this one's
                let _ = std::fs::remove_file(&port_file);
                return Instance::First(Guard { _lock: Some(lock), port_file });
            }
            Err(TryLockError::WouldBlock) => {
                if let Some(port) = port_told(&port_file) {
                    return Instance::Running(port);
                }
                if from.elapsed() >= patience {
                    return Instance::Silent;
                }
                // its holder is starting (the port comes once it listens), or just ended
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(TryLockError::Error(_)) => return unguarded(port_file),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_interface_finds_the_first() {
        let data = std::env::temp_dir().join(format!("btms-tests-{}-instance", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let port_file = data.join(FOLDER).join(PORT);
        let short = Duration::from_millis(250);

        // the first one: the lock is its own, the port of a server that ended is forgotten
        std::fs::create_dir_all(data.join(FOLDER)).unwrap();
        std::fs::write(&port_file, r#"{"port": 8800, "pid": 1}"#).unwrap();
        let Instance::First(first) = claim(&data, short) else { panic!("the first interface must be the server") };
        assert!(first._lock.is_some());
        assert!(!port_file.exists());

        // a second one while the first has not told its port: it waits, then gives up
        let from = Instant::now();
        assert!(matches!(claim(&data, short), Instance::Silent));
        assert!(from.elapsed() >= short);

        // once the port is told, the second one is given it, at once
        first.publish(8791);
        for _ in 0..2 {
            assert!(matches!(claim(&data, short), Instance::Running(8791)));
        }
        // a port file that is not whole is waited for, not believed
        std::fs::write(&port_file, r#"{"port": "#).unwrap();
        assert!(matches!(claim(&data, short), Instance::Silent));
        for bad in [r#"{"port": 0}"#, r#"{"port": 70000}"#, r#"{"port": "8791"}"#, "[]"] {
            std::fs::write(&port_file, bad).unwrap();
            assert_eq!(port_told(&port_file), None, "{bad}");
        }

        // the first one gone, the next is the server, and the old port is forgotten again
        first.publish(8791);
        drop(first);
        let Instance::First(next) = claim(&data, short) else {
            panic!("the lock must be free once its holder is gone")
        };
        assert!(next._lock.is_some());
        assert!(!port_file.exists());
        drop(next);
        let _ = std::fs::remove_dir_all(&data);
    }
}

//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The game: where it is installed (FiveM's `CitizenFX.ini`) and the keys of its archives.
//!
//! The keys are not shipped with this tool. The AES key is found in the user's own `GTA5.exe` by its
//! SHA-1; the NG keys, which no executable on disk holds, come from CodeWalker's `magic.dat` given
//! by the user (`btms import-keys <magic.dat>`, [`crate::game::magic`]). They are kept in the
//! user's local data folder (`%LOCALAPPDATA%\BTMS\keys`) in rage-rpf's format (`gtav_aes_key.dat`,
//! `gtav_ng_key.dat`, `gtav_ng_decrypt_tables.dat`), never in the repository, and checked against
//! their SHA-1 each time they are read. The game folder is only read.

pub mod index;
pub mod magic;
pub mod store;
pub mod vfs;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rage_rpf::GtaKeys;

/// The GTA V folder FiveM uses (`IVPath` of `%LOCALAPPDATA%\FiveM\FiveM.app\CitizenFX.ini`).
pub fn fivem_game_dir() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    let ini = Path::new(&local).join("FiveM").join("FiveM.app").join("CitizenFX.ini");
    let text = std::fs::read_to_string(ini).ok()?;
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("IVPath="))
        .map(|p| PathBuf::from(p.trim()))
        .find(|p| p.join("GTA5.exe").is_file())
}

/// The game folder: the one given, else FiveM's.
pub fn game_dir(given: Option<&Path>) -> Result<PathBuf> {
    let dir = match given {
        Some(p) => p.to_path_buf(),
        None => fivem_game_dir()
            .context("GTA V folder not found: give it with --game (FiveM records it in CitizenFX.ini, IVPath)")?,
    };
    if !dir.join("GTA5.exe").is_file() {
        bail!("{} is not a GTA V folder (no GTA5.exe)", dir.display());
    }
    Ok(dir)
}

/// Where the keys are kept: `%LOCALAPPDATA%\BTMS\keys`.
pub fn keys_dir() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(Path::new(&local).join("BTMS").join("keys"))
}

/// SHA-1 of the PC AES key of GTA V (`PC_AES_KEY_HASH` of rage-rpf 0.5.1, MIT, crypto/keys.rs).
const PC_AES_KEY_HASH: [u8; 20] = [
    0xA0, 0x79, 0x61, 0x28, 0xA7, 0x75, 0x72, 0x0A, 0xC2, 0x04, 0xD9, 0x81, 0x9F, 0x68, 0xC1, 0x72, 0xE3, 0x95, 0x2C,
    0x6D,
];
pub const AES_KEY_FILE: &str = "gtav_aes_key.dat";
pub const NG_KEYS_FILE: &str = "gtav_ng_key.dat";
pub const NG_TABLES_FILE: &str = "gtav_ng_decrypt_tables.dat";

/// The 32 bytes of `exe` whose SHA-1 is the AES key's.
fn find_aes_key(exe: &[u8]) -> Option<[u8; 32]> {
    use sha1::{Digest, Sha1};
    exe.windows(32).find(|w| <[u8; 20]>::from(Sha1::digest(w)) == PC_AES_KEY_HASH).map(|w| w.try_into().unwrap())
}

/// The keys of the archives.
///
/// The archives of the game are NG encrypted (the top-level ones and the DLC packs, header
/// 0x0FEFFFFF), and the NG keys and tables are in no executable on disk: none of the 101 key hashes
/// and none of the 84 distinct table hashes is found in the game's `GTA5.exe` or in FiveM's copy
/// (the executable is protected; they only exist once it runs). They are read from the local folder
/// once imported (`btms import-keys <magic.dat>`); until then they are empty and an NG archive is
/// refused with an error by `vfs`, never decrypted with empty keys. The AES key is read from the
/// local folder, else found in `GTA5.exe` and saved there.
pub fn keys(game: &Path) -> Result<GtaKeys> {
    let aes_key = aes_key(game)?;
    let dir = keys_dir()?;
    let (keys_file, tables_file) = (dir.join(NG_KEYS_FILE), dir.join(NG_TABLES_FILE));
    if !keys_file.is_file() || !tables_file.is_file() {
        return Ok(GtaKeys { aes_key, ng_keys: Vec::new(), ng_decrypt_tables: tables(&[]) });
    }
    let read = |f: &Path| std::fs::read(f).with_context(|| format!("reading {}", f.display()));
    let (ng, tb) = (read(&keys_file)?, read(&tables_file)?);
    crate::game::magic::check(&ng, &tb).with_context(|| format!("NG keys in {} (run `btms import-keys <magic.dat>` again, with the magic.dat of your own copy of CodeWalker)", dir.display()))?;
    Ok(GtaKeys {
        aes_key,
        ng_keys: ng.as_chunks::<272>().0.iter().map(|k| k.to_vec()).collect(),
        ng_decrypt_tables: tables(&tb),
    })
}

/// The NG decrypt tables from their bytes (17 × 16 × 256 `u32`, little endian; empty: zeroed), on
/// the heap (272 KB).
fn tables(bytes: &[u8]) -> Box<[[[u32; 256]; 16]; 17]> {
    let mut t: Box<[[[u32; 256]; 16]; 17]> =
        vec![[[0u32; 256]; 16]; 17].into_boxed_slice().try_into().expect("17 tables");
    for (n, w) in bytes.as_chunks::<4>().0.iter().enumerate().take(17 * 16 * 256) {
        t[n / 4096][n / 256 % 16][n % 256] = u32::from_le_bytes(*w);
    }
    t
}

/// Decodes `magic.dat` (CodeWalker's) with the AES key, checks every key and table, and keeps them
/// in the local folder. Returns that folder.
pub fn import_magic(game: &Path, magic_path: &Path) -> Result<PathBuf> {
    let aes_key = aes_key(game)?;
    let magic = std::fs::read(magic_path).with_context(|| format!("reading {}", magic_path.display()))?;
    let (ng, tb) = crate::game::magic::decode(&magic, &aes_key)?;
    let dir = keys_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, bytes) in [(NG_KEYS_FILE, &ng), (NG_TABLES_FILE, &tb)] {
        let file = dir.join(name);
        std::fs::write(&file, bytes).with_context(|| format!("writing {}", file.display()))?;
    }
    Ok(dir)
}

/// The AES key: from the local folder, else found in `GTA5.exe` and saved there.
fn aes_key(game: &Path) -> Result<[u8; 32]> {
    let dir = keys_dir()?;
    let file = dir.join(AES_KEY_FILE);
    let aes_key: [u8; 32] = match std::fs::read(&file).ok().and_then(|b| b.try_into().ok()) {
        Some(k) => k,
        None => {
            let exe_path = game.join("GTA5.exe");
            eprintln!("keys: looking for the AES key in {} (once only)…", exe_path.display());
            let exe = std::fs::read(&exe_path).with_context(|| format!("reading {}", exe_path.display()))?;
            let key = find_aes_key(&exe).context("AES key not found in GTA5.exe (game version not recognised?)")?;
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            std::fs::write(&file, key).with_context(|| format!("writing {}", file.display()))?;
            key
        }
    };
    Ok(aes_key)
}

/// Whether the NG keys are there (imported: see [`keys`]).
pub fn has_ng(keys: &GtaKeys) -> bool {
    keys.ng_keys.len() == 101
}

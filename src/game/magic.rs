//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The NG keys of GTA V from CodeWalker's `magic.dat` (github.com/dexyfex/CodeWalker,
//! `CodeWalker.Core/Resources/magic.dat`, given by the user: not shipped with this tool).
//!
//! The file is the NG keys and tables, deflated, AES encrypted with the PC AES key, then masked with
//! four rounds of .NET's `System.Random` seeded with the Jenkins hash of that same key. Undone here
//! in the reverse order, as CodeWalker does it (`GTA5Keys.UseMagicData`). Every key and table is
//! then checked against its SHA-1 ([`NG_KEY_HASHES`], [`NG_TABLE_HASHES`]): a wrong decoding is refused,
//! never used.

use aes::Aes256;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, KeyInit};
use anyhow::{Context, Result, bail};
use sha1::{Digest, Sha1};

/// The SHA-1 of each NG key (101, 0x110 bytes) and of each NG decrypt table (272, 0x400 bytes).
/// The hash lists are rage-rpf 0.5.1's (MIT, src/crypto/ng_key_hashes.rs and ng_table_hashes.rs),
/// copied in `src/game/hashes/`: hashes only, no key.
static NG_KEY_HASHES: [[u8; 20]; 101] = include!("hashes/ng_key_hashes.rs");
static NG_TABLE_HASHES: [[u8; 20]; 272] = include!("hashes/ng_table_hashes.rs");

/// 101 NG keys of 0x110 bytes.
pub const KEYS_LEN: usize = 101 * 272;
/// 17 × 16 NG decrypt tables of 256 `u32`.
pub const TABLES_LEN: usize = 17 * 16 * 256 * 4;

/// .NET's `System.Random` built with a seed: the subtractive generator of Numerical Recipes (Knuth),
/// which .NET Framework and .NET (its seeded "compat" implementation) both keep unchanged. The
/// arithmetic wraps like C#'s unchecked `int`.
struct DotNetRandom {
    seeds: [i32; 56],
    inext: usize,
    inextp: usize,
}

impl DotNetRandom {
    const MBIG: i32 = i32::MAX;
    const MSEED: i32 = 161_803_398;

    fn new(seed: i32) -> DotNetRandom {
        let mut a = [0i32; 56];
        let subtraction = if seed == i32::MIN { i32::MAX } else { seed.abs() };
        let mut mj = Self::MSEED.wrapping_sub(subtraction);
        a[55] = mj;
        let mut mk = 1i32;
        for i in 1..55 {
            let ii = (21 * i) % 55;
            a[ii] = mk;
            mk = mj.wrapping_sub(mk);
            if mk < 0 {
                mk = mk.wrapping_add(Self::MBIG);
            }
            mj = a[ii];
        }
        for _ in 1..5 {
            for i in 1..56 {
                a[i] = a[i].wrapping_sub(a[1 + (i + 30) % 55]);
                if a[i] < 0 {
                    a[i] = a[i].wrapping_add(Self::MBIG);
                }
            }
        }
        DotNetRandom { seeds: a, inext: 0, inextp: 21 }
    }

    fn sample(&mut self) -> i32 {
        self.inext = if self.inext + 1 >= 56 { 1 } else { self.inext + 1 };
        self.inextp = if self.inextp + 1 >= 56 { 1 } else { self.inextp + 1 };
        let mut r = self.seeds[self.inext].wrapping_sub(self.seeds[self.inextp]);
        if r == Self::MBIG {
            r -= 1;
        }
        if r < 0 {
            r = r.wrapping_add(Self::MBIG);
        }
        self.seeds[self.inext] = r;
        r
    }

    /// `NextBytes`: one sample per byte, its low byte.
    fn next_byte(&mut self) -> u8 {
        self.sample() as u8
    }
}

/// CodeWalker's `JenkHash.GenHash(byte[])`: Jenkins one-at-a-time over the bytes as they are.
fn jenkins(data: &[u8]) -> u32 {
    let mut h: u32 = 0;
    for &b in data {
        h = h.wrapping_add(b as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}

/// The NG keys (`KEYS_LEN` bytes) and tables (`TABLES_LEN` bytes) held by `magic`.
pub fn decode(magic: &[u8], aes_key: &[u8; 32]) -> Result<(Vec<u8>, Vec<u8>)> {
    // the mask: four buffers of the file's length drawn one after the other, subtracted
    let mut rnd = DotNetRandom::new(jenkins(aes_key) as i32);
    let mut data = magic.to_vec();
    for _ in 0..4 {
        for b in data.iter_mut() {
            *b = b.wrapping_sub(rnd.next_byte());
        }
    }
    // AES-256 ECB, whole blocks only (the tail stays as is, like CodeWalker's DecryptAESData)
    let cipher = Aes256::new(GenericArray::from_slice(aes_key));
    let whole = data.len() - data.len() % 16;
    for block in data[..whole].as_chunks_mut::<16>().0 {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    // raw DEFLATE
    let mut plain = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::DeflateDecoder::new(&data[..]), &mut plain)
        .context("magic.dat does not inflate: an unexpected file, or an AES key other than CodeWalker's")?;
    if plain.len() < KEYS_LEN + TABLES_LEN {
        bail!("decoded magic.dat too short: {} bytes (at least {} expected)", plain.len(), KEYS_LEN + TABLES_LEN);
    }
    let keys = plain[..KEYS_LEN].to_vec();
    let tables = plain[KEYS_LEN..KEYS_LEN + TABLES_LEN].to_vec();
    check(&keys, &tables)?;
    Ok((keys, tables))
}

/// Every NG key and table has the SHA-1 it must have, in its place (rage-rpf's lists, in the
/// order of `gtav_ng_key.dat` and `gtav_ng_decrypt_tables.dat`).
pub fn check(keys: &[u8], tables: &[u8]) -> Result<()> {
    if keys.len() != KEYS_LEN || tables.len() != TABLES_LEN {
        bail!(
            "unexpected size: keys {} bytes ({KEYS_LEN} expected), tables {} ({TABLES_LEN} expected)",
            keys.len(),
            tables.len()
        );
    }
    let sha = |b: &[u8]| <[u8; 20]>::from(Sha1::digest(b));
    let bad_keys =
        keys.as_chunks::<272>().0.iter().zip(NG_KEY_HASHES.iter()).filter(|(k, h)| sha(&k[..]) != **h).count();
    let bad_tables =
        tables.as_chunks::<1024>().0.iter().zip(NG_TABLE_HASHES.iter()).filter(|(t, h)| sha(&t[..]) != **h).count();
    if bad_keys + bad_tables > 0 {
        bail!("wrong NG keys: {bad_keys} key(s) of 101 and {bad_tables} table(s) of 272 do not match their SHA-1");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(seed: i32) -> Vec<u8> {
        let mut r = DotNetRandom::new(seed);
        (0..8).map(|_| r.next_byte()).collect()
    }

    /// Values of `new System.Random(seed).NextBytes(new byte[8])` on .NET 10 (PowerShell 7).
    #[test]
    fn dotnet_random_matches_net() {
        assert_eq!(bytes(42), [0x3e, 0x17, 0xba, 0x96, 0xae, 0x04, 0xcd, 0x3b]);
        // the lowest seed whose first value of the table is negative: wrapping arithmetic
        assert_eq!(bytes(-1_985_680_249), [0x89, 0x59, 0xca, 0x0e, 0x74, 0xda, 0xa2, 0xb5]);
        assert_eq!(bytes(i32::MIN), [0x1a, 0x0c, 0x4c, 0x6f, 0x5d, 0x73, 0xe4, 0xd8]);
    }

    #[test]
    fn check_refuses_wrong_keys() {
        assert!(check(&vec![0; KEYS_LEN], &vec![0; TABLES_LEN]).is_err());
        assert!(check(&[0; 10], &[0; 10]).is_err());
    }
}

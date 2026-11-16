//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! Models and textures loaded once for all threads.
//!
//! Each drawing thread used to keep its own models and textures: neighbouring blocks go to
//! different threads, so every thread read, inflated, parsed and decoded the same files (2.8
//! reads per file on the downtown test zone with 10 threads). Here a file is loaded by the first
//! thread that asks for it; a thread asking meanwhile waits for that load instead of doing it
//! again; all of them share the result (`Arc`).
//!
//! What is loaded is decided by [`crate::render::assets::Assets`], which keeps the loading itself: this
//! module only makes sure each key is loaded once ([`Loaded::get`]), counts the bytes held, and
//! empties everything when they pass a budget ([`Shared::trim`]). The keys are complete: a value
//! depends on nothing but its key (and the world), so that a picture cannot depend on which thread
//! asked first.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Instant;

use crate::formats::ydr::Drawable;
use crate::formats::ytd::Texture;
use crate::render::scene::Tex;
use crate::util::profile::{self, Phase};
use crate::world::World;

/// A model: (asset type, name, drawable dictionary or 0).
pub type ModelKey = (u32, u32, u32);

/// A model dictionary as loaded: (name of each model, model).
pub type ModelDictionary = Arc<Vec<(u32, Arc<Drawable>)>>;

/// What holds a texture: a model (its embedded textures) or a texture dictionary (its name's hash).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Holder {
    Model(ModelKey),
    Dictionary(u32),
}

/// Bytes held and how many times everything was emptied.
#[derive(Default)]
struct Account {
    bytes: AtomicUsize,
    generation: AtomicU64,
}

/// Values loaded once each, by key.
pub struct Loaded<K, V> {
    cells: Mutex<HashMap<K, Arc<OnceLock<V>>>>,
}

impl<K, V> Default for Loaded<K, V> {
    fn default() -> Self {
        Loaded { cells: Mutex::new(HashMap::new()) }
    }
}

impl<K: Eq + Hash, V: Clone> Loaded<K, V> {
    /// The value of `key`. The first thread to ask runs `load`, which returns the value and about
    /// how many bytes it holds; a thread asking while it runs waits for it.
    fn get(&self, account: &Account, key: K, load: impl FnOnce() -> (V, usize)) -> V {
        let cell = self.cells.lock().unwrap().entry(key).or_default().clone();
        let generation = account.generation.load(Ordering::Relaxed);
        let started = profile::on().then(Instant::now);
        let mut loaded_here = false;
        let value = cell
            .get_or_init(|| {
                loaded_here = true;
                let (value, bytes) = load();
                // emptied meanwhile: this value is no longer held by the cache
                if account.generation.load(Ordering::Relaxed) == generation {
                    account.bytes.fetch_add(bytes, Ordering::Relaxed);
                }
                value
            })
            .clone();
        if let (Some(started), false) = (started, loaded_here) {
            profile::add(Phase::Shared, started.elapsed());
        }
        value
    }

    fn clear(&self) {
        self.cells.lock().unwrap().clear();
    }
}

/// Everything loaded so far, for all threads.
#[derive(Default)]
pub struct Shared {
    account: Account,
    /// Models of their own file (`.ydr`, `.yft`), by (asset type, name).
    models: Loaded<(u32, u32), Option<Arc<Drawable>>>,
    /// Model dictionaries (`.ydd`), by name: (name of each model, model).
    dictionaries: Loaded<u32, Option<ModelDictionary>>,
    /// Texture dictionaries (`.ytd`) as stored, by name.
    txds: Loaded<u32, Option<Arc<Vec<Texture>>>>,
    /// Decoded textures, by what holds them and their lowercase name.
    images: Loaded<(Holder, String), Option<Tex>>,
    /// The world these were loaded for (its address; 0: none yet).
    world: AtomicUsize,
}

static SHARED: LazyLock<Shared> = LazyLock::new(Shared::default);

impl Shared {
    /// The cache of the assets of `world`. A run has one world; should another one come, what was
    /// loaded for the first is dropped (names resolve to files through the world's index).
    pub fn of(world: &World) -> &'static Shared {
        let id = world as *const World as usize;
        let before = SHARED.world.swap(id, Ordering::SeqCst);
        if before != 0 && before != id {
            SHARED.clear();
        }
        &SHARED
    }

    pub fn model(
        &self,
        key: (u32, u32),
        load: impl FnOnce() -> (Option<Arc<Drawable>>, usize),
    ) -> Option<Arc<Drawable>> {
        self.models.get(&self.account, key, load)
    }

    pub fn dictionary(
        &self,
        hash: u32,
        load: impl FnOnce() -> (Option<ModelDictionary>, usize),
    ) -> Option<ModelDictionary> {
        self.dictionaries.get(&self.account, hash, load)
    }

    pub fn txd(
        &self,
        hash: u32,
        load: impl FnOnce() -> (Option<Arc<Vec<Texture>>>, usize),
    ) -> Option<Arc<Vec<Texture>>> {
        self.txds.get(&self.account, hash, load)
    }

    pub fn image(&self, holder: Holder, name: &str, load: impl FnOnce() -> (Option<Tex>, usize)) -> Option<Tex> {
        self.images.get(&self.account, (holder, name.to_string()), load)
    }

    /// About how many bytes the cache holds.
    pub fn bytes(&self) -> usize {
        self.account.bytes.load(Ordering::Relaxed)
    }

    /// How many times the cache was emptied: what a thread keeps of an older generation is to be
    /// dropped too (the memory is only freed once nobody holds it).
    pub fn generation(&self) -> u64 {
        self.account.generation.load(Ordering::Relaxed)
    }

    /// Empties the cache when it holds more than `budget` bytes.
    pub fn trim(&self, budget: usize) {
        if self.bytes() > budget {
            self.clear();
        }
    }

    fn clear(&self) {
        self.account.generation.fetch_add(1, Ordering::Relaxed);
        self.models.clear();
        self.dictionaries.clear();
        self.txds.clear();
        self.images.clear();
        self.account.bytes.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loaded_once_and_counted() {
        let (account, loaded) = (Account::default(), Loaded::<u32, u32>::default());
        let calls = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for key in 0..50 {
                        let v = loaded.get(&account, key, || {
                            calls.fetch_add(1, Ordering::Relaxed);
                            (key * 2, 10)
                        });
                        assert_eq!(v, key * 2);
                    }
                });
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), 50);
        assert_eq!(account.bytes.load(Ordering::Relaxed), 500);
    }
}

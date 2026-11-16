//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The game loaded on every core: many files read, decrypted and parsed at once, their results
//! given back in the order of the files.
//!
//! The loading reads thousands of files that do not depend on one another (archetypes, placement
//! files, manifests, texture dictionaries), then folds what they hold into the map in a fixed
//! order where a later copy may win over an earlier one. [`map`] does the first half on all the
//! logical cores and returns the results in the order of the items, so that the fold, left as it
//! was, sees exactly what it saw when the files were read one after the other: the same order of
//! the archetypes, entities and files, the same winners.
//!
//! `BTMS_LOAD_THREADS=<n>` sets how many threads load (1: one after the other, as without
//! [`map`]; a check); [`with_threads`] does the same for the calls made by one thread (the tests).

use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    /// The number of threads [`with_threads`] asks for on this thread (0: not set).
    static FORCED: Cell<usize> = const { Cell::new(0) };
}

/// How many threads load: [`with_threads`] on this thread, else `BTMS_LOAD_THREADS`, else one
/// per logical core.
pub fn threads() -> usize {
    let forced = FORCED.with(Cell::get);
    if forced > 0 {
        return forced;
    }
    static DEFAULT: OnceLock<usize> = OnceLock::new();
    *DEFAULT.get_or_init(|| {
        let cores = std::thread::available_parallelism().map(usize::from).unwrap_or(8);
        std::env::var("BTMS_LOAD_THREADS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| (1..=256).contains(n))
            .unwrap_or(cores)
    })
}

/// Runs `f` with [`threads`] giving `n` on this thread (the loading it starts uses `n` threads).
#[cfg(test)]
pub fn with_threads<R>(n: usize, f: impl FnOnce() -> R) -> R {
    let before = FORCED.with(|c| c.replace(n.max(1)));
    // put back even if `f` panics
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCED.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(before);
    f()
}

/// `f` of every item, computed on [`threads`] threads, in the order of the items.
pub fn map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    map_with(threads(), items, f)
}

/// [`map`] on `threads` threads. With one thread, or one item, everything is done on the calling
/// thread, one item after the other.
pub fn map_with<T: Sync, R: Send>(threads: usize, items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = threads.min(items.len());
    if threads <= 1 {
        return items.iter().map(f).collect();
    }
    // each thread takes the next item not yet taken (the files differ a lot in size: a fixed share
    // per thread would leave some idle), and keeps its results with their places
    let next = AtomicUsize::new(0);
    let parts: Vec<Vec<(usize, R)>> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        done.push((i, f(item)));
                    }
                    done
                })
            })
            .collect();
        // (a panic in `f` is passed on, as it would be without the threads)
        workers.into_iter().map(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
    });
    let mut slots: Vec<Option<R>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    for (i, r) in parts.into_iter().flatten() {
        slots[i] = Some(r);
    }
    slots.into_iter().map(|r| r.expect("every item done")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_come_in_the_order_of_the_items() {
        let items: Vec<u64> = (0..10_000).collect();
        // uneven work, so that the threads finish out of order
        let slow = |&x: &u64| -> u64 {
            let mut v = x;
            for _ in 0..(x % 97) * 50 {
                v = v.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            }
            v ^ x
        };
        let alone: Vec<u64> = items.iter().map(slow).collect();
        for threads in [1, 2, 3, 16, 64] {
            assert_eq!(map_with(threads, &items, slow), alone, "{threads} threads");
        }
        assert!(map_with(8, &[] as &[u64], slow).is_empty());
        assert_eq!(map_with(8, &[5u64], slow), vec![slow(&5)]);
    }

    #[test]
    fn the_number_of_threads_can_be_forced_on_one_thread() {
        let n = with_threads(1, threads);
        assert_eq!(n, 1);
        assert_eq!(with_threads(3, || with_threads(5, threads)), 5);
        assert_eq!(with_threads(3, threads), 3);
        assert_ne!(FORCED.with(Cell::get), 3, "put back as it was");
    }

    #[test]
    #[should_panic(expected = "item 7")]
    fn a_panic_is_passed_on() {
        let items: Vec<usize> = (0..100).collect();
        map_with(4, &items, |&i| if i == 7 { panic!("item 7") } else { i });
    }
}

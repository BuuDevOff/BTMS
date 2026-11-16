//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The tiles of the blocks written apart from their drawing ([`pipeline`]): a thread that drew a
//! block hands its picture over and goes on to the next block at once, while threads of their own
//! halve it (zooms 8 and 7), encode its 21 tiles and write them ([`tiles`], what the drawing
//! thread did itself before, unchanged). Between them a bounded [`Queue`]: a picture of a block is
//! 16 MB, and when the writing lags the drawing waits for room, which hands its cores to the
//! writers. A block is reported done (its event, its counts) once its tiles are on disk. A block
//! the card made through the chain (`chain.rs`) comes with its zooms 8 and 7 already halved by the
//! card ([`Pixels::Zooms`]): only the cropping, the encoding and the files are left here.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::{BLOCK_PX, TILE, Written, crop, halve, write_tile};
use crate::util::profile::{Phase, time};

/// A block drawn: its picture (RGBA, `BLOCK_PX` × `BLOCK_PX`, the first row its north edge).
pub struct Picture {
    pub bx: i32,
    pub by: i32,
    pub pixels: Pixels,
    /// When its drawing began.
    pub started: Instant,
}

/// The pixels of a block: its picture, halved here (zooms 8 and 7), or its three zooms as the
/// chain on the card made them (the same halving: `chain.wgsl`).
pub enum Pixels {
    Whole(Vec<u8>),
    Zooms(crate::render::gpu::chain::Zooms),
}

/// A block written (or not): which one, when its drawing began, how long the writing took.
pub struct Report {
    pub bx: i32,
    pub by: i32,
    pub started: Instant,
    pub spent: Duration,
}

/// Writes a block's zoom 9, 8 and 7 tiles from its picture: 4 × 4, 2 × 2 and 1, in this order.
/// The first tile that cannot be written ends it, with its error.
pub fn tiles(out: &Path, bx: i32, by: i32, pixels: &Pixels) -> Result<Vec<Written>> {
    match pixels {
        Pixels::Whole(rgba) => tiles_of(out, bx, by, rgba),
        Pixels::Zooms(zooms) => tiles_of_zooms(out, bx, by, zooms),
    }
}

/// [`tiles`] of a block whose zooms 8 and 7 are made already: the same files, in the same order,
/// as [`tiles_of`] writes from the zoom 9 picture they were halved from.
fn tiles_of_zooms(out: &Path, bx: i32, by: i32, zooms: &crate::render::gpu::chain::Zooms) -> Result<Vec<Written>> {
    let mut written = Vec::with_capacity(21);
    for row in 0..4 {
        for col in 0..4 {
            let tile = time(Phase::Shrink, || crop(&zooms.z9, BLOCK_PX, col * TILE, row * TILE, TILE));
            written.push(write_tile(out, 9, bx * 4 + col as i32, by * 4 + (3 - row as i32), &tile)?);
        }
    }
    for row in 0..2 {
        for col in 0..2 {
            let tile = time(Phase::Shrink, || crop(&zooms.z8, BLOCK_PX / 2, col * TILE, row * TILE, TILE));
            written.push(write_tile(out, 8, bx * 2 + col as i32, by * 2 + (1 - row as i32), &tile)?);
        }
    }
    written.push(write_tile(out, 7, bx, by, &zooms.z7)?);
    Ok(written)
}

/// [`tiles`] of a whole picture: the code from before the chain, unchanged.
fn tiles_of(out: &Path, bx: i32, by: i32, rgba: &[u8]) -> Result<Vec<Written>> {
    let mut written = Vec::with_capacity(21);
    // zoom 9: 4 × 4 tiles; the picture's first row is the block's north edge
    for row in 0..4 {
        for col in 0..4 {
            let tile = time(Phase::Shrink, || crop(rgba, BLOCK_PX, col * TILE, row * TILE, TILE));
            written.push(write_tile(out, 9, bx * 4 + col as i32, by * 4 + (3 - row as i32), &tile)?);
        }
    }
    let half = time(Phase::Shrink, || halve(rgba, BLOCK_PX, BLOCK_PX));
    for row in 0..2 {
        for col in 0..2 {
            let tile = time(Phase::Shrink, || crop(&half, BLOCK_PX / 2, col * TILE, row * TILE, TILE));
            written.push(write_tile(out, 8, bx * 2 + col as i32, by * 2 + (1 - row as i32), &tile)?);
        }
    }
    let quarter = time(Phase::Shrink, || halve(&half, BLOCK_PX / 2, BLOCK_PX / 2));
    written.push(write_tile(out, 7, bx, by, &quarter)?);
    Ok(written)
}

/// A queue of at most `bound` items between threads that put them and threads that take them.
pub struct Queue<T> {
    state: Mutex<State<T>>,
    /// An item was taken, or the takers are gone.
    room: Condvar,
    /// An item was put, or the queue closed.
    ready: Condvar,
    bound: usize,
}

struct State<T> {
    items: VecDeque<T>,
    /// Nothing more will be put.
    closed: bool,
    /// Threads still taking.
    takers: usize,
    /// The most items it held at once.
    peak: usize,
}

impl<T> Queue<T> {
    pub fn new(bound: usize, takers: usize) -> Queue<T> {
        let bound = bound.max(1);
        Queue {
            state: Mutex::new(State { items: VecDeque::with_capacity(bound), closed: false, takers, peak: 0 }),
            room: Condvar::new(),
            ready: Condvar::new(),
            bound,
        }
    }

    /// Puts `item`, once there is room for it. Gives it back when no thread takes any more (they
    /// all ended, a panic among them): the caller deals with it.
    pub fn put(&self, item: T) -> Result<(), T> {
        let mut state = self.state.lock().unwrap();
        while state.items.len() >= self.bound && state.takers > 0 {
            state = self.room.wait(state).unwrap();
        }
        if state.takers == 0 {
            return Err(item);
        }
        state.items.push_back(item);
        state.peak = state.peak.max(state.items.len());
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    /// The next item, once there is one; None when the queue is closed and empty.
    pub fn take(&self) -> Option<T> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(item) = state.items.pop_front() {
                drop(state);
                self.room.notify_one();
                return Some(item);
            }
            if state.closed {
                return None;
            }
            state = self.ready.wait(state).unwrap();
        }
    }

    /// Nothing more will be put: the takers end once it is empty.
    pub fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.ready.notify_all();
    }

    /// A taker ended (or panicked). With none left, what is put is given back.
    pub fn taker_gone(&self) {
        let mut state = self.state.lock().unwrap();
        state.takers = state.takers.saturating_sub(1);
        drop(state);
        self.room.notify_all();
    }

    /// The most items it held at once.
    pub fn peak(&self) -> usize {
        self.state.lock().unwrap().peak
    }
}

/// Runs its function when dropped: at the end of a thread or of a scope, a panic included.
pub struct OnDrop<F: FnMut()>(pub F);

impl<F: FnMut()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

/// Pictures a queue holds, for `writers` threads writing: as many again as they write at once,
/// so that each finds the next one there when it is done.
pub fn bound(writers: usize) -> usize {
    2 * writers.max(1)
}

/// Draws and writes. `draw` runs on `drawers` threads (once each: it takes its blocks itself) and
/// hands every picture it makes to the function it is given, which returns once the picture is
/// queued (the time spent waiting for room is counted as `Phase::Queue`). `writers` threads
/// write them ([`tiles`]) and call `done` for each, with its result, once its tiles are on disk
/// or have failed. Returns when every picture is written, with the most pictures that waited in
/// the queue at once. Should every writer end (a panic), the drawing threads write their
/// pictures themselves.
pub fn pipeline<D, F>(out: &Path, drawers: usize, writers: usize, draw: D, done: F) -> usize
where
    D: Fn(&dyn Fn(Picture)) + Sync,
    F: Fn(Report, Result<Vec<Written>>) + Sync,
{
    let (drawers, writers) = (drawers.max(1), writers.max(1));
    let queue: Queue<Picture> = Queue::new(bound(writers), writers);
    let drawing = AtomicUsize::new(drawers);
    let write = |picture: Picture| {
        let t = Instant::now();
        let result = time(Phase::Write, || tiles(out, picture.bx, picture.by, &picture.pixels));
        let report = Report { bx: picture.bx, by: picture.by, started: picture.started, spent: t.elapsed() };
        // (16 MB: freed before the report is made)
        drop(picture);
        done(report, result);
    };
    std::thread::scope(|s| {
        for _ in 0..writers {
            s.spawn(|| {
                let _gone = OnDrop(|| queue.taker_gone());
                while let Some(picture) = queue.take() {
                    write(picture);
                }
            });
        }
        for _ in 0..drawers {
            s.spawn(|| {
                // the last drawing thread to end closes the queue, a panic included
                let _last = OnDrop(|| {
                    if drawing.fetch_sub(1, Ordering::AcqRel) == 1 {
                        queue.close();
                    }
                });
                draw(&|picture| {
                    if let Err(picture) = time(Phase::Queue, || queue.put(picture)) {
                        write(picture);
                    }
                });
            });
        }
    });
    queue.peak()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn random(seed: &mut u64) -> u64 {
        *seed ^= *seed >> 12;
        *seed ^= *seed << 25;
        *seed ^= *seed >> 27;
        seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A block's picture: `kind` 0 opaque, 1 holed here and there, 2 empty, 3 its western half
    /// empty (the coast); soft patches of colour, as drawn ground is.
    fn picture(kind: u8, seed: &mut u64) -> Vec<u8> {
        let n = BLOCK_PX;
        let mut rgba = vec![0u8; n * n * 4];
        let tone: Vec<u8> = (0..6).map(|_| random(seed) as u8).collect();
        for y in 0..n {
            for x in 0..n {
                let r = random(seed);
                let alpha = match kind {
                    0 => 255,
                    1 => {
                        if (x / 37 + y / 53).is_multiple_of(7) {
                            0
                        } else if r.is_multiple_of(11) {
                            (r >> 32) as u8
                        } else {
                            255
                        }
                    }
                    2 => 0,
                    _ => {
                        if x < n / 2 {
                            0
                        } else {
                            255
                        }
                    }
                };
                let p = &mut rgba[(y * n + x) * 4..][..4];
                p[0] = tone[0].wrapping_add((x / 16) as u8).wrapping_add((r % 9) as u8);
                p[1] = tone[1].wrapping_add((y / 16) as u8).wrapping_add((r >> 8) as u8 % 9);
                p[2] = tone[2].wrapping_add(((x + y) / 32) as u8);
                p[3] = alpha;
            }
        }
        rgba
    }

    /// The end of `block_timed` as it was before the writing went to threads of its own: the
    /// reference the stage is held to.
    fn written_before(out: &Path, bx: i32, by: i32, rgba: &[u8]) -> Result<Vec<Written>> {
        let mut written = Vec::with_capacity(21);
        for row in 0..4 {
            for col in 0..4 {
                let tile = time(Phase::Shrink, || crop(rgba, BLOCK_PX, col * TILE, row * TILE, TILE));
                written.push(write_tile(out, 9, bx * 4 + col as i32, by * 4 + (3 - row as i32), &tile)?);
            }
        }
        let half = time(Phase::Shrink, || halve(rgba, BLOCK_PX, BLOCK_PX));
        for row in 0..2 {
            for col in 0..2 {
                let tile = time(Phase::Shrink, || crop(&half, BLOCK_PX / 2, col * TILE, row * TILE, TILE));
                written.push(write_tile(out, 8, bx * 2 + col as i32, by * 2 + (1 - row as i32), &tile)?);
            }
        }
        let quarter = time(Phase::Shrink, || halve(&half, BLOCK_PX / 2, BLOCK_PX / 2));
        written.push(write_tile(out, 7, bx, by, &quarter)?);
        Ok(written)
    }

    /// Every file under `dir`: its path from there, and its bytes.
    fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut all = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let name = p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
                    all.insert(name, std::fs::read(&p).unwrap());
                }
            }
        }
        all
    }

    fn counts(w: &[Written]) -> [usize; 3] {
        [
            w.iter().filter(|x| **x == Written::Jpeg).count(),
            w.iter().filter(|x| **x == Written::Png).count(),
            w.iter().filter(|x| **x == Written::Empty).count(),
        ]
    }

    #[test]
    fn the_stage_writes_the_files_written_before_byte_for_byte() {
        let mut seed = 0x5DEE_CE66_D1CE_4E5Bu64;
        // blocks side by side and apart, of every kind, two of them sharing columns of tiles
        let blocks: Vec<((i32, i32), Vec<u8>)> =
            [((0, 0), 0), ((1, 0), 1), ((-3, 7), 2), ((-4, 7), 3), ((5, -2), 1), ((0, 1), 0)]
                .iter()
                .map(|&(b, kind)| (b, picture(kind, &mut seed)))
                .collect();
        let (before, after) = (scratch("write-before"), scratch("write-after"));
        let mut expected = BTreeMap::new();
        for ((bx, by), rgba) in &blocks {
            expected.insert((*bx, *by), counts(&written_before(&before, *bx, *by, rgba).unwrap()));
        }
        // three drawing threads hand the blocks over in an order of their own, two threads write
        let next = AtomicUsize::new(0);
        let order = [4, 1, 5, 0, 3, 2];
        let reported = Mutex::new(BTreeMap::new());
        pipeline(
            &after,
            3,
            2,
            |hand| {
                while let Some(&i) = order.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let ((bx, by), rgba) = &blocks[i];
                    hand(Picture { bx: *bx, by: *by, pixels: Pixels::Whole(rgba.clone()), started: Instant::now() });
                }
            },
            |report, result| {
                let previous = reported.lock().unwrap().insert((report.bx, report.by), counts(&result.unwrap()));
                assert!(previous.is_none(), "block {},{} reported twice", report.bx, report.by);
            },
        );
        assert_eq!(*reported.lock().unwrap(), expected);
        let (a, b) = (files(&before), files(&after));
        assert!(a.len() > 60, "{} files: the test checks little", a.len());
        assert!(a.keys().any(|k| k.ends_with(".png")) && a.keys().any(|k| k.ends_with(".jpg")));
        assert!(!b.keys().any(|k| k.ends_with(".tmp")), "a temporary file is left");
        assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
        for (name, bytes) in &a {
            assert!(b[name] == *bytes, "{name} differs");
        }
        let _ = (std::fs::remove_dir_all(&before), std::fs::remove_dir_all(&after));
    }

    /// A block whose zooms 8 and 7 come made (the chain on the card halves as `halve` does): the
    /// same files, byte for byte, as from its whole picture.
    #[test]
    fn zooms_made_already_give_the_same_files() {
        use crate::tiles::format::{self, Format};
        use crate::tiles::webp::{Effort, Quality};
        // a folder in the older format (JPEG and PNG), and WebP folders with a render's effort and
        // with the careful one
        for (name, format) in [
            ("jpeg-png", None),
            ("webp", Some((Format::default(), Effort::Fast))),
            ("webp-careful", Some((Format::Webp(Quality::Lossy(90)), Effort::Careful))),
        ] {
            let mut seed = 0x0123_4567_89AB_CDEFu64;
            let (whole, made) = (scratch(&format!("zooms-whole-{name}")), scratch(&format!("zooms-made-{name}")));
            if let Some((f, e)) = format {
                format::set(&whole, f, e);
                format::set(&made, f, e);
            }
            for (b, kind) in [((0, 0), 0u8), ((1, 0), 1), ((2, 3), 2), ((-1, -1), 3)] {
                let rgba = picture(kind, &mut seed);
                let half = halve(&rgba, BLOCK_PX, BLOCK_PX);
                let quarter = halve(&half, BLOCK_PX / 2, BLOCK_PX / 2);
                let a = tiles(&whole, b.0, b.1, &Pixels::Whole(rgba.clone())).unwrap();
                let z = tiles(
                    &made,
                    b.0,
                    b.1,
                    &Pixels::Zooms(crate::render::gpu::chain::Zooms { z9: rgba, z8: half, z7: quarter }),
                )
                .unwrap();
                assert_eq!(counts(&a), counts(&z));
            }
            let (a, b) = (files(&whole), files(&made));
            assert!(a.len() > 40);
            let extension = if format.is_some() { ".webp" } else { ".jpg" };
            assert!(a.keys().any(|k| k.ends_with(extension)), "{name}: {:?}", a.keys().collect::<Vec<_>>());
            assert!(format.is_none() || a.keys().all(|k| k.ends_with(".webp")), "{name}");
            assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
            for (file, bytes) in &a {
                assert!(b[file] == *bytes, "{name}: {file} differs");
            }
            let _ = (std::fs::remove_dir_all(&whole), std::fs::remove_dir_all(&made));
        }
    }

    #[test]
    fn a_failed_write_is_reported_and_the_other_blocks_written() {
        let mut seed = 77u64;
        let out = scratch("write-refused");
        // where the zoom 9 tiles of block (0, 0) go is a file: their folder cannot be made
        std::fs::create_dir_all(out.join("9")).unwrap();
        std::fs::write(out.join("9").join("1"), b"not a folder").unwrap();
        let blocks = [((0, 0), picture(0, &mut seed)), ((5, 0), picture(0, &mut seed))];
        let next = AtomicUsize::new(0);
        let results = Mutex::new(BTreeMap::new());
        pipeline(
            &out,
            2,
            2,
            |hand| {
                while let Some(((bx, by), rgba)) = blocks.get(next.fetch_add(1, Ordering::Relaxed)) {
                    hand(Picture { bx: *bx, by: *by, pixels: Pixels::Whole(rgba.clone()), started: Instant::now() });
                }
            },
            |report, result| {
                results
                    .lock()
                    .unwrap()
                    .insert((report.bx, report.by), result.map(|w| w.len()).map_err(|e| format!("{e:#}")));
            },
        );
        let results = results.into_inner().unwrap();
        let failure = results[&(0, 0)].as_ref().unwrap_err();
        assert!(failure.starts_with("creating "), "{failure}");
        assert_eq!(results[&(5, 0)].as_ref().ok(), Some(&21));
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Counts the items alive.
    struct Counted(Arc<AtomicUsize>);

    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn the_queue_is_bounded_and_loses_nothing() {
        let (alive, most) = (Arc::new(AtomicUsize::new(0)), AtomicUsize::new(0));
        let queue: Queue<(usize, Counted)> = Queue::new(3, 2);
        let taken = Mutex::new(Vec::new());
        let putting = AtomicUsize::new(4);
        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    while let Some((i, item)) = queue.take() {
                        // a slow writer: the putters wait for room
                        std::thread::sleep(Duration::from_millis(2));
                        taken.lock().unwrap().push(i);
                        drop(item);
                    }
                    queue.taker_gone();
                });
            }
            for t in 0..4 {
                let (alive, most, queue, putting) = (alive.clone(), &most, &queue, &putting);
                s.spawn(move || {
                    for k in 0..25 {
                        let n = alive.fetch_add(1, Ordering::Relaxed) + 1;
                        most.fetch_max(n, Ordering::Relaxed);
                        assert!(queue.put((t * 100 + k, Counted(alive.clone()))).is_ok());
                    }
                    if putting.fetch_sub(1, Ordering::AcqRel) == 1 {
                        queue.close();
                    }
                });
            }
        });
        let mut taken = taken.into_inner().unwrap();
        taken.sort_unstable();
        let all: Vec<usize> = (0..4).flat_map(|t| (0..25).map(move |k| t * 100 + k)).collect();
        assert_eq!(taken, all);
        assert!(queue.peak() <= 3, "{} items held at once", queue.peak());
        // alive at once: those in the queue, one in each taker's hands, one being put by each putter
        assert!(most.load(Ordering::Relaxed) <= 3 + 2 + 4, "{} alive at once", most.load(Ordering::Relaxed));
        assert_eq!(alive.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn with_no_writer_left_the_picture_comes_back() {
        let queue: Queue<u32> = Queue::new(1, 1);
        assert_eq!(queue.put(1), Ok(()));
        // full: a putter waits, until the last taker goes and hands its item back
        std::thread::scope(|s| {
            let waiting = s.spawn(|| queue.put(2));
            std::thread::sleep(Duration::from_millis(30));
            queue.taker_gone();
            assert_eq!(waiting.join().unwrap(), Err(2));
        });
        assert_eq!(queue.put(3), Err(3));
        // and a closed queue gives what it holds, then None
        queue.close();
        assert_eq!((queue.take(), queue.take()), (Some(1), None));
        assert_eq!(bound(0), 2);
        assert_eq!(bound(6), 12);
    }

    #[test]
    fn a_drawing_thread_that_panics_does_not_hang_the_others() {
        let out = scratch("write-panic");
        let mut seed = 3u64;
        let rgba = picture(2, &mut seed);
        let next = AtomicUsize::new(0);
        let written = AtomicUsize::new(0);
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline(
                &out,
                3,
                1,
                |hand| {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i == 0 {
                        panic!("a drawing thread fails");
                    }
                    hand(Picture { bx: i as i32, by: 0, pixels: Pixels::Whole(rgba.clone()), started: Instant::now() });
                },
                |_, _| {
                    written.fetch_add(1, Ordering::Relaxed);
                },
            )
        }));
        // the panic comes out, once the two other pictures are written
        assert!(run.is_err());
        assert_eq!(written.load(Ordering::Relaxed), 2);
        let _ = std::fs::remove_dir_all(&out);
    }
}

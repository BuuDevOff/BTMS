//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The machine the interface runs on, and its load, for the page's performance settings and
//! gauges. Read by the system only, without any tool of a card's maker:
//!
//! - [`machine`], once: the logical cores, the memory, the graphics cards (DXGI: name, memory of
//!   their own, kind). No device is created on a card: listing them is all;
//! - [`Sampler`], every second: the processor (`GetSystemTimes`), the memory in use
//!   (`GlobalMemoryStatusEx`), the card (the system's performance counters, `GPU Engine` and
//!   `GPU Adapter Memory`, those of the task manager).
//!
//! What cannot be read is None (null for the page), never an error. Memory is in GB of 2^30
//! bytes, as Windows shows it (a machine of 64 GB is 64, not 68.7).
//!
//! The pure parts (the names of the counters' instances, the load of a card from them, the
//! processor's from two readings, the history kept) are apart from the system calls, and tested.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;

/// Seconds of history kept for the page's curve (one reading a second).
pub const HISTORY: usize = 120;

/// A graphics card as the system lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Card {
    pub name: String,
    /// Its own memory (none for a software adapter).
    pub memory_gb: Option<f64>,
    #[serde(rename = "type")]
    pub kind: CardKind,
    /// How the performance counters name it (`0x00000000_0x000161f2`), to read its load.
    #[serde(skip)]
    pub luid: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CardKind {
    Dedicated,
    Integrated,
    Software,
    Unknown,
}

/// The machine (`machine` of the state).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Machine {
    pub cores: usize,
    pub memory_gb: Option<f64>,
    pub cards: Vec<Card>,
}

impl Card {
    /// The card in one line of text: "NVIDIA GeForce RTX 3090 Ti (dedicated, 23.74 GB)".
    pub fn line(&self) -> String {
        let kind = match self.kind {
            CardKind::Dedicated => "dedicated",
            CardKind::Integrated => "integrated",
            CardKind::Software => "software",
            CardKind::Unknown => "unknown type",
        };
        match self.memory_gb {
            Some(gb) => format!("{} ({kind}, {gb:.2} GB)", self.name),
            None => format!("{} ({kind})", self.name),
        }
    }
}

impl Machine {
    /// The card a render would draw on: a dedicated one before any other, the one with the most
    /// memory first; never a software one. The page chooses the same.
    pub fn main_card(&self) -> Option<&Card> {
        let rank = |c: &Card| {
            (
                if c.kind == CardKind::Dedicated { 0 } else { 1 },
                std::cmp::Reverse((c.memory_gb.unwrap_or(0.0) * 1000.0) as u64),
            )
        };
        self.cards.iter().filter(|c| c.kind != CardKind::Software).min_by_key(|c| rank(c))
    }
}

/// The kind of a card from what DXGI says of it: the software flag, its own memory, and the
/// budget of the memory it shares with the processor (`non_local`, None when it cannot be
/// asked). A card whose memory is the processor's (integrated) has no such second memory: its
/// budget there is zero. Unsure: unknown, rather than a guess.
pub fn kind_of(software: bool, dedicated_bytes: u64, non_local: Option<u64>) -> CardKind {
    if software {
        return CardKind::Software;
    }
    // (an adapter of nothing but screens, a virtual one: no memory of its own)
    if dedicated_bytes == 0 {
        return CardKind::Unknown;
    }
    match non_local {
        Some(0) => CardKind::Integrated,
        Some(_) => CardKind::Dedicated,
        None if dedicated_bytes >= 2 << 30 => CardKind::Dedicated,
        None => CardKind::Unknown,
    }
}

/// Bytes as GB of 2^30, to two decimals.
pub fn gb(bytes: u64) -> f64 {
    round(bytes as f64 / (1u64 << 30) as f64, 2)
}

fn round(x: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (x * f).round() / f
}

/// A card as the performance counters name it in their instances: `0x<high>_0x<low>`, the two
/// halves of its LUID in lowercase hexadecimal, eight digits each.
pub fn luid_name(high: i32, low: u32) -> String {
    format!("0x{:08x}_0x{:08x}", high as u32, low)
}

/// The card and the engine of an instance of `GPU Engine`:
/// `pid_12732_luid_0x00000000_0x000161f2_phys_0_eng_0_engtype_3d` → (`0x00000000_0x000161f2`,
/// `0_0`: the physical card and the engine on it).
pub fn engine_instance(name: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = name.split('_').collect();
    let after = |word: &str| parts.iter().position(|p| p.eq_ignore_ascii_case(word)).and_then(|i| parts.get(i + 1));
    let i = parts.iter().position(|p| p.eq_ignore_ascii_case("luid"))?;
    let (high, low) = (parts.get(i + 1)?, parts.get(i + 2)?);
    if !high.starts_with("0x") || !low.starts_with("0x") {
        return None;
    }
    Some((format!("{high}_{low}").to_ascii_lowercase(), format!("{}_{}", after("phys")?, after("eng")?)))
}

/// The card of an instance of `GPU Adapter Memory`: `luid_0x00000000_0x000161f2_phys_0`.
pub fn adapter_instance(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('_').collect();
    let i = parts.iter().position(|p| p.eq_ignore_ascii_case("luid"))?;
    let (high, low) = (parts.get(i + 1)?, parts.get(i + 2)?);
    (high.starts_with("0x") && low.starts_with("0x")).then(|| format!("{high}_{low}").to_ascii_lowercase())
}

/// The value of the main card among the values of every card: its own when the counters know it
/// (`known`: it is among the cards they name, so that a card nobody uses reads 0, not another's),
/// else the highest (the counters name the cards some other way than DXGI: better the busiest
/// card than nothing).
fn pick(per_card: &HashMap<String, f64>, main: Option<&str>, known: bool) -> Option<f64> {
    if let Some(main) = main.map(str::to_ascii_lowercase) {
        if let Some(v) = per_card.get(&main) {
            return Some(*v);
        }
        if known {
            return Some(0.0);
        }
    }
    per_card.values().copied().reduce(f64::max)
}

/// The load of the main card, in percent, from the instances of `GPU Engine` (one per process,
/// card and engine), as the task manager counts it: each engine is the sum over the processes,
/// and the card is its busiest engine. `cards`: the cards the counters name.
pub fn gpu_percent(engines: &[(String, f64)], main: Option<&str>, cards: &[String]) -> Option<f64> {
    let mut per_engine: HashMap<(String, String), f64> = HashMap::new();
    for (name, value) in engines {
        if let Some(key) = engine_instance(name) {
            *per_engine.entry(key).or_default() += value.max(0.0);
        }
    }
    let mut per_card: HashMap<String, f64> = HashMap::new();
    for ((card, _), value) in per_engine {
        let slot = per_card.entry(card).or_default();
        *slot = slot.max(value);
    }
    if per_card.is_empty() && cards.is_empty() {
        return None;
    }
    let known = main.is_some_and(|m| cards.iter().any(|c| c.eq_ignore_ascii_case(m)));
    pick(&per_card, main, known).map(|v| round(v.min(100.0), 1))
}

/// The memory of the main card in use, in GB, from the instances of `GPU Adapter Memory`
/// (`Dedicated Usage`, in bytes, one per card and physical card).
pub fn card_memory_gb(adapters: &[(String, f64)], main: Option<&str>) -> Option<f64> {
    let mut per_card: HashMap<String, f64> = HashMap::new();
    for (name, value) in adapters {
        if let Some(card) = adapter_instance(name) {
            *per_card.entry(card).or_default() += value.max(0.0);
        }
    }
    let known = main.is_some_and(|m| per_card.contains_key(&m.to_ascii_lowercase()));
    pick(&per_card, main, known).map(|bytes| gb(bytes as u64))
}

/// The processor's load between two readings of `GetSystemTimes` (idle, kernel, user, in
/// hundreds of nanoseconds: the kernel's time holds the idle time). None when no time passed.
pub fn cpu_percent(before: [u64; 3], after: [u64; 3]) -> Option<f64> {
    let idle = after[0].checked_sub(before[0])?;
    let total = after[1].checked_sub(before[1])? + after[2].checked_sub(before[2])?;
    if total == 0 {
        return None;
    }
    Some(round((total.saturating_sub(idle) as f64 / total as f64 * 100.0).clamp(0.0, 100.0), 1))
}

/// One reading of the load.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub cpu: Option<f64>,
    pub gpu: Option<f64>,
    pub ram_used_gb: Option<f64>,
    pub vram_used_gb: Option<f64>,
}

/// The load as the server keeps it: the last reading and those of the last two minutes.
#[derive(Clone, Debug, Default)]
pub struct Load {
    latest: Option<Sample>,
    history: VecDeque<(f64, Option<f64>, Option<f64>)>,
}

/// The load as the page gets it (`load`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LoadView {
    pub cpu: Option<f64>,
    pub gpu: Option<f64>,
    pub ram_used_gb: Option<f64>,
    pub vram_used_gb: Option<f64>,
    /// `[seconds since the interface started, processor %, card % | null]`, oldest first.
    pub history: Vec<(f64, Option<f64>, Option<f64>)>,
}

impl Load {
    /// A reading made `seconds` after the interface started.
    pub fn push(&mut self, seconds: f64, sample: Sample) {
        self.latest = Some(sample);
        self.history.push_back((round(seconds, 1), sample.cpu, sample.gpu));
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
    }

    /// None before the first reading.
    pub fn view(&self) -> Option<LoadView> {
        let s = self.latest?;
        Some(LoadView {
            cpu: s.cpu,
            gpu: s.gpu,
            ram_used_gb: s.ram_used_gb,
            vram_used_gb: s.vram_used_gb,
            history: self.history.iter().copied().collect(),
        })
    }
}

/// The machine, read once (a fraction of a second: the cards are listed, not opened).
pub fn machine() -> Machine {
    let cores = std::thread::available_parallelism().map(usize::from).unwrap_or(1);
    Machine { cores, memory_gb: system::memory().map(|(total, _)| gb(total)), cards: system::cards() }
}

/// Reads the load, once a second, on a thread of its own (its handles stay on that thread).
pub struct Sampler {
    primed: bool,
    times: Option<[u64; 3]>,
    counters: Option<system::Counters>,
}

impl Default for Sampler {
    fn default() -> Sampler {
        Sampler::new()
    }
}

impl Sampler {
    pub fn new() -> Sampler {
        Sampler { primed: false, times: None, counters: system::Counters::open() }
    }

    /// A reading; None the first time, which only starts the counts (a load is a difference
    /// between two readings). `main`: the card whose load is wanted (its LUID name).
    pub fn sample(&mut self, main: Option<&str>) -> Option<Sample> {
        let times = system::times();
        let cpu = match (self.times, times) {
            (Some(before), Some(after)) => cpu_percent(before, after),
            _ => None,
        };
        self.times = times.or(self.times);
        let (gpu, vram) = match self.counters.as_mut().map(system::Counters::read) {
            Some(Some((engines, adapters))) => {
                let cards: Vec<String> = adapters.iter().filter_map(|(name, _)| adapter_instance(name)).collect();
                (gpu_percent(&engines, main, &cards), card_memory_gb(&adapters, main))
            }
            _ => (None, None),
        };
        let ram_used_gb = system::memory().map(|(total, free)| gb(total.saturating_sub(free)));
        let primed = std::mem::replace(&mut self.primed, true);
        primed.then_some(Sample { cpu, gpu, ram_used_gb, vram_used_gb: vram })
    }
}

#[cfg(windows)]
mod system {
    use std::collections::HashSet;

    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL,
        DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIFactory1,
    };
    use windows::Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER,
        PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
        PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    };
    use windows::Win32::System::Threading::GetSystemTimes;
    use windows::core::{Interface, PCWSTR, w};

    use super::{Card, gb, kind_of, luid_name};

    /// Counter instances and their values.
    type Readings = Vec<(String, f64)>;

    /// `MEMORYSTATUSEX` (kernel32, linked by every program: no feature of the crate for it).
    #[repr(C)]
    struct MemoryStatus {
        length: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatus) -> i32;
    }

    /// The physical memory: total and free, in bytes.
    pub fn memory() -> Option<(u64, u64)> {
        let mut status = MemoryStatus {
            length: std::mem::size_of::<MemoryStatus>() as u32,
            load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        // SAFETY: the structure is the system's, its length given as the call wants
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) } != 0;
        (ok && status.total_phys > 0).then_some((status.total_phys, status.avail_phys))
    }

    /// Idle, kernel and user time of all the processors since the start, in hundreds of nanoseconds.
    pub fn times() -> Option<[u64; 3]> {
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: three structures of this frame, written by the call
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
        let n = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        Some([n(idle), n(kernel), n(user)])
    }

    /// The graphics cards DXGI lists, each once: no device is created. It may list a card twice,
    /// under two LUIDs (the second one a LUID the performance counters do not know): the first of
    /// each model is kept, by its maker, device, subsystem, revision and memory. Two identical
    /// cards are then one.
    pub fn cards() -> Vec<Card> {
        // SAFETY: DXGI's own factory; every interface is released when dropped
        let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return Vec::new() };
        let mut seen = HashSet::new();
        let mut cards = Vec::new();
        for i in 0..64 {
            // (the end of the list is an error, DXGI_ERROR_NOT_FOUND)
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
            let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
            let luid = luid_name(desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart);
            if !seen.insert((desc.VendorId, desc.DeviceId, desc.SubSysId, desc.Revision, desc.DedicatedVideoMemory)) {
                continue;
            }
            let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
            let name = String::from_utf16_lossy(&desc.Description[..end]).trim().to_string();
            let software = (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0;
            // the budget of the memory shared with the processor: zero on an integrated card
            let non_local = adapter.cast::<IDXGIAdapter3>().ok().and_then(|a| {
                let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                // SAFETY: one structure of this frame, written by the call
                unsafe { a.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, &mut info) }
                    .ok()
                    .map(|_| info.Budget)
            });
            let dedicated = desc.DedicatedVideoMemory as u64;
            cards.push(Card {
                name,
                memory_gb: (dedicated > 0).then(|| gb(dedicated)),
                kind: kind_of(software, dedicated, non_local),
                luid: Some(luid),
            });
        }
        cards
    }

    /// The query of the two counters of the cards, open as long as the sampler lives.
    pub struct Counters {
        query: PDH_HQUERY,
        engines: Option<PDH_HCOUNTER>,
        adapters: Option<PDH_HCOUNTER>,
    }

    impl Counters {
        /// None when the system has no performance counters to give.
        pub fn open() -> Option<Counters> {
            let mut query = PDH_HQUERY::default();
            // SAFETY: a query of this process, closed when dropped
            if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != 0 {
                return None;
            }
            let add = |path: PCWSTR| {
                let mut counter = PDH_HCOUNTER::default();
                // SAFETY: the query is open; the English names are those of every language of Windows
                (unsafe { PdhAddEnglishCounterW(query, path, 0, &mut counter) } == 0).then_some(counter)
            };
            let engines = add(w!("\\GPU Engine(*)\\Utilization Percentage"));
            let adapters = add(w!("\\GPU Adapter Memory(*)\\Dedicated Usage"));
            Some(Counters { query, engines, adapters })
        }

        /// The instances of both counters and their values; None when the reading failed.
        pub fn read(&mut self) -> Option<(Readings, Readings)> {
            // SAFETY: the query is open
            if unsafe { PdhCollectQueryData(self.query) } != 0 {
                return None;
            }
            Some((self.engines.map(values).unwrap_or_default(), self.adapters.map(values).unwrap_or_default()))
        }
    }

    impl Drop for Counters {
        fn drop(&mut self) {
            // SAFETY: opened by `open`, closed once
            unsafe { PdhCloseQuery(self.query) };
        }
    }

    /// Every instance of a counter with its value, those whose value is valid this time (a rate
    /// has none at its first reading, nor an instance that just appeared).
    fn values(counter: PDH_HCOUNTER) -> Vec<(String, f64)> {
        // the instances may grow between the call that measures and the one that reads: again then
        for _ in 0..4 {
            let (mut bytes, mut count) = (0u32, 0u32);
            // SAFETY: no buffer: the call says how many bytes it needs
            let status = unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, None) };
            if status != PDH_MORE_DATA {
                return Vec::new();
            }
            // aligned for the items (pointers and doubles); the names follow them in the same buffer
            let mut buffer = vec![0u64; (bytes as usize).div_ceil(8) + 1];
            let mut size = (buffer.len() * 8) as u32;
            let items = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            // SAFETY: a buffer of `size` bytes, aligned to 8
            let status =
                unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) };
            if status == PDH_MORE_DATA {
                continue;
            }
            if status != 0 {
                return Vec::new();
            }
            // SAFETY: the call wrote `count` items at the start of the buffer, which lives meanwhile
            let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
            return items
                .iter()
                .filter(|item| matches!(item.FmtValue.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA))
                // SAFETY: a name the call wrote in the buffer; a double, asked as such
                .filter_map(|item| {
                    Some((unsafe { item.szName.to_string() }.ok()?, unsafe { item.FmtValue.Anonymous.doubleValue }))
                })
                .filter(|(_, v)| v.is_finite())
                .collect();
        }
        Vec::new()
    }
}

#[cfg(not(windows))]
mod system {
    use super::Card;

    /// Counter instances and their values.
    type Readings = Vec<(String, f64)>;

    pub fn memory() -> Option<(u64, u64)> {
        None
    }

    pub fn times() -> Option<[u64; 3]> {
        None
    }

    pub fn cards() -> Vec<Card> {
        Vec::new()
    }

    pub struct Counters;

    impl Counters {
        pub fn open() -> Option<Counters> {
            None
        }

        pub fn read(&mut self) -> Option<(Readings, Readings)> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RTX: &str = "0x00000000_0x000161f2";
    const RADEON: &str = "0x00000000_0x00019bf9";

    fn engine(pid: u32, luid: &str, eng: u32, kind: &str, value: f64) -> (String, f64) {
        (format!("pid_{pid}_luid_{luid}_phys_0_eng_{eng}_engtype_{kind}"), value)
    }

    #[test]
    fn the_counters_instances_are_read() {
        assert_eq!(
            engine_instance("pid_12732_luid_0x00000000_0x000161f2_phys_0_eng_0_engtype_3d"),
            Some((RTX.into(), "0_0".into()))
        );
        assert_eq!(
            engine_instance("pid_4_luid_0x00000000_0x000161F2_phys_1_eng_11_engtype_compute_1"),
            Some((RTX.into(), "1_11".into()))
        );
        assert_eq!(adapter_instance("luid_0x00000000_0x000161f2_phys_0"), Some(RTX.into()));
        for bad in ["", "_Total", "pid_1_phys_0_eng_0", "luid_12_34_phys_0_eng_0", "pid_1_luid_0x0_0x1", "luid"] {
            assert_eq!(engine_instance(bad), None, "{bad}");
        }
        assert_eq!(adapter_instance("luid_0x1"), None);
        // the name DXGI's LUID is given: the same as the counters'
        assert_eq!(luid_name(0, 0x161f2), RTX);
        assert_eq!(luid_name(-1, 0xABCDEF01), "0xffffffff_0xabcdef01");
    }

    #[test]
    fn the_card_s_load_is_its_busiest_engine() {
        let cards = vec![RTX.to_string(), RADEON.to_string()];
        let engines = vec![
            // two processes on the 3D engine of the RTX: 30 + 25
            engine(100, RTX, 0, "3d", 30.0),
            engine(200, RTX, 0, "3d", 25.0),
            // the compute engine, busier alone
            engine(200, RTX, 11, "compute_1", 71.5),
            // the copy engine
            engine(200, RTX, 3, "copy", 5.0),
            // the integrated card, busy: not the main one
            engine(300, RADEON, 0, "3d", 90.0),
        ];
        assert_eq!(gpu_percent(&engines, Some(RTX), &cards), Some(71.5));
        assert_eq!(gpu_percent(&engines, Some(RADEON), &cards), Some(90.0));
        // never above 100, nor below 0
        let full = vec![engine(1, RTX, 0, "3d", 80.0), engine(2, RTX, 0, "3d", 70.0), engine(3, RTX, 1, "3d", -4.0)];
        assert_eq!(gpu_percent(&full, Some(RTX), &cards), Some(100.0));
        // the main card used by nobody: 0, not the other card's load
        assert_eq!(gpu_percent(&[engine(300, RADEON, 0, "3d", 40.0)], Some(RTX), &cards), Some(0.0));
        // a main card the counters do not name (another LUID): the busiest card
        assert_eq!(gpu_percent(&engines, Some("0x00000000_0x0000dead"), &cards), Some(90.0));
        // no main card known: the busiest
        assert_eq!(gpu_percent(&engines, None, &cards), Some(90.0));
        // nothing to read: null
        assert_eq!(gpu_percent(&[], Some(RTX), &[]), None);
        assert_eq!(gpu_percent(&[], None, &cards), None);
        // instances of another shape are left out
        assert_eq!(gpu_percent(&[("_Total".into(), 50.0)], None, &[]), None);
    }

    #[test]
    fn the_card_s_memory_in_use() {
        let adapters = vec![
            (format!("luid_{RTX}_phys_0"), 16_828_014_592.0),
            (format!("luid_{RADEON}_phys_0"), 24_014_848.0),
            ("luid_0x00000000_0x00019ba6_phys_0".to_string(), 0.0),
        ];
        assert_eq!(card_memory_gb(&adapters, Some(RTX)), Some(15.67));
        assert_eq!(card_memory_gb(&adapters, Some(RADEON)), Some(0.02));
        assert_eq!(card_memory_gb(&adapters, None), Some(15.67));
        assert_eq!(card_memory_gb(&adapters, Some("0x00000000_0x0000dead")), Some(15.67));
        assert_eq!(card_memory_gb(&[], Some(RTX)), None);
    }

    #[test]
    fn the_processor_s_load_between_two_readings() {
        // 4 s of four processors' time, of which 1 s idle: 75 %
        assert_eq!(cpu_percent([0, 0, 0], [10_000_000, 25_000_000, 15_000_000]), Some(75.0));
        assert_eq!(cpu_percent([5, 5, 5], [5, 5, 5]), None);
        // a counter that went back (never, but not a panic)
        assert_eq!(cpu_percent([9, 9, 9], [1, 1, 1]), None);
        assert_eq!(cpu_percent([0, 0, 0], [10, 10, 0]), Some(0.0));
        assert_eq!(cpu_percent([0, 0, 0], [0, 0, 10]), Some(100.0));
    }

    #[test]
    fn the_kind_of_a_card() {
        assert_eq!(kind_of(true, 0, Some(0)), CardKind::Software);
        assert_eq!(kind_of(false, 24 << 30, Some(32 << 30)), CardKind::Dedicated);
        assert_eq!(kind_of(false, 512 << 20, Some(0)), CardKind::Integrated);
        assert_eq!(kind_of(false, 0, Some(0)), CardKind::Unknown);
        assert_eq!(kind_of(false, 8 << 30, None), CardKind::Dedicated);
        assert_eq!(kind_of(false, 512 << 20, None), CardKind::Unknown);
        let names: Vec<String> = [CardKind::Dedicated, CardKind::Integrated, CardKind::Software, CardKind::Unknown]
            .iter()
            .map(|k| serde_json::to_string(k).unwrap())
            .collect();
        assert_eq!(names, ["\"dedicated\"", "\"integrated\"", "\"software\"", "\"unknown\""]);
    }

    #[test]
    fn the_main_card_is_the_dedicated_one_with_the_most_memory() {
        let card = |name: &str, memory_gb: Option<f64>, kind| Card { name: name.into(), memory_gb, kind, luid: None };
        let m = Machine {
            cores: 16,
            memory_gb: Some(63.9),
            cards: vec![
                card("AMD Radeon(TM) Graphics", Some(0.5), CardKind::Integrated),
                card("Microsoft Basic Render Driver", None, CardKind::Software),
                card("small", Some(4.0), CardKind::Dedicated),
                card("NVIDIA GeForce RTX 3090 Ti", Some(23.7), CardKind::Dedicated),
            ],
        };
        assert_eq!(m.main_card().map(|c| c.name.as_str()), Some("NVIDIA GeForce RTX 3090 Ti"));
        let integrated = Machine { cards: m.cards[..2].to_vec(), ..m.clone() };
        assert_eq!(integrated.main_card().map(|c| c.name.as_str()), Some("AMD Radeon(TM) Graphics"));
        let software = Machine { cards: m.cards[1..2].to_vec(), ..m.clone() };
        assert_eq!(software.main_card(), None);
        // in the server's log
        assert_eq!(m.cards[3].line(), "NVIDIA GeForce RTX 3090 Ti (dedicated, 23.70 GB)");
        assert_eq!(m.cards[1].line(), "Microsoft Basic Render Driver (software)");
        // as the page gets it: no LUID
        let page = serde_json::to_value(&m).unwrap();
        assert_eq!(
            page["cards"][3],
            serde_json::json!({"name": "NVIDIA GeForce RTX 3090 Ti", "memory_gb": 23.7, "type": "dedicated"})
        );
        assert_eq!(page["cards"][1]["memory_gb"], serde_json::Value::Null);
    }

    #[test]
    fn the_history_is_two_minutes() {
        let mut load = Load::default();
        assert_eq!(load.view(), None);
        for i in 0..300 {
            let gpu = (i % 2 == 0).then_some(i as f64 / 10.0);
            load.push(i as f64 + 0.04, Sample { cpu: Some(50.0), gpu, ram_used_gb: Some(20.0), vram_used_gb: None });
        }
        let view = load.view().unwrap();
        assert_eq!(view.history.len(), HISTORY);
        assert_eq!(view.history[0], (180.0, Some(50.0), Some(18.0)));
        assert_eq!(view.history[HISTORY - 1], (299.0, Some(50.0), None));
        assert_eq!((view.cpu, view.gpu, view.ram_used_gb, view.vram_used_gb), (Some(50.0), None, Some(20.0), None));
        let page = serde_json::to_value(&view).unwrap();
        assert_eq!(page["history"][HISTORY - 1], serde_json::json!([299.0, 50.0, null]));
        assert_eq!(page["vram_used_gb"], serde_json::Value::Null);
    }

    #[test]
    fn readings_that_fail_are_null_not_errors() {
        // nothing at all could be read: every value null, the history kept all the same
        let mut load = Load::default();
        load.push(1.0, Sample::default());
        let view = load.view().unwrap();
        assert_eq!((view.cpu, view.gpu, view.ram_used_gb, view.vram_used_gb), (None, None, None, None));
        assert_eq!(view.history, [(1.0, None, None)]);
    }

    /// Whether a query of the counters opened before a process starts using the card counts that
    /// process (a render starts after the interface): this test process opens the counters, then
    /// starts using the card (a device and empty submissions: nothing drawn), and the instances of
    /// its own pid are looked for in the old query and in a new one. By hand:
    /// `cargo test --release counted_later -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn a_process_that_starts_using_the_card_later_is_counted() {
        fn now<F: std::future::Future>(future: F) -> F::Output {
            let mut future = std::pin::pin!(future);
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            loop {
                if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
                    return value;
                }
                std::thread::yield_now();
            }
        }
        let mine = format!("pid_{}_", std::process::id());
        let own = |engines: &[(String, f64)]| engines.iter().filter(|(name, _)| name.starts_with(&mine)).count();
        let mut old = system::Counters::open().expect("counters");
        old.read();
        std::thread::sleep(std::time::Duration::from_secs(1));
        let (before, _) = old.read().expect("reading");
        println!("before: {} instances, {} of them from this process", before.len(), own(&before));

        // this process starts using the card (kept until the end of the process, as `render::gpu` does)
        let instance: &'static wgpu::Instance =
            Box::leak(Box::new(wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())));
        let adapter = now(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .expect("card");
        println!("card: {}", adapter.get_info().name);
        let (device, queue) = now(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
        let busy = |seconds: u64| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
            while std::time::Instant::now() < until {
                queue.submit([device.create_command_encoder(&Default::default()).finish()]);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        busy(2);
        let mut fresh = system::Counters::open().expect("counters");
        fresh.read();
        for i in 0..3 {
            busy(1);
            let (in_old, _) = old.read().expect("reading");
            let (in_fresh, _) = fresh.read().expect("reading");
            println!(
                "after {} s: old query {} instances of this process, new query {}",
                i + 3,
                own(&in_old),
                own(&in_fresh)
            );
        }
        let (in_old, _) = old.read().expect("reading");
        assert!(own(&in_old) > 0, "the query opened before does not see this process");
        std::mem::forget((adapter, device, queue));
    }

    /// What this machine says of itself, for a look by hand: `cargo test machine -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn this_machine() {
        let m = machine();
        println!("{}", serde_json::to_string_pretty(&m).unwrap());
        for c in &m.cards {
            println!("{}: {:?}", c.name, c.luid);
        }
        let main = m.main_card().and_then(|c| c.luid.clone());
        let mut sampler = Sampler::new();
        let mut load = Load::default();
        for i in 0..4 {
            if let Some(s) = sampler.sample(main.as_deref()) {
                load.push(i as f64, s);
                println!("{s:?}");
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        println!("{}", serde_json::to_string(&load.view()).unwrap());
    }
}

//! Ports `runtime/GpuInventory.java`.
//!
//! Enumerates GPUs and picks the engine backend. NVIDIA cards are read through `nvidia-smi`,
//! which ships with every driver and reports free memory live. Without an NVIDIA card the runtime
//! uses Vulkan, and since 2026-09-22 reads the card's memory through the Vulkan engine itself:
//! `llama-server --list-devices` prints every device the engine will use with its total and free
//! memory ("Vulkan0: AMD Radeon RX 7800 XT (16368 MiB, 15734 MiB free)"), the same numbers the
//! engine plans with, from the Vulkan memory budget, for AMD, Intel and NVIDIA alike. It costs
//! about a third of a second, so those readings are cached longer and refreshed off the calling
//! task for the status bar; placement still asks for a fresh one. Before the Vulkan engine is
//! installed there is no reading, as before, and the engine fits itself.
//!
//! On a Mac the GPU is Apple silicon's, run through Metal, and its memory is the machine's own:
//! the reading comes from Metal itself ([`query_metal`]), one device, planned against the working
//! set Metal recommends for a process or the memory the system has available, whichever is less.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use super::backend::Backend;

/// Memory the driver and the desktop keep for themselves; never planned against.
pub const DRIVER_RESERVE_BYTES: u64 = 512 << 20;
/// How long a reading from the engine's device list stays fresh for the status bar.
pub const ENGINE_LIST_CACHE: Duration = Duration::from_secs(15);
/// How long an nvidia-smi reading stays fresh.
pub const NVIDIA_SMI_CACHE: Duration = Duration::from_secs(2);
/// How long the first snapshot waits for the first background reading before answering.
pub const FIRST_READING_WAIT: Duration = Duration::from_millis(1500);
/// A device whose "memory" is the machine's RAM: at or above this share of physical memory it is
/// shared, not video, memory.
pub const SHARED_MEMORY_SHARE: f64 = 0.4;
/// QA on an NVIDIA machine: `NOOK_RS_INVENTORY=engine` skips nvidia-smi so the engine's device
/// list, the path every AMD and Intel machine takes, can be exercised without the hardware.
pub const INVENTORY_ENV: &str = "NOOK_RS_INVENTORY";
/// An explicit backend (`cuda`, `vulkan`, `cpu`) that wins over detection.
pub const BACKEND_ENV: &str = "NOOK_RS_BACKEND";

const NVIDIA_SMI_TIMEOUT: Duration = Duration::from_secs(5);
const ENGINE_LIST_TIMEOUT: Duration = Duration::from_secs(10);

static DEVICE_LINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*([A-Za-z]+)(\d+):\s+(.+?)\s+\((\d+)\s+MiB,\s+(\d+)\s+MiB free\)\s*$")
        .expect("device line pattern")
});
static INTEGRATED_NAME: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"(?i)^(intel\(r\) (hd |uhd |iris|graphics|arc\(tm\) graphics)|intel\(r\) iris|amd radeon\(tm\) graphics|amd radeon graphics|",
        r"amd radeon\(tm\) \d{3}m\b|amd radeon \d{3}m\b|radeon\(tm\) (graphics|vega)|amd radeon\(tm\) vega|apple m\d)"
    ))
    .expect("integrated name pattern")
});
static MODEL_NUMBER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\d{3,}").expect("model number pattern"));
static SOFTWARE_NAME: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)llvmpipe|swiftshader|microsoft basic render|software rasterizer")
        .expect("software name pattern")
});

/// Where a reading came from. Serialized as the Java constant name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Source {
    None,
    NvidiaSmi,
    Engine,
    /// Apple silicon, read through Metal (macOS).
    Metal,
}

/// One graphics device.
///
/// `integrated`: graphics on the processor sharing the machine's memory (an Intel or AMD iGPU),
/// which the engine lists beside a real card with the shared memory as its size; never planned
/// against while a discrete card is present, and named as slow when it is the only device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuDevice {
    pub index: u32,
    pub name: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub driver_version: Option<String>,
    pub compute_capability: Option<String>,
    pub vendor: String,
    pub integrated: bool,
}

impl GpuDevice {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }

    pub fn as_integrated(&self) -> GpuDevice {
        GpuDevice {
            integrated: true,
            ..self.clone()
        }
    }
}

/// Live load of one card, where the driver reports it (nvidia-smi); None where a sensor is
/// unavailable, never a made-up zero.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub index: u32,
    pub usage_percent: Option<f64>,
    pub temperature_c: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub devices: Vec<GpuDevice>,
    pub metrics: Vec<Metrics>,
}

/// Where the Vulkan engine's server executable is, when it is installed.
pub type EngineLocator = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;

struct State {
    last_seen: Vec<GpuDevice>,
    source: Source,
    cached: Snapshot,
    next_refresh: Option<Instant>,
    last_dropped: Vec<String>,
}

/// The GPU inventory. Share it as `Arc<GpuInventory>`: the snapshot refreshes in the background.
pub struct GpuInventory {
    state: Mutex<State>,
    /// Serializes readings (the Java class's `synchronized refresh()`).
    refresh_lock: tokio::sync::Mutex<()>,
    refreshing: AtomicBool,
    /// Turns true once the first reading is in, so an early caller can wait for it instead of
    /// seeing nothing.
    first_reading: watch::Sender<bool>,
    /// The Vulkan engine's server executable when it is installed; none until then.
    vulkan_engine: RwLock<Option<EngineLocator>>,
    nvidia_smi: OsString,
}

impl Default for GpuInventory {
    fn default() -> Self {
        GpuInventory::new()
    }
}

impl GpuInventory {
    pub fn new() -> GpuInventory {
        GpuInventory::with_nvidia_smi("nvidia-smi")
    }

    /// An inventory that runs `program` in place of `nvidia-smi` (tests, QA).
    pub fn with_nvidia_smi(program: impl Into<OsString>) -> GpuInventory {
        GpuInventory {
            state: Mutex::new(State {
                last_seen: Vec::new(),
                source: Source::None,
                cached: Snapshot::default(),
                next_refresh: None,
                last_dropped: Vec::new(),
            }),
            refresh_lock: tokio::sync::Mutex::new(()),
            refreshing: AtomicBool::new(false),
            first_reading: watch::channel(false).0,
            vulkan_engine: RwLock::new(None),
            nvidia_smi: program.into(),
        }
    }

    /// Tells the inventory where the Vulkan engine is, so it can read the card's memory through
    /// it, and starts the first reading in the background so that the status bar's first look,
    /// seconds later, already has it (Fable's note of 2026-09-22: the first status showed no
    /// devices).
    pub fn set_vulkan_engine(self: &Arc<Self>, engine: Option<EngineLocator>) {
        *self.vulkan_engine.write() = engine;
        if self.state.lock().last_seen.is_empty() {
            self.refresh_in_background();
        }
    }

    pub fn source(&self) -> Source {
        self.state.lock().source
    }

    fn vulkan_engine_path(&self) -> Option<PathBuf> {
        let locator = self.vulkan_engine.read().clone();
        locator.and_then(|f| f())
    }

    fn due(&self) -> bool {
        match self.state.lock().next_refresh {
            None => true,
            Some(at) => Instant::now() >= at,
        }
    }

    /// Shared by the footer and settings; placement still requests a fresh reading. A reading
    /// from nvidia-smi refreshes in place (50 ms); one from the engine's device list refreshes in
    /// the background and the caller gets the last one meanwhile.
    pub async fn snapshot(self: &Arc<Self>) -> Snapshot {
        if self.due() {
            let source = self.source();
            if source == Source::Engine
                || (source == Source::None && self.vulkan_engine_path().is_some())
            {
                self.refresh_in_background();
            } else {
                let _guard = self.refresh_lock.lock().await;
                if self.due() {
                    self.refresh_locked().await;
                }
            }
        }
        if *self.first_reading.borrow() {
            return self.state.lock().cached.clone();
        }
        // Nothing read yet and the engine is being asked in the background: give it a moment
        // rather than answer "no GPU".
        let mut first = self.first_reading.subscribe();
        let _ = tokio::time::timeout(FIRST_READING_WAIT, first.wait_for(|done| *done)).await;
        self.state.lock().cached.clone()
    }

    fn refresh_in_background(self: &Arc<Self>) {
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Arc::clone(self);
        let work = async move {
            struct Done(Arc<GpuInventory>);
            impl Drop for Done {
                fn drop(&mut self) {
                    self.0.refreshing.store(false, Ordering::SeqCst);
                }
            }
            let done = Done(me);
            done.0.refresh().await;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(work);
            }
            Err(_) => {
                let spawned = std::thread::Builder::new()
                    .name("nook-gpu-inventory".into())
                    .spawn(move || {
                        match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(rt) => rt.block_on(work),
                            Err(e) => {
                                tracing::warn!("could not read the GPUs in the background: {e}")
                            }
                        }
                    });
                if let Err(e) = spawned {
                    tracing::warn!("could not read the GPUs in the background: {e}");
                    self.refreshing.store(false, Ordering::SeqCst);
                }
            }
        }
    }

    /// Re-queries the driver, or the engine's device list when the driver has no tool of its
    /// own. Cheap with nvidia-smi (about 50 ms), a third of a second through the engine; safe to
    /// call before every placement.
    pub async fn refresh(&self) -> Vec<GpuDevice> {
        let _guard = self.refresh_lock.lock().await;
        self.refresh_locked().await
    }

    async fn refresh_locked(&self) -> Vec<GpuDevice> {
        if cfg!(target_os = "macos") {
            let reading = query_metal();
            let devices = reading.devices.clone();
            let mut state = self.state.lock();
            state.last_seen = devices.clone();
            state.source = if devices.is_empty() {
                Source::None
            } else {
                Source::Metal
            };
            state.cached = reading;
            state.next_refresh = Some(Instant::now() + NVIDIA_SMI_CACHE);
            drop(state);
            self.first_reading.send_replace(true);
            return devices;
        }
        let engine_only = std::env::var(INVENTORY_ENV)
            .map(|v| v.trim().eq_ignore_ascii_case("engine"))
            .unwrap_or(false);
        let mut reading = if engine_only {
            Snapshot::default()
        } else {
            self.query_nvidia().await
        };
        let mut from = if reading.devices.is_empty() {
            Source::None
        } else {
            Source::NvidiaSmi
        };
        if reading.devices.is_empty() {
            if let Some(exe) = self.vulkan_engine_path() {
                reading = GpuInventory::query_engine(&exe).await;
                if !reading.devices.is_empty() {
                    from = Source::Engine;
                }
            }
        }
        let mut state = self.state.lock();
        if from == Source::Engine {
            let counted = countable(&reading.devices, physical_ram_bytes());
            if counted.len() != reading.devices.len() {
                let dropped: Vec<String> = reading
                    .devices
                    .iter()
                    .filter(|d| {
                        !counted
                            .iter()
                            .any(|c| c.index == d.index && c.name == d.name)
                    })
                    .map(|d| format!("{} ({} MiB shared)", d.name, d.total_bytes >> 20))
                    .collect();
                if dropped != state.last_dropped {
                    tracing::info!(
                        "Integrated graphics left out of the GPU budget beside a discrete card: {}",
                        dropped.join(", ")
                    );
                    state.last_dropped = dropped;
                }
            }
            reading = Snapshot {
                devices: counted,
                metrics: reading.metrics,
            };
        }
        let devices = reading.devices.clone();
        state.last_seen = devices.clone();
        state.source = from;
        state.cached = reading;
        state.next_refresh = Some(
            Instant::now()
                + if from == Source::Engine {
                    ENGINE_LIST_CACHE
                } else {
                    NVIDIA_SMI_CACHE
                },
        );
        drop(state);
        self.first_reading.send_replace(true);
        devices
    }

    pub fn last_seen(&self) -> Vec<GpuDevice> {
        self.state.lock().last_seen.clone()
    }

    /// The busiest card's utilisation in percent, from the shared nvidia-smi reading (at most two
    /// seconds old). None where the driver has no tool of its own: the engine's device list gives
    /// memory only, and is not run for this.
    pub async fn utilization(self: &Arc<Self>) -> Option<f64> {
        if self.source() != Source::NvidiaSmi {
            return None;
        }
        busiest(&self.snapshot().await.metrics)
    }

    /// An NVIDIA card with its own driver tool: the CUDA engine is the one to run.
    pub fn has_nvidia(&self) -> bool {
        let state = self.state.lock();
        state.source == Source::NvidiaSmi && state.last_seen.iter().any(|d| d.vendor == "nvidia")
    }

    /// The GPU backend when no NVIDIA card answers: Metal on a Mac (the CPU when Metal has no
    /// device), Vulkan elsewhere.
    pub fn other_gpu_backend(&self) -> Backend {
        if cfg!(target_os = "macos") {
            let state = self.state.lock();
            if state.source == Source::Metal || state.last_seen.is_empty() {
                // Before the first reading every Apple silicon Mac has Metal.
                Backend::Metal
            } else {
                Backend::Cpu
            }
        } else {
            Backend::Vulkan
        }
    }

    /// Chooses the backend: an explicit override in `NOOK_RS_BACKEND` wins, then CUDA when an
    /// NVIDIA GPU answers, otherwise Vulkan (Metal on a Mac).
    pub async fn select_backend(&self) -> Backend {
        if let Ok(value) = std::env::var(BACKEND_ENV) {
            if !value.trim().is_empty() {
                match Backend::from_id(value.trim()) {
                    Ok(b) => return b,
                    Err(_) => tracing::warn!("Ignoring unknown backend override '{value}'"),
                }
            }
        }
        if self.state.lock().last_seen.is_empty() {
            self.refresh().await;
        }
        if self.has_nvidia() {
            Backend::Cuda
        } else {
            self.other_gpu_backend()
        }
    }

    /// Free memory across all devices, each minus the driver reserve, or 0 when there is no GPU.
    /// With several cards the engine splits layers across them, so the budgets add up.
    pub async fn budget_bytes(&self) -> u64 {
        budget_of(&self.refresh().await)
    }

    pub fn is_multi_gpu(&self) -> bool {
        self.state.lock().last_seen.len() > 1
    }

    /// Proportions for llama.cpp's `--tensor-split`, one per device, weighted by free memory.
    /// None with a single device.
    pub fn tensor_split(&self) -> Option<String> {
        let devices = self.last_seen();
        if devices.len() < 2 {
            return None;
        }
        Some(
            devices
                .iter()
                .map(|d| {
                    ((d.free_bytes.saturating_sub(DRIVER_RESERVE_BYTES)) >> 20)
                        .max(1)
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(","),
        )
    }

    async fn query_nvidia(&self) -> Snapshot {
        let mut cmd = crate::process::command(&self.nvidia_smi);
        cmd.args([
            "--query-gpu=index,name,memory.total,memory.free,driver_version,compute_cap,utilization.gpu,temperature.gpu",
            "--format=csv,noheader,nounits",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        // The timeout also covers a stalled driver: dropping the child kills it.
        match tokio::time::timeout(NVIDIA_SMI_TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) if out.status.success() => {
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                text.push('\n');
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                parse_nvidia_smi(&text)
            }
            Ok(Ok(_)) | Err(_) => Snapshot::default(),
            Ok(Err(e)) => {
                tracing::debug!("nvidia-smi not available: {e}");
                Snapshot::default()
            }
        }
    }

    /// Runs `llama-server --list-devices` and reads the devices it prints. No engine, no reading.
    pub async fn query_engine(server_exe: &Path) -> Snapshot {
        if !server_exe.is_file() {
            return Snapshot::default();
        }
        let mut cmd = crate::process::command(server_exe);
        cmd.arg("--list-devices")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = server_exe.parent() {
            cmd.current_dir(dir);
        }
        match tokio::time::timeout(ENGINE_LIST_TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) => {
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                text.push('\n');
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                Snapshot {
                    devices: parse_device_list(&text),
                    metrics: Vec::new(),
                }
            }
            Ok(Err(e)) => {
                tracing::debug!("Engine device list not available: {e}");
                Snapshot::default()
            }
            Err(_) => Snapshot::default(),
        }
    }
}

/// Memory a Mac keeps for the system and the apps beside Nook's engines, on top of the driver
/// reserve: the machine's memory is the GPU's too, and a model that takes all of it pages.
pub const MAC_SYSTEM_RESERVE_BYTES: u64 = 1536 << 20;

/// Apple silicon's GPU through Metal: one device whose total is the working set Metal recommends
/// for one process (about two thirds of the memory, three quarters on the larger machines), and
/// whose free memory is that or what the system has available less [`MAC_SYSTEM_RESERVE_BYTES`],
/// whichever is less. Each engine is a process of its own, so Metal's count of what this process
/// has allocated says nothing about the models already loaded; the system's available memory
/// does, since their weights are the machine's memory taken.
#[cfg(target_os = "macos")]
pub fn query_metal() -> Snapshot {
    use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};

    let Some(device) = MTLCreateSystemDefaultDevice() else {
        return Snapshot::default();
    };
    let name = device.name().to_string();
    let working_set = device.recommendedMaxWorkingSetSize();
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let free = working_set.min(
        sys.available_memory()
            .saturating_sub(MAC_SYSTEM_RESERVE_BYTES),
    );
    Snapshot {
        devices: vec![GpuDevice {
            index: 0,
            vendor: vendor_of(&name).to_string(),
            name,
            total_bytes: working_set,
            free_bytes: free,
            driver_version: None,
            compute_capability: None,
            // Unified memory is what the GPU is built on, not a weak card's shared memory.
            integrated: false,
        }],
        metrics: Vec::new(),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn query_metal() -> Snapshot {
    Snapshot::default()
}

/// Free memory across `devices`, each minus the driver reserve.
pub fn budget_of(devices: &[GpuDevice]) -> u64 {
    devices
        .iter()
        .map(|d| d.free_bytes.saturating_sub(DRIVER_RESERVE_BYTES))
        .sum()
}

/// The busiest card's utilisation; a card with no reading does not count as idle.
pub fn busiest(metrics: &[Metrics]) -> Option<f64> {
    metrics
        .iter()
        .filter_map(|m| m.usage_percent)
        .reduce(f64::max)
}

/// The machine's physical memory, or 0 when it cannot be read.
pub fn physical_ram_bytes() -> u64 {
    static RAM: Lazy<u64> = Lazy::new(|| {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        sys.total_memory()
    });
    *RAM
}

/// Reads `nvidia-smi --query-gpu=index,name,memory.total,memory.free,driver_version,compute_cap,
/// utilization.gpu,temperature.gpu --format=csv,noheader,nounits`: one card per line; header or
/// error lines are skipped.
pub fn parse_nvidia_smi(text: &str) -> Snapshot {
    let mut out = Snapshot::default();
    for line in text.lines() {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 6 {
            continue;
        }
        let (Ok(index), Ok(total), Ok(free)) = (
            f[0].trim().parse::<u32>(),
            f[2].trim().parse::<u64>(),
            f[3].trim().parse::<u64>(),
        ) else {
            continue; // header or error line; skip
        };
        out.devices.push(GpuDevice {
            index,
            name: f[1].trim().to_string(),
            total_bytes: total << 20,
            free_bytes: free << 20,
            driver_version: Some(f[4].trim().to_string()),
            compute_capability: Some(f[5].trim().to_string()),
            vendor: "nvidia".to_string(),
            integrated: false,
        });
        out.metrics.push(Metrics {
            index,
            usage_percent: f.get(6).and_then(|v| optional_number(v)),
            temperature_c: f.get(7).and_then(|v| optional_number(v)),
        });
    }
    out
}

/// The devices worth planning against: the discrete cards when there are any, else the
/// integrated graphics (a real, slow target when it is all there is). A device the name does not
/// give away as integrated is still taken for one when its "memory" is a large share of the
/// machine's RAM (Fable's finding of 2026-09-22: the Vulkan engine listed this machine's Intel
/// graphics with 18 GB of shared memory beside the 8 GB card, and the budget summed them).
pub fn countable(all: &[GpuDevice], physical_ram: u64) -> Vec<GpuDevice> {
    let marked: Vec<GpuDevice> = all
        .iter()
        .map(|d| {
            // a large share of the RAM under a name with no model number ("Some Vendor Graphics"):
            // shared memory; a card with a model number keeps its size, however big
            let shared = physical_ram > 0
                && d.total_bytes as f64 >= physical_ram as f64 * SHARED_MEMORY_SHARE
                && !MODEL_NUMBER.is_match(&d.name);
            if d.integrated || shared {
                d.as_integrated()
            } else {
                d.clone()
            }
        })
        .collect();
    let discrete: Vec<GpuDevice> = marked.iter().filter(|d| !d.integrated).cloned().collect();
    if discrete.is_empty() {
        marked
    } else {
        discrete
    }
}

/// True when the name is one an integrated graphics unit reports.
pub fn integrated_by_name(name: &str) -> bool {
    INTEGRATED_NAME.is_match(name.trim())
}

/// The devices in the engine's `--list-devices` output: one per line as
/// `Vulkan0: <name> (<total> MiB, <free> MiB free)`; CPU and other non-GPU entries are left out,
/// and the vendor is read from the name.
pub fn parse_device_list(text: &str) -> Vec<GpuDevice> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(m) = DEVICE_LINE.captures(line) else {
            continue;
        };
        let kind = m[1].to_lowercase();
        if kind == "cpu" || kind == "blas" || kind == "rpc" {
            continue;
        }
        let name = m[3].trim();
        if SOFTWARE_NAME.is_match(name) {
            continue; // a software rasterizer is the CPU with a Vulkan face
        }
        let (Ok(index), Ok(total), Ok(free)) = (
            m[2].parse::<u32>(),
            m[4].parse::<u64>(),
            m[5].parse::<u64>(),
        ) else {
            continue; // a line that only looked like a device
        };
        out.push(GpuDevice {
            index,
            name: name.to_string(),
            total_bytes: total << 20,
            free_bytes: free << 20,
            driver_version: None,
            compute_capability: None,
            vendor: vendor_of(name).to_string(),
            integrated: integrated_by_name(name),
        });
    }
    out
}

pub fn vendor_of(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("nvidia") || n.contains("geforce") || n.contains("quadro") || n.contains("rtx") {
        return "nvidia";
    }
    if n.contains("amd") || n.contains("radeon") {
        return "amd";
    }
    if n.contains("intel") || n.contains("arc ") || n.starts_with("arc") {
        return "intel";
    }
    if n.contains("apple") {
        return "apple";
    }
    "vulkan"
}

/// A sensor reading, or None when it is unavailable ("[N/A]", NaN, negative), never a zero.
pub fn optional_number(value: &str) -> Option<f64> {
    let n: f64 = value.trim().parse().ok()?;
    (n.is_finite() && n >= 0.0).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_sensors_are_not_reported_as_zero() {
        assert_eq!(optional_number("[N/A]"), None);
        assert_eq!(optional_number("NaN"), None);
        assert_eq!(optional_number("-1"), None);
        assert_eq!(optional_number("0"), Some(0.0));
        assert_eq!(optional_number(" 62 "), Some(62.0));
    }

    #[test]
    fn nvidia_smi_output_is_read_card_by_card() {
        let text = concat!(
            "0, NVIDIA GeForce RTX 4060 Laptop GPU, 8188, 7011, 560.94, 8.9, 12, 45\n",
            "1, NVIDIA GeForce RTX 3090, 24576, 20000, 560.94, 8.6, [N/A], [N/A]\n",
            "Failed to initialize NVML: Unknown Error\n",
            "index, name, memory.total\n",
        );
        let s = parse_nvidia_smi(text);
        assert_eq!(s.devices.len(), 2);
        let d = &s.devices[0];
        assert_eq!(d.name, "NVIDIA GeForce RTX 4060 Laptop GPU");
        assert_eq!(d.total_bytes, 8188 << 20);
        assert_eq!(d.free_bytes, 7011 << 20);
        assert_eq!(d.used_bytes(), (8188 - 7011) << 20);
        assert_eq!(d.driver_version.as_deref(), Some("560.94"));
        assert_eq!(d.compute_capability.as_deref(), Some("8.9"));
        assert_eq!(d.vendor, "nvidia");
        assert_eq!(
            s.metrics[0],
            Metrics {
                index: 0,
                usage_percent: Some(12.0),
                temperature_c: Some(45.0)
            }
        );
        assert_eq!(
            s.metrics[1],
            Metrics {
                index: 1,
                usage_percent: None,
                temperature_c: None
            }
        );
        assert_eq!(
            budget_of(&s.devices),
            ((7011 << 20) - DRIVER_RESERVE_BYTES) + ((20000 << 20) - DRIVER_RESERVE_BYTES)
        );
        assert!(parse_nvidia_smi("").devices.is_empty());
    }

    /// Vulkan cards have no nvidia-smi: the engine's own device list is the reading (2026-09-22).
    #[tokio::test]
    async fn the_engines_device_list_is_read_for_memory() {
        let text = [
            "ggml_vulkan: Found 2 Vulkan devices:",
            "Available devices:",
            "  Vulkan0: AMD Radeon RX 7800 XT (16368 MiB, 15734 MiB free)",
            "  Vulkan1: Intel(R) Arc(TM) A770 Graphics (16256 MiB, 15900 MiB free)",
            "  CPU: AMD Ryzen 9 7950X (65000 MiB, 40000 MiB free)",
            "  CUDA0: NVIDIA GeForce RTX 4060 (8187 MiB, 5785 MiB free)",
            "  Vulkan2: something odd (n/a MiB, ? MiB free)",
            "",
        ]
        .join("\n");
        let devices = parse_device_list(&text);
        assert_eq!(devices.len(), 3, "{devices:?}");
        assert_eq!(devices[0].name, "AMD Radeon RX 7800 XT");
        assert_eq!(devices[0].total_bytes, 16368 << 20);
        assert_eq!(devices[0].free_bytes, 15734 << 20);
        assert_eq!(devices[0].vendor, "amd");
        assert_eq!(devices[0].index, 0);
        assert_eq!(devices[1].vendor, "intel");
        assert_eq!(devices[1].index, 1);
        assert_eq!(devices[2].vendor, "nvidia");
        assert_eq!(devices[0].driver_version, None, "the engine does not say");
        assert!(parse_device_list("").is_empty());
        assert_eq!(vendor_of("llvmpipe (LLVM 17.0.6, 256 bits)"), "vulkan");
        assert_eq!(vendor_of("Apple M3 Max"), "apple");

        // Fable's finding: the engine lists the processor's graphics with the machine's RAM as its memory
        let here = [
            "Available devices:",
            "  Vulkan0: NVIDIA GeForce RTX 4060 (7956 MiB, 7188 MiB free)",
            "  Vulkan1: Intel(R) Graphics (18310 MiB, 17542 MiB free)",
            "  Vulkan2: llvmpipe (LLVM 17.0.6, 256 bits) (32768 MiB, 32768 MiB free)",
            "",
        ]
        .join("\n");
        let listed = parse_device_list(&here);
        assert_eq!(
            listed.len(),
            2,
            "the software rasterizer is not a device: {listed:?}"
        );
        assert!(!listed[0].integrated);
        assert!(listed[1].integrated, "Intel(R) Graphics is the processor's");
        let counted = countable(&listed, 32 << 30);
        assert_eq!(counted.len(), 1);
        assert_eq!(counted[0].name, "NVIDIA GeForce RTX 4060");

        // a name that does not give it away is still taken for shared memory by its size against the RAM
        let unnamed = parse_device_list(
            "  Vulkan0: AMD Radeon RX 7800 XT (16368 MiB, 15734 MiB free)\n  Vulkan1: Some Vendor Graphics (16000 MiB, 15000 MiB free)\n",
        );
        assert_eq!(
            countable(&unnamed, 32 << 30).len(),
            1,
            "16 GB of a 32 GB machine is shared memory"
        );
        assert_eq!(
            countable(&unnamed, 128 << 30).len(),
            2,
            "on a 128 GB machine 16 GB is a card"
        );

        // the integrated graphics alone is a real, slow target and stays
        let only = parse_device_list("  Vulkan0: AMD Radeon(TM) 780M (8192 MiB, 7000 MiB free)\n");
        assert_eq!(countable(&only, 16 << 30).len(), 1);
        assert!(countable(&only, 16 << 30)[0].integrated);

        assert!(integrated_by_name("Intel(R) Iris(R) Xe Graphics"));
        assert!(integrated_by_name("Intel(R) UHD Graphics 770"));
        assert!(integrated_by_name("Intel(R) Arc(TM) Graphics"));
        assert!(
            !integrated_by_name("Intel(R) Arc(TM) A770 Graphics"),
            "a discrete Arc has a model number"
        );
        assert!(integrated_by_name("AMD Radeon(TM) Graphics"));
        assert!(integrated_by_name("AMD Radeon 890M"));
        assert!(!integrated_by_name("AMD Radeon RX 7800 XT"));
        assert!(!integrated_by_name("NVIDIA GeForce RTX 4060"));

        // the inventory takes the engine's reading when nvidia-smi has none, and remembers where it came from
        let inv = GpuInventory::new();
        assert_eq!(inv.source(), Source::None);
        let nowhere = Path::new("Z:").join("nowhere").join("llama-server.exe");
        assert!(
            GpuInventory::query_engine(&nowhere)
                .await
                .devices
                .is_empty(),
            "no engine: no reading, no error"
        );
    }

    /// The mark in the title strip spins at the busiest card's share; a card with no reading
    /// does not count as idle.
    #[test]
    fn the_busiest_card_sets_the_marks_speed() {
        let metrics = [
            Metrics {
                index: 0,
                usage_percent: Some(12.0),
                temperature_c: Some(50.0),
            },
            Metrics {
                index: 1,
                usage_percent: None,
                temperature_c: None,
            },
            Metrics {
                index: 2,
                usage_percent: Some(87.0),
                temperature_c: Some(60.0),
            },
        ];
        assert_eq!(busiest(&metrics), Some(87.0));
        assert_eq!(
            busiest(&[Metrics {
                index: 0,
                usage_percent: None,
                temperature_c: None
            }]),
            None
        );
    }

    #[test]
    fn tensor_split_weights_by_free_memory() {
        let inv = GpuInventory::new();
        assert_eq!(inv.tensor_split(), None);
        inv.state.lock().last_seen = parse_nvidia_smi(
            "0, A, 8192, 4608, 1, 8.9\n1, B, 8192, 2560, 1, 8.9\n2, C, 8192, 100, 1, 8.9\n",
        )
        .devices;
        assert!(inv.is_multi_gpu());
        assert_eq!(inv.tensor_split().as_deref(), Some("4096,2048,1"));
    }

    /// A fake tool: a batch file that prints `lines` (Windows only; the port is Windows-first).
    #[cfg(windows)]
    fn fake_tool(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(name);
        let mut script = String::from("@echo off\r\n");
        for l in lines {
            script.push_str("echo ");
            script.push_str(l);
            script.push_str("\r\n");
        }
        std::fs::write(&path, script).unwrap();
        path
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn nvidia_smi_answers_first_and_picks_cuda() {
        let dir = tempfile::tempdir().unwrap();
        let smi = fake_tool(
            dir.path(),
            "nvidia-smi.cmd",
            &["0, NVIDIA GeForce RTX 4060, 8188, 7188, 560.94, 8.9, 40, 50"],
        );
        let inv = Arc::new(GpuInventory::with_nvidia_smi(smi));
        let snapshot = inv.snapshot().await;
        assert_eq!(snapshot.devices.len(), 1);
        assert_eq!(inv.source(), Source::NvidiaSmi);
        assert!(inv.has_nvidia());
        assert_eq!(inv.utilization().await, Some(40.0));
        assert_eq!(
            inv.budget_bytes().await,
            (7188 << 20) - DRIVER_RESERVE_BYTES
        );
        if std::env::var(BACKEND_ENV).is_err() {
            assert_eq!(inv.select_backend().await, Backend::Cuda);
        }
        assert_eq!(inv.tensor_split(), None);
    }

    /// The first status after start: the engine path reads in the background, and the first
    /// snapshot waits for that reading instead of answering "no GPU" (Fable's note of 2026-09-22).
    #[cfg(windows)]
    #[tokio::test]
    async fn without_nvidia_smi_the_engine_is_asked_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        let engine = fake_tool(
            dir.path(),
            "llama-server.cmd",
            &[
                "ggml_vulkan: Found 2 Vulkan devices:",
                "Available devices:",
                "  Vulkan0: AMD Radeon RX 7800 XT (16368 MiB, 15734 MiB free)",
                "  Vulkan1: Intel(R) Graphics (18310 MiB, 17542 MiB free)",
            ],
        );
        let inv = Arc::new(GpuInventory::with_nvidia_smi(
            dir.path().join("no-such-nvidia-smi.exe"),
        ));
        let exe = engine.clone();
        let t0 = Instant::now();
        inv.set_vulkan_engine(Some(Arc::new(move || Some(exe.clone()))));
        let first = inv.snapshot().await;
        assert_eq!(
            first.devices.len(),
            1,
            "the first snapshot carries the engine's reading ({:?})",
            t0.elapsed()
        );
        assert_eq!(
            first.devices[0].name, "AMD Radeon RX 7800 XT",
            "the integrated graphics is left out"
        );
        assert!(
            first.metrics.is_empty(),
            "no usage or temperature from the engine"
        );
        assert_eq!(inv.source(), Source::Engine);
        assert!(!inv.has_nvidia());
        assert_eq!(
            inv.utilization().await,
            None,
            "the engine's list gives memory only"
        );
        if std::env::var(BACKEND_ENV).is_err() {
            assert_eq!(inv.select_backend().await, Backend::Vulkan);
        }
        assert_eq!(
            inv.budget_bytes().await,
            (15734 << 20) - DRIVER_RESERVE_BYTES
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn no_gpu_at_all_is_an_empty_reading() {
        let dir = tempfile::tempdir().unwrap();
        let inv = Arc::new(GpuInventory::with_nvidia_smi(
            dir.path().join("no-such-nvidia-smi.exe"),
        ));
        assert!(inv.snapshot().await.devices.is_empty());
        assert_eq!(inv.budget_bytes().await, 0);
        assert_eq!(inv.source(), Source::None);
    }
}

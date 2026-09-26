//! Ports `runtime/HardwareMonitor.java`: one non-blocking CPU sample and one shared GPU query per
//! display refresh. (The 0.4.2 UI does not show it yet; it is ported for the status bar.)

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;

use super::gpu_inventory::{GpuInventory, Snapshot};

/// One reading for the status bar. `cpu_percent` is None on the first reading (usage is measured
/// between two), `cpu_temperature_c` None where the board exposes no sensor.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    pub cpu_percent: Option<f64>,
    pub cpu_temperature_c: Option<f64>,
    pub used_memory: u64,
    pub total_memory: u64,
    pub gpu: Snapshot,
}

/// Where the CPU and memory numbers come from (sysinfo in the app, a fake in tests).
pub trait HardwareSource: Send {
    /// CPU usage in percent since the previous call; None on the first call.
    fn cpu_percent(&mut self) -> Option<f64>;
    /// The CPU temperature in Celsius as the sensor reports it (0, NaN or None when there is none).
    fn cpu_temperature(&mut self) -> Option<f64>;
    /// (total, available) physical memory in bytes.
    fn memory(&mut self) -> (u64, u64);
}

/// A monotonic clock in nanoseconds.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

struct Inner {
    hardware: Box<dyn HardwareSource>,
    retry_temperature_at: u64,
    last_temperature: Option<f64>,
}

pub struct HardwareMonitor {
    inventory: Arc<GpuInventory>,
    clock: Clock,
    inner: Mutex<Inner>,
}

impl HardwareMonitor {
    pub fn new(inventory: Arc<GpuInventory>) -> HardwareMonitor {
        let start = Instant::now();
        HardwareMonitor::with_source(
            inventory,
            Box::new(SysinfoHardware::default()),
            Arc::new(move || start.elapsed().as_nanos() as u64),
        )
    }

    pub fn with_source(
        inventory: Arc<GpuInventory>,
        hardware: Box<dyn HardwareSource>,
        clock: Clock,
    ) -> HardwareMonitor {
        HardwareMonitor {
            inventory,
            clock,
            inner: Mutex::new(Inner {
                hardware,
                retry_temperature_at: 0,
                last_temperature: None,
            }),
        }
    }

    pub async fn read(&self) -> Reading {
        let (usage, temperature, used, total) = {
            let mut inner = self.inner.lock();
            let usage = inner.hardware.cpu_percent();
            let now = (self.clock)();
            if now >= inner.retry_temperature_at {
                // Many Windows boards do not expose this sensor: ask again in a minute, not every
                // refresh.
                inner.last_temperature = inner
                    .hardware
                    .cpu_temperature()
                    .filter(|c| c.is_finite() && *c > 0.0);
                let wait = if inner.last_temperature.is_none() {
                    60
                } else {
                    2
                };
                inner.retry_temperature_at =
                    (self.clock)() + Duration::from_secs(wait).as_nanos() as u64;
            }
            let (total, available) = inner.hardware.memory();
            (
                usage,
                inner.last_temperature,
                total.saturating_sub(available),
                total,
            )
        };
        let gpu = Arc::clone(&self.inventory).snapshot().await;
        Reading {
            cpu_percent: usage,
            cpu_temperature_c: temperature,
            used_memory: used,
            total_memory: total,
            gpu,
        }
    }
}

/// The machine's numbers through sysinfo.
#[derive(Default)]
pub struct SysinfoHardware {
    system: sysinfo::System,
    components: Option<sysinfo::Components>,
    primed: bool,
}

impl HardwareSource for SysinfoHardware {
    fn cpu_percent(&mut self) -> Option<f64> {
        self.system.refresh_cpu_usage();
        let first = !self.primed;
        self.primed = true;
        (!first).then(|| self.system.global_cpu_usage() as f64)
    }

    fn cpu_temperature(&mut self) -> Option<f64> {
        let components = self
            .components
            .get_or_insert_with(sysinfo::Components::new_with_refreshed_list);
        components.refresh(false);
        let list = components.list();
        // Windows reports the ACPI thermal zone ("Computer"); elsewhere prefer the package sensor.
        let preferred = list.iter().find(|c| {
            let l = c.label().to_lowercase();
            l.contains("cpu") || l.contains("package") || l.contains("tctl")
        });
        preferred
            .or_else(|| list.first())
            .and_then(|c| c.temperature())
            .map(|t| t as f64)
    }

    fn memory(&mut self) -> (u64, u64) {
        self.system.refresh_memory();
        (self.system.total_memory(), self.system.available_memory())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    struct Fake {
        temperatures: Vec<f64>,
        calls: Arc<AtomicUsize>,
    }

    impl HardwareSource for Fake {
        fn cpu_percent(&mut self) -> Option<f64> {
            None
        }
        fn cpu_temperature(&mut self) -> Option<f64> {
            let i = self.calls.fetch_add(1, Ordering::SeqCst);
            self.temperatures.get(i).copied()
        }
        fn memory(&mut self) -> (u64, u64) {
            (1000, 400)
        }
    }

    #[tokio::test]
    async fn unavailable_cpu_sensor_backs_off_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        // No GPU tool at all: the GPU part is an empty reading.
        let inventory = Arc::new(GpuInventory::with_nvidia_smi(
            dir.path().join("no-such-nvidia-smi.exe"),
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let clock = Arc::new(AtomicU64::new(0));
        let c = clock.clone();
        let monitor = HardwareMonitor::with_source(
            inventory,
            Box::new(Fake {
                temperatures: vec![0.0, 61.0],
                calls: calls.clone(),
            }),
            Arc::new(move || c.load(Ordering::SeqCst)),
        );
        assert_eq!(monitor.read().await.cpu_temperature_c, None);
        clock.store(2_000_000_000, Ordering::SeqCst);
        assert_eq!(monitor.read().await.cpu_temperature_c, None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        clock.store(61_000_000_000, Ordering::SeqCst);
        let reading = monitor.read().await;
        assert_eq!(reading.cpu_temperature_c, Some(61.0));
        assert_eq!(reading.used_memory, 600);
        assert!(reading.gpu.devices.is_empty());
    }
}

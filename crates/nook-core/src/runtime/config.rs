//! Ports `runtime/RuntimeConfig.java`, the Spring wiring of the runtime: one of each service,
//! shared. The home comes from [`Home`] (the original's `nook.home` system property).

use std::sync::Arc;

use anyhow::Result;

use super::downloader::Downloader;
use super::engine_packages::EnginePackages;
use super::gpu_inventory::GpuInventory;
use super::hugging_face_hub::HuggingFaceHub;
use super::model_catalog::ModelCatalog;
use super::model_registry::ModelRegistry;
use crate::home::Home;

/// The runtime's base services, built once and shared. The runtime manager (engines, placement,
/// admission) is built on top of these.
#[derive(Clone)]
pub struct RuntimeConfig {
    pub home: Home,
    /// One download client for engines, catalog models and Hub models.
    pub downloader: Arc<Downloader>,
    pub catalog: Arc<ModelCatalog>,
    pub inventory: Arc<GpuInventory>,
    pub packages: Arc<EnginePackages>,
    pub registry: Arc<ModelRegistry>,
    pub hub: Arc<HuggingFaceHub>,
}

impl RuntimeConfig {
    /// Builds the services for a home: reads the bundled catalog and engine manifest; runs no
    /// process and touches no network. The registry and the Hub also read the installed Nook's
    /// models folder when there is one ([`Home::shared_models_dir`]).
    ///
    /// The GPU inventory is not yet pointed at the Vulkan engine: the runtime manager does that
    /// (`inventory.set_vulkan_engine(...)`) when it starts, which also starts the first reading.
    pub fn new(home: Home) -> Result<RuntimeConfig> {
        let downloader = Arc::new(Downloader::new());
        let catalog = Arc::new(ModelCatalog::bundled()?);
        let packages = Arc::new(EnginePackages::new(home.clone(), downloader.clone())?);
        let registry = Arc::new(ModelRegistry::new(
            home.clone(),
            catalog.clone(),
            downloader.clone(),
        ));
        let hub = Arc::new(HuggingFaceHub::new(home.clone(), downloader.clone()));
        Ok(RuntimeConfig {
            home,
            downloader,
            catalog,
            inventory: Arc::new(GpuInventory::new()),
            packages,
            registry,
            hub,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wires_one_of_each() {
        let dir = tempfile::tempdir().unwrap();
        let config = RuntimeConfig::new(Home::at(dir.path())).unwrap();
        assert!(Arc::ptr_eq(config.registry.catalog(), &config.catalog));
        assert_eq!(config.registry.models_dir(), dir.path().join("models"));
        assert_ne!(config.packages.version(), "?");
    }
}

//! Ports `runtime/Backend.java`.

use serde::{Deserialize, Serialize};

/// Engine build the runtime runs on. Each backend is a separate directory of binaries.
///
/// Serialized as the Java constant name (`"CUDA"`, `"VULKAN"`, `"CPU"`), as Jackson wrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Backend {
    Cuda,
    Vulkan,
    Cpu,
}

impl Backend {
    pub const ALL: [Backend; 3] = [Backend::Cuda, Backend::Vulkan, Backend::Cpu];

    /// The directory and manifest name: `cuda`, `vulkan`, `cpu`.
    pub fn id(self) -> &'static str {
        match self {
            Backend::Cuda => "cuda",
            Backend::Vulkan => "vulkan",
            Backend::Cpu => "cpu",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Backend::Cuda => "NVIDIA CUDA 12",
            Backend::Vulkan => "Vulkan",
            Backend::Cpu => "CPU",
        }
    }

    /// The backend with this id, ignoring case.
    pub fn from_id(id: &str) -> anyhow::Result<Backend> {
        Backend::ALL
            .into_iter()
            .find(|b| b.id().eq_ignore_ascii_case(id))
            .ok_or_else(|| anyhow::anyhow!("Unknown backend: {id}"))
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_ignoring_case() {
        for b in Backend::ALL {
            assert_eq!(Backend::from_id(&b.id().to_uppercase()).unwrap(), b);
        }
        assert_eq!(
            Backend::from_id("nope").unwrap_err().to_string(),
            "Unknown backend: nope"
        );
        assert_eq!(serde_json::to_string(&Backend::Cuda).unwrap(), "\"CUDA\"");
    }
}

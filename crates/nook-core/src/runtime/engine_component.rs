//! Ports `runtime/EngineComponent.java`.

use serde::{Deserialize, Serialize};

use super::backend::Backend;

/// The engine binaries the runtime can install. Each is a separate upstream project on the same
/// ggml backend, installed into its own directory per backend.
///
/// Serialized as the Java constant name (`"LLAMA"`, `"WHISPER"`, `"SD"`, `"FFMPEG"`, `"AUDIO"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EngineComponent {
    /// llama.cpp: text, vision and embedding models (llama-server).
    Llama,
    /// whisper.cpp: speech to text (whisper-server).
    Whisper,
    /// stable-diffusion.cpp: text to image and text to video (sd CLI).
    Sd,
    /// FFmpeg: reads what Nook does not and puts a dubbed video back together for the flows
    /// (ffmpeg CLI). One build serves every backend.
    Ffmpeg,
    /// audio.cpp: the flows' voices, text to speech with voice cloning (audiocpp_cli).
    Audio,
    /// PDFium: the PDF editor's reading, drawing and writing (pdfium.dll, bound at run time). One
    /// build serves every backend.
    Pdfium,
}

impl EngineComponent {
    pub const ALL: [EngineComponent; 6] = [
        EngineComponent::Llama,
        EngineComponent::Whisper,
        EngineComponent::Sd,
        EngineComponent::Ffmpeg,
        EngineComponent::Audio,
        EngineComponent::Pdfium,
    ];

    /// The manifest key and sub-directory name: `llama`, `whisper`, `sd`, `ffmpeg`, `audio`,
    /// `pdfium`.
    pub fn id(self) -> &'static str {
        match self {
            EngineComponent::Llama => "llama",
            EngineComponent::Whisper => "whisper",
            EngineComponent::Sd => "sd",
            EngineComponent::Ffmpeg => "ffmpeg",
            EngineComponent::Audio => "audio",
            EngineComponent::Pdfium => "pdfium",
        }
    }

    /// The build the component runs on the selected backend: its own, but FFmpeg and PDFium have
    /// one build for all, filed under the processor's. (Until 0.5.6 audio.cpp ran its Vulkan
    /// build on NVIDIA cards too; `EnginePackages::ensure_installed` removes that copy.)
    pub fn runs_on(self, backend: Backend) -> Backend {
        match (self, backend) {
            (EngineComponent::Ffmpeg | EngineComponent::Pdfium, _) => Backend::Cpu,
            (_, b) => b,
        }
    }

    /// The component with this id, ignoring case (the manifest's keys).
    pub fn from_id(id: &str) -> Option<EngineComponent> {
        EngineComponent::ALL
            .into_iter()
            .find(|c| c.id().eq_ignore_ascii_case(id))
    }

    /// The component that serves a model with the given catalog task.
    pub fn for_task(task: &str) -> EngineComponent {
        match task {
            "speech" => EngineComponent::Whisper,
            "image" | "video" => EngineComponent::Sd,
            _ => EngineComponent::Llama,
        }
    }
}

impl std::fmt::Display for EngineComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_map_to_components() {
        assert_eq!(
            EngineComponent::for_task("speech"),
            EngineComponent::Whisper
        );
        assert_eq!(EngineComponent::for_task("image"), EngineComponent::Sd);
        assert_eq!(EngineComponent::for_task("video"), EngineComponent::Sd);
        assert_eq!(EngineComponent::for_task("chat"), EngineComponent::Llama);
        assert_eq!(EngineComponent::for_task(""), EngineComponent::Llama);
        assert_eq!(EngineComponent::from_id("SD"), Some(EngineComponent::Sd));
        assert_eq!(
            EngineComponent::from_id("audio"),
            Some(EngineComponent::Audio)
        );
    }

    #[test]
    fn each_engine_runs_its_own_build_but_ffmpeg_and_pdfium_one_for_all() {
        assert_eq!(EngineComponent::Audio.runs_on(Backend::Cuda), Backend::Cuda);
        assert_eq!(
            EngineComponent::Audio.runs_on(Backend::Vulkan),
            Backend::Vulkan
        );
        assert_eq!(EngineComponent::Audio.runs_on(Backend::Cpu), Backend::Cpu);
        assert_eq!(
            EngineComponent::Pdfium.runs_on(Backend::Vulkan),
            Backend::Cpu
        );
        assert_eq!(EngineComponent::Ffmpeg.runs_on(Backend::Cuda), Backend::Cpu);
        assert_eq!(
            EngineComponent::Whisper.runs_on(Backend::Cuda),
            Backend::Cuda
        );
    }
}

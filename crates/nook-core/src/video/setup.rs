//! What the Video page needs before a clip can be asked for: the port of `VideoScreen.kt`'s
//! `readSetup` (with `VideoStudio.problem()` and `folder()`) and of its `DownloadState`, in the
//! shapes of `ui/src/api/video.ts` (`VideoSetup`, `VideoModel`, `DownloadState`).

use serde::{Deserialize, Serialize};

use super::studio::VideoStudio;
use crate::runtime::downloads::DownloadState as LibraryState;
use crate::runtime::engine_component::EngineComponent;
use crate::runtime::manager::RuntimeManager;

/// A clip's size and length when the catalog does not say (the Wan 2.1 defaults).
pub const DEFAULT_WIDTH: u32 = 832;
pub const DEFAULT_HEIGHT: u32 = 480;
pub const DEFAULT_FRAMES: u32 = 33;
pub const DEFAULT_FPS: u32 = 16;

/// An installed video model a clip can be made with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoModel {
    pub id: String,
    pub name: String,
}

/// What the page needs to know before a clip can be asked for (`VideoScreen.kt` `VideoSetup`):
/// whether the model and engine are in, which model makes the clips, and what they come out as.
///
/// - `problem`: why no clip can be made yet (`RuntimeManager.videoProblem`), or None
/// - `model_id`: the catalog id of the model: the installed one asked for or preferred, else the
///   catalog default
/// - `model_name`: its display name, or "No video model"
/// - `download_bytes`: what the setup card offers to download: the model when missing, plus the
///   sd engine when missing
/// - `width`, `height`, `frames`, `fps`: the model's clip defaults (catalog `defaults`, else 832,
///   480, 33 and 16); the Kotlin page showed them as one `clipText` line, which the web page
///   builds itself
/// - `models`: every installed video model, for the choice in the header
/// - `folder`: where finished clips are kept (`VideoStudio.folder()`)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSetup {
    pub problem: Option<String>,
    pub model_id: Option<String>,
    pub model_name: String,
    pub download_bytes: u64,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub fps: u32,
    pub models: Vec<VideoModel>,
    pub folder: String,
}

impl VideoSetup {
    /// Reads the setup for the model a clip would use (`RuntimeManager::video_model(model_id)`;
    /// None is the preferred one). Reads the models folder, so it is file work; the answer
    /// changes when a download finishes (the page reads it again on `downloads` events).
    pub async fn read(
        runtime: &RuntimeManager,
        studio: &VideoStudio,
        model_id: Option<&str>,
    ) -> VideoSetup {
        let backend = runtime.backend().await;
        let catalog = runtime.catalog();
        let installed = runtime.video_model(model_id);
        let cat = installed
            .as_ref()
            .and_then(|m| catalog.find(&m.id))
            .or_else(|| {
                catalog
                    .default_video_model()
                    .and_then(|id| catalog.find(id))
            });
        let engine_bytes = if runtime.is_component_installed(EngineComponent::Sd) {
            0
        } else {
            runtime
                .packages()
                .package_for(EngineComponent::Sd, backend)
                .map(|p| p.total_bytes())
                .unwrap_or(0)
        };
        let model_bytes = if installed.is_some() {
            0
        } else {
            cat.map(|c| c.total_bytes()).unwrap_or(0)
        };
        let default = |key: &str, fallback: u32| {
            cat.and_then(|c| c.defaults.get(key))
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(fallback)
        };
        let models = runtime
            .registry()
            .list()
            .into_iter()
            .filter(|m| m.is_video())
            .map(|m| VideoModel {
                id: m.id,
                name: m.display_name,
            })
            .collect();
        VideoSetup {
            problem: studio.problem(),
            model_id: cat.map(|c| c.id.clone()),
            model_name: installed
                .as_ref()
                .map(|m| m.display_name.clone())
                .or_else(|| cat.map(|c| c.display_name.clone()))
                .unwrap_or_else(|| "No video model".to_string()),
            download_bytes: engine_bytes + model_bytes,
            width: default("width", DEFAULT_WIDTH),
            height: default("height", DEFAULT_HEIGHT),
            frames: default("frames", DEFAULT_FRAMES),
            fps: default("fps", DEFAULT_FPS),
            models,
            folder: studio.folder().display().to_string(),
        }
    }
}

/// Where the video model's download is, for the setup card (`VideoScreen.kt` `DownloadState`,
/// read from the download service's state).
///
/// - `offered`: the catalog lists the model, so there is something to download
/// - `progress`: 0..1, or None before the first progress report
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadState {
    pub offered: bool,
    pub downloading: bool,
    pub paused: bool,
    pub progress: Option<f64>,
}

impl DownloadState {
    /// The state of one catalog model's download in the library's
    /// ([`ModelDownloadService::state`](crate::runtime::ModelDownloadService::state)).
    pub fn of(library: &LibraryState, model_id: &str) -> DownloadState {
        DownloadState {
            offered: library.available_models.iter().any(|m| m.model == model_id),
            downloading: library.downloading_models.iter().any(|m| m == model_id),
            paused: library.paused_models.iter().any(|m| m == model_id),
            progress: library.downloading_progress.get(model_id).copied(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::downloads::AiModelDto;
    use crate::runtime::manager::testing::*;
    use std::collections::BTreeMap;

    const CATALOG: &str = r#"{"models":[
        {"id":"wan2.1-t2v-1.3b","displayName":"Wan 2.1 T2V 1.3B","family":"wan","task":"video",
         "artifacts":[{"file":"Wan2.1-T2V-1.3B-Q8_0.gguf","url":"http://127.0.0.1:9/w","sha256":"","bytes":1000},
                      {"file":"umt5.gguf","url":"http://127.0.0.1:9/t","sha256":"","bytes":500}],
         "defaults":{"width":"832","height":"480","frames":"33","fps":"16"}},
        {"id":"wan2.2-ti2v-5b","displayName":"Wan 2.2 TI2V 5B","family":"wan","task":"video",
         "artifacts":[{"file":"Wan2.2-TI2V-5B-Q8_0.gguf","url":"http://127.0.0.1:9/w2","sha256":"","bytes":4000}],
         "defaults":{"width":"1280","height":"704","frames":"121","fps":"24"}}
    ],"defaultVideoModel":"wan2.1-t2v-1.3b"}"#;

    const MANIFEST: &str = r#"{"components":{"sd":{"version":"master-1","backends":{"cuda":[
        {"name":"sd.zip","url":"http://127.0.0.1:9/sd.zip","bytes":300},
        {"name":"cudart.zip","url":"http://127.0.0.1:9/cudart.zip","bytes":200}]}}}}"#;

    #[tokio::test]
    async fn the_setup_says_what_is_missing_and_what_the_clips_come_out_as() {
        let rig = rig(RigSpec {
            catalog: Some(CATALOG.into()),
            manifest: Some(MANIFEST.into()),
            ..RigSpec::default()
        });
        let m = rig.manager.clone();
        let studio = VideoStudio::for_runtime(m.clone());

        let empty = VideoSetup::read(&m, &studio, None).await;
        assert_eq!(
            empty.problem.as_deref(),
            Some("No video model is downloaded yet.")
        );
        assert_eq!(
            empty.model_id.as_deref(),
            Some("wan2.1-t2v-1.3b"),
            "the catalog default"
        );
        assert_eq!(empty.model_name, "Wan 2.1 T2V 1.3B");
        assert_eq!(empty.download_bytes, 1500 + 500, "the model and the engine");
        assert_eq!(
            (empty.width, empty.height, empty.frames, empty.fps),
            (832, 480, 33, 16)
        );
        assert!(empty.models.is_empty());
        assert_eq!(empty.folder, rig.home.videos_dir().display().to_string());

        sidecar_model(
            &rig,
            "wan",
            "Wan2.2-TI2V-5B-Q8_0.gguf",
            "wan2.2-ti2v-5b",
            "video",
        );
        let model_in = VideoSetup::read(&m, &studio, None).await;
        assert_eq!(
            model_in.problem.as_deref(),
            Some("The video engine is not installed yet. Download the video model to install it.")
        );
        assert_eq!(
            model_in.model_id.as_deref(),
            Some("wan2.2-ti2v-5b"),
            "the installed one"
        );
        assert_eq!(model_in.download_bytes, 500, "only the engine");
        assert_eq!(
            (
                model_in.width,
                model_in.height,
                model_in.frames,
                model_in.fps
            ),
            (1280, 704, 121, 24)
        );

        install(&rig, EngineComponent::Sd);
        sidecar_model(
            &rig,
            "wan",
            "Wan2.1-T2V-1.3B-Q8_0.gguf",
            "wan2.1-t2v-1.3b",
            "video",
        );
        let ready = VideoSetup::read(&m, &studio, Some("wan2.1-t2v-1.3b")).await;
        assert_eq!(ready.problem, None);
        assert_eq!(
            ready.model_id.as_deref(),
            Some("wan2.1-t2v-1.3b"),
            "the one asked for"
        );
        assert_eq!(ready.download_bytes, 0);
        assert_eq!(ready.models.len(), 2);
        let json = serde_json::to_value(&ready).unwrap();
        for key in [
            "problem",
            "modelId",
            "modelName",
            "downloadBytes",
            "width",
            "models",
            "folder",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert!(json["models"][0].get("name").is_some());
    }

    #[test]
    fn a_models_download_state_comes_from_the_library() {
        let mut library = LibraryState {
            available_models: vec![AiModelDto {
                model: "wan2.1-t2v-1.3b".into(),
                ..AiModelDto::default()
            }],
            ..LibraryState::default()
        };
        assert_eq!(
            DownloadState::of(&library, "wan2.1-t2v-1.3b"),
            DownloadState {
                offered: true,
                downloading: false,
                paused: false,
                progress: None
            }
        );
        library.downloading_models.push("wan2.1-t2v-1.3b".into());
        library.downloading_progress = BTreeMap::from([("wan2.1-t2v-1.3b".to_string(), 0.25)]);
        let d = DownloadState::of(&library, "wan2.1-t2v-1.3b");
        assert!(d.downloading && !d.paused);
        assert_eq!(d.progress, Some(0.25));
        assert!(!DownloadState::of(&library, "other").offered);
    }
}

//! Files built into the binary, from the repository's `resources` folder (the Kotlin app's
//! classpath resources).

/// The curated model catalog (`runtime/catalog.json`).
pub const CATALOG_JSON: &str = include_str!("../../../resources/runtime/catalog.json");
/// The voices the flows speak with (`runtime/voices.json`).
pub const VOICES_JSON: &str = include_str!("../../../resources/runtime/voices.json");
/// The pinned engine releases (`runtime/engines.json`; `runtime/engines-macos.json` on a Mac,
/// Apple silicon's builds under the backends `metal` and `cpu`).
#[cfg(not(target_os = "macos"))]
pub const ENGINES_JSON: &str = include_str!("../../../resources/runtime/engines.json");
#[cfg(target_os = "macos")]
pub const ENGINES_JSON: &str = include_str!("../../../resources/runtime/engines-macos.json");
/// Prompt templates (`prompts.json`).
pub const PROMPTS_JSON: &str = include_str!("../../../resources/prompts.json");
/// Public keys trusted to sign update manifests, one base64 key per line (`release-keys.txt`).
pub const RELEASE_KEYS: &str = include_str!("../../../resources/release-keys.txt");
/// The licence text shown in Settings › About.
pub const EULA_HTML: &str = include_str!("../../../resources/eula.html");
pub const THIRD_PARTY_NOTICES_HTML: &str =
    include_str!("../../../resources/third-party-notices.html");

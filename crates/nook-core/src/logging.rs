//! Logging to `<home>\logs\nook.log` (and stderr in debug builds), like the Kotlin app's logback
//! file. `RUST_LOG` overrides the filter; the default is info, debug for Nook's own crates.

use std::fs::OpenOptions;
use std::sync::Mutex;

use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use crate::Home;

/// Starts logging once. Later calls are ignored (tests start it more than once).
pub fn init(home: &Home) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,nook_core=debug,nook=debug,tao=warn,wry=warn"));
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.log_file());
    let file_layer = file.ok().map(|f| {
        fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(Mutex::new(f))
    });
    let stderr_layer = cfg!(debug_assertions).then(|| fmt::layer().with_writer(std::io::stderr));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init();
}

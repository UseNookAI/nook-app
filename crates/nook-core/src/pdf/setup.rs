//! The PDF editor's one download: the PDF engine (PDFium, the `pdfium` engine component), shown
//! on the page as the translator shows its own: what it is and its size, one button, a progress
//! bar and Stop, or what went wrong and Try again.
//!
//! [`PdfInstaller::start`] runs the download in the background; every change goes out on
//! [`topic::PDF`] as `{"install": Install | null}` (null once it is done or forgotten), the shape
//! of the translator's `Install`.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::busy::BusyWork;
use crate::events::{self, topic};
use crate::flow::Install;
use crate::runtime::{Backend, EngineComponent, RuntimeManager, StagedProgress};

/// What the download line calls it.
pub const WHAT: &str = "the PDF engine";

pub struct PdfInstaller {
    runtime: Arc<RuntimeManager>,
    state: Mutex<Option<Install>>,
    cancel: Mutex<Option<CancellationToken>>,
    stopping: CancellationToken,
    me: Weak<PdfInstaller>,
}

impl PdfInstaller {
    pub fn new(runtime: Arc<RuntimeManager>) -> Arc<PdfInstaller> {
        Arc::new_cyclic(|me| PdfInstaller {
            runtime,
            state: Mutex::new(None),
            cancel: Mutex::new(None),
            stopping: CancellationToken::new(),
            me: me.clone(),
        })
    }

    /// Whether the engine is in (one build for every backend).
    pub fn installed(&self) -> bool {
        self.runtime
            .packages()
            .is_installed(EngineComponent::Pdfium, Backend::Cpu)
    }

    /// The download's size.
    pub fn bytes(&self) -> u64 {
        self.runtime
            .packages()
            .package_for(EngineComponent::Pdfium, Backend::Cpu)
            .map(|p| p.total_bytes())
            .unwrap_or(0)
    }

    /// The download while it runs, or what stopped it; None when there is none.
    pub fn state(&self) -> Option<Install> {
        self.state.lock().clone()
    }

    fn changed(&self) {
        events::emit(topic::PDF, json!({ "install": self.state() }));
    }

    /// Starts the download in the background (nothing when it runs already or the engine is
    /// in). Needs the tokio runtime.
    pub fn start(&self) -> Result<(), String> {
        if self.stopping.is_cancelled() {
            return Err("Nook is closing.".into());
        }
        if self.installed() || self.state().is_some_and(|i| i.error.is_none()) {
            return Ok(());
        }
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "Downloads need the app's async runtime.".to_string())?;
        let total = self.bytes();
        let cancel = self.stopping.child_token();
        *self.cancel.lock() = Some(cancel.clone());
        *self.state.lock() = Some(Install {
            what: WHAT.into(),
            done: 0,
            total,
            error: None,
        });
        self.changed();
        let me = self.me.clone();
        handle.spawn(async move {
            let Some(me) = me.upgrade() else { return };
            let progress: StagedProgress = {
                let me = Arc::downgrade(&me);
                Arc::new(move |_stage: &str, done, of| {
                    let Some(me) = me.upgrade() else { return };
                    {
                        let mut state = me.state.lock();
                        let Some(i) = state.as_mut().filter(|i| i.error.is_none()) else {
                            return;
                        };
                        i.done = done.min(if of > 0 { of } else { i.total.max(done) });
                        if of > 0 {
                            i.total = of;
                        }
                    }
                    me.changed();
                })
            };
            let outcome = me
                .runtime
                .ensure_component(EngineComponent::Pdfium, Some(progress), &cancel)
                .await;
            let done = me.state().map_or(0, |i| i.done);
            *me.state.lock() = match outcome {
                Ok(true) => None,
                Ok(false) => Some(Install {
                    what: WHAT.into(),
                    done,
                    total,
                    error: Some("The download was stopped.".into()),
                }),
                Err(e) => {
                    tracing::warn!("The PDF engine did not install: {e:#}");
                    Some(Install {
                        what: WHAT.into(),
                        done,
                        total,
                        error: Some(format!("The download failed: {e:#}")),
                    })
                }
            };
            me.cancel.lock().take();
            me.changed();
        });
        Ok(())
    }

    /// Stops the download; what came down is kept for next time.
    pub fn cancel(&self) {
        if let Some(c) = self.cancel.lock().as_ref() {
            c.cancel();
        }
    }

    /// Forgets a failed download, so the button comes back.
    pub fn clear_error(&self) {
        let cleared = {
            let mut state = self.state.lock();
            if state.as_ref().is_some_and(|i| i.error.is_some()) {
                *state = None;
                true
            } else {
                false
            }
        };
        if cleared {
            self.changed();
        }
    }

    /// Stops a download in progress when the app closes.
    pub fn shutdown(&self) {
        self.stopping.cancel();
    }
}

impl BusyWork for PdfInstaller {
    fn busy_with(&self) -> Option<String> {
        self.state()
            .is_some_and(|i| i.error.is_none())
            .then(|| "the PDF engine is downloading".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::downloader::tests::serve;
    use crate::runtime::manager::testing::{rig, RigSpec};
    use axum::routing::get;
    use axum::Router;
    use std::io::Write;
    use std::time::Duration;

    fn tgz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, *data).unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar.into_inner().unwrap()).unwrap();
        gz.finish().unwrap()
    }

    async fn settled(installer: &PdfInstaller) -> Option<Install> {
        for _ in 0..400 {
            match installer.state() {
                Some(i) if i.error.is_none() => tokio::time::sleep(Duration::from_millis(10)).await,
                other => return other,
            }
        }
        panic!("the download did not end");
    }

    #[tokio::test]
    async fn the_engine_downloads_with_its_progress_and_says_why_when_it_cannot() {
        let archive = tgz(&[("bin/pdfium.dll", b"dll"), ("LICENSE", b"bsd")]);
        let size = archive.len();
        let base = serve(
            Router::new()
                .route(
                    "/pdfium.tgz",
                    get(move || {
                        let a = archive.clone();
                        async move { a }
                    }),
                )
                .route(
                    "/gone.tgz",
                    get(|| async { (axum::http::StatusCode::NOT_FOUND, "no") }),
                ),
        )
        .await;
        let manifest = |file: &str| {
            format!(
                r#"{{"components":{{"pdfium":{{"version":"p1","backends":{{"any":[{{"name":"pdfium.tgz","url":"{base}/{file}","bytes":{size}}}]}}}}}}}}"#
            )
        };

        let good = rig(RigSpec {
            manifest: Some(manifest("pdfium.tgz")),
            ..RigSpec::default()
        });
        let installer = PdfInstaller::new(good.manager.clone());
        assert!(!installer.installed());
        assert_eq!(installer.bytes(), size as u64);
        let mut events = events::subscribe();
        installer.start().unwrap();
        assert!(installer.busy_with().is_some());
        assert_eq!(settled(&installer).await, None);
        assert!(installer.installed());
        assert_eq!(installer.busy_with(), None);
        let mut seen = Vec::new();
        while let Ok(e) = events.try_recv() {
            if e.topic == topic::PDF {
                seen.push(e.payload["install"].clone());
            }
        }
        assert_eq!(seen.first().unwrap()["what"], WHAT);
        assert_eq!(seen.first().unwrap()["done"], 0);
        assert!(seen.last().unwrap().is_null(), "done: null");
        installer.start().unwrap();
        assert_eq!(installer.state(), None, "nothing to do once it is in");

        let bad = rig(RigSpec {
            manifest: Some(manifest("gone.tgz")),
            ..RigSpec::default()
        });
        let installer = PdfInstaller::new(bad.manager.clone());
        installer.start().unwrap();
        let failed = settled(&installer).await.unwrap();
        assert!(failed.error.unwrap().starts_with("The download failed:"));
        assert_eq!(installer.busy_with(), None);
        installer.clear_error();
        assert_eq!(installer.state(), None, "the button comes back");
    }
}

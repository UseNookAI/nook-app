//! The screen recorder on a Mac. FFmpeg has no ScreenCaptureKit input, so Nook captures itself
//! and FFmpeg only encodes: ScreenCaptureKit gives a screen (Nook's own recording windows left
//! out), a window wherever it is (behind others too, as Windows' capture does), an area of a
//! screen, with or without the pointer, and the Mac's own sound. Frames come as NV12 at the size
//! recorded; [`video_feed`] writes the latest one, at the frame rate, into a named pipe FFmpeg
//! reads as raw video, and the sound goes to the mixer like any other source ([`system_sound`]).
//!
//! Screens are listed by CoreGraphics, which needs no permission; windows by ScreenCaptureKit,
//! which asks for the Screen Recording permission the first time (as capturing does).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode, CGError,
    CGGetActiveDisplayList, CGMainDisplayID,
};
use objc2_core_media::{CMBlockBuffer, CMSampleBuffer, CMTime};
use objc2_core_video::{
    kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType, SCWindow,
};
use parking_lot::Mutex;

use super::plan::{Source, Video};
use super::sources::{Screen, Window, OWN_TITLES};

/// What the person is told when macOS has not let Nook record the screen.
pub const PERMISSION: &str = "Nook may not record the screen yet: allow it in System Settings, Privacy & Security, Screen & System Audio Recording, then try again.";
/// How long ScreenCaptureKit may take to answer.
const ANSWER: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------- what can be recorded

/// A screen's place and size in points (the desktop's coordinates, from the main screen's top
/// left), and its pixels per point.
#[derive(Clone, Copy, Debug)]
pub struct Points {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

fn displays() -> Vec<CGDirectDisplayID> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    // SAFETY: a buffer of 16 ids with its length, and a count to fill.
    let error = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if error != CGError::Success {
        return Vec::new();
    }
    ids[..(count as usize).min(ids.len())].to_vec()
}

/// Where a screen is, in points, and its scale (2 on a Retina screen).
pub fn points(display: u64) -> Option<Points> {
    let id = u32::try_from(display).ok()?;
    if !displays().contains(&id) {
        return None;
    }
    let bounds = CGDisplayBounds(id);
    let mode = CGDisplayCopyDisplayMode(id);
    let pixels = CGDisplayMode::pixel_width(mode.as_deref()) as f64;
    let scale = if bounds.size.width > 0.0 && pixels > 0.0 {
        pixels / bounds.size.width
    } else {
        1.0
    };
    Some(Points {
        x: bounds.origin.x,
        y: bounds.origin.y,
        width: bounds.size.width,
        height: bounds.size.height,
        scale,
    })
}

/// The screens, the main one first: in pixels, placed at their points times their scale (so an
/// area picked in a screen's pixels maps back to its points).
pub fn screens() -> Vec<Screen> {
    let main = CGMainDisplayID();
    let mut ids = displays();
    ids.sort_by_key(|id| *id != main);
    ids.iter()
        .enumerate()
        .filter_map(|(i, &id)| {
            let p = points(u64::from(id))?;
            Some(Screen {
                handle: u64::from(id),
                name: format!("Screen {}", i + 1),
                x: (p.x * p.scale).round() as i32,
                y: (p.y * p.scale).round() as i32,
                width: (p.width * p.scale).round() as u32,
                height: (p.height * p.scale).round() as u32,
                primary: id == main,
            })
        })
        .collect()
}

/// The screen whose middle a rectangle (in points) is nearest: the one most of it is on.
fn display_of(frame: CGRect) -> Option<u64> {
    let (cx, cy) = (
        frame.origin.x + frame.size.width / 2.0,
        frame.origin.y + frame.size.height / 2.0,
    );
    displays()
        .into_iter()
        .filter_map(|id| Some((id, points(u64::from(id))?)))
        .min_by(|(_, a), (_, b)| {
            let d = |p: &Points| {
                let dx = (cx - (p.x + p.width / 2.0)).abs() - p.width / 2.0;
                let dy = (cy - (p.y + p.height / 2.0)).abs() - p.height / 2.0;
                dx.max(0.0).hypot(dy.max(0.0))
            };
            d(a).total_cmp(&d(b))
        })
        .map(|(id, _)| u64::from(id))
}

/// A ScreenCaptureKit object held across threads: it is thread-safe, and only its reference
/// count moves.
struct Held<T>(Retained<T>);
// SAFETY: ScreenCaptureKit's content, filters and streams may be used from any thread.
unsafe impl<T> Send for Held<T> {}

/// What there is to capture, as ScreenCaptureKit sees it: asks for the permission the first time.
fn content() -> Result<Retained<SCShareableContent>> {
    let (tx, rx) = mpsc::channel::<std::result::Result<Held<SCShareableContent>, String>>();
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: the pointers ScreenCaptureKit hands the handler, each null or valid.
            let answer = match unsafe { Retained::retain(content) } {
                Some(c) => Ok(Held(c)),
                None => Err(unsafe { error.as_ref() }
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_default()),
            };
            let _ = tx.send(answer);
        },
    );
    // SAFETY: a handler of the declared signature, kept alive by ScreenCaptureKit until called.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, true, &block,
        );
    }
    match rx.recv_timeout(ANSWER) {
        Ok(Ok(Held(c))) => Ok(c),
        Ok(Err(why)) => {
            tracing::warn!("ScreenCaptureKit: {why}");
            bail!(PERMISSION)
        }
        Err(_) => bail!("macOS did not say what can be recorded in time."),
    }
}

/// The windows that can be recorded: applications' own, on screen, with a size, not Nook's.
pub fn windows() -> Vec<Window> {
    let Ok(content) = content() else {
        return Vec::new();
    };
    let own = std::process::id() as i32;
    let mut out = Vec::new();
    // SAFETY: plain properties of what ScreenCaptureKit listed.
    unsafe {
        for w in content.windows().to_vec() {
            let frame = w.frame();
            let app = w.owningApplication();
            if w.windowLayer() != 0
                || !w.isOnScreen()
                || frame.size.width < 64.0
                || frame.size.height < 64.0
                || app.as_ref().is_some_and(|a| a.processID() == own)
            {
                continue;
            }
            let app_name = app
                .as_ref()
                .map(|a| a.applicationName().to_string())
                .unwrap_or_default();
            let title = w
                .title()
                .map(|t| t.to_string())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| app_name.clone());
            if title.is_empty() {
                continue;
            }
            let scale = display_of(frame).and_then(points).map_or(1.0, |p| p.scale);
            out.push(Window {
                handle: u64::from(w.windowID()),
                title,
                app: app_name,
                width: (frame.size.width * scale).round() as u32,
                height: (frame.size.height * scale).round() as u32,
            });
        }
    }
    out
}

/// The screen most of a window is on.
pub fn monitor_of_window(handle: u64) -> Option<u64> {
    let content = content().ok()?;
    // SAFETY: plain properties of what ScreenCaptureKit listed.
    unsafe {
        content
            .windows()
            .to_vec()
            .into_iter()
            .find(|w| u64::from(w.windowID()) == handle)
            .and_then(|w| display_of(w.frame()))
    }
}

/// The person's Movies folder.
pub fn videos_folder() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Movies"))
}

/// Keeps a window of Nook's out of screen captures (`NSWindowSharingNone`); on the main thread.
pub fn exclude_from_capture(ns_window: isize) {
    let window = ns_window as *mut AnyObject;
    // SAFETY: the NSWindow Tauri handed over, on the main thread; sharingType is an NSUInteger.
    if let Some(window) = unsafe { window.as_ref() } {
        let _: () = unsafe { msg_send![window, setSharingType: 0usize] };
    }
}

// ---------------------------------------------------------------------- capturing

/// How frames come: NV12 (FFmpeg's `nv12`, what H.264 takes) for a recording, BGRA for a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pixels {
    Nv12,
    Bgra,
}

/// One frame, its rows packed.
struct Frame {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

type FrameSink = Box<dyn Fn(Frame) + Send + Sync>;
type SoundSink = Box<dyn Fn(&[f32]) + Send + Sync>;

struct Ivars {
    pixels: Pixels,
    frames: Option<FrameSink>,
    sound: Option<SoundSink>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and Output does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "NookCaptureOutput"]
    #[ivars = Ivars]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            autoreleasepool(|_| {
                let ivars = self.ivars();
                if kind == SCStreamOutputType::Screen {
                    if let (Some(sink), Some(frame)) =
                        (&ivars.frames, frame_of(sample, ivars.pixels))
                    {
                        sink(frame);
                    }
                } else if kind == SCStreamOutputType::Audio {
                    if let Some(sink) = &ivars.sound {
                        let stereo = stereo_of(sample);
                        if !stereo.is_empty() {
                            sink(&stereo);
                        }
                    }
                }
            });
        }
    }
);

impl Output {
    fn new(ivars: Ivars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        // SAFETY: NSObject's init on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

/// The pixels of a sample that has any (an unchanged screen sends samples without), packed.
fn frame_of(sample: &CMSampleBuffer, pixels: Pixels) -> Option<Frame> {
    // SAFETY: the sample ScreenCaptureKit hands the output, valid for this call; the pixel
    // buffer is locked while its planes are read, each within its height times its row length.
    unsafe {
        if !sample.is_valid() {
            return None;
        }
        let buffer = sample.image_buffer()?;
        if CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) != 0 {
            return None;
        }
        let planes: &[(usize, usize)] = match pixels {
            // Y at one byte a pixel, then CbCr at two bytes per two pixels, half as many rows.
            Pixels::Nv12 => &[(0, 1), (1, 2)],
            Pixels::Bgra => &[(0, 4)],
        };
        let width = CVPixelBufferGetWidthOfPlane(&buffer, 0);
        let height = CVPixelBufferGetHeightOfPlane(&buffer, 0);
        let mut data =
            Vec::with_capacity(width * height * if pixels == Pixels::Nv12 { 3 } else { 8 } / 2);
        for &(plane, bytes_per) in planes {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane) as *const u8;
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            let rows = CVPixelBufferGetHeightOfPlane(&buffer, plane);
            let row = CVPixelBufferGetWidthOfPlane(&buffer, plane) * bytes_per;
            if base.is_null() || row > stride {
                CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly);
                return None;
            }
            for r in 0..rows {
                data.extend_from_slice(std::slice::from_raw_parts(base.add(r * stride), row));
            }
        }
        CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly);
        Some(Frame {
            width,
            height,
            data,
        })
    }
}

/// An AudioBufferList with room for two buffers (left and right, not interleaved).
#[repr(C)]
struct TwoBuffers {
    list: AudioBufferList,
    second: AudioBuffer,
}

/// The sound of a sample as interleaved stereo floats (ScreenCaptureKit sends 32-bit floats,
/// each channel in a buffer of its own).
fn stereo_of(sample: &CMSampleBuffer) -> Vec<f32> {
    // SAFETY: the list has room for the two buffers it is sized as; the block buffer that holds
    // the samples is kept (retained) until they are copied, then released.
    unsafe {
        let mut buffers: TwoBuffers = std::mem::zeroed();
        let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
        let status = sample.audio_buffer_list_with_retained_block_buffer(
            std::ptr::null_mut(),
            &mut buffers.list,
            std::mem::size_of::<TwoBuffers>(),
            None,
            None,
            0,
            &mut block,
        );
        let _hold = std::ptr::NonNull::new(block).map(|b| CFRetained::from_raw(b));
        if status != 0 {
            return Vec::new();
        }
        let floats = |b: &AudioBuffer| -> &[f32] {
            if b.mData.is_null() {
                return &[];
            }
            std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4)
        };
        let first = buffers.list.mBuffers[0];
        match (buffers.list.mNumberBuffers, first.mNumberChannels) {
            (2, _) => {
                let (l, r) = (floats(&first), floats(&buffers.second));
                l.iter().zip(r).flat_map(|(&a, &b)| [a, b]).collect()
            }
            (1, 2) => floats(&first).to_vec(),
            (1, 1) => floats(&first).iter().flat_map(|&s| [s, s]).collect(),
            _ => Vec::new(),
        }
    }
}

/// A running ScreenCaptureKit stream; stopped when dropped.
pub struct Capture {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
}

// SAFETY: SCStream may be started and stopped from any thread; the output is only called on the
// stream's own queue.
unsafe impl Send for Capture {}

impl Drop for Capture {
    fn drop(&mut self) {
        let (tx, rx) = mpsc::channel::<()>();
        let done = RcBlock::new(move |_error: *mut NSError| {
            let _ = tx.send(());
        });
        // SAFETY: a handler of the declared signature.
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&done)) };
        let _ = rx.recv_timeout(Duration::from_secs(3));
    }
}

/// What a stream captures, and how.
struct Plan {
    filter: Retained<SCContentFilter>,
    config: Retained<SCStreamConfiguration>,
}

/// The filter and configuration for `source` at `size` pixels: a screen without Nook's own
/// recording windows, an area of it (its pixels back to the screen's points), or a window alone.
fn plan(source: &Source, size: (u32, u32), fps: u32, cursor: bool, pixels: Pixels) -> Result<Plan> {
    let content = content()?;
    // SAFETY: ScreenCaptureKit's objects, made and set up as its documentation says.
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(size.0 as usize);
        config.setHeight(size.1 as usize);
        config.setMinimumFrameInterval(CMTime::new(1, fps.max(1) as i32));
        config.setPixelFormat(match pixels {
            Pixels::Nv12 => kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            Pixels::Bgra => kCVPixelFormatType_32BGRA,
        });
        config.setShowsCursor(cursor);
        config.setQueueDepth(5);
        let display = |handle: u64| -> Result<Retained<SCDisplay>> {
            content
                .displays()
                .to_vec()
                .into_iter()
                .find(|d| u64::from(d.displayID()) == handle)
                .ok_or_else(|| anyhow!("That screen is no longer connected."))
        };
        let own = std::process::id() as i32;
        let ours: Vec<Retained<SCWindow>> = content
            .windows()
            .to_vec()
            .into_iter()
            .filter(|w| {
                w.owningApplication().is_some_and(|a| a.processID() == own)
                    && w.title()
                        .is_some_and(|t| OWN_TITLES.contains(&t.to_string().as_str()))
            })
            .collect();
        let without_ours = |d: &SCDisplay| {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                d,
                &NSArray::from_retained_slice(&ours),
            )
        };
        let filter = match source {
            Source::Screen { handle } => without_ours(&*display(*handle)?),
            Source::Area {
                screen,
                x,
                y,
                width,
                height,
            } => {
                let scale = points(*screen).map_or(1.0, |p| p.scale);
                config.setSourceRect(CGRect {
                    origin: CGPoint {
                        x: f64::from(*x) / scale,
                        y: f64::from(*y) / scale,
                    },
                    size: CGSize {
                        width: f64::from(*width) / scale,
                        height: f64::from(*height) / scale,
                    },
                });
                without_ours(&*display(*screen)?)
            }
            Source::Window { handle } => {
                let window = content
                    .windows()
                    .to_vec()
                    .into_iter()
                    .find(|w| u64::from(w.windowID()) == *handle)
                    .ok_or_else(|| {
                        anyhow!(
                            "That window is closed or minimised: choose another, or restore it."
                        )
                    })?;
                SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window)
            }
        };
        Ok(Plan { filter, config })
    }
}

/// Starts a stream for `plan`, its screen frames to `frames` and its sound to `sound`.
fn start(
    plan: Plan,
    pixels: Pixels,
    frames: Option<FrameSink>,
    sound: Option<SoundSink>,
) -> Result<Capture> {
    let wants_sound = sound.is_some();
    let output = Output::new(Ivars {
        pixels,
        frames,
        sound,
    });
    let queue = DispatchQueue::new("ai.nook.capture", None);
    // SAFETY: ScreenCaptureKit's objects, set up as its documentation says; the output and its
    // queue live as long as the stream (they are held in the Capture).
    unsafe {
        if wants_sound {
            plan.config.setCapturesAudio(true);
            plan.config.setExcludesCurrentProcessAudio(true);
            plan.config.setSampleRate(super::audio::RATE as isize);
            plan.config.setChannelCount(2);
        }
        let stream = SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &plan.filter,
            &plan.config,
            None,
        );
        let out = ProtocolObject::from_ref(&*output);
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                out,
                SCStreamOutputType::Screen,
                Some(&queue),
            )
            .map_err(|e| anyhow!("{}", e.localizedDescription()))?;
        if wants_sound {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    out,
                    SCStreamOutputType::Audio,
                    Some(&queue),
                )
                .map_err(|e| anyhow!("{}", e.localizedDescription()))?;
        }
        let (tx, rx) = mpsc::channel::<Option<String>>();
        let started = RcBlock::new(move |error: *mut NSError| {
            let why = error.as_ref().map(|e| e.localizedDescription().to_string());
            let _ = tx.send(why);
        });
        stream.startCaptureWithCompletionHandler(Some(&started));
        match rx.recv_timeout(ANSWER) {
            Ok(None) => {}
            Ok(Some(why)) => {
                tracing::warn!("ScreenCaptureKit did not start: {why}");
                bail!("{PERMISSION} ({why})")
            }
            Err(_) => bail!("macOS did not start the capture in time."),
        }
        Ok(Capture {
            stream,
            _output: output,
            _queue: queue,
        })
    }
}

/// One frame of `source`, `size` pixels, as a PNG: what it records.
pub fn preview(source: &Source, size: (u32, u32)) -> Result<Vec<u8>> {
    let (tx, rx) = mpsc::sync_channel::<Frame>(1);
    let plan = plan(source, size, 5, false, Pixels::Bgra)?;
    let capture = start(
        plan,
        Pixels::Bgra,
        Some(Box::new(move |f| {
            let _ = tx.try_send(f);
        })),
        None,
    )?;
    let frame = rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| anyhow!("The picture did not come in time."))?;
    drop(capture);
    let mut rgba = frame.data;
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        px[3] = 255;
    }
    let image = image::RgbaImage::from_raw(frame.width as u32, frame.height as u32, rgba)
        .ok_or_else(|| anyhow!("The picture came in an odd size."))?;
    let mut png = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .context("Could not make the picture")?;
    Ok(png.into_inner())
}

/// The Mac's own sound (all of it but Nook's), 48 kHz stereo floats, to `push`, until the
/// capture returned is dropped. ScreenCaptureKit takes it from the main screen's stream, whose
/// pictures are not kept.
pub fn system_sound(push: impl Fn(&[f32]) + Send + Sync + 'static) -> Result<Capture> {
    let main = u64::from(CGMainDisplayID());
    let plan = plan(
        &Source::Screen { handle: main },
        (2, 2),
        1,
        false,
        Pixels::Bgra,
    )?;
    start(plan, Pixels::Bgra, None, Some(Box::new(push)))
}

/// The pictures of a recording, written into a named pipe FFmpeg reads as raw NV12 video.
pub struct Feed {
    capture: Option<Capture>,
    stop: Arc<AtomicBool>,
    writer: Option<std::thread::JoinHandle<()>>,
    fifo: PathBuf,
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        self.capture.take();
        let _ = std::fs::remove_file(&self.fifo);
    }
}

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A named pipe (FIFO) in `dir`, made for this process only.
pub fn fifo(dir: &Path, ext: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("Could not create {}", dir.display()))?;
    let path = dir.join(format!(
        "nook-capture-{}-{}.{ext}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&path);
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    // SAFETY: a NUL-terminated path; the pipe is the owner's alone.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
        bail!(
            "Could not make the pipe {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(path)
}

/// Opens a named pipe to write once its reader (FFmpeg) has it open, or gives up after `wait`
/// or when `stop` is set. Writes block from then on, as a pipe's should.
pub fn open_writer(fifo: &Path, wait: Duration, stop: &AtomicBool) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let until = Instant::now() + wait;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(fifo)
        {
            Ok(file) => {
                // SAFETY: the file's own descriptor; back to blocking writes.
                unsafe {
                    let flags = libc::fcntl(file.as_raw_fd(), libc::F_GETFL);
                    libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK);
                }
                return Some(file);
            }
            // No reader yet.
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {}
            Err(_) => return None,
        }
        if stop.load(Ordering::SeqCst) || Instant::now() > until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Starts capturing `video`'s source at its size and frame rate, and a writer that puts the
/// latest frame into a new pipe in `dir` once a frame's time has come (the last one again when
/// the screen did not change, black until the first comes). Returns the pipe for FFmpeg.
pub fn video_feed(video: &Video, dir: &Path, wait: Duration) -> Result<(PathBuf, Feed)> {
    let (w, h) = video.size();
    let fifo = fifo(dir, "nv12")?;
    let latest: Arc<Mutex<Option<Arc<Vec<u8>>>>> = Arc::new(Mutex::new(None));
    let expected = (w as usize) * (h as usize) * 3 / 2;
    let plan = plan(&video.source, (w, h), video.fps, video.cursor, Pixels::Nv12)?;
    let sink = latest.clone();
    let capture = start(
        plan,
        Pixels::Nv12,
        Some(Box::new(move |f: Frame| {
            // A window that changed size is still scaled to the size asked for; anything else
            // would not be the size FFmpeg reads.
            if f.data.len() == expected {
                *sink.lock() = Some(Arc::new(f.data));
            }
        })),
        None,
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let (path, st) = (fifo.clone(), stop.clone());
    let fps = video.fps.max(1);
    let writer = std::thread::Builder::new()
        .name("nook-capture-frames".into())
        .spawn(move || {
            let Some(mut pipe) = open_writer(&path, wait, &st) else {
                return;
            };
            // Black in NV12's video range: Y at 16, CbCr at 128.
            let mut black = vec![16u8; expected];
            black[(w as usize) * (h as usize)..].fill(128);
            let black = Arc::new(black);
            let tick = Duration::from_secs_f64(1.0 / f64::from(fps));
            let mut next = Instant::now();
            while !st.load(Ordering::SeqCst) {
                let frame = latest.lock().clone().unwrap_or_else(|| black.clone());
                if pipe.write_all(&frame).is_err() {
                    break;
                }
                next += tick;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    // Fell behind (FFmpeg busy): start counting again from now.
                    next = now;
                }
            }
        })
        .context("Could not start the frame writer")?;
    Ok((
        fifo.clone(),
        Feed {
            capture: Some(capture),
            stop,
            writer: Some(writer),
            fifo,
        },
    ))
}

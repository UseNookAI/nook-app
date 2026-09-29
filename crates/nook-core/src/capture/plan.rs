//! FFmpeg's command line for a recording: what it captures (`gfxcapture`, Windows' own capture:
//! a window by its handle, a screen by its handle, part of a screen by cropping it), how it
//! encodes (the graphics card's encoder when there is one, frames staying on the card for
//! NVIDIA's and AMD's), the sound from Nook's pipe, and where it all goes (a Matroska file, an
//! RTMP stream, or both at once through `tee`).
//!
//! On a Mac, Nook captures (ScreenCaptureKit, see `super::mac`) and hands FFmpeg the frames
//! through a pipe as raw NV12 video ([`raw_input`]); FFmpeg encodes them with VideoToolbox, the
//! Mac's own encoder (Apple silicon's media engine).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::audio::RATE;

/// What to record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Source {
    /// A whole screen, by its handle (HMONITOR).
    Screen { handle: u64 },
    /// A window, by its handle (HWND), wherever it goes.
    Window { handle: u64 },
    /// Part of a screen: `x`, `y` from its top left, in its pixels.
    Area {
        screen: u64,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
}

/// How much picture a recording keeps for its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Quality {
    Standard,
    High,
}

/// The H.264 encoders Nook tries, the best first: the graphics card's, then Windows' own, then
/// OpenH264 on the processor. A Mac has one, VideoToolbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Encoder {
    Nvenc,
    Amf,
    Qsv,
    MediaFoundation,
    OpenH264,
    VideoToolbox,
}

impl Encoder {
    #[cfg(not(target_os = "macos"))]
    pub const ALL: [Encoder; 5] = [
        Encoder::Nvenc,
        Encoder::Amf,
        Encoder::Qsv,
        Encoder::MediaFoundation,
        Encoder::OpenH264,
    ];
    #[cfg(target_os = "macos")]
    pub const ALL: [Encoder; 1] = [Encoder::VideoToolbox];

    pub fn codec(self) -> &'static str {
        match self {
            Encoder::Nvenc => "h264_nvenc",
            Encoder::Amf => "h264_amf",
            Encoder::Qsv => "h264_qsv",
            Encoder::MediaFoundation => "h264_mf",
            Encoder::OpenH264 => "libopenh264",
            Encoder::VideoToolbox => "h264_videotoolbox",
        }
    }

    /// For the person: "NVIDIA graphics card".
    pub fn name(self) -> &'static str {
        match self {
            Encoder::Nvenc => "NVIDIA graphics card",
            Encoder::Amf => "AMD graphics card",
            Encoder::Qsv => "Intel graphics",
            Encoder::MediaFoundation => "Windows' encoder",
            Encoder::OpenH264 => "the processor (OpenH264)",
            Encoder::VideoToolbox => "the Mac's video encoder",
        }
    }

    /// Whether it takes the captured frames as they are, on the graphics card; the others get
    /// them copied to memory first.
    pub fn takes_card_frames(self) -> bool {
        matches!(self, Encoder::Nvenc | Encoder::Amf)
    }

    /// Its options for a constant `kbps` and a key frame every `gop` frames (streaming services
    /// want one every two seconds).
    pub fn options(self, kbps: u32, gop: u32) -> Vec<String> {
        let own: &[&str] = match self {
            Encoder::Nvenc => &["-preset", "p4", "-tune", "ll", "-rc", "cbr", "-bf", "0"],
            Encoder::Amf => &["-usage", "lowlatency", "-quality", "balanced", "-rc", "cbr"],
            Encoder::Qsv => &["-preset", "veryfast"],
            Encoder::MediaFoundation => &["-rate_control", "cbr", "-scenario", "display_remoting"],
            Encoder::OpenH264 => &["-allow_skip_frames", "1"],
            // In real time, the processor's encoder should the media engine be busy.
            Encoder::VideoToolbox => &["-realtime", "1", "-allow_sw", "1", "-profile:v", "high"],
        };
        let mut o: Vec<String> = own.iter().map(|s| s.to_string()).collect();
        o.extend([
            "-b:v".into(),
            format!("{kbps}k"),
            "-maxrate".into(),
            format!("{kbps}k"),
            "-bufsize".into(),
            format!("{}k", kbps * 2),
            "-g".into(),
            gop.to_string(),
        ]);
        o
    }
}

/// The bit rate a recording of `width`×`height` at `fps` gets, in kbit/s: about 0.07 bits a
/// pixel (1080p30 at about 4.4 Mbit/s), twice that for high quality.
pub fn bitrate_kbps(width: u32, height: u32, fps: u32, quality: Quality) -> u32 {
    let bits_per_pixel = match quality {
        Quality::Standard => 0.07,
        Quality::High => 0.14,
    };
    let kbps = f64::from(width) * f64::from(height) * f64::from(fps) * bits_per_pixel / 1000.0;
    (kbps.round() as u32).clamp(1_000, 80_000)
}

/// The size frames come out at: `width`×`height`, or scaled down to `scale_to` lines keeping its
/// shape (never up); both even, as H.264's 4:2:0 wants.
pub fn output_size(width: u32, height: u32, scale_to: Option<u32>) -> (u32, u32) {
    let (w, h) = match scale_to {
        Some(t) if t < height => {
            let w = (f64::from(width) * f64::from(t) / f64::from(height)).round() as u32;
            (w, t)
        }
        _ => (width, height),
    };
    ((w / 2 * 2).max(2), (h / 2 * 2).max(2))
}

/// A recording's picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Video {
    pub source: Source,
    /// The size of what the source captures before any crop: the screen's (for a screen or an
    /// area of it) or the window's.
    pub captured: (u32, u32),
    pub fps: u32,
    /// The height to scale down to, if any.
    pub scale_to: Option<u32>,
    pub cursor: bool,
    pub encoder: Encoder,
    pub kbps: u32,
}

impl Video {
    /// The size of the picture before scaling: the area's, else what is captured.
    pub fn picture(&self) -> (u32, u32) {
        match &self.source {
            Source::Area { width, height, .. } => (*width, *height),
            _ => self.captured,
        }
    }

    /// The size frames come out at.
    pub fn size(&self) -> (u32, u32) {
        let (w, h) = self.picture();
        output_size(w, h, self.scale_to)
    }
}

/// Where a recording goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A Matroska file (a part of the recording, made into MP4 at the end).
    File(PathBuf),
    /// An RTMP or RTMPS address, the stream key included.
    Stream(String),
    /// Both at once; the stream failing does not stop the file.
    Both(PathBuf, String),
}

/// The `gfxcapture` source for `video`, its frames copied to memory when its encoder wants them
/// there.
pub fn capture_filter(video: &Video) -> String {
    let (w, h) = video.size();
    let mut f = match &video.source {
        Source::Screen { handle } => format!("gfxcapture=hmonitor={handle}"),
        Source::Window { handle } => format!("gfxcapture=hwnd={handle}"),
        Source::Area {
            screen,
            x,
            y,
            width,
            height,
        } => {
            // Crops count from each edge of the whole screen.
            let (sw, sh) = video.captured;
            format!(
                "gfxcapture=hmonitor={screen}:crop_left={x}:crop_top={y}:crop_right={}:crop_bottom={}",
                sw.saturating_sub(x + width),
                sh.saturating_sub(y + height)
            )
        }
    };
    // A window that changes size is scaled to fit the first frame's, bars around it.
    f.push_str(&format!(
        ":max_framerate={}:capture_cursor={}:width={w}:height={h}:resize_mode=scale_aspect",
        video.fps,
        u8::from(video.cursor)
    ));
    if !video.encoder.takes_card_frames() {
        f.push_str(",hwdownload,format=bgra,format=yuv420p");
    }
    f
}

/// A path as a `tee` output names it: forward slashes (a backslash escapes there), and `|`, `[`
/// and `]` escaped.
fn tee_escaped(s: &str) -> String {
    s.replace('\\', "/")
        .replace('|', "\\|")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// FFmpeg's arguments for `video` (and the sound from `audio`, a pipe of 48 kHz stereo floats)
/// into `target`. Progress goes to its standard output as `key=value` lines; `q` on its standard
/// input ends it cleanly.
pub fn args(video: &Video, audio: Option<&str>, target: &Target) -> Vec<String> {
    let input = vec![
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        capture_filter(video),
    ];
    args_from(input, video, audio, target)
}

/// The input for frames Nook captured itself (a Mac's), from `pipe`: raw NV12 at `video`'s size,
/// timed by the clock as they come.
pub fn raw_input(video: &Video, pipe: &str) -> Vec<String> {
    let (w, h) = video.size();
    [
        "-f",
        "rawvideo",
        "-pix_fmt",
        "nv12",
        "-video_size",
        &format!("{w}x{h}"),
        "-framerate",
        &video.fps.to_string(),
        "-use_wallclock_as_timestamps",
        "1",
        "-thread_queue_size",
        "64",
        "-i",
        pipe,
    ]
    .map(String::from)
    .to_vec()
}

/// [`args`] with the picture from `input` (FFmpeg's options for its first input).
pub fn args_from(
    input: Vec<String>,
    video: &Video,
    audio: Option<&str>,
    target: &Target,
) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostats",
        "-progress",
        "pipe:1",
        "-y",
    ]
    .map(String::from)
    .to_vec();
    a.extend(input);
    if let Some(pipe) = audio {
        a.extend(
            [
                "-thread_queue_size",
                "4096",
                "-f",
                "f32le",
                "-ar",
                &RATE.to_string(),
                "-ac",
                "2",
                "-i",
                pipe,
            ]
            .map(String::from),
        );
    }
    a.extend(["-map", "0:v:0"].map(String::from));
    if audio.is_some() {
        a.extend(["-map", "1:a:0"].map(String::from));
    }
    // Every frame the rate asks for, repeated when the screen did not change.
    a.extend(["-fps_mode", "cfr", "-r"].map(String::from));
    a.push(video.fps.to_string());
    a.extend(["-c:v".to_string(), video.encoder.codec().to_string()]);
    a.extend(video.encoder.options(video.kbps, video.fps * 2));
    if audio.is_some() {
        a.extend(["-c:a", "aac", "-b:a", "160k"].map(String::from));
    }
    match target {
        Target::File(path) => {
            a.extend(["-f", "matroska"].map(String::from));
            a.push(path.display().to_string());
        }
        Target::Stream(url) => {
            a.extend(["-f", "flv", "-flvflags", "no_duration_filesize"].map(String::from));
            a.push(url.clone());
        }
        Target::Both(path, url) => {
            a.extend(["-flags", "+global_header", "-f", "tee"].map(String::from));
            a.push(format!(
                "[f=matroska]{}|[f=flv:onfail=ignore:flvflags=no_duration_filesize]{}",
                tee_escaped(&path.display().to_string()),
                tee_escaped(url)
            ));
        }
    }
    a
}

/// A streaming service's address for `key`: its server (ending in its application, "app" or
/// "live2") and the key after a slash.
pub fn stream_url(server: &str, key: &str) -> String {
    let server = server.trim().trim_end_matches('/');
    let key = key.trim().trim_start_matches('/');
    if key.is_empty() {
        server.to_string()
    } else {
        format!("{server}/{key}")
    }
}

/// `text` with every occurrence of `key` hidden, for what is shown or logged about a stream.
pub fn hidden(text: &str, key: &str) -> String {
    let key = key.trim();
    if key.len() < 4 {
        return text.to_string();
    }
    text.replace(key, "(stream key)")
}

/// The arguments for turning the parts of a recording (Matroska files, in order) into one MP4 at
/// `out`, the pieces listed in `list` (a file this writes) when there are several.
pub fn finish_args(parts: &[PathBuf], list: &Path, out: &Path) -> std::io::Result<Vec<String>> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y"]
        .map(String::from)
        .to_vec();
    if parts.len() == 1 {
        a.push("-i".into());
        a.push(parts[0].display().to_string());
    } else {
        let mut text = String::new();
        for p in parts {
            text.push_str(&format!(
                "file '{}'\n",
                p.display().to_string().replace('\'', "'\\''")
            ));
        }
        std::fs::write(list, text)?;
        a.extend(["-f", "concat", "-safe", "0", "-i"].map(String::from));
        a.push(list.display().to_string());
    }
    a.extend(["-c", "copy", "-movflags", "+faststart"].map(String::from));
    a.push(out.display().to_string());
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(source: Source, encoder: Encoder) -> Video {
        Video {
            source,
            captured: (3840, 2160),
            fps: 30,
            scale_to: Some(1080),
            cursor: true,
            encoder,
            kbps: 6_000,
        }
    }

    #[test]
    fn sizes_are_even_and_never_scaled_up() {
        assert_eq!(output_size(3840, 2160, Some(1080)), (1920, 1080));
        assert_eq!(output_size(1280, 720, Some(1080)), (1280, 720));
        assert_eq!(output_size(1001, 601, None), (1000, 600));
        assert_eq!(output_size(2000, 1000, Some(720)), (1440, 720));
        assert_eq!(output_size(1, 1, None), (2, 2));
    }

    #[test]
    fn bit_rates_follow_the_picture() {
        assert_eq!(bitrate_kbps(1920, 1080, 30, Quality::Standard), 4_355);
        assert_eq!(bitrate_kbps(1920, 1080, 60, Quality::High), 17_418);
        assert_eq!(
            bitrate_kbps(320, 200, 30, Quality::Standard),
            1_000,
            "a floor"
        );
    }

    #[test]
    fn each_source_is_captured_as_windows_names_it() {
        let screen = capture_filter(&video(Source::Screen { handle: 65537 }, Encoder::Nvenc));
        assert_eq!(
            screen,
            "gfxcapture=hmonitor=65537:max_framerate=30:capture_cursor=1:width=1920:height=1080:resize_mode=scale_aspect"
        );
        let mut window = video(Source::Window { handle: 1234 }, Encoder::OpenH264);
        window.captured = (1281, 721);
        window.scale_to = None;
        window.cursor = false;
        assert_eq!(
            capture_filter(&window),
            "gfxcapture=hwnd=1234:max_framerate=30:capture_cursor=0:width=1280:height=720:resize_mode=scale_aspect,hwdownload,format=bgra,format=yuv420p"
        );
        let area = video(
            Source::Area {
                screen: 7,
                x: 200,
                y: 100,
                width: 2000,
                height: 1000,
            },
            Encoder::Nvenc,
        );
        assert_eq!(area.size(), (2000, 1000), "under 1080 lines: kept");
        assert_eq!(
            capture_filter(&area),
            "gfxcapture=hmonitor=7:crop_left=200:crop_top=100:crop_right=1640:crop_bottom=1060:max_framerate=30:capture_cursor=1:width=2000:height=1000:resize_mode=scale_aspect"
        );
    }

    #[test]
    fn a_recording_with_sound_goes_to_a_file() {
        let a = args(
            &video(Source::Screen { handle: 1 }, Encoder::Nvenc),
            Some(r"\\.\pipe\nook-capture-1"),
            &Target::File(PathBuf::from(r"C:\Videos\part 1.mkv")),
        );
        let line = a.join(" ");
        assert!(line.starts_with("-hide_banner -loglevel warning -nostats -progress pipe:1 -y -f lavfi -i gfxcapture=hmonitor=1:"), "{line}");
        assert!(
            line.contains(
                r"-thread_queue_size 4096 -f f32le -ar 48000 -ac 2 -i \\.\pipe\nook-capture-1"
            ),
            "{line}"
        );
        assert!(
            line.contains("-map 0:v:0 -map 1:a:0 -fps_mode cfr -r 30 -c:v h264_nvenc -preset p4"),
            "{line}"
        );
        assert!(
            line.contains(
                "-b:v 6000k -maxrate 6000k -bufsize 12000k -g 60 -c:a aac -b:a 160k -f matroska"
            ),
            "{line}"
        );
        assert_eq!(a.last().unwrap(), r"C:\Videos\part 1.mkv");
    }

    #[test]
    fn a_stream_without_sound_and_a_stream_with_a_file() {
        let v = video(Source::Window { handle: 9 }, Encoder::MediaFoundation);
        let a = args(
            &v,
            None,
            &Target::Stream("rtmp://live.twitch.tv/app/live_123".into()),
        );
        let line = a.join(" ");
        assert!(
            !line.contains("-map 1:a") && !line.contains("aac"),
            "{line}"
        );
        assert!(
            line.ends_with(
                "-f flv -flvflags no_duration_filesize rtmp://live.twitch.tv/app/live_123"
            ),
            "{line}"
        );
        let both = args(
            &v,
            Some("pipe"),
            &Target::Both(
                PathBuf::from(r"C:\My [Videos]\a.mkv"),
                "rtmps://live-api-s.facebook.com:443/rtmp/FB-1|2".into(),
            ),
        );
        assert_eq!(
            both.last().unwrap(),
            r"[f=matroska]C:/My \[Videos\]/a.mkv|[f=flv:onfail=ignore:flvflags=no_duration_filesize]rtmps://live-api-s.facebook.com:443/rtmp/FB-1\|2"
        );
        assert!(both.join(" ").contains("-flags +global_header -f tee"));
    }

    #[test]
    fn stream_addresses_and_keys() {
        assert_eq!(
            stream_url("rtmp://live.twitch.tv/app/", " live_42 "),
            "rtmp://live.twitch.tv/app/live_42"
        );
        assert_eq!(
            stream_url("rtmp://host/app/full-key", ""),
            "rtmp://host/app/full-key"
        );
        assert_eq!(
            hidden("Could not open rtmp://h/app/live_42abc", "live_42abc"),
            "Could not open rtmp://h/app/(stream key)"
        );
    }

    #[test]
    fn parts_become_one_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("parts.txt");
        let one = finish_args(&[PathBuf::from("a.mkv")], &list, Path::new("out.mp4")).unwrap();
        assert_eq!(
            one.join(" "),
            "-hide_banner -loglevel error -y -i a.mkv -c copy -movflags +faststart out.mp4"
        );
        assert!(!list.exists());
        let two = finish_args(
            &[PathBuf::from("a.mkv"), PathBuf::from("it's b.mkv")],
            &list,
            Path::new("out.mp4"),
        )
        .unwrap();
        assert!(two.join(" ").contains("-f concat -safe 0 -i"));
        assert_eq!(
            std::fs::read_to_string(&list).unwrap(),
            "file 'a.mkv'\nfile 'it'\\''s b.mkv'\n"
        );
    }
}

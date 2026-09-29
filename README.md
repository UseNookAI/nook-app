# Nook

Private and secure AI on your GPU, for Windows. A local coding worker changes your repository in a
scratch copy and hands the change back as a diff; a Code editor keeps the worker beside the open
file; short video clips render on your GPU; and a local OpenAI-compatible gateway serves the same
models to other programs. Everything runs on models on your own machine.

This is Nook 0.4.2 (the Kotlin/Compose app, now retired) rewritten in Rust with Tauri 2: all logic in Rust, the
window drawn by WebView2 from a React/TypeScript UI. The installer is about 8 MB instead of 236 MB,
and there is no JVM.

## What you see

- The title strip has **Chat**, **Code** and **Nooklets**, an icon each, the open page's lit; the
  sidebar has the sessions so far, and Settings.
- **Chat** starts a session: the worker model, the repository (earlier sessions' folders or Choose
  a folder…), and the voice prompt. A session shows each request, the worker's live steps, its
  summary, the check's result and the change as a diff by file, with Undo last request, Discard and
  Apply to repository. **Video**, on the same Chat | Video switch, renders short clips on the GPU.
- **Code** is an editor for a folder (CodeMirror: highlighting for about fifty languages, folding,
  undo, Ctrl+F, Ctrl+S; right-click a file for new file, new folder, rename and delete) with Nook
  on the right: the same worker session on a narrow column, told which file is open and what is
  selected. Apply reloads the open files. The folder, the open files and the panels come back
  after a restart (`<home>\code\ide.json`).
- **Nooklets** are ready-made jobs done in steps on this computer, one run at a time, with each
  step's progress shown. The page opens on the finder: Scout, a small animated Nooklet, asks
  *What do you want done?*, and the request, typed or spoken in any language, goes to a small
  fixed model on the processor ([multilingual-e5-small](https://huggingface.co/intfloat/multilingual-e5-small),
  133 MB, served by llama.cpp's processor build and stopped after ten minutes unused) that
  compares it with each Nooklet's examples and answers with the one for the job, opened with what
  the request set ("into German" chooses the language, "to PDF" the format). Enter asks; Enter
  again opens the answer. Until the finder is downloaded, the request's words choose. Once a
  Nooklet is open, the others are in a rail on the left, with the way back to the finder.
- The first Nooklet, *Translate speech*, takes what you say into the microphone (press,
  speak, press again: the translation plays by itself when it is ready) or a recording, a podcast
  or a video (dropped anywhere on the window or chosen), and gives it back spoken in another
  language: Whisper writes down what is said, your chat model translates it in numbered batches so
  the timing survives, the audio engine speaks the lines (in the speaker's own voice, cloned from a
  few seconds of the track, when a voice can), and the lines are laid over the original timeline,
  sped up a little (pitch kept) when one runs long. A video gets its picture back with the new
  track. The run's folder under `<home>\flows\<run>` holds the track, the video, the recording,
  and the text and subtitles in both languages. Everything a run still needs is one download with
  its size: the voice engine ([audio.cpp](https://github.com/0xShug0/audio.cpp): its CUDA build
  on NVIDIA cards, 1.07 GB with the CUDA runtime; its Vulkan build on other cards, 60 MB; 25 MB for
  the processor), the voice (Qwen3-TTS 2.0 GB for ten
  languages, VoxCPM2 3.0 GB for thirty, both cloning; Supertonic 0.3 GB for a standard voice in
  thirty-one), Whisper Large v3 Turbo (574 MB) when no speech model is in, and FFmpeg (81 MB) only
  for a video or a format Nook does not read itself (Opus, WMA, AMR…): MP3, M4A/AAC, FLAC, Ogg
  Vorbis, WAV, AIFF and the sound of MP4 and MKV files are read by Nook.
- The second Nooklet, *Edit a PDF*, opens a PDF of any length (dropped or chosen; pages draw as
  they scroll into view) and changes its text in place: drag across the words (or click a line,
  which picks the run of one style under the pointer, say a bold name in a sentence), type, press
  Enter. The editing box shows the text in its own font, size and colour on the page's own
  background, and the new text goes into the PDF as text, in the line's own font object. PDFs
  embed only the letters they use, so when a new letter is missing Nook sets the line in the same
  face installed on Windows (found through the registry), at the same size, colour and place, and
  says so; a figure keeps its right edge, the rest of a line moves up to follow a longer or
  shorter word, and the page's reading order is kept. Text that is part of a picture (a scan, a
  form printed flat, letters turned into shapes) is edited the same way: Windows' own text
  recognition reads it from the page drawn at 288 dpi, its ink gives its colour, the paper's, its
  baseline and size, and the installed face it is set in is found by drawing the words in the
  usual Windows and Office faces and comparing (Arial, Calibri, Times New Roman, Courier New and
  some thirty more); the old letters are covered in the paper's colour and the new text is set
  over them as text, so it can be changed again (a scan's invisible text layer there goes too).
  The editing box offers the other way beside it, *Page letters*: the new text made of letters
  cut from the page itself (the whole page read once, each word it reads cut into its letters,
  and for each letter the copy in the same ink, as tall as the face's and shaped like it), so it
  looks like the scan down to its blur; a letter the page does not have is drawn in the matched
  face, and the note says which. The line goes in as a picture over the old one, with its words
  beneath as invisible text so the PDF still finds and copies them.
  Every edit is checked against the page as it was: PDFium rewrites a page it edits and cannot
  write back everything (colours in an ICC profile, gradients), so when anything outside the
  edited lines changed, the edit is made again with the page's colours set anew, or else over the
  page's drawing without rewriting it (the old words covered, and said so). Undo, Save
  ("<name> (edited).pdf" beside the original) and Save as. It runs on PDFium, Chrome's PDF engine
  (3.7 MB, downloaded the first time).
- The third, *Convert documents*, turns files dropped or chosen (several at once) into the format
  picked from what they can all become: Word, OpenDocument and RTF documents, PDF, web pages,
  Markdown, plain text, LaTeX, reStructuredText, AsciiDoc, Typst, Org, MediaWiki, Jupyter
  notebooks, EPUB and FictionBook, Excel, OpenDocument, CSV, TSV and JSON tables, PowerPoint and
  OpenDocument slides, and PNG, JPEG, WebP, BMP, TIFF, GIF and icon pictures. Office files go
  through an office suite so they keep their layout: Word, Excel and PowerPoint when they are on
  the computer (driven through their own automation, hidden), else LibreOffice (373 MB, downloaded
  when first needed), with a document's macros turned off in either. A workbook with Excel 4.0
  macro sheets, which a hidden Excel may stop to ask about, goes to LibreOffice, or is refused
  with a word on what to do. Text formats go through Pandoc (42 MB), web pages and text to PDF through
  Edge's printing, PDFs to text and documents through PDFium's reading of each line (headings by
  size, lists, bold, hyphens joined, page numbers dropped) and to pictures page by page, pictures
  and tables through Nook itself (a workbook's sheets become one file each, several pictures one
  PDF when asked). A result goes beside its file ("report.pdf", "report (2).pdf" when that is
  taken) or into a folder chosen; the conversions follow, one at a time, each result to open or
  show in Explorer.
- Three more run on the translator's queue, so no two of them want the card at once:
  *Transcribe a recording* writes down a file or what the microphone hears (Whisper, the language
  told or chosen) as paragraphs, a timed transcript, SRT and WebVTT subtitles, and with *Notes*
  the chat model adds what was said in short: key points, decisions, who does what by when, open
  questions. *Summarize a document* reads a PDF (scans through Windows' text recognition), a Word
  file, a web page, an e-book, slides or pasted text through the converter's engines, and the chat
  model writes a short or detailed summary with what is worth checking (deadlines, amounts,
  obligations), in the document's language or another, looking first at what the person asks
  about; a long document is summarized in parts of about 2,800 tokens, then as one. *Read it
  aloud* reads a document or pasted text with a standard voice (Supertonic's woman's or man's
  voice, VoxCPM2 for the languages only it speaks), the language told from the text (whatlang),
  Markdown's marks and code left out, sentences joined into lines of about 240 characters, as one
  track (M4A with FFmpeg in, else WAV) that plays with its text lit line by line.
- *Record your screen* records or streams a whole screen, one window (by Windows' own capture, so
  it records even behind other windows) or an area dragged out over the screen, with a
  microphone and the computer's own sound (Windows' loopback), each chosen by device and with
  its meter; a picture shows what it records before it starts. It encodes H.264 on the
  graphics card when it can (NVIDIA, AMD or Intel, the frames staying on NVIDIA's and AMD's
  cards), else with Windows' encoder or OpenH264, at 30 or 60 frames a second, its own size or
  scaled to 1080p or 720p. A recording can pause (each run a Matroska part, safe if anything
  stops) and becomes one MP4 in Videos › Nook or a chosen folder. A stream goes to Twitch,
  YouTube, Facebook, Kick or any RTMP, RTMPS or SRT address, and can be recorded at once; its key
  can be kept, encrypted for the Windows account (DPAPI). While it runs, small controls with the
  time, pause and stop sit on the recorded screen, kept out of the recording, and Nook's window
  can step aside. FFmpeg does the work (81 MB, downloaded the first time).
- **Settings**: *General* (theme, updates, erase everything), *Models* (the curated *Library*,
  *Browse* for any GGUF on Hugging Face sized against your GPU, *Workers*: which model writes the
  code and which transcribes the voice prompt, and the worker's web access), *Runtime* (engines,
  GPU memory, measured speed, loaded models, recent events) and *About*.
- The light theme is white; a dark theme follows the system or the title bar's toggle.

## The worker, the runtime, the gateway

- The worker is a local model made for tool use (gpt-oss 20B or Qwen3-Coder 30B-A3B from the
  catalog). The first load of a model measures its speed; under 8 tokens per second it is marked too
  slow for Code.
- Each turn works in a scratch copy under `<home>\tmp\code` and may run only the commands listed
  under `verify` in the repository's `nook.json` (or a built-in set for its ecosystem). Nothing
  touches your repository until you press Apply. Links in your folder (symbolic links,
  junctions) are left out of the copy, so nothing outside it comes in through one.
- What a check runs is its own code (a test the worker wrote, a build script), so on Windows each
  check runs sandboxed: a low-integrity process with its privileges dropped, in a job of its own.
  Windows lets it write only inside its scratch copy and that copy's own caches
  (`%USERPROFILE%\AppData\LocalLow\Nook\checks\<id>`: temporary files, npm, Gradle, Go, pip and
  Cargo caches, removed after 30 days unused), never to your repository, your other files,
  another scratch copy or your registry settings, and keeps it off the clipboard. Each scratch
  copy and its caches are given to you and a SID only its own checks carry, whatever the drive
  lets other users do, so one project's checks cannot reach another's. A check reads only its
  scratch copy, what every account on the computer may read, and the toolchains on your PATH
  (Nook grants those in your profile to the checks to read, never your profile's own folders
  such as Documents): not the rest of your profile, where your keys, tokens and browser data
  are, nor your registry settings. It gets only the part of Nook's environment toolchains need.
  It still has the network (a build fetches its packages). A program is found on PATH, never in the scratch copy.
  Windows PowerShell 5.1 cannot start in the sandbox (PowerShell 7 can). For a toolchain that
  cannot work this way, starting Nook with `NOOK_UNSANDBOXED_CHECKS=1` runs checks as before.
- With web access on, the worker can search DuckDuckGo and read public pages it was shown, spaced
  out and capped at eight searches a request.
- Engines come from the pinned llama.cpp, whisper.cpp and stable-diffusion.cpp releases in
  `resources/runtime/engines.json` (CUDA 12 for NVIDIA, Vulkan otherwise), verified by checksum.
  Models come from `resources/runtime/catalog.json` or Hugging Face, downloaded with resume.
  Models the installed Kotlin Nook already downloaded are reused, read-only.
- Each loaded text model is one `llama-server` on a random loopback port with its own key; the
  runtime plans GPU layers from the free VRAM, evicts idle models and restarts crashed ones.
  Engines die with the app.
- The **gateway** listens on `http://127.0.0.1:41434` (port and per-start token in
  `<home>\gateway.json`): `/v1/chat/completions`, `/v1/completions`, `/v1/models`,
  `/v1/embeddings`, `/v1/audio/transcriptions`, `/v1/images/generations`, `/v1/videos`, and
  `/runtime/*` for status, downloads, load, unload and pin.

## Identity

Nook, `Nook.exe`, Tauri identifier `ai.nook.app`, gateway port 41434 (`NOOK_RS_GATEWAY_PORT`),
updates from https://dl.usenook.ai/nook. A per-user install goes to
`%LOCALAPPDATA%\Programs\Nook`. Its data folder is `%LOCALAPPDATA%\Nook-rs` (`NOOK_RS_HOME`); the
Kotlin Nook's `%LOCALAPPDATA%\Nook` is never written, and its downloaded models are reused
read-only (`NOOK_RS_SHARED_MODELS` names another folder; empty turns it off).

## Replacing the Kotlin Nook

Every installed Kotlin Nook updates from the same host, so the next update it takes is this app.
Its updater runs the installer with Inno Setup's flags (`/SILENT /SUPPRESSMSGBOXES
/CLOSEAPPLICATIONS`, and `/RESTARTAPPLICATIONS` in 0.3.0) and quits; from 0.3.0+ecd64d8 on it then
starts `%LOCALAPPDATA%\Nook\app\Nook.exe` again. The installer (Tauri's NSIS template adapted in
`src-tauri/windows/installer.nsi`, with `src-tauri/windows/hooks.nsh`):

- takes `/SILENT` and `/VERYSILENT` for a silent install, as it takes `/S`;
- finds the Kotlin Nook (its uninstall entry `{7D0E4E1E-6B5A-4E9C-9C1A-NOOK-DESKTOP}_is1`, or
  `%LOCALAPPDATA%\Nook\app\unins000.exe`), waits for it to exit (up to a minute when silent, then
  ends it; an interactive install asks), and runs its uninstaller silently, which removes the
  program and its shortcuts and keeps `%LOCALAPPDATA%\Nook`;
- installs this app and leaves a hard link of its `Nook.exe` (a copy on another volume) in
  `%LOCALAPPDATA%\Nook\app` with `nook-handover.txt` beside it. Started there by the old updater,
  the app starts the installed Nook and exits (`nook_core::update::handover`); the installed
  Nook removes the pair once nothing needs it;
- keeps a desktop icon only if the old app had one, points its taskbar pins at the new app, and
  after 0.3.0's `/RESTARTAPPLICATIONS` starts the new app itself, since nothing else will.

At its first start the app imports the old app's data into its new home (`nook_core::migrate`):
the Code sessions and `code\ide.json` (a session's scratch copy is made again from the repository,
with its pending change, the first time it is needed), `runtime\workers.json`,
`runtime\probe.json`, `web.json`, and the engines in `runtime\bin` as hard links, so nothing is
downloaded again. Nothing in the old folder changes; `<home>\data\kotlin-import.json` records the
import, which runs once and only into a home not yet in use (`NOOK_RS_KOTLIN_HOME` names another
old home; empty turns it off, and a home moved by `NOOK_RS_HOME` imports nothing by itself).

## Project structure

- `crates/nook-core`: everything but the window. `runtime` (engines, models, GPU, downloads,
  inference), `worker` (the tool loop, its tools, the path policy, verify commands), `code`
  (sessions and scratch copies), `ide` (the editor's files), `video` (the clip queue), `gateway`,
  `web`, `speech`, `update`, `settings`, `home`.
- `crates/nook-release`: signs and verifies update manifests, makes release keys.
- `src-tauri`: the window and the commands (`src/commands/<area>.rs`), forwarding core events to
  the UI as `nook:<topic>`.
- `ui`: React + TypeScript. `src/api` is the contract with the Rust side; `src/api/mocks` lets
  every screen run in a plain browser (`npm run dev` in `ui/`, then e.g. `?settings=models`).

## Build and run

Needs Windows 11, Node 22, and Rust with the MSVC toolchain (`tools\env.ps1` finds a portable one
in a `tools\rust` folder above the repository).

```powershell
. .\tools\env.ps1
.\tools\dev.ps1           # the app from source, UI hot-reloading, on the normal home
.\tools\dev-sandbox.ps1   # a sandboxed copy beside an installed one: own home, port, identifier
.\tools\package.ps1       # the installer from the working tree
cargo test --workspace; npm --prefix ui test
```

## CI/CD

- **GitHub Actions** (`.github/workflows`): every push and pull request runs rustfmt, clippy, the
  Rust and UI tests and the typecheck on Windows, then builds the installer as an artifact. A tag
  `v<version>` builds that version and attaches the installer and signed manifests to a release
  (secret `NOOK_RS_RELEASE_KEY`, variable `NOOK_RS_UPDATE_BASE`).
- **On this PC** (`tools/`): `install-pipeline.ps1` (run once, from your own terminal) registers the
  "Nook pipeline" task and git hooks. Each commit on main then goes through `pipeline.ps1` →
  `publish.ps1 -Test`: the checks in a clean worktree, the installer with the next version, signed
  `dev/latest.json` on the download host every Nook follows, https://dl.usenook.ai/nook (a folder
  with `-Feed` is a test feed). A Nook on the dev channel installs it by itself once nothing runs;
  `promote.ps1` makes a dev build the stable release. Failures show as Windows notifications;
  `<feed>\pipeline\status.txt`, `state.json` and `logs` say what happened.
- The host is signed with the original release key every installed Nook believes, test feeds with
  this repository's own; both stay outside the repository, as do the host's address and SSH key
  (`tools/release.local.json` on the machine that publishes, shaped like
  `tools/release.local.example.json`), and `resources/release-keys.txt` holds the keys' public
  halves.

## Data

Everything stays on the machine, under `%LOCALAPPDATA%\Nook-rs`. The only outbound traffic is the
model and engine downloads you start, the update check when a feed or host is configured, and,
while web access is on, the worker's searches and the pages it reads. Settings › General › Erase
everything puts the settings back to their defaults and empties the log, as the original did;
sessions, models and videos stay until you delete them.

Coming from the Kotlin Nook, its sessions, worker choices, measured speeds, web-access switch
and engines are imported once (see above) and its models are read where they are. Its settings
(the H2 database: theme, update channel), videos and Nooklets' runs are not imported: this app starts
on the stable channel with the default theme, and setup counts as done when the old app was used.
The handover's silent uninstall of the Kotlin Nook keeps `%LOCALAPPDATA%\Nook`, and this app's
uninstaller never touches it.

## License

Nook's source is public under the [PolyForm Noncommercial License 1.0.0](LICENSE.md): free for
personal use and any other noncommercial purpose, and for charities, schools, public research and
government bodies. Commercial use needs a separate license from us. That makes Nook
source-available, not open source.

The license covers the code only; it grants no rights to the Nook name or logo, or to the Nooklet
characters. What Nook downloads or builds on (engines, models, Rust and npm packages) keeps its own
license.

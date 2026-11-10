<p align="center">
  <img src="assets/logo-256.png" width="96" alt="WispShot logo">
</p>

<h1 align="center">WispShot</h1>

<p align="center">
  A screenshot and screen recording tool for Windows that stays out of your way.<br>
  One small exe, no installer, about 2 MB of memory and zero CPU while it waits.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0-blue" alt="License: GPL-3.0"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D4" alt="Platform: Windows 10 and 11">
  <img src="https://img.shields.io/badge/written%20in-Rust-B7410E" alt="Written in Rust">
</p>

<p align="center">
  <b>English</b> · <a href="README.tr.md">Türkçe</a>
</p>

<p align="center">
  <img src="docs/media/capture.gif" width="720" alt="Selecting an area, marking it up with a box, numbered steps and an arrow, blurring a line of text, then copying it">
</p>

## Why WispShot

Most screenshot tools are fine at taking screenshots. The trouble is everything they do the rest of the day:
background processes, tens of megabytes of memory, sometimes a whole runtime. WispShot sits in the tray and does
nothing until you press the key.

| | WispShot 0.2 | Snipaste 2.11 | Flameshot 14 | Snow Shot 1.4 |
|---|---|---|---|---|
| Download size | **0.8 MB** (one exe) | 47 MB | 95 MB | 177 MB |
| Processes while idle | **1** | 1 | 1 | 4 |
| Memory while idle, after one screenshot (private) | **1.9 MB** | 61 MB | 9 MB | 22 MB |
| Memory while idle, after one screenshot (working set) | **0.7 MB** | 102 MB | 47 MB | 94 MB |
| CPU time in 30 s of idle | **0.000 s** | 0.031 s | 0.047 s | 0.031 s |
| Key press to the dimmed capture screen (median of 10) | **154 ms** | 194 ms | 201 ms¹ | 209 ms |

WispShot also puts a blank veil on screen about 4 ms after the key press, so the press never feels ignored, and
it catches any clicks you make before the editor is ready.

¹ Flameshot only covers the monitor under the cursor; the others cover every monitor.

Measured on 10 November 2026 on one laptop (Windows 11, two 1080p monitors, one at 125%), each app on its own with
default settings, using [`tools/bench/bench.ps1`](tools/bench/bench.ps1). The latency test presses the shortcut and
times how long it takes until the screen visibly changes. Your numbers will differ; the gap in memory won't by much.

## Features

**Capture**
- Region capture on a global shortcut: the screen freezes and everything outside your selection dims
- Automatic window detection; press `D` to pick individual parts of a window instead
- Move and resize the selection with handles or the arrow keys, with a live size label
- Magnifier with the pixel color under the cursor (`C` copies it as `#RRGGBB`)
- Copy without clicking first: press `Enter` and the window under the cursor is copied

**Annotate**
- Rectangle, ellipse, arrow, line, pen, highlighter, text, numbered steps
- Blur and pixelate: paint them on like a brush, or hold `Shift` and drag a rectangle
- Scroll to change the width, with a live preview at the cursor
- Hold `Ctrl` to move or delete any drawing; double-click text to edit it again
- Unlimited undo and redo

**Share**
- Copy to the clipboard, or **copy as a file** to paste straight into Explorer, email or chat apps
- Save as PNG, JPG or BMP, either asking every time or straight into a folder
- Pin an image on top of other windows; zoom it, make it see-through, copy it again later
- **OCR** with the text recognition built into Windows: nothing to download

**Record**
- Record any area to MP4 (H.264) with the GPU's own encoder (Intel Quick Sync, NVIDIA NVENC, AMD AMF)
- **Draw on the recording** while it runs: pen, highlighter, arrows, boxes and step numbers that fade after a few
  seconds, or stay if you prefer. They end up in the video, the toolbar doesn't
- Works with rotated and portrait monitors, and records the cursor

<p align="center">
  <img src="docs/media/draw-on-recording.gif" width="640" alt="A recording with a box, an arrow and two step numbers drawn on it; the box and arrow fade after a few seconds, the numbers stay">
</p>

**Everything else**
- Every shortcut can be changed in Settings
- English and Turkish interface, light and dark theme (or follow Windows)
- Shortcuts step aside while a full-screen game or video is in front, so they reach that app instead
- Settings live in a plain `wispshot.ini` next to the exe, so it's portable

## Getting started

1. Download `wispshot.exe` from the [Releases](../../releases) page.2. Run it. An icon appears in the tray; nothing else happens until you need it.
3. Press `F1` to take a screenshot, or `Shift+F1` to record an area.

Right-click the tray icon for Settings, where you can change shortcuts, choose where files are saved and turn on
*Start with Windows*.

<p align="center">
  <img src="docs/media/tray-menu.png" width="300" alt="The tray menu">
  &nbsp;
  <img src="docs/media/settings.png" width="520" alt="The About page in Settings, showing the tray app using 1.9 MB">
</p>

**Requirements:** Windows 10 (version 2004 or newer) or Windows 11, 64-bit. Developed and tested on Windows 11.
For OCR, Windows needs a text recognition language installed
(*Settings > Time & language > Language & region*), which is usually already there for your display language.

## Shortcuts

These are the defaults. All of them except the fixed keys (`Esc`, `Enter`, arrows, `Delete` and typing in a text box)
can be changed under *Settings > Shortcuts*.

| Anywhere | |
|---|---|
| `F1` | Take a screenshot |
| `Shift+F1` | Start or stop recording an area |
| `F3` | Pin the image on the clipboard to the screen |
| `Shift+F2` (while recording) | Draw on the recording |

| In the capture editor | |
|---|---|
| Drag / click | Select an area / select the detected window |
| `Enter`, `Ctrl+C`, double-click | Copy and close |
| `Ctrl+Shift+C` | Copy as a file |
| `Ctrl+S` | Save |
| `Ctrl+T` | Pin to screen |
| `Ctrl+O` | Copy the text in the selection (OCR) |
| `Ctrl+R` | Record the selected area |
| `Ctrl+A` | Select the current monitor (again: all monitors) |
| `R` `E` `A` `L` `P` `M` `T` `N` `X` `B` | Rectangle, ellipse, arrow, line, pen, highlighter, text, numbered step, pixelate, blur |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| Right-click / `Esc` | Go back a step / close |

<details>
<summary>More editor shortcuts</summary>

| | |
|---|---|
| `Shift` while drawing | Straight lines at 45° steps, squares and circles |
| `Shift` + drag with blur or pixelate | Rectangle instead of a brush |
| Mouse wheel | Line width or brush size |
| `Ctrl` + drag on a drawing | Move it |
| `Ctrl` + click, then `Delete` | Select a drawing and delete it (holding `Ctrl`, `Delete` removes the one under the cursor) |
| Arrow keys on a selected drawing | Nudge by 1 px (`Shift`: 10 px) |
| Double-click text, or `F2` on selected text | Edit the text, its color and size |
| Arrow keys / `Shift` + arrow keys | Move / resize the selection by 1 px |
| `C` (before selecting) | Copy the color under the cursor |
| `D` | Detect parts of windows, not just whole windows |

The text box works like a normal one: click to place the caret, drag or `Shift` + arrows to select, double-click a
word, `Ctrl+A`, `Ctrl+←/→`, `Home`/`End`, `Ctrl+C/X/V`, `Ctrl+Backspace`, and its own undo.
</details>

<details>
<summary>Drawing on a recording</summary>

| | |
|---|---|
| *Draw* on the recording bar | Opens the toolbar: pen, highlighter, arrow, box, step number, four colors |
| `P` `M` `A` `R` `N`, `1` to `4` | Tool, color |
| Mouse wheel | Width |
| `F` | Let drawings fade after about 3 seconds (default) or keep them. Step numbers always stay |
| `Ctrl+Z`, `Delete` | Undo the last drawing, clear everything |
| `Esc`, right-click, *Done* | Stop drawing; the mouse goes back to the app underneath |
</details>

<details>
<summary>Pinned images</summary>

| | |
|---|---|
| Drag | Move |
| Mouse wheel / `Ctrl` + wheel | Zoom / opacity |
| `0` | Actual size |
| Right-click | Copy, copy as file, save, OCR, close |
| Double-click / `Esc` | Close |
</details>

From the command line: `wispshot.exe --capture`, `--record`, `--pin` or `--exit` talk to the running tray app.

## Settings

Changes in the Settings window apply right away; there's no Save button. Everything is stored in `wispshot.ini`
next to the exe (or in `%APPDATA%\WispShot` if that folder isn't writable). You can also edit it by hand
(*Settings > General > Settings file* opens it) and restart WispShot; your comments are kept.

<details>
<summary>wispshot.ini reference</summary>

| Key | Values |
|---|---|
| `[output] save_mode` | `ask` (default) or `folder` |
| `[output] save_dir`, `notify` | Empty means `Pictures\WispShot`; `true` / `false` |
| `[record] save_mode` | `folder` (default) or `ask`. If you cancel the dialog, the video goes to the default folder instead of being lost |
| `[record] video_dir`, `notify`, `fps`, `cursor` | Empty means `Videos\WispShot` |
| `[record] ink_fade` | `true`: drawings on a recording fade after a few seconds |
| `[general] language` | `system`, `en` or `tr` |
| `[general] theme` | `system`, `light` or `dark` |
| `[hotkeys]` | Global shortcuts, including `record_draw` |
| `[hotkeys] pause_in_fullscreen` | `true` (default): while a full-screen app is in front, the shortcuts are released. A running recording can still be stopped |
| `[keys] editor.*`, `draw.*`, `pin.*` | Shortcuts inside the editor, while drawing on a recording, and on pinned images |
</details>

If another app already holds a shortcut (Snow Shot uses `F1`, for example), WispShot tells you and you can pick
another one.

## How it stays light

The same exe runs in two roles.

- **The tray app** only owns the tray icon and the global shortcuts. It blocks in `GetMessage`: no timers,
  no polling, no hooks, no worker threads. About 2 MB.
- **A session process** starts for each screenshot or recording and exits when its last window closes. Direct2D,
  WIC, OCR and the video encoder are only ever loaded here. This matters more than it sounds: some GPU drivers keep
  140 to 180 MB around after Direct2D is used, and the only reliable way to get it back is to end the process.
  If a session crashes, the tray app is unaffected.

Pinned images and OCR results can stay open for hours, so they are handed over to a fresh process that never loaded
Direct2D (about 2.5 MB for a pinned image).

For speed, the tray app shows a transparent veil the moment you press the key (it's excluded from screen capture,
so it never ends up in the shot). Clicks and drags on it are passed on to the editor once it's ready, so you can
start selecting before the editor has finished loading.

## Building from source

You need Rust with the GNU toolchain and MinGW-w64 (for `dlltool` and `windres`).

```sh
winget install Rustlang.Rustup
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup default stable-x86_64-pc-windows-gnu
winget install BrechtSanders.WinLibs.POSIX.UCRT
./build.sh          # builds target/release/wispshot.exe
```

A running tray app locks its exe, so build test copies elsewhere (`./build.sh --target-dir target/t`) and start a
session directly: `target/t/release/wispshot.exe --session capture 0` (also `record` and `settings`). Set
`WISP_TRACE=1` to write a timing log to `%TEMP%\wispshot-trace.log`.

To add a language, copy `lang/en.txt`, translate the values and add it to `LANGS` in `src/i18n.rs`.

<details>
<summary>Source layout</summary>

| File | What's in it |
|---|---|
| `main.rs` | Tray app: shortcuts, autostart, starting sessions, full-screen detection |
| `traymenu.rs` | The tray menu, drawn with GDI only |
| `session.rs` | Session process: actions, recording lifecycle, hand-over, exiting when done |
| `veil.rs` | The instant veil and input forwarding |
| `overlay.rs` | Selection and the capture editor (GDI first frame, then Direct2D) |
| `capture.rs` | Screen grab, window and element detection |
| `gfx.rs`, `textedit.rs` | Drawing model, Direct2D helpers, export; the text box |
| `img.rs` | Image buffer, blur, pixelate |
| `output.rs` | Clipboard (image, file, text), encoding, save dialog |
| `pin.rs` | Pinned images (plain GDI) |
| `ocr.rs` | Windows OCR and its result window |
| `record.rs` | Video recording (Desktop Duplication, Direct2D, Media Foundation H.264) |
| `recui.rs`, `ink.rs`, `layered.rs` | Recording bar and frame, drawing on recordings, layered window helper |
| `settings.rs` | Settings window (Direct2D, its own process) |
| `config.rs`, `keys.rs`, `i18n.rs` | `wispshot.ini`, the shortcut table, interface text |
| `logo.rs` | The logo, drawn in code for the tray icon and Settings |
</details>

## Known limitations

- No scrolling capture or capture history yet.
- You can move, delete and edit text drawings, but not yet change the color or width of other drawings.
- Recordings have no audio and cover a single monitor (an area is clipped to its monitor).
- While recording, the GPU driver can take around 180 MB and part of a CPU core on busy content. It all goes away
  when the recording ends.
- Windows only. Capture and windowing are written straight against Win32.

## License

Copyright (C) 2026 Efe Kaputlu. WispShot is free software under the [GNU General Public License v3.0](LICENSE) or
any later version. You can use, study, change and share it. If you share a changed version, it has to stay under the
same license with its source code available.

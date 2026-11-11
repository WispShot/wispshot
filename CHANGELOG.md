# Changelog

## [Unreleased]

### Added

- Select several drawings in the editor and move or delete them together: `Ctrl` + click adds or removes one,
  `Ctrl` + drag on empty space selects with a rectangle, `Ctrl+Shift+A` selects them all. Undo takes it back in
  one step.
- Delayed screenshot: `Ctrl+Shift+F1` (or the tray menu) counts down 3 seconds, then opens the editor, so you
  can open a menu or tooltip first. Press it again to cancel. The delay (3, 5 or 10 seconds) is under
  *Settings > Capture*.

## [0.2.1] - 2026-11-11

### Fixed

- Shortcuts now step aside for games that go full screen a moment after they start (for example Counter-Strike 2).
  Before, the capture key could still reach WispShot and pull the game out of full screen. WispShot now ignores a
  shortcut pressed while a full-screen app is in front.

## [0.2.0] - 2026-11-10

First public release.

- Region, window and element capture on a global shortcut, with a magnifier and color picker
- Annotations: rectangle, ellipse, arrow, line, pen, highlighter, text, numbered steps, blur and pixelate;
  move, delete and re-edit them; unlimited undo
- Copy, copy as a file, save (PNG, JPG, BMP), pin to screen, OCR with the engine built into Windows
- Area recording to MP4 (H.264, hardware encoder) and drawing on a recording while it runs
- Every shortcut can be changed; English and Turkish interface; light and dark theme
- Shortcuts step aside while a full-screen app is in front
- Installer (per-user, no admin rights) and portable zip, x64

The files aren't code-signed yet; see the README for how to check a download.

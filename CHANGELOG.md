# Changelog

## [Unreleased]

### Changed

- Settings > Shortcuts: a row of buttons under the title jumps to each section (Programs, Global, Editing, Video,
  Pinned), so the list no longer has to be scrolled through. The groups have clearer names (*Editing a
  screenshot*, *Drawing on a video recording*), and *Turn shortcuts off in some programs* says plainly which
  shortcuts are off while each program is in front. With several programs, each folds into one line that lists
  them and opens with a click.
- Settings > Screenshots: *Upload to the internet* and *Add a frame* (was *Share frame*) list their choices one
  under the other, each with a line on what it does; the frame choices show a small picture of the result.

### Fixed

- Text recognition also looks at your keyboard layouts. Many work PCs run an English Windows with a Turkish
  keyboard added under English; Turkish wasn't in the language list there, so text was read with the English
  engine ("Günaydln", "Mü9teri"). Now the Turkish engine reads it, or, if Windows has no Turkish text recognition
  pack, a notice says how to add it.
- A notice opens only an upload link or a file that exists, whatever another program sends the tray.

## [0.4.1] - 2026-11-12

### Added

- *Take screenshots with Print Screen* (Settings > Shortcuts): Print Screen opens WispShot instead of the Windows
  screen capture. WispShot turns off that Windows setting for you (it applies at once), and turning the switch off
  again, or uninstalling, gives the key back.
- Updates: once a day WispShot asks GitHub whether a new version is out and says so from the tray. One click
  downloads the setup, checks it against the release's SHA-256 list, installs it silently and starts WispShot
  again; your settings stay. The portable copy opens the release page. Turn it off under *Settings > General*
  (`[general] check_updates`). The check runs in a short-lived process, so the tray app stays as small as before.
  Coming from 0.4.0 this is a one-time manual install, after that it updates itself. *Settings > About* has a
  *Check now* button.
- Programs that keep their own keys (*Settings > Shortcuts*): while one of them is in front, the shortcuts you
  choose for it go to that program (SAP uses F1, F3 and Shift+F1, which are also WispShot's defaults), and they
  come back as soon as you switch away. Switch the others off and they keep working there, so you can still
  take a screenshot in SAP with, say, `Ctrl+Shift+F1`. *Add program* lists the open programs by name and icon, or
  finds the program file. In the ini: `[hotkeys] pause_in_apps = C:\...\saplogon.exe | capture record, excel.exe`.

### Fixed

- Settings: a description next to a wide row of options (*Delete old captures*, *Upload*) was cut off; the
  options now sit under it.
- On a new install `wispshot.ini` stayed empty until a setting was changed in the window (since 0.4.0).

### Changed

- The tray menu says *Record a video* instead of *Record an area*.
- Text recognition (OCR) reads Turkish and other languages with extra letters much better. It used to read with
  only the first language in your Windows list, so Turkish text on an English Windows came out garbled. Now it
  reads with your other Latin-script language first and switches to English when the text turns out to be
  English. Small screen text is enlarged smoothly before reading, which helps most at 8 pt and below. To pick one
  language yourself, set `[capture] ocr_language` (for example `tr` or `en-US`).
  When Windows has no text recognition pack for your language, a notice says so once; clicking it opens the
  language settings where you can download it.

## [0.4.0] - 2026-11-12

### Added

- Upload (`Ctrl+U` or the new button in the editor): the capture goes to an image host that needs no account or
  key, and the link is copied to the clipboard. Clicking the notice opens it. Choose the host under
  *Settings > Screenshots*: Uguu (deleted after 3 hours; run by Pomf AB in Sweden, which keeps no download logs
  and forgets the uploader's IP when the file expires) or Litterbox (1 or 3 days). `[upload] host = custom` with
  a `url` sends it to your own server instead. Nothing is sent until you press Upload, and the first upload asks
  first, while the editor is still open (No takes you back to it). Uses WinHTTP, which is part of Windows.

### Changed

- Editing `wispshot.ini` by hand applies as soon as you save it; no restart needed.

## [0.3.0] - 2026-11-11

### Added

- Select several drawings in the editor and move or delete them together: `Ctrl` + click adds or removes one,
  `Ctrl` + drag on empty space selects with a rectangle, `Ctrl+Shift+A` selects them all. Undo takes it back in
  one step.
- Delayed screenshot: `Ctrl+Shift+F1` (or the tray menu) counts down 3 seconds, then opens the editor, so you
  can open a menu or tooltip first. Press it again to cancel. The delay (3, 5 or 10 seconds) is under
  *Settings > Capture*.
- Choose the file format: PNG, JPG, BMP or TIFF for screenshots (*Settings > Screenshots*), MP4 or GIF for
  recordings (*Settings > Video*). The save dialog starts with the chosen format. A GIF is made from the recording
  when it ends, with one shared palette, only the changed part of each frame stored, and frames that don't
  change merged. JPG quality and GIF frame rate and width can be set in `wispshot.ini`. Everything uses the
  encoders built into Windows; nothing extra is bundled.
- Scrolling capture (`Ctrl+L` or the new button in the editor): scroll a long page inside the selected area and
  the frames are stitched into one tall image, copied to the clipboard and pinned. *Auto* scrolls by itself and
  stops at the end of the page. A sticky header or footer appears once. `Enter` finishes, `Esc` cancels.
- A pin larger than the screen opens shrunk to fit.
- Capture history, off by default (*Settings > Capture*, or *Capture history* in the tray menu). Finished
  captures are kept in `%LOCALAPPDATA%\WispShot\history` (the last 100, at most 500 MB, both adjustable in
  `wispshot.ini`). Windows OCR reads the text in each one in the background, so the history window can search
  inside the pictures. Click copies, double-click pins, right-click offers copy as file, show in folder and
  delete. Nothing is kept while the history is off. *Delete old captures* removes the ones older than a day, a
  week, a month or three months, also when the history has been turned off since.
- Hide sensitive info (`Ctrl+B` or the shield button in the editor): Windows OCR reads the selection and every
  e-mail address, phone number, IBAN, card number, Turkish ID number (T.C. kimlik no) and API key or token is
  blurred, as one undo step. IBANs, cards and ID numbers must pass their checksum, so amounts, dates and order
  numbers stay as they are.
- Share frame (*Settings > Screenshots*): copied and saved screenshots get a margin, rounded corners and a soft
  shadow on a color, light or dark background. `F` in the editor switches it for one capture.
- A *Support WispShot* card with a Sponsor button on *Settings > About*, and *Support WispShot* in the tray menu.
- Ruler tool (`U`): a line labelled with its length in pixels (and width × height when it is diagonal).

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

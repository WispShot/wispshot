# Contributing

Bug reports, ideas and pull requests are welcome. Please open an issue first for anything larger than a small fix,
so we can agree on the approach before you spend time on it.

## The one rule

WispShot has to stay close to zero cost while it waits: one process, about 2 MB of memory, no CPU, no polling.
A feature that needs a background thread, a timer, a hook or a heavy library in the tray process won't be merged.
Heavy work belongs in the short-lived session process (see *How it stays light* in the README).

## Building and checking

See *Building from source* in the README. Before sending a pull request:

```sh
cargo fmt --check
cargo clippy --release -- -D warnings
./build.sh
target/release/wispshot.exe --selftest selftest   # then read selftest/selftest.txt
```

The PowerShell helpers in `tools/test/` drive the mouse and keyboard and take screenshots, which helps when testing
the editor by hand or from a script.

## Interface text

All text lives in `lang/en.txt` and `lang/tr.txt`. Add every new key to both files. Keep the wording plain and
short, the way a person would say it.

By contributing you agree that your contribution is licensed under GPL-3.0-or-later, like the rest of the project.

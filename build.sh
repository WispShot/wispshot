#!/bin/sh
# Build helper: puts Rust + MinGW-w64 (dlltool/as/windres) on PATH, then builds release.
# Uses MinGW from PATH, or $MINGW, or a WinLibs install from WinGet.
if ! command -v dlltool >/dev/null 2>&1; then
  MINGW="${MINGW:-$(dirname "$(ls -d "$LOCALAPPDATA"/Microsoft/WinGet/Packages/BrechtSanders.WinLibs*/mingw64/bin/dlltool.exe 2>/dev/null | head -1)" 2>/dev/null)}"
fi
export PATH="$HOME/.cargo/bin:${MINGW:+$MINGW:}$PATH"
cd "$(dirname "$0")" && cargo build --release "$@"

# Packages a built wispshot.exe into dist/:
#   WispShot-<ver>-portable-x64.zip   exe + LICENSE + notices (settings are kept next to the exe)
#   WispShot-<ver>-setup-x64.exe      Inno Setup installer (needs ISCC.exe from Inno Setup 6)
#   SHA256SUMS.txt
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools/package.ps1 [-Exe target/release/wispshot.exe]
param([string]$Exe = "target/release/wispshot.exe", [string]$Iscc = "")
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

$ver = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
$rc = Get-Content assets/wispshot.rc -Raw
if ($rc -notmatch [regex]::Escape("`"FileVersion`", `"$ver`"")) { throw "assets/wispshot.rc version does not match Cargo.toml ($ver)" }
if (-not (Test-Path $Exe)) { throw "$Exe not found; build first (./build.sh)" }

$dist = "dist"
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory $dist | Out-Null

# portable zip
$stage = Join-Path $dist "WispShot"
New-Item -ItemType Directory $stage | Out-Null
Copy-Item $Exe "$stage/wispshot.exe"
Copy-Item LICENSE "$stage/LICENSE.txt"
Copy-Item THIRD-PARTY-NOTICES.txt $stage
Compress-Archive -Path "$stage/*" -DestinationPath "$dist/WispShot-$ver-portable-x64.zip"

# installer
if (-not $Iscc) {
  $Iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
            "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $Iscc) { throw "ISCC.exe (Inno Setup 6) not found" }
$src = (Resolve-Path (Split-Path $Exe -Parent)).Path
& $Iscc /Q "/DAppVersion=$ver" "/DSrcDir=$src" installer/wispshot.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
Remove-Item $stage -Recurse -Force

# checksums (LF line endings, so `sha256sum -c` works everywhere)
$sums = Get-ChildItem $dist -File | Where-Object Name -ne "SHA256SUMS.txt" | ForEach-Object {
  "{0}  {1}`n" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name
}
[IO.File]::WriteAllText((Join-Path (Resolve-Path $dist) "SHA256SUMS.txt"), -join $sums)
Get-ChildItem $dist | Select-Object Name, Length

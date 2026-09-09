# Build the release programs and assemble dist\windows\ (FFLocal.exe = the launcher,
# double-click it; ffl-app.exe = the program it starts; no console windows, the log is in
# %APPDATA%\fflocal\fflocal.log). Needs a Rust toolchain (https://rustup.rs) with the MSVC
# build tools. The games are not needed to build.
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
cargo build --release -p ffl-app -p ffl-launcher
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
New-Item -ItemType Directory -Force -Path dist\windows | Out-Null
Copy-Item target\release\fflocal.exe dist\windows\FFLocal.exe -Force
Copy-Item target\release\ffl-app.exe dist\windows\ffl-app.exe -Force
Copy-Item packaging\icon.png dist\windows\icon.png -Force
Write-Host "built dist\windows\FFLocal.exe (launcher) and ffl-app.exe"

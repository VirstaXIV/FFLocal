@echo off
rem Start FFLocal from this source checkout: builds the small launcher if needed, which
rem then builds and starts the program itself. Double-click this file.
cd /d "%~dp0"
cargo run --release -p ffl-launcher
if errorlevel 1 pause

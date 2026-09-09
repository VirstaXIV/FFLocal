#!/usr/bin/env sh
# Start FFLocal from this source checkout without a terminal: builds the small launcher if
# needed (a minute or two the first time), which then builds and starts the program itself.
# Double-click it in a file manager ("Run"), or `./FFLocal.sh`.
cd "$(dirname "$0")" || exit 1
exec cargo run --release -p ffl-launcher

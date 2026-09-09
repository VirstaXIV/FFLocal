# FFLocal

FFLocal lets you walk around Final Fantasy XIV and Final Fantasy XI zones with your own
character, using the game files already on your computer. It doesn't copy, edit, or upload
anything — it just reads the game the same way the game itself does.

It's built in Rust, using [Bevy](https://bevy.org) for rendering,
[Physis](https://github.com/redstrate/Physis) for reading FFXIV's file formats, and
[Avian](https://github.com/Jondolf/avian) for collision.

## Getting started

You need:
- Final Fantasy XIV and/or Final Fantasy XI installed.
- A Rust toolchain, installed once from [rustup.rs](https://rustup.rs). FFLocal builds itself
  the first time it runs, so this is the only extra thing to install.

Then:
1. Get the code — either `git clone https://github.com/VirstaXIV/FFLocal`, or, without git,
   download and unzip the [source code](https://github.com/VirstaXIV/FFLocal/tags) (the "Source
   code (zip)" link on any release, or the tip of `main`).
2. Double-click `FFLocal.sh` (Linux/macOS) or `FFLocal.cmd` (Windows), in the folder you just
   downloaded.

That's the whole install — no terminal, no separate installer. The first launch takes a minute
or two while it builds itself; every launch after that is fast. FFLocal also checks for updates
on its own, and can download and apply them for you.

## What it does

- Walk around **FFXIV zones** with your own character — the same look and gear as your actual
  game saves. Collision, jumping, sprinting, flying and swimming all work.
- Walk around **FFXI zones** too, with a character you build in FFLocal (race, face, hair, gear).
- Mix the two: an FFXIV character can walk through an FFXI zone, and the other way round.
- Characters can play emotes and idle poses, and draw or sheathe their weapon.
- Mods (Penumbra packs, for FFXIV) are supported and can be turned on or off per character.

## Using it

Starting FFLocal opens a small **launcher window** first. It shows whether your FFXIV/FFXI
installs were found (and lets you point it at the right folder, or turn a game off entirely if
you don't want FFLocal touching it), has basic graphics and volume settings, and builds +
starts the main program when you press **Launch**.

The main program opens into a menu: pick a character on the left, pick somewhere to go on the
right, then press Enter. In the world: right mouse button to look around, WASD to move, Shift
to sprint, Space to jump, Z to draw/sheathe your weapon, F to fly, Esc to go back to the menu.

## Your data stays yours

FFLocal only reads your game install; it never copies, edits or uploads anything from it. Your
characters and settings live on your own computer:
- Linux: `~/.local/share/fflocal`
- Windows: `%APPDATA%\fflocal`
- macOS: `~/Library/Application Support/fflocal`

Mods you add yourself can be freely shared with other people, but anything read from the actual
game files always stays local — FFLocal keeps track of which is which, so that rule can be
enforced automatically once sharing a world with someone else is possible.

## For developers

| Crate | Role |
|---|---|
| `crates/ffl-core` | Engine-agnostic data types: scenes, models, materials, skeletons, clips, presets, sound. |
| `engines/ff14/assets` | FFXIV file reading: the game locator, loaders, Excel sheets, zone graph. |
| `engines/ff14/chara` | FFXIV character data: race/appearance tables, gear resolution. |
| `engines/ff14/engine` | The FFXIV `Engine` implementation: maps, scenes, characters, animation, presets, mods. |
| `engines/ff11` | The FFXI engine: zones, characters, gear, emotes, all read from the installed DAT files. |
| `crates/ffl-hub` | The menu (hub) UI model, with no dependency on the renderer. |
| `crates/ffl-app` | The Bevy runtime: rendering, streaming, the player controller, animation. |
| `crates/ffl-cli` | Command-line inspection tools for every file format FFLocal reads. |
| `crates/ffl-launcher` | The small pre-start window: game setup, basic settings, build-and-launch, self-update. |
| `vendor/physis` | A patched copy of Physis (see `patches/`) for a few fixes FFLocal needed upstream. |

Useful commands:
```sh
cargo run -p ffl-app --features dev                    # run the main program directly (faster rebuilds)
cargo run -p ffl-app --features dev -- --zone 339       # jump straight into a zone (Mist)
cargo run -p ffl-cli -- info                            # check FFLocal can find your game
cargo run -p ffl-cli -- resolve-character                # list every file your character needs
```

Game and mod paths can be set with environment variables or a `config.toml` in the project
root — see `engines/ff14/assets/src/locate.rs` and `engines/ff11/src/dat.rs` for the exact
search order, and `engines/ff14/assets/src/mods.rs` for mod packs.

## License

GPL-3.0-or-later (Physis is GPL-3 too). FFXIV/FFXI game data belongs to Square Enix and is
never redistributed.

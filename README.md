# FFLocal

A standalone engine that renders Final Fantasy XIV zones and a controllable player character
**straight from the installed game**. Game files are read in place from the `sqpack` archives;
nothing is copied, extracted or modified.

Built with Rust: [Bevy](https://bevy.org) 0.19 for rendering/input/ECS,
[Physis](https://github.com/redstrate/Physis) 0.6 (vendored with small patches, see `patches/`)
for the game formats, and [Avian](https://github.com/Jondolf/avian) 0.7 for collision.

## What works

- Zone loading from `TerritoryType` → `.lvb` → `.lgb` layers (BG parts, nested shared groups) and
  terrain plates, with diffuse textures and alpha masking. Mist (339) loads in about a second.
- Trimesh collision from the zone geometry and a kinematic player controller (move-and-slide,
  gravity, jump, slopes, stairs, flight) with a third-person orbit camera; zones start at
  their aetheryte or player spawn ranges.
- The player's own character from the vanilla saves: `FFXIV_CHARA_xx.dat` (look) and
  `FFXIV_CHR*/GEARSET.DAT` (equipment). Race fallbacks via EQDP, IMC material variants, racial
  bone deformers (PBD), skin/hair/eye/lip colours from `human.cmp`, skinning to the base skeleton,
  and idle/walk/run/jump animations decoded from `.pap` Havok data.
- Per-engine shaders hosted side by side: the FF14 engine ships WGSL for background, gear
  (colour tables), skin, hair and iris materials following the Dawntrail channel layouts.
- Actions: an engine-agnostic list of idle poses and emotes per character, played from the
  in-world "Actions" window (FF14: the `Emote` sheet and `poseNN_loop` idles).
- Addons: weapons (and later other attachments) are addons of the character with their own
  toggles and rules (weapons hide during emotes); `--addon weapon:main=off` at start.
- FFXI zones from the installed game's DAT files (all 293 named zones): `--map ff11:100` is
  West Ronfaure, `ff11:230` Southern San d'Oria.
- FFXI player characters from the same DATs: race, face/hair and one model per gear slot
  (head, body, hands, legs, feet, main, sub, ranged) with the client's model-id tables, idle,
  walk, run and jump motions, the emotes, weapons drawn and sheathed with the game's own
  motions and sounds (Z), the battle stance and engaged run, footsteps from the game's step
  bank per race and ground. An FFXI character walks in FFXIV zones and the FFXIV character in
  FFXI zones.

## Architecture

FFLocal is engine-agnostic: an *engine* is an asset provider for one game or content source, and
everything the runtime renders goes through the plain data types in `ffl-core` (scenes, models,
materials by shader class, skeletons, clips, presets). Engines never touch Bevy; the runtime never
touches game formats. Characters and maps from different engines can therefore be mixed in one
world (an FFXI character in an FFXIV zone, or either in a Unity/Unreal export, once those engines
exist). Engines declare a content policy: game archives are local-only, mod files may be shared.

Shading is per engine too: each engine registers its own WGSL shader functions and texture-slot
layouts, and the runtime hosts all of them in one program. An FFXI character and an FFXIV
character on the same map are each drawn with their own game's shaders.

## Installing (no terminal needed afterwards)

Get the source — either `git clone https://github.com/VirstaXIV/FFLocal` or, without git,
download and unzip a [release](https://github.com/VirstaXIV/FFLocal/tags) (any `vX.Y.Z` tag's
"Source code (zip)" link) or the tip of `main`. Then build once with a Rust toolchain
([rustup](https://rustup.rs)); nothing about the games is needed to build, and the result is a
normal double-clickable program:

| OS | Build | Result |
|---|---|---|
| Linux | `tools/package.sh` then `tools/install-desktop.sh` | "FFLocal" in the application menu (`~/.local/bin/fflocal` + `ffl-app`, `fflocal.desktop`) |
| Windows | `tools\package.ps1` in PowerShell | `dist\windows\FFLocal.exe` — double-click it (no console window) |
| macOS | `tools/package.sh` | `dist/macos/FFLocal.app` — open it or drag it to Applications |

In a source checkout, `FFLocal.sh` (Linux/macOS) or `FFLocal.cmd` (Windows) at the top level
is the double-clickable start: it builds the small launcher if needed and opens it.

### The launcher

What opens first is a small window, the **launcher** (`crates/ffl-launcher`, the `fflocal`
program): it shows which games were found, lets you point at an install (type, **Browse…**
or **Scan**), switch a game off entirely, set the basic graphics options (fullscreen, VSync,
anti-aliasing, shadows, distances, UI scale) and the master volume, and then **Launch**
starts the program (`ffl-app`). From a source checkout the launcher builds the program first
and says so — the first build takes minutes, later ones seconds when nothing changed; an
installed package starts at once. Everything the launcher sets is written to the same
`settings.toml` the program reads, so the in-app **Games** and **Settings** windows show the
same state. **Save** writes without starting; **Build** rebuilds without starting.

In a source checkout, the launcher also checks `github.com/VirstaXIV/FFLocal`'s tags on
startup; if a newer `vX.Y.Z` release exists it shows **Update available: vX.Y.Z** with a
**Download & apply** button that downloads that tag's source, overlays it onto the checkout
(local settings and build output are left alone) and rebuilds — no separate download step.
Set `FFL_NO_UPDATE_CHECK=1` to skip the check entirely (e.g. offline).

FFLocal reads the games in place and copies nothing: the program is about 130 MB, the
launcher a few MB, and the data folder (presets, settings, log) a few KB. A source checkout
also grows a `target/` build cache of a few GB, which `cargo clean` removes at any time.

If no game was found the program still starts, with its **Games** window open; point it at
the games and enter. Every run writes `fflocal.log` into the data directory next to the
presets, so a start from the desktop leaves a log to read. `cargo run -p ffl-app --features
dev` stays the developer way to run the program directly (dynamic linking, fast rebuilds).

## Games and installs

Each engine reads one installed game in place. The **Games** window (top bar) shows what each
engine found and lets you change it: type or browse to the install folder (the one holding
`game/` for FFXIV, `VTABLE.DAT` for FFXI), press **Use**, and the engine reopens on the spot —
worlds, presets and the preview refresh. **Scan** lists the installs found in the usual places:
XIVLauncher's config (Linux/macOS `launcher.ini`, Windows `launcherConfigV3.json`), every Steam
library on the machine, the default install folders (`Program Files (x86)\SquareEnix\…`,
`PlayOnline\…`, XIV on Mac). **Auto** forgets the path and searches again. Choices are saved in
`settings.toml` under `[games.<engine>]`; `--game-path` / `--ff11-path` override them for one run.
When no game is found the window opens by itself.

**Switching a game off** (the checkbox in front of its name, in the launcher or in Games)
closes its engine: its characters, worlds, gear catalogs and sounds disappear until it is on
again, exactly as if that game were not installed. The presets stay on disk. The switch is only
offered in the hub, never inside a world, so it takes effect at once without a restart; a
character of the switched-off game that was selected is deselected.

Nothing about a game is compiled into FFLocal: `cargo build --release` on Linux, Windows or
macOS produces the same program, and the games are only read at run time. Data lives in the
platform's data directory (`~/.local/share/fflocal`, `%APPDATA%\fflocal`,
`~/Library/Application Support/fflocal`).

## Running

```sh
cargo run -p ffl-app --features dev                          # launcher: pick a character and a map
cargo run -p ffl-app --features dev -- --zone 339            # straight into Mist with the last preset
cargo run -p ffl-app --features dev -- --map ff14:129        # any engine:map id
cargo run -p ffl-app --features dev -- --mdl <game path>     # inspect one FF14 model
cargo run -p ffl-cli -- info                                 # locate the game
cargo run -p ffl-cli -- resolve-character                    # list every file the character needs
```

The hub fills the screen: characters on the left, the selected character rendered live in the
middle (drag to turn, wheel to zoom), worlds on the right (engine tabs, category chips, search,
recent worlds) and the enter bar at the bottom. A character is an FFLocal configuration saved
to `~/.local/share/fflocal/presets.toml`: "Import from game" fills an FFXIV one from a
character-creator save and a gear set, "New…" starts one for any engine, and the editor (a side
panel with one tab per field group) changes race, clan, face, hairstyle, colours (palettes from
the game's own tables) and every gear slot through searchable item lists — every change
reloads the preview at once; Save keeps it, Cancel drops it. "Gear" opens the editor straight
on the gear slots. FFXI characters use the same editor (race, face and hair, gear model ids
named from the community lists; the game stores no character locally, so there is nothing to
import). Double-click a world or press Enter world. In the world: RMB look, WASD move, Shift sprint,
`/` walk, Space jump, Z draw/sheathe the weapon (emotes sheathe it automatically, stances and
victory poses keep it drawn), F fly. Esc (or the "Menu" button in the overlay) returns to the
hub to pick another world or character. "Settings" (in the hub and in the overlay) holds the
graphics options: fullscreen, vsync, MSAA, shadows, render and detail distance, anisotropic
filtering, UI scale; they are saved to `~/.local/share/fflocal/settings.toml`.

## Mods and protected content

Vanilla files read from a game install are *protected*: they stay on this machine. Mod packs
(the modder's own files) are *shareable*. FFLocal keeps track per asset: the in-world overlay
shows whether a character or world uses protected game assets or only mods, which is what a
future shared world will check before sending anything to other players.

Mod packs go under `~/.local/share/fflocal/mods/ff14/<pack>/` (or `ff14_mods` in
`config.toml`), either in Penumbra's layout (`meta.json`, `default_mod.json`, option groups)
or as a plain mirror of game paths. Enable packs globally in Settings → Mods, or per character
in its editor; a pack can replace anything from one texture to the whole character.

Game path precedence: `--game-path`, `FFLOCAL_GAME_PATH`, `config.toml` (`game_path = ...`),
XIVLauncher.Core's `~/.xlcore/launcher.ini`, then Physis' default search. FFXI: `--ff11-path`,
`FFLOCAL_FF11_PATH`, `config.toml` (`ff11_path = ...`), then the Steam libraries.

Controls: right mouse to look, WASD to move (Shift sprints, `/` toggles walking), Space jumps, Z sheathes, F toggles character
flight (Space/Ctrl for height), F1 toggles the fly camera
(QE/Space/Ctrl for height, wheel for speed), F3 cycles face culling, F12 saves a screenshot.
`--screenshot out.png --settle 5` renders, captures and exits (used for verification);
`--autowalk N`, `--anim <clip>` and `--action "<emote or idle pose>"` drive the player without
input, `--no-normal-maps` and `--no-collision` trade quality for speed.

## Layout

| Crate | Role |
|---|---|
| `crates/ffl-core` | Engine trait and plain data: scenes, models, materials (shader classes), skeletons, clips with timeline events, presets, content policy, sound (decoded clips, music sets, ambient emitters, footstep/voice banks, game clocks). |
| `engines/ff14/assets` | FFXIV sqpack source, game locator, typed loaders, Excel with EXDSchema names, zone graph. |
| `engines/ff14/chara` | FFXIV vanilla appearance sources, race codes/paths, EQDP/IMC/CMP/EST parsers, character resolver. |
| `engines/ff14/engine` | The FFXIV `Engine`: maps with place names, scenes, models, characters, animation clips, presets. The modding overlay belongs here. |
| `engines/ff11` | FFXI engine: zones from the installed DATs (MZB/MMB/IMG), zone shader, player characters (skeleton/mesh/motion DATs, gear tables), emotes. |
| `crates/ffl-hub` | The hub (menu) model and its egui views plus the settings; no Bevy dependency, so a different runtime can reuse it. |
| `crates/ffl-app` | Bevy runtime: draws the hub, material shaders per class, zone streaming with block-compressed textures and size-based culling, player controller, character rig and animation. |
| `crates/ffl-cli` | Inspection commands for every stage (`info`, `excel`, `zone`, `mdl`, `mtrl`, `tex`, `chara`, `gearset`, `resolve-character`, `pap`, `ff11-zone`, `ff11-pc`, ...). |
| `vendor/physis` | Physis 0.6.0 plus `patches/physis-0.6.0-fflocal.patch` (bone tables, race codes, Havok exposure, neck-morph table, POLAR32 decode fix, public sampler fields). |

## License

GPL-3.0-or-later (Physis is GPL-3). FFXIV data belongs to Square Enix and is never redistributed.

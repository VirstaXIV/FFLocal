//! ffl-cli: inspect the installed game through the same code paths the engine uses.

mod chara;

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use ffl_ff14_assets::loaders::{load_lgb, load_lvb, load_mdl, load_mtrl, load_sgb, load_tex};
use ffl_ff14_assets::zone::{PlacedKind, build_zone, build_zone_from_bg};
use ffl_ff14_assets::{AssetSource, GameData};
use physis::layer::{Layer, LayerEntryData};

#[derive(Parser)]
#[command(name = "ffl-cli", version, about = "FFLocal game-data inspector")]
struct Cli {
    /// FFXIV install root (or its game/ directory). Overrides env/config/launcher.ini.
    #[arg(long, global = true)]
    game_path: Option<PathBuf>,

    /// Resolve every path through a Penumbra collection (id or name) first, the way the engine
    /// does for a character that selects it.
    #[arg(long, global = true)]
    collection: Option<String>,

    /// FFLocal mod packs (ids under the mods directory) layered over the game.
    #[arg(long, global = true)]
    mods: Vec<String>,

    /// Dalamud plugin-config directory (default: the usual places).
    #[arg(long, global = true)]
    plugins: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the located game and its repositories.
    Info,
    /// Check whether a game path exists.
    Exists { path: String },
    /// Dump an Excel row. With --schema, label columns by EXDSchema field names.
    Excel {
        sheet: String,
        row: u32,
        #[arg(long)]
        schema: bool,
    },
    /// Dump a level (`.lvb`) file: bg path, LGB list, layer sets.
    Lvb { path: String },
    /// Dump a layer group (`.lgb`): per-layer entry counts and BG asset paths.
    Lgb {
        path: String,
        /// Print every BG asset path (not only counts).
        #[arg(long)]
        assets: bool,
    },
    /// Dump a shared group (`.sgb`).
    Sgb { path: String },
    /// Build the zone graph for a TerritoryType row and print statistics.
    Zone {
        territory: u32,
        /// Use this TerritoryType.Bg value instead of reading the sheet.
        #[arg(long)]
        bg: Option<String>,
        /// List unique model paths.
        #[arg(long)]
        models: bool,
        /// Print world positions of placed models whose path contains this text.
        #[arg(long)]
        find: Option<String>,
        /// List placed sounds, env sets and map ranges with music.
        #[arg(long)]
        sounds: bool,
    },
    /// Dump a model (`.mdl`): LODs, parts, vertex/index counts, materials, bones.
    Mdl {
        path: String,
        /// Print the vertices of every part whose uv0 lies within `r` of `u,v`.
        #[arg(long)]
        near_uv: Option<String>,
        /// Print the vertices within `r` of `x,y,z` on each given axis (`nan` = any).
        #[arg(long)]
        near_pos: Option<String>,
    },
    /// Test which candidate paths exist in a sqpack index by hash: `--folders` lists folder
    /// paths to test, `--files` file names tested inside `--in-folder` (research aid for
    /// paths that cannot be listed).
    HashProbe {
        /// Category index, e.g. `sqpack/ffxiv/040000.win32.index` relative to the game dir.
        index: String,
        #[arg(long)]
        folders: Option<PathBuf>,
        #[arg(long)]
        files: Option<PathBuf>,
        #[arg(long)]
        in_folder: Option<String>,
    },
    /// Dump a material (`.mtrl`): shader, textures, samplers, keys, color table summary.
    Mtrl {
        path: String,
        /// Stain ids of the two dye channels (`a,b`): prints the dyeable rows after dyeing.
        #[arg(long)]
        dyes: Option<String>,
    },
    /// Dump a texture (`.tex`) header; optionally write mip 0 as raw RGBA8 for inspection.
    Tex {
        path: String,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write a game file's raw bytes to disk (for hex inspection).
    Raw { path: String, out: PathBuf },
    /// Dump a timeline (`.tmb`) or the timelines embedded in a `.pap`: animation, weapon, footstep, voice and sound entries.
    Tmb { path: String },
    /// Resolve a territory's background music chain (TerritoryType.BGM → BGMSwitch/BGMSituation → BGM).
    Bgm { territory: u32 },
    /// Dump a sound container (`.scd`): entries, codecs, loop points; decode one entry to WAV.
    Scd {
        path: String,
        /// Entry slot to decode (default: the first slot that holds audio).
        #[arg(long)]
        entry: Option<usize>,
        /// Decode the entry and write a 16-bit PCM WAV file.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Write the entry's descrambled Ogg Vorbis bytes instead (codec 6 only).
        #[arg(long)]
        ogg: Option<PathBuf>,
    },
    /// Dump a skeleton (`.sklb`): bones with parents and reference pose.
    Sklb { path: String },
    /// Dump racial deform matrices from human.pbd for (from, to) race codes.
    Pbd { from: u16, to: u16 },
    /// Scan this machine for game installs of every engine (what the hub's Games window finds).
    Scan,
    /// The colours `human.cmp` gives a preset's customize values (skin, hair, highlights, eyes,
    /// lips, facial features, face paint), as the character shaders receive them.
    Colors {
        /// Preset name or id from the presets library.
        preset: String,
    },
    /// Copy a preset's modded files into its archive (`<data dir>/characters/ff14/<preset id>/`
    /// or `--out`), the way the hub's "Keep a modded copy" does; `--collection` enables a
    /// Penumbra collection (id or name) for the resolution, `--mods` FFLocal packs.
    ArchiveCharacter {
        /// Preset name or id from the presets library.
        preset: String,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Dalamud plugin data: the plugin-config directory found, Penumbra's mod directory and
    /// collections, Glamourer's designs. `--collection <id or name>` resolves a collection to
    /// its file replacements; `--design <id or name>` prints what a design applies.
    Dalamud {
        #[arg(long)]
        design: Option<String>,
        /// Print every replaced game path of the collection.
        #[arg(long)]
        files: bool,
    },
    /// Dump a race's weapon attach-offset table (`chara/xls/attachOffset/c{race}.atch`): every
    /// attach type with its entries (0 in hand, 1 sheathed, ...) and their bones.
    Atch {
        #[arg(default_value_t = 801)]
        race: u16,
        /// Only types whose entries name this bone (e.g. `n_buki_l`).
        #[arg(long)]
        bone: Option<String>,
    },
    /// Parse an animation package (`.pap`) and sample its Havok animations at t=0.
    Pap {
        path: String,
        /// Print all tracks of this binding index at time `--at`, with bone names from the race's skeleton.
        #[arg(long)]
        binding: Option<usize>,
        #[arg(long, default_value_t = 0.3)]
        at: f32,
        #[arg(long, default_value_t = 801)]
        race: u16,
    },
    /// Print rows of a sheet whose string fields contain `pattern`.
    ExcelScan {
        sheet: String,
        pattern: String,
        #[arg(long, default_value_t = 100000)]
        max_rows: u32,
    },
    #[command(flatten)]
    Chara(chara::CharaCommand),
    /// FFXI: parse a zone DAT and dump its textures as raw RGBA files (`<out>/<name>.<w>x<h>.rgba`).
    Ff11Zone {
        zone: u16,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Print mesh details (texture, blend, uv/colour ranges) of models whose name contains this.
        #[arg(long)]
        model: Option<String>,
        /// List the zone's SeSep sound ids (grouped by se folder, with the file each resolves
        /// to) and its LandSandBoat music row instead of the mesh/texture report.
        #[arg(long)]
        sounds: bool,
        /// Report the MZB collision blocks (counts, triangles, bounds against the render meshes).
        #[arg(long)]
        collision: bool,
        /// Write the DAT with its MZB/MMB sections decrypted to this path (format research).
        #[arg(long)]
        dump: Option<PathBuf>,
        #[arg(long)]
        ff11_path: Option<PathBuf>,
    },
    /// FFXI: header, codec, duration and loop of a music (`bgw`) or effect (`spw`) file by id;
    /// `--out` decodes it to a WAV.
    Ff11Sound {
        /// `bgw` (music) or `spw` (sound effect).
        kind: String,
        id: u32,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        ff11_path: Option<PathBuf>,
    },
    /// FFXI: compose a player character (race, face, gear model ids) and report its
    /// skeleton, parts, clips and emotes; `--dat <file id>` dumps one entity DAT instead.
    Ff11Pc {
        /// Look race 1..8 (1 Hume ♂, 2 Hume ♀, 3 Elvaan ♂, 4 Elvaan ♀, 5/6 Tarutaru, 7 Mithra, 8 Galka).
        #[arg(long, default_value_t = 1)]
        race: u8,
        #[arg(long, default_value_t = 0)]
        face: u16,
        /// Gear model ids as `slot=model` (head, body, hands, legs, feet, main, sub, range).
        #[arg(long = "gear")]
        gear: Vec<String>,
        /// Dump one entity DAT by file id (sections, skeleton, meshes, clips, routines).
        #[arg(long)]
        dat: Option<u32>,
        /// Sample this clip of the composed character at `--at` seconds and print the root joints.
        #[arg(long)]
        clip: Option<String>,
        #[arg(long, default_value_t = 0.0)]
        at: f32,
        /// List the gear catalog of a slot (model id, file id, name).
        #[arg(long)]
        list: Option<String>,
        #[arg(long)]
        ff11_path: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let game_path = cli.game_path.clone();
    let (cli_collection, cli_mods, cli_plugins) = (cli.collection.clone(), cli.mods.clone(), cli.plugins.clone());
    let game = std::sync::Arc::new(GameData::open(cli.game_path.as_deref())?);
    // With --collection / --mods every command reads through the engine's layered source.
    let layered = if cli.collection.is_some() || !cli.mods.is_empty() {
        use ffl_core::Engine;
        let engine = ffl_ff14::Ff14Engine::from_game(game.clone(), cli.plugins.as_deref());
        let mut ids = cli.mods.clone();
        if let Some(sel) = &cli.collection {
            let profile = engine
                .mod_profiles()
                .into_iter()
                .find(|pr| &pr.id == sel || pr.id == format!("penumbra:{sel}") || pr.name.eq_ignore_ascii_case(sel))
                .ok_or_else(|| anyhow!("no collection {sel}"))?;
            eprintln!("collection {} ({})", profile.name, profile.detail);
            ids.insert(0, profile.id);
        }
        engine.set_enabled_mods(&ids);
        Some(engine.mod_source())
    } else {
        None
    };
    let source: &dyn AssetSource = match &layered {
        Some(l) => l,
        None => game.source(),
    };

    match cli.command {
        Command::Ff11Sound { kind, id, out, ff11_path } => {
            use ffl_ff11::sound::{self, SoundKind};
            let kind = SoundKind::from_label(&kind).ok_or_else(|| anyhow!("kind must be bgw or spw, not {kind}"))?;
            let root = ffl_ff11::dat::locate(ff11_path.as_deref())?;
            let install = ffl_ff11::dat::Install::open(&root)?;
            let path = install.sound_path(kind, id).ok_or_else(|| anyhow!("no {} {id} under {}/sound*", kind.label(), root.display()))?;
            let bytes = std::fs::read(&path)?;
            let h = sound::header(&bytes)?;
            println!("{} ({} bytes)", path.display(), bytes.len());
            println!(
                "kind {} id {} codec {} ({}) file_size {} block_count {} block_align {} channels {} rate {} Hz data @{:#x}",
                h.kind.label(),
                h.id,
                h.codec,
                h.codec_name(),
                h.file_size,
                h.block_count,
                h.block_align,
                h.channels,
                h.sample_rate,
                h.data_offset
            );
            if kind == SoundKind::Bgw {
                println!("name: {}", ffl_ff11::zone_music::music_name(id as u16).unwrap_or("?"));
            }
            if h.file_size as usize != bytes.len() {
                println!("warning: header file size {} != {} bytes on disk", h.file_size, bytes.len());
            }
            match h.loop_frames() {
                Some((s, e)) => println!("loop: block {} → frames {s}..{e} ({:.3} s → {:.3} s)", h.loop_start, s as f32 / h.sample_rate as f32, e as f32 / h.sample_rate as f32),
                None => println!("loop: none (loop_start {})", h.loop_start),
            }
            if !h.decodable() {
                println!("duration: unknown ({} is not decodable)", h.codec_name());
                if out.is_some() {
                    return Err(anyhow!("{} {id} is {}: cannot decode", kind.label(), h.codec_name()));
                }
            } else {
                println!("duration: {} frames, {:.3} s", h.frames(), h.duration_secs());
                let key = sound::sound_key(kind, id);
                let d = ffl_ff11::sound::parse(&key, &bytes)?;
                let ffl_core::SoundEncoding::Pcm16(pcm) = &d.encoding else { unreachable!() };
                let peak = pcm.iter().map(|s| (*s as i32).abs()).max().unwrap_or(0);
                let rms = (pcm.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / pcm.len().max(1) as f64).sqrt();
                println!("decoded: {} frames, {:.3} s, peak {peak}, rms {rms:.0}, loop {:?}", d.frames, d.duration_secs(), d.loop_range);
                if let Some(out) = out {
                    std::fs::write(&out, ffl_core::wav_bytes(d.sample_rate, d.channels, pcm)).with_context(|| format!("writing {}", out.display()))?;
                    println!("wrote {}", out.display());
                }
            }
        }
        Command::Ff11Pc { race, face, gear, dat, clip, at, list, ff11_path } => {
            use ffl_ff11::pc::{Look, Race, Slot};
            let root = ffl_ff11::dat::locate(ff11_path.as_deref())?;
            let install = ffl_ff11::dat::Install::open(&root)?;
            if let Some(id) = dat {
                let data = install.read(id)?;
                println!("DAT {id}: {} ({} bytes)", install.file_path(id).unwrap().display(), data.len());
                for s in ffl_ff11::dat::sections(&data) {
                    println!("  section {:?} type {:#04x} {} bytes", ffl_ff11::entity::id_str(&s.id), s.kind, s.end - s.start);
                }
                let e = ffl_ff11::entity::parse_entity("dat", &data);
                if let Some(sk) = &e.skeleton {
                    println!("skeleton {}: {} joints, {} references", sk.id, sk.joints.len(), sk.references.len());
                    for (i, j) in sk.joints.iter().enumerate() {
                        println!("  joint {i:3} parent {:?} rot {:?} trans {:?}", j.parent, j.rotation, j.translation);
                    }
                    for (i, r) in sk.references.iter().enumerate().filter(|(i, _)| matches!(i, 0..=3 | 124..=127)) {
                        println!("  ref {i} -> joint {} offset {:?}", r.joint, r.offset);
                    }
                }
                for m in &e.meshes {
                    let joints: std::collections::BTreeSet<u16> = m.vertices.iter().flat_map(|v| [Some(v.joint0), v.joint1]).flatten().collect();
                    println!(
                        "mesh {}: {} vertices ({} two-joint), mirrored {}, normals {}, occludes {:#04x}, {} pieces, joints {:?}",
                        m.id,
                        m.vertices.len(),
                        m.vertices.iter().filter(|v| v.joint1.is_some()).count(),
                        m.mirrored.is_some(),
                        m.has_normals,
                        m.occlude_type,
                        m.pieces.len(),
                        joints
                    );
                    for p in &m.pieces {
                        println!("    piece {} {:?} {} corners tex {:?} display {} mirrored {}", if p.strip { "strip" } else { "list" }, p.triangles().len(), p.corners.len(), p.texture, p.props.display_type, p.mirrored);
                    }
                }
                for t in &e.textures {
                    println!("texture {:?} {}x{} alpha {}", ffl_ff11::zone::name_str(&t.name), t.texture.width, t.texture.height, t.texture.has_alpha);
                }
                for a in &e.animations {
                    let reset = a.tracks.iter().filter(|t| t.reset).count();
                    println!("animation {}: {} frames, kfd {}, {:.2} s, {} tracks ({reset} reset), joints {:?}", a.id, a.frames, a.key_frame_duration, a.duration(), a.tracks.len(), a.tracks.iter().map(|t| t.joint).collect::<Vec<_>>());
                }
                for r in &e.routines {
                    println!("routine {}: {:?} calls {:?}", r.id, r.commands.iter().map(|c| format!("{}@{}", c.clip, c.delay)).collect::<Vec<_>>(), r.calls);
                }
                if let Some(i) = &e.info {
                    println!("info: {i:?}");
                }
                for n in &e.notes {
                    println!("note: {n}");
                }
                return Ok(());
            }
            let race = Race::from_id(race).ok_or_else(|| anyhow!("race must be 1..8"))?;
            if let Some(slot) = list {
                let slot = Slot::from_key(&slot).ok_or_else(|| anyhow!("unknown slot {slot}"))?;
                for (model, file) in ffl_ff11::pc::slot_models(race, slot) {
                    if install.exists(file) {
                        println!("{model:4} {file:6} {}", ffl_ff11::gear_names::gear_name(slot.key(), race as u8, model).unwrap_or("?"));
                    }
                }
                return Ok(());
            }
            let mut look = Look { race, face, ..Default::default() };
            for g in &gear {
                let (k, v) = g.split_once('=').ok_or_else(|| anyhow!("--gear expects slot=model"))?;
                let slot = Slot::from_key(k).ok_or_else(|| anyhow!("unknown slot {k}"))?;
                look.gear.insert(slot, v.parse()?);
            }
            let model = ffl_ff11::character::load_character(&install, &look, "cli")?;
            println!("{} : {} bones, {} parts, {} clips, height {:.2}", model.name, model.skeleton.bones.len(), model.parts.len(), model.clips.len(), model.height);
            for n in &model.notes {
                println!("note: {n}");
            }
            for p in &model.parts {
                let tris: usize = p.model.meshes.iter().map(|m| m.indices.len() / 3).sum();
                let verts: usize = p.model.meshes.iter().map(|m| m.positions.len()).sum();
                println!(
                    "part {:6} {}: {} meshes, {verts} vertices, {tris} triangles, bounds {:?}..{:?}, addon {:?}",
                    p.name,
                    p.model.key,
                    p.model.meshes.len(),
                    p.model.bounds_min.map(|v| (v * 100.0).round() / 100.0),
                    p.model.bounds_max.map(|v| (v * 100.0).round() / 100.0),
                    p.addon
                );
                for m in &p.model.materials {
                    println!("    material {} diffuse {:?} mask {} color {:?}", m.key, m.diffuse.as_ref().map(|t| t.key.clone()), m.alpha_mask, m.base_color);
                }
            }
            let mut names: Vec<&String> = model.clips.keys().collect();
            names.sort();
            println!("clips: {}", names.iter().map(|n| format!("{n}({:.2}s)", model.clips[*n].duration)).collect::<Vec<_>>().join(" "));
            println!("locomotion: {:?}", model.locomotion);
            println!("emotes: {}", model.actions.iter().map(|a| format!("{} [{}]", a.name, a.id)).collect::<Vec<_>>().join(", "));
            println!("content: {}", model.content.summary());
            if let Some(c) = clip {
                let c = model.clips.get(&c).ok_or_else(|| anyhow!("no clip {c}"))?;
                let f = ((at * c.fps).round() as usize).min(c.frames.len() - 1);
                println!("clip {} frame {f}/{} at {at}s", c.name, c.frames.len());
                for (track, bone) in c.track_to_bone.iter().enumerate().take(8) {
                    let p = c.frames[f][track];
                    println!("  bone {bone} ({}) rot {:?} trans {:?} scale {:?}", model.skeleton.bones[*bone].name, p.rotation.map(|v| (v * 1000.0).round() / 1000.0), p.translation.map(|v| (v * 1000.0).round() / 1000.0), p.scale);
                }
            }
        }
        Command::Ff11Zone { zone, out, model, sounds, collision, dump, ff11_path } => {
            let root = ffl_ff11::dat::locate(ff11_path.as_deref())?;
            let install = ffl_ff11::dat::Install::open(&root)?;
            let mut data = install.read(100 + zone as u32)?;
            println!("zone {zone}: {} ({} bytes)", install.file_path(100 + zone as u32).unwrap().display(), data.len());
            let z = ffl_ff11::zone::parse_zone(zone, &mut data)?;
            if let Some(path) = dump {
                std::fs::write(&path, &data)?;
                println!("decrypted DAT written to {}", path.display());
            }
            println!("{} instances, {} meshes, {} textures", z.instances.len(), z.models.len(), z.textures.len());
            for n in &z.notes {
                println!("note: {n}");
            }
            if collision {
                let Some(c) = &z.collision else {
                    println!("no MZB collision blocks");
                    return Ok(());
                };
                let s = c.stats();
                println!(
                    "collision: {} pieces, {} entries, {} (transform, piece) pairs, {} transforms of {} bytes (one per MZB instance), {} instances with collision",
                    c.pieces.len(),
                    c.entries,
                    c.pairs,
                    c.transform_count,
                    c.transform_stride,
                    c.placements.len()
                );
                println!("  {} triangles, {} vertices, unused pieces {}, piece flags {:?}", s.triangles, s.vertices, s.unused_pieces, s.flag_counts);
                let scene = ffl_ff11::scene_collision(&z);
                println!("  scene collision: {} meshes (by surface and 64-unit cell), {} triangles", scene.len(), scene.iter().map(|m| m.indices.len() / 3).sum::<usize>());
                match s.bounds {
                    Some((min, max)) => println!("  world bounds (Y up) {min:?} .. {max:?}"),
                    None => println!("  no geometry"),
                }
                match ffl_ff11::render_bounds(&z) {
                    Some((min, max)) => println!("  render bounds        {min:?} .. {max:?}"),
                    None => println!("  no render meshes"),
                }
                let mut per_model: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
                let mut without = 0usize;
                let mut placed: std::collections::HashSet<usize> = c.placements.iter().map(|p| p.instance).collect();
                for pl in &c.placements {
                    if let Some(inst) = z.instances.get(pl.instance) {
                        let e = per_model.entry(ffl_ff11::zone::name_str(&inst.name)).or_default();
                        e.0 += 1;
                        e.1 += pl.pieces.iter().map(|&pi| c.pieces[pi].faces.len()).sum::<usize>();
                    }
                }
                let mut no_col: std::collections::BTreeMap<String, usize> = Default::default();
                for (i, inst) in z.instances.iter().enumerate() {
                    if !placed.remove(&i) {
                        without += 1;
                        *no_col.entry(ffl_ff11::zone::name_str(&inst.name)).or_default() += 1;
                    }
                }
                let mut top: Vec<_> = per_model.iter().collect();
                top.sort_by(|a, b| b.1.1.cmp(&a.1.1));
                println!("  models with collision (instances, triangles): {:?}", &top[..top.len().min(10)]);
                let mut top: Vec<_> = no_col.iter().collect();
                top.sort_by(|a, b| b.1.cmp(a.1));
                println!("  {without} instances without collision, most placed: {:?}", &top[..top.len().min(8)]);
                for n in &c.notes {
                    println!("  note: {n}");
                }
                return Ok(());
            }
            if sounds {
                use ffl_ff11::sound::{self, SoundKind};
                let mut groups: std::collections::BTreeMap<u32, Vec<u32>> = Default::default();
                for id in &z.sounds {
                    groups.entry(id / 1000).or_default().push(*id);
                }
                println!("{} distinct sound ids in {} se groups", z.sounds.len(), groups.len());
                let mut unresolved = 0;
                for (g, ids) in &groups {
                    let surface = sound::SURFACE_LETTERS.iter().find(|(_, l)| sound::footstep_group(*l, 1) == Some(*g)).map(|(s, _)| format!(" (footsteps: {})", s.label())).unwrap_or_default();
                    let role = match g {
                        1 => " (ambience, stereo; looping files become beds)",
                        2 => " (ambience, mono; looping files become beds)",
                        _ => "",
                    };
                    println!("se{g:03}: {} ids{role}{surface}", ids.len());
                    for id in ids {
                        match install.sound_path(SoundKind::Spw, *id) {
                            Some(p) => {
                                let rel = p.strip_prefix(&root).unwrap_or(&p);
                                let info = install.sound_header(SoundKind::Spw, *id).ok().and_then(|h| sound::header(&h).ok());
                                match info {
                                    Some(h) => println!("  {id:6}  {}  {} {}ch {} Hz {:.2} s{}", rel.display(), h.codec_name(), h.channels, h.sample_rate, h.duration_secs(), if h.loop_start > 0 { " loop" } else { "" }),
                                    None => println!("  {id:6}  {}  (unreadable header)", rel.display()),
                                }
                            }
                            None => {
                                unresolved += 1;
                                println!("  {id:6}  MISSING");
                            }
                        }
                    }
                }
                println!("{unresolved} ids without a .spw file");
                match ffl_ff11::zone_music::zone_music(zone) {
                    Some((day, night, solo, party)) => {
                        let show = |label: &str, id: u16| {
                            let name = ffl_ff11::zone_music::music_name(id).unwrap_or("?");
                            let codec = if id == 0 {
                                "silence".to_string()
                            } else {
                                match install.sound_header(SoundKind::Bgw, id as u32).and_then(|h| sound::header(&h)) {
                                    Ok(h) => format!("{}{}", h.codec_name(), if h.decodable() { "" } else { " (unsupported)" }),
                                    Err(e) => format!("{e:#}"),
                                }
                            };
                            println!("  {label:<13} {id:3}  {name:<32} {codec}");
                        };
                        println!("music (LandSandBoat):");
                        show("day", day);
                        show("night", night);
                        show("battle_solo", solo);
                        show("battle_party", party);
                    }
                    None => println!("music: zone {zone} has no LandSandBoat entry"),
                }
                return Ok(());
            }
            let mut used: std::collections::BTreeMap<String, usize> = Default::default();
            for m in z.models.values().flatten() {
                *used.entry(ffl_ff11::zone::name_str(&m.texture)).or_default() += 1;
            }
            for (name, tex) in &z.textures {
                let n = ffl_ff11::zone::name_str(name);
                println!("  tex {:<18} {}x{} alpha={} used_by={}", n, tex.width, tex.height, tex.has_alpha, used.get(&n).copied().unwrap_or(0));
                if let Some(out) = &out {
                    std::fs::create_dir_all(out)?;
                    std::fs::write(out.join(format!("{}.{}x{}.rgba", n.replace(' ', "_"), tex.width, tex.height)), &tex.mips[0])?;
                }
            }
            let missing: Vec<String> = used.keys().filter(|k| !z.textures.keys().any(|t| ffl_ff11::zone::name_str(t) == **k)).cloned().collect();
            println!("mesh textures without image: {:?}", missing);
            let mut untextured = 0;
            let mut samples = Vec::new();
            for (name, meshes) in &z.models {
                for m in meshes {
                    if ffl_ff11::zone::name_str(&m.texture).is_empty() {
                        untextured += 1;
                        if samples.len() < 12 {
                            samples.push(format!("{}({}v,{}i,blend {:#x},tex {:?})", ffl_ff11::zone::name_str(name), m.positions.len(), m.indices.len() / 3, m.blend, m.texture));
                        }
                    }
                }
            }
            let total: usize = z.models.values().map(|m| m.len()).sum();
            println!("{untextured} of {total} meshes have an empty texture name: {}", samples.join(" "));
            let swaying: usize = z.models.values().flatten().filter(|m| !m.sway.is_empty()).count();
            let cutout_models = z.models.keys().filter(|n| ffl_ff11::zone::name_str(n).starts_with('_')).count();
            println!("{swaying} meshes carry a wind-sway stream; {cutout_models} of {} models are `_` cutout models", z.models.len());
            if let Some(pat) = &model {
                for (name, meshes) in &z.models {
                    let n = ffl_ff11::zone::name_str(name);
                    if !n.contains(pat.as_str()) {
                        continue;
                    }
                    println!("model {n}: {} meshes", meshes.len());
                    for m in meshes {
                        let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                        for uv in &m.uvs {
                            u0 = u0.min(uv[0]); u1 = u1.max(uv[0]); v0 = v0.min(uv[1]); v1 = v1.max(uv[1]);
                        }
                        let c = m.colors.first().copied().unwrap_or([0.0; 4]);
                        let (mut pmin, mut pmax) = ([f32::MAX; 3], [f32::MIN; 3]);
                        for p in &m.positions { for k in 0..3 { pmin[k] = pmin[k].min(p[k]); pmax[k] = pmax[k].max(p[k]); } }
                        println!("  tex {:?} blend {:#06x} verts {} tris {} uv u[{:.2},{:.2}] v[{:.2},{:.2}] color0 {:?} bounds {:?}..{:?}", ffl_ff11::zone::name_str(&m.texture), m.blend, m.positions.len(), m.indices.len() / 3, u0, u1, v0, v1, c, pmin, pmax);
                    }
                }
            }
            {
                let mut stats: std::collections::BTreeMap<u16, (usize, usize, f64, usize, f64)> = Default::default();
                for meshes in z.models.values() {
                    for m in meshes {
                        let e = stats.entry(m.blend).or_default();
                        for c in &m.colors {
                            e.0 += 1;
                            if c[0] > 1.01 || c[1] > 1.01 || c[2] > 1.01 { e.1 += 1; }
                            e.2 += (c[0] + c[1] + c[2]) as f64 / 3.0;
                            if c[3] < 0.99 { e.3 += 1; }
                            e.4 += c[3] as f64;
                        }
                    }
                }
                for (b, (n, over, sum, a_lt, asum)) in &stats {
                    println!("blend {b:#06x}: {n} verts, {:.1}% rgb>1, mean rgb {:.2}, {:.1}% alpha<1, mean alpha {:.2}", *over as f64 * 100.0 / *n as f64, sum / *n as f64, *a_lt as f64 * 100.0 / *n as f64, asum / *n as f64);
                }
                // Per blend flag: how many meshes, and how many of them use a texture with alpha.
                let mut per_flag: std::collections::BTreeMap<u16, (usize, usize, std::collections::BTreeSet<String>)> = Default::default();
                for meshes in z.models.values() {
                    for m in meshes {
                        let e = per_flag.entry(m.blend).or_default();
                        e.0 += 1;
                        if z.textures.get(&m.texture).map(|t| t.has_alpha).unwrap_or(false) {
                            e.1 += 1;
                            if e.2.len() < 8 {
                                e.2.insert(ffl_ff11::zone::name_str(&m.texture));
                            }
                        }
                    }
                }
                for (b, (meshes, with_alpha, names)) in &per_flag {
                    println!("blend {b:#06x}: {meshes} meshes, {with_alpha} with an alpha texture {:?}", names);
                }
            }
            let mut inst_counts: std::collections::BTreeMap<String, usize> = Default::default();
            for i in &z.instances {
                *inst_counts.entry(ffl_ff11::zone::name_str(&i.name)).or_default() += 1;
            }
            let mut top: Vec<_> = inst_counts.iter().collect();
            top.sort_by(|a, b| b.1.cmp(a.1));
            println!("most placed models: {:?}", &top[..top.len().min(12)]);
        }
        Command::Chara(cmd) => chara::run(&game, cmd)?,
        Command::Sklb { path } => {
            let skel = ffl_ff14_assets::loaders::load_sklb(source, &path)?;
            println!("{path}: {} bones", skel.bones.len());
            for (i, b) in skel.bones.iter().enumerate() {
                println!("[{i:3}] parent={:3} {:<24} pos={:?} rot={:?} scale={:?}", b.parent_index, b.name, b.position, b.rotation, b.scale);
            }
            for m in &skel.mappers {
                println!(
                    "mapper: {:?} ({} bones) -> {:?}: {} simple, {} chain, {} unmapped, keep_unmapped_local={}",
                    m.source_name,
                    m.source_bones.len(),
                    m.target_name,
                    m.simple_mappings.len(),
                    m.chain_mappings.len(),
                    m.unmapped_bones.len(),
                    m.keep_unmapped_local
                );
                let pos = |bones: &[physis::skeleton::Bone], n: &str| bones.iter().find(|b| b.name == n).map(|b| b.position);
                println!(
                    "  type {}  source j_te_r {:?} j_kosi {:?}  target j_te_r {:?} j_kosi {:?}",
                    m.mapping_type,
                    pos(&m.source_bones, "j_te_r"),
                    pos(&m.source_bones, "j_kosi"),
                    pos(&m.target_bones, "j_te_r"),
                    pos(&m.target_bones, "j_kosi")
                );
                for sm in m.simple_mappings.iter().filter(|x| ["j_kosi", "j_ude_a_r", "j_te_r", "n_buki_r", "j_kubi"].contains(&x.bone_a.as_str())) {
                    println!("  {} -> {}  t={:?} r={:?} s={:?}", sm.bone_a, sm.bone_b, sm.a_from_b_translation, sm.a_from_b_rotation, sm.a_from_b_scale);
                }
                // Self-check: the mapping applied to the source rest pose must give this
                // skeleton's rest pose. Tests the local-space interpretation with both
                // quaternion orders.
                let find = |bones: &[physis::skeleton::Bone], n: &str| bones.iter().find(|b| b.name == n).cloned();
                let mut err: [(f32, String); 2] = [(0.0, String::new()), (0.0, String::new())];
                let mut terr = (0.0f32, String::new());
                for sm in &m.simple_mappings {
                    let (Some(a), Some(b)) = (find(&m.source_bones, &sm.bone_a), find(&m.target_bones, &sm.bone_b)) else {
                        continue;
                    };
                    let qa = glam::Quat::from_array(a.rotation);
                    let qb = glam::Quat::from_array(b.rotation);
                    let r = glam::Quat::from_array(sm.a_from_b_rotation);
                    let cands = [qa * r, r * qa];
                    for (i, c) in cands.iter().enumerate() {
                        let e = 1.0 - c.dot(qb).abs();
                        if e > err[i].0 {
                            err[i] = (e, sm.bone_b.clone());
                        }
                    }
                    let ta = glam::Vec3::from(a.position);
                    let tb = glam::Vec3::from(b.position);
                    let s = sm.a_from_b_scale[0];
                    let t = ta * s + glam::Vec3::from(sm.a_from_b_translation);
                    let e = (t - tb).length();
                    if e > terr.0 {
                        terr = (e, sm.bone_b.clone());
                    }
                }
                println!("  rest check: max rot error qa*r {:.5} ({}), r*qa {:.5} ({}); max translation error {:.5} m ({})", err[0].0, err[0].1, err[1].0, err[1].1, terr.0, terr.1);
                for sm in m.simple_mappings.iter().take(0) {
                    println!("  {} -> {}  t={:?} r={:?} s={:?}", sm.bone_a, sm.bone_b, sm.a_from_b_translation, sm.a_from_b_rotation, sm.a_from_b_scale);
                }
                let renamed: Vec<String> = m.simple_mappings.iter().filter(|x| x.bone_a != x.bone_b).map(|x| format!("{}->{}", x.bone_a, x.bone_b)).collect();
                println!("  simple mappings with different names: {:?}", renamed);
                for c in &m.chain_mappings {
                    println!("  chain {}..{} -> {}..{}", c.start_bone_a, c.end_bone_a, c.start_bone_b, c.end_bone_b);
                }
                println!("  unmapped: {:?}", m.unmapped_bones);
            }
        }
        Command::Pbd { from, to } => {
            let pbd = ffl_ff14_assets::loaders::load_pbd(source, ffl_ff14_chara::race::paths::PBD)?;
            match pbd.get_deform_matrices(from, to) {
                Some(m) => {
                    println!("deform {from} -> {to}: {} bones", m.bones.len());
                    for b in m.bones.iter().take(12) {
                        println!("  {:<16} {:?}", b.name, b.deform);
                    }
                }
                None => println!("deform {from} -> {to}: None"),
            }
        }
        Command::Dalamud { design, files } => {
            let (plugins, collection) = (cli_plugins.clone(), cli_collection.clone());
            use ffl_ff14_assets::dalamud::{Dalamud, candidates};
            println!("candidates:");
            for c in candidates() {
                println!("  {}  ({})", c.path.display(), c.source);
            }
            let Some(d) = Dalamud::locate(plugins.as_deref()) else {
                bail!("no Dalamud plugin data found");
            };
            println!("{}", d.summary());
            if let Some(p) = &d.penumbra {
                println!("Penumbra config {}", p.config_path.display());
                println!("  active: yourself={:?} current={:?} default={:?}", p.yourself, p.current, p.default);
                for (who, id) in &p.individuals {
                    println!("  individual {who} -> {id}");
                }
                for c in &p.collections {
                    let enabled = p.resolve(&c.id).map(|m| m.len()).unwrap_or(0);
                    println!("  collection {} {:?}: {} settings, {enabled} enabled after inheritance, inherits {:?}", c.id, c.name, c.settings.len(), c.inherits);
                }
                if let Some(sel) = collection {
                    let c = p.collections.iter().find(|c| c.id == sel || c.name.eq_ignore_ascii_case(&sel)).ok_or_else(|| anyhow!("no collection {sel}"))?;
                    let mods = p.resolve(&c.id)?;
                    let mut total = 0;
                    let mut swaps = 0;
                    let mut missing = 0;
                    let mut all: std::collections::BTreeMap<String, String> = Default::default();
                    for m in mods.iter().rev() {
                        let root = p.mod_directory.join(&m.dir);
                        if !root.is_dir() {
                            missing += 1;
                            continue;
                        }
                        match ffl_ff14_assets::mods::ModPack::load_penumbra_dir(&root, &m.dir, Some(&m.options)) {
                            Ok(pack) => {
                                total += pack.files.len();
                                swaps += pack.swaps.len();
                                for k in pack.files.keys() {
                                    all.entry(k.clone()).or_insert_with(|| m.dir.clone());
                                }
                            }
                            Err(err) => println!("  {}: {err:#}", m.dir),
                        }
                    }
                    println!("collection {:?}: {} mods enabled ({missing} not on disk), {total} file replacements ({} distinct paths after priority), {swaps} swaps", c.name, mods.len(), all.len());
                    let chara = all.keys().filter(|k| k.starts_with("chara/")).count();
                    println!("  {chara} under chara/, {} elsewhere", all.len() - chara);
                    if files {
                        for (k, m) in &all {
                            println!("  {k}  <- {m}");
                        }
                    }
                }
            }
            if let Some(g) = &d.glamourer {
                for de in &g.designs {
                    println!("  design {} {:?} in {:?}: {} customize fields applied, {} gear slots applied, {} advanced colours", de.id, de.name, de.folder, de.customize.iter().filter(|c| c.2).count(), de.equipment.iter().filter(|e| e.2).count(), de.parameters.iter().filter(|p| p.2).count());
                }
                for a in &g.automation {
                    println!("  automation {:?} for {:?} ({}): {:?}", a.name, a.player, if a.enabled { "enabled" } else { "disabled" }, a.designs);
                }
                if let Some(sel) = design {
                    let de = g.designs.iter().find(|d| d.id == sel || d.name.eq_ignore_ascii_case(&sel)).ok_or_else(|| anyhow!("no design {sel}"))?;
                    let mut s = std::collections::BTreeMap::new();
                    let (c, gear, colors) = ffl_ff14_assets::dalamud::apply_design(de, &mut s);
                    println!("design {:?}: {c} customize fields, {gear} gear slots, {colors} advanced colours -> {:?}", de.name, s);
                }
            }
        }
        Command::ArchiveCharacter { preset, out } => {
            let (plugins, collection, mods) = (cli_plugins.clone(), cli_collection.clone(), cli_mods.clone());
            use ffl_core::Engine;
            let library = ffl_core::Library::load();
            let p = library
                .presets
                .iter()
                .find(|p| p.id == preset || p.name.eq_ignore_ascii_case(&preset))
                .cloned()
                .ok_or_else(|| anyhow!("no preset {preset} in {}", ffl_core::Library::path().display()))?;
            let engine = ffl_ff14::Ff14Engine::from_game(std::sync::Arc::new(GameData::open(game_path.as_deref())?), plugins.as_deref());
            let mut ids = mods.clone();
            if let Some(sel) = collection {
                let profile = engine
                    .mod_profiles()
                    .into_iter()
                    .find(|pr| pr.id == sel || pr.id == format!("penumbra:{sel}") || pr.name.eq_ignore_ascii_case(&sel))
                    .ok_or_else(|| anyhow!("no collection {sel}"))?;
                println!("using {} ({})", profile.name, profile.detail);
                ids.insert(0, profile.id);
            }
            engine.set_enabled_mods(&ids);
            let dir = out.unwrap_or_else(|| ffl_ff14::archive_dir(&p.id));
            let r = engine.archive_character(&p, &dir)?;
            println!("{}: {} modded files, {:.2} MB, from {:?}; {} vanilla files stay in the game -> {}", p.name, r.files, r.bytes as f64 / 1e6, r.packs, r.vanilla, r.dir.display());
            if let Ok(m) = std::fs::read_to_string(dir.join("manifest.toml")) {
                for l in m.lines().take(12) {
                    println!("  {l}");
                }
            }
        }
        Command::Colors { preset } => {
            use ffl_ff14_chara::meta::cmp::CmpFile;
            let library = ffl_core::Library::load();
            let p = library
                .presets
                .iter()
                .find(|p| p.id == preset || p.name.eq_ignore_ascii_case(&preset))
                .cloned()
                .ok_or_else(|| anyhow!("no preset {preset} in {}", ffl_core::Library::path().display()))?;
            let c = ffl_ff14_chara::preset::customize_from_settings(&p.settings)?.ok_or_else(|| anyhow!("{} has no customize values", p.name))?;
            let bytes = source.read(ffl_ff14_chara::race::paths::CMP).ok_or_else(|| anyhow!("no human.cmp"))?;
            let cmp = CmpFile::parse(&bytes)?;
            println!("{}: tribe {:?} gender {:?}", p.name, c.tribe, c.gender);
            let show = |what: &str, idx: u8, v: ffl_ff14_chara::meta::cmp::Rgba8| println!("  {what:16} {idx:3} -> {} alpha {}", v.hex(), v.0[3]);
            show("skin", c.skin_tone, cmp.skin_color(c.tribe, &c.gender, c.skin_tone));
            show("hair", c.hair_tone, cmp.hair_color(c.tribe, &c.gender, c.hair_tone));
            show("highlights", c.highlights, cmp.hair_highlight_color(c.highlights));
            show("eye left", c.left_eye_color, cmp.eye_color(c.left_eye_color));
            show("eye right", c.right_eye_color, cmp.eye_color(c.right_eye_color));
            show("lips", c.lips_tone_fur_pattern, cmp.lip_color(c.lips_tone_fur_pattern));
            show("feature", c.facial_feature_color, cmp.feature_color(c.facial_feature_color));
            show("face paint", c.face_paint_color, cmp.face_paint_color(c.face_paint_color));
            println!("  face paint {} facial features {:#x} lips {} highlights {}", c.face_paint, c.facial_features, c.lips_tone_fur_pattern, c.enable_highlights);
        }
        Command::Scan => {
            println!("FFXIV candidates:");
            for c in ffl_ff14_assets::locate::candidates() {
                println!("  {:60} {}", c.path.display(), c.source);
            }
            println!("FFXIV installs found:");
            for c in ffl_ff14_assets::locate::scan() {
                println!("  {}  ({})", c.path.display(), c.source);
            }
            println!("FFXI candidates:");
            for c in ffl_ff11::dat::candidates() {
                println!("  {:60} {}", c.path.display(), c.source);
            }
            println!("FFXI installs found:");
            for c in ffl_ff11::dat::scan() {
                println!("  {}  ({})", c.path.display(), c.source);
            }
        }
        Command::Atch { race, bone } => {
            let path = ffl_ff14_chara::race::paths::atch(ffl_ff14_chara::RaceCode(race));
            let bytes = source.read(&path).ok_or_else(|| anyhow!("{path} not found"))?;
            let atch = ffl_ff14_chara::meta::atch::AtchFile::parse(&bytes)?;
            let mut codes: Vec<&String> = atch.points.keys().collect();
            codes.sort();
            println!("{path}: {} attach types", codes.len());
            for code in codes {
                let entries = &atch.points[code];
                if bone.as_ref().is_some_and(|b| !entries.iter().any(|e| &e.bone == b)) {
                    continue;
                }
                println!("{code}:");
                for (i, e) in entries.iter().enumerate() {
                    println!("  [{i}] bone {:12} scale {:.3} offset {:?} rotation {:?}", e.bone, e.scale, e.offset, e.rotation);
                }
            }
        }
        Command::Pap { path, binding, at, race } => {
            let pap = ffl_ff14_assets::loaders::load_pap(source, &path)?;
            if let Some(bi) = binding {
                let skel = ffl_ff14_assets::loaders::load_sklb(source, &ffl_ff14_chara::race::paths::skeleton(ffl_ff14_chara::RaceCode(race)))?;
                let root = physis::havok::HavokBinaryTagFileReader::read(pap.havok_data());
                let container = physis::havok::HavokAnimationContainer::new(root.find_object_by_type("hkaAnimationContainer"));
                let b = container.bindings.get(bi).ok_or_else(|| anyhow!("no binding {bi}"))?;
                let pose = b.animation.sample(at);
                println!("binding {bi} at t={at}: {} tracks", pose.len());
                for (t, x) in pose.iter().enumerate() {
                    let bone = b.transform_track_to_bone_indices.get(t).copied().unwrap_or(0) as usize;
                    let name = skel.bones.get(bone).map(|b| b.name.as_str()).unwrap_or("?");
                    let refb = skel.bones.get(bone);
                    println!("  track {t:3} -> bone {bone:3} {name:<18} t=[{:+.3} {:+.3} {:+.3}] r=[{:+.3} {:+.3} {:+.3} {:+.3}] s=[{:.2} {:.2} {:.2}] | ref r=[{}]",
                        x.translation[0], x.translation[1], x.translation[2], x.rotation[0], x.rotation[1], x.rotation[2], x.rotation[3], x.scale[0], x.scale[1], x.scale[2],
                        refb.map(|b| format!("{:+.3} {:+.3} {:+.3} {:+.3}", b.rotation[0], b.rotation[1], b.rotation[2], b.rotation[3])).unwrap_or_default());
                }
                return Ok(());
            }
            println!("{path}: type {:?}, {} animation(s), {} havok bytes", pap.model_type, pap.animations.len(), pap.havok_data().len());
            for a in &pap.animations {
                println!("  anim {:?}", a.name);
            }
            let root = physis::havok::HavokBinaryTagFileReader::read(pap.havok_data());
            let container = physis::havok::HavokAnimationContainer::new(root.find_object_by_type("hkaAnimationContainer"));
            println!("skeletons {} bindings {}", container.skeletons.len(), container.bindings.len());
            for (i, s) in container.skeletons.iter().enumerate() {
                println!("  skeleton {i}: {} bones: {} ...", s.bone_names.len(), s.bone_names.iter().take(6).cloned().collect::<Vec<_>>().join(" "));
            }
            for (i, b) in container.bindings.iter().enumerate() {
                let dur = b.animation.duration();
                let pose = b.animation.sample(0.0);
                println!("  binding {i}: {} tracks, duration {dur:.3}s, sampled {} transforms", b.transform_track_to_bone_indices.len(), pose.len());
                for (t, x) in pose.iter().take(3).enumerate() {
                    println!("    track {t} -> bone {}: t={:?} r={:?} s={:?}", b.transform_track_to_bone_indices.get(t).copied().unwrap_or(0), x.translation, x.rotation, x.scale);
                }
                // Sanity statistics over the whole clip.
                let mut min_scale = f32::MAX;
                let mut max_scale = f32::MIN;
                let mut max_trans = 0.0f32;
                let mut bad_quat = 0;
                let mut nan = 0;
                let steps = 24;
                for k in 0..=steps {
                    let t = dur * k as f32 / steps as f32;
                    for x in b.animation.sample(t) {
                        for v in x.scale.iter().take(3) {
                            min_scale = min_scale.min(*v);
                            max_scale = max_scale.max(*v);
                        }
                        let tl = (x.translation[0].powi(2) + x.translation[1].powi(2) + x.translation[2].powi(2)).sqrt();
                        max_trans = max_trans.max(tl);
                        let ql = (x.rotation[0].powi(2) + x.rotation[1].powi(2) + x.rotation[2].powi(2) + x.rotation[3].powi(2)).sqrt();
                        if (ql - 1.0).abs() > 0.05 {
                            bad_quat += 1;
                        }
                        if x.translation.iter().chain(x.rotation.iter()).chain(x.scale.iter()).any(|v| !v.is_finite()) {
                            nan += 1;
                        }
                    }
                }
                println!("    stats: scale [{min_scale:.3}, {max_scale:.3}] max|t|={max_trans:.3} non-unit quats={bad_quat} non-finite={nan}");
            }
        }
        Command::ExcelScan { sheet, pattern, max_rows } => {
            let loaded = game.excel.sheet(&sheet)?;
            let mut hits = 0;
            for page in &loaded.sheet.pages {
                for entry in &page.entries {
                    if entry.id >= max_rows {
                        continue;
                    }
                    for (_, row) in &entry.subrows {
                        for (i, f) in row.columns.iter().enumerate() {
                            if let physis::excel::Field::String(v) = f
                                && v.contains(&pattern)
                            {
                                println!("[{}] col {i}: {v}", entry.id);
                                hits += 1;
                            }
                        }
                    }
                }
            }
            println!("{hits} hits");
        }
        Command::Raw { path, out } => {
            let bytes = source.read(&path).ok_or_else(|| anyhow!("file not found: {path}"))?;
            std::fs::write(&out, &bytes).with_context(|| format!("writing {}", out.display()))?;
            println!("wrote {} bytes to {}", bytes.len(), out.display());
        }
        Command::Tmb { path } => {
            use ffl_ff14_chara::meta::tmb::{TmbInfo, embedded_timelines};
            let bytes = source.read(&path).ok_or_else(|| anyhow!("file not found: {path}"))?;
            let blocks: Vec<&[u8]> = if bytes.starts_with(b"pap ") { embedded_timelines(&bytes) } else { vec![bytes.as_slice()] };
            println!("{path}: {} timeline block(s)", blocks.len());
            for (i, b) in blocks.iter().enumerate() {
                let info = TmbInfo::parse(b);
                println!(
                    "  [{i}] {} ({} frames = {:.3} s) entries {:?}",
                    info.animation.as_deref().unwrap_or("?"),
                    info.animation_frames,
                    info.animation_frames as f32 / 30.0,
                    info.entry_magics
                );
                for e in &info.weapon_events {
                    println!("      weapon  frame {:>3} ({:.3} s) {} {}", e.frame, e.frame as f32 / 30.0, if e.drawn { "drawn" } else { "stowed" }, if e.off_hand { "off hand" } else { "main hand" });
                }
                for e in &info.footsteps {
                    println!("      foot    frame {:>3} ({:.3} s) {} sound_id {}", e.frame, e.frame as f32 / 30.0, match e.foot_id { 34 => "left", 35 => "right", _ => "?" }, e.sound_id);
                }
                for e in &info.voices {
                    println!("      voice   frame {:>3} ({:.3} s) line {} bind {} flags {:#x}{}", e.frame, e.frame as f32 / 30.0, e.sound_id, e.bind, e.flags, if e.stop_on_movement() { " stop-on-movement" } else { "" });
                }
                for e in &info.sounds {
                    println!("      sound   frame {:>3} ({:.3} s) {}#{} loop {} pos {:#x} bind {} {}", e.frame, e.frame as f32 / 30.0, e.path, e.index, e.looped, e.position_flags, e.bind, if source.exists(&e.path) { "ok" } else { "MISSING" });
                }
            }
        }
        Command::Bgm { territory } => {
            let chain = ffl_ff14_assets::music::resolve_chain(&game.excel, territory)?;
            println!("TerritoryType {territory}: BGM = {} ({:?})", chain.value, chain.kind());
            if let Some(s) = chain.switch_row {
                println!("  BGMSwitch {s} default subrow -> {}", chain.switch_value.unwrap_or(0));
            }
            if let Some(s) = chain.situation_row {
                println!("  BGMSituation {s}");
            }
            for (slot, r) in chain.rows() {
                println!(
                    "  {slot:<9} BGM {:<5} {} {} priority {} disable_restart {} special {}",
                    r.row,
                    r.file,
                    if source.exists(&r.file) { "ok" } else { "MISSING" },
                    r.priority,
                    r.disable_restart,
                    r.special_mode
                );
            }
            match ffl_ff14_assets::music::music_set(&game.excel, source, territory)? {
                Some(set) => {
                    println!("music set: day={:?} night={:?} battle={:?} daybreak={:?} extra={:?} fade {}/{} restart_on_return {}", set.day, set.night, set.battle, set.daybreak, set.extra, set.fade_in, set.fade_out, set.restart_on_return);
                    for n in &set.notes {
                        println!("  note: {n}");
                    }
                }
                None => println!("music set: none"),
            }
        }
        Command::Scd { path, entry, out, ogg } => {
            let scd = ffl_ff14_assets::loaders::load_scd(source, &path)?;
            println!("{path}: {} slots, {} with audio, {} sound programs", scd.entries.len(), scd.present().count(), scd.sounds.len());
            for (i, s) in scd.sounds.iter().enumerate() {
                println!("  program[{i}] kind {} volume {:.2} audio {:?}{}", s.kind, s.volume, s.audio, if s.weights.is_empty() { String::new() } else { format!(" weights {:?}", s.weights) });
            }
            for e in scd.present() {
                let loop_desc = match &e.marker {
                    Some(m) => format!("MARK loop {}..{} samples, {} markers", m.loop_start, m.loop_end, m.markers.len()),
                    None if e.loop_end > 0 => format!(
                        "loop {}..{} {}",
                        e.loop_start,
                        e.loop_end,
                        if e.codec == ffl_ff14_assets::scd::ScdCodec::OggVorbis { "bytes" } else { "samples" }
                    ),
                    None => "no loop".to_string(),
                };
                let extra = match e.vorbis_info().ok().filter(|_| e.codec == ffl_ff14_assets::scd::ScdCodec::OggVorbis) {
                    Some(v) => format!(" xor mode {:#06x} key {:#04x} seek {} entries header {} B", v.encode_mode, v.encode_byte, v.seek_table.len(), v.header_size),
                    None => String::new(),
                };
                println!(
                    "  [{:2}] {:<10} {} ch {:>6} Hz stream {:>9} B flags {:#x} sub-info {} B {}{}",
                    e.index,
                    e.codec.label(),
                    e.channels,
                    e.sample_rate,
                    e.stream_size,
                    e.flags,
                    e.extradata.len(),
                    loop_desc,
                    extra
                );
            }
            let pick = |wanted: Option<usize>| -> Result<&ffl_ff14_assets::scd::ScdEntry> {
                match wanted {
                    Some(i) => scd.entries.get(i).and_then(|e| e.as_ref()).ok_or_else(|| anyhow!("slot {i} holds no audio")),
                    None => scd.present().next().ok_or_else(|| anyhow!("no audio entries")),
                }
            };
            if let Some(ogg_out) = ogg {
                let e = pick(entry)?;
                let bytes = e.ogg_bytes()?;
                std::fs::write(&ogg_out, &bytes).with_context(|| format!("writing {}", ogg_out.display()))?;
                println!("wrote {} bytes of ogg (entry {}) to {}", bytes.len(), e.index, ogg_out.display());
            }
            if let Some(out) = out {
                let e = pick(entry)?;
                let d = e.decode()?;
                let secs = d.frames() as f32 / d.sample_rate as f32;
                let loop_desc = d
                    .loop_range
                    .map(|(s, en)| format!("loop {:.3}s..{:.3}s ({s}..{en} frames)", s as f32 / d.sample_rate as f32, en as f32 / d.sample_rate as f32))
                    .unwrap_or_else(|| "no loop".into());
                let peak = d.samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                std::fs::write(&out, ffl_core::wav_bytes(d.sample_rate, d.channels, &d.samples)).with_context(|| format!("writing {}", out.display()))?;
                println!("decoded entry {}: {} ch {} Hz {} frames = {secs:.2} s, peak {peak}, {loop_desc}; wrote {}", e.index, d.channels, d.sample_rate, d.frames(), out.display());
            }
        }
        Command::Info => {
            println!("game dir : {}", game.install.game_dir.display());
            println!("version  : {}", game.install.version);
            println!("origin   : {:?}", game.install.origin);
            for (name, ver) in game.source.repositories() {
                println!("repo {:6} {}", name, ver.unwrap_or_else(|| "?".into()));
            }
        }
        Command::Exists { path } => {
            let exists = source.exists(&path);
            println!("{path}: {}", if exists { "exists" } else { "MISSING" });
            if !exists {
                std::process::exit(1);
            }
        }
        Command::Excel { sheet, row, schema } => {
            let exh = game.excel.header(&sheet)?;
            println!("{sheet}.exh: languages={:?} pages={} row_count={}", exh.languages, exh.pages.len(), exh.header.row_count);
            let loaded = game.excel.sheet(&sheet)?;
            println!(
                "{sheet} language={:?} rows={} columns={}",
                loaded.language,
                loaded.row_count(),
                loaded.sheet.exh.column_definitions.len()
            );
            let r = loaded
                .row(row)
                .ok_or_else(|| anyhow!("no row {row} in {sheet}"))?;
            for (i, field) in r.columns.iter().enumerate() {
                let def = loaded.sheet.exh.column_definitions[i];
                let name = if schema {
                    loaded
                        .schema
                        .as_ref()
                        .and_then(|s| s.names.get(i))
                        .filter(|n| !n.is_empty())
                        .cloned()
                        .unwrap_or_else(|| "?".into())
                } else {
                    String::new()
                };
                println!(
                    "[{i:3}] off={:4} {:<12} {name:<28} {field:?}",
                    def.offset,
                    format!("{:?}", def.data_type)
                );
            }
        }
        Command::Lvb { path } => {
            let lvb = load_lvb(source, &path)?;
            println!("{path}: {} section(s)", lvb.sections.len());
            for (i, s) in lvb.sections.iter().enumerate() {
                println!("section {i}: bg_path={:?}", s.general.bg_path.value);
                println!("  svb={:?} lcb={:?}", s.general.svb_path.value, s.general.lcb_path.value);
                println!("  env_spaces={}", s.general.env_spaces.len());
                for e in &s.general.env_spaces {
                    println!("    envb={:?} essb={:?}", e.envb_path.value, e.essb_path.value);
                }
                println!("  lgb_paths ({}):", s.lgb_paths.len());
                for p in &s.lgb_paths {
                    println!("    {p}  [{}]", if source.exists(p) { "ok" } else { "MISSING" });
                }
                println!("  layer_sets ({}):", s.layer_sets.layer_sets.len());
                for ls in &s.layer_sets.layer_sets {
                    println!(
                        "    id={} territory={} cfc={} nvm={:?}",
                        ls.id, ls.territory_type_id, ls.content_finder_condition_id, ls.nvm_path.value
                    );
                }
                println!("  embedded layer_groups={} timelines={}", s.layer_groups.len(), s.timelines.timelines.len());
            }
        }
        Command::Lgb { path, assets } => {
            let lgb = load_lgb(source, &path)?;
            println!("{path}: {} chunk(s)", lgb.chunks.len());
            for chunk in &lgb.chunks {
                println!("chunk {:?} id={} layers={}", chunk.name, chunk.layer_group_id, chunk.layers.len());
                for layer in &chunk.layers {
                    print_layer(layer, assets, "  ");
                }
            }
        }
        Command::Sgb { path } => {
            let sgb = load_sgb(source, &path)?;
            println!("{path}: {} section(s)", sgb.sections.len());
            for (i, s) in sgb.sections.iter().enumerate() {
                println!("section {i}: layer_groups={}", s.layer_groups.len());
                for lg in &s.layer_groups {
                    println!("  group {:?} id={} layers={}", lg.name.value, lg.layer_group_id, lg.layers.len());
                    for layer in &lg.layers {
                        print_layer(layer, true, "    ");
                    }
                }
            }
        }
        Command::Zone { territory, bg, models, find, sounds } => {
            let graph = match bg {
                Some(bg) => build_zone_from_bg(source, territory, &bg)?,
                None => build_zone(source, &game.excel, territory)?,
            };
            println!("territory {territory}: bg={} base_dir={}", graph.bg, graph.base_dir);
            println!("lvb: {}", graph.lvb_path);
            for p in &graph.lgb_paths {
                println!("lgb: {p}");
            }
            let bg_parts = graph.models.iter().filter(|m| m.kind == PlacedKind::BgPart).count();
            println!(
                "models: {} placed ({} bg parts, {} terrain plates), {} unique mdl paths",
                graph.models.len(),
                bg_parts,
                graph.stats.terrain_plates,
                graph.stats.unique_models
            );
            println!(
                "groups: {} placed sgb instances, {} unique sgb files, max depth {}",
                graph.groups.len(),
                graph.stats.unique_groups,
                graph.stats.max_group_depth
            );
            println!("lights: {}", graph.lights.len());
            if let Some(pat) = &find {
                fn rot(r: [f32; 3], v: [f32; 3]) -> [f32; 3] {
                    // Matches the runtime's Quat::from_euler(ZYX, r[2], r[1], r[0]).
                    let (sx, cx) = r[0].sin_cos();
                    let (sy, cy) = r[1].sin_cos();
                    let (sz, cz) = r[2].sin_cos();
                    let v = [v[0], cx * v[1] - sx * v[2], sx * v[1] + cx * v[2]];
                    let v = [cy * v[0] + sy * v[2], v[1], -sy * v[0] + cy * v[2]];
                    [cz * v[0] - sz * v[1], sz * v[0] + cz * v[1], v[2]]
                }
                fn world(graph: &ffl_ff14_assets::zone::ZoneGraph, parent: Option<usize>, local: [f32; 3]) -> [f32; 3] {
                    let mut pos = local;
                    let mut p = parent;
                    while let Some(i) = p {
                        let g = &graph.groups[i];
                        let scaled = [pos[0] * g.placement.scale[0], pos[1] * g.placement.scale[1], pos[2] * g.placement.scale[2]];
                        let r = rot(g.placement.rotation, scaled);
                        pos = [r[0] + g.placement.translation[0], r[1] + g.placement.translation[1], r[2] + g.placement.translation[2]];
                        p = g.parent;
                    }
                    pos
                }
                let mut n = 0;
                for m in graph.models.iter().filter(|m| m.mdl_path.contains(pat.as_str())) {
                    let w = world(&graph, m.parent, m.placement.translation);
                    println!("{:>9.2} {:>8.2} {:>9.2}  rot_y={:>6.1}  {}", w[0], w[1], w[2], m.placement.rotation[1].to_degrees(), m.mdl_path.rsplit('/').next().unwrap_or(""));
                    n += 1;
                }
                println!("{n} instances match {pat}");
            }
            if sounds {
                println!("placed sounds: {} (obstruction probes skipped), env sets: {}, map ranges: {}", graph.sounds.len(), graph.env_sets.len(), graph.map_ranges.len());
                let mut by_path: std::collections::BTreeMap<&str, usize> = Default::default();
                for s in &graph.sounds {
                    *by_path.entry(s.scd_path.as_str()).or_default() += 1;
                }
                for (path, n) in &by_path {
                    println!("  sound {n:>3}x {path} {}", if source.exists(path) { "ok" } else { "MISSING" });
                }
                for s in &graph.sounds {
                    println!("    {} pos=({:.1}, {:.1}, {:.1}) inner={} height={} max={} param={}", s.scd_path.rsplit('/').next().unwrap_or(""), s.placement.translation[0], s.placement.translation[1], s.placement.translation[2], s.placement.scale[0], s.placement.scale[1], s.placement.scale[2], s.sound_effect_param);
                }
                let mut essb: std::collections::BTreeSet<&str> = Default::default();
                for e in graph.env_sets.iter().filter(|e| !e.sound_asset_path.is_empty()) {
                    essb.insert(e.sound_asset_path.as_str());
                }
                for p in essb {
                    println!("  env sound scape {p}");
                }
                let names = game.excel.sheet("PlaceName").ok();
                for r in graph.map_ranges.iter().filter(|r| r.bgm_enabled && r.bgm != 0) {
                    let name = names.as_ref().and_then(|n| n.string(r.place_name_spot, "Name").ok()).unwrap_or_default();
                    let set = ffl_ff14_assets::music::music_set_for_value(&game.excel, source, territory, r.bgm as u64);
                    let desc = match set {
                        Ok(Some(m)) => format!("day={:?} night={:?}", m.day.as_deref().map(|k| k.rsplit('/').next().unwrap_or(k)), m.night.as_deref().map(|k| k.rsplit('/').next().unwrap_or(k))),
                        Ok(None) => "silence".into(),
                        Err(e) => format!("error: {e:#}"),
                    };
                    println!("  map range {:?} \"{name}\" bgm={} prio={} pos={:?} half={:?} zone_in_only={} -> {desc}", r.shape, r.bgm, r.priority, r.placement.translation, r.placement.scale, r.bgm_play_zone_in_only);
                }
            }
            println!("layers skipped (festival): {}", graph.stats.layers_skipped_festival);
            for (kind, n) in &graph.stats.skipped_by_type {
                println!("skipped {kind:<20} {n}");
            }
            if !graph.stats.missing_sgb.is_empty() {
                println!("missing sgb: {:?}", graph.stats.missing_sgb);
            }
            if models {
                let mut hist: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
                for m in &graph.models {
                    *hist.entry(m.clip_range.round() as u32).or_default() += 1;
                }
                println!("clip ranges (metres: instances): {hist:?}");
                let mut missing = 0;
                for p in graph.unique_model_paths() {
                    let ok = source.exists(p);
                    if !ok {
                        missing += 1;
                    }
                    println!("{} {p}", if ok { "ok     " } else { "MISSING" });
                }
                println!("{missing} missing model files");
            }
        }
        Command::HashProbe { index, folders, files, in_folder } => {
            let path = game.install.game_dir.join(&index);
            let idx = physis::sqpack::index::SqPackIndex::from_existing(physis::common::Platform::Win32, &path).ok_or_else(|| anyhow!("cannot read {}", path.display()))?;
            let mut folder_hashes = std::collections::HashSet::new();
            let mut file_hashes: std::collections::HashMap<u32, Vec<u32>> = Default::default();
            for e in &idx.entries {
                if let physis::sqpack::index::Hash::SplitPath { name, path } = e.hash {
                    folder_hashes.insert(path);
                    file_hashes.entry(path).or_default().push(name);
                }
            }
            println!("{} entries, {} folders", idx.entries.len(), folder_hashes.len());
            let crc = |s: &str| physis::sqpack::index::SqPackIndex::calculate_partial_hash(s);
            if let Some(f) = folders {
                for line in std::fs::read_to_string(f)?.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    if folder_hashes.contains(&crc(line)) {
                        println!("folder {line}: exists ({} files)", file_hashes[&crc(line)].len());
                    }
                }
            }
            if let (Some(f), Some(folder)) = (files, in_folder) {
                let Some(names) = file_hashes.get(&crc(&folder)) else {
                    bail!("folder {folder} not in the index");
                };
                let names: std::collections::HashSet<u32> = names.iter().copied().collect();
                println!("{folder}: {} files", names.len());
                for line in std::fs::read_to_string(f)?.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    if names.contains(&crc(line)) {
                        println!("file {folder}/{line}: exists");
                    }
                }
            }
        }
        Command::Mdl { path, near_uv, near_pos } => {
            let near_p: Option<([f32; 3], f32)> = near_pos.as_deref().and_then(|s| {
                let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                (v.len() == 4).then(|| ([v[0], v[1], v[2]], v[3]))
            });
            let mdl = load_mdl(source, &path)?;
            let near: Option<(f32, f32, f32)> = near_uv.as_deref().and_then(|s| {
                let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                (v.len() == 3).then(|| (v[0], v[1], v[2]))
            });
            println!("{path}: lods={} lod ranges={:?} radius={}", mdl.lods.len(), mdl.lod_ranges(), mdl.file_radius());
            println!("materials ({}):", mdl.material_names.len());
            for m in &mdl.material_names {
                println!("  {m}");
            }
            println!("bones ({}): {}", mdl.affected_bone_names.len(), mdl.affected_bone_names.join(" "));
            println!("attributes ({}): {}", mdl.attribute_names.len(), mdl.attribute_names.join(" "));
            for (li, lod) in mdl.lods.iter().enumerate() {
                println!("lod {li}: {} part(s)", lod.parts.len());
                for (pi, part) in lod.parts.iter().enumerate() {
                    let bbox = bounds(&part.vertices);
                    let n = part.vertices.len().max(1) as f32;
                    let col_a = part.vertices.iter().map(|v| v.color[3]).sum::<f32>() / n;
                    let col_rgb = part.vertices.iter().map(|v| (v.color[0] + v.color[1] + v.color[2]) / 3.0).sum::<f32>() / n;
                    let uv_wild = part.vertices.iter().filter(|v| !v.uv0[0].is_finite() || v.uv0[0].abs() > 64.0 || v.uv0[1].abs() > 64.0).count();
                    let uv_degenerate = part.indices.chunks_exact(3).filter(|t| {
                        let uv = |i: u16| part.vertices.get(i as usize).map(|v| v.uv0).unwrap_or([0.0; 2]);
                        let (a, b, c) = (uv(t[0]), uv(t[1]), uv(t[2]));
                        ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() < 1e-7
                    }).count();
                    println!("    colour mean rgb {col_rgb:.2} alpha {col_a:.2}, uv0 out of range: {uv_wild}, uv-degenerate triangles: {uv_degenerate} of {}, indices max {:?} of {} vertices", part.indices.len() / 3, part.indices.iter().max(), part.vertices.len());
                    if let Some((c, r)) = near_p {
                        for (i, vert) in part.vertices.iter().enumerate() {
                            if (0..3).all(|k| c[k].is_nan() || (vert.position[k] - c[k]).abs() <= r) {
                                println!("      v{i}: pos=[{:.4}, {:.4}, {:.4}] uv0=[{:.3}, {:.3}] uv1=[{:.3}, {:.3}]", vert.position[0], vert.position[1], vert.position[2], vert.uv0[0], vert.uv0[1], vert.uv1[0], vert.uv1[1]);
                            }
                        }
                    }
                    if let Some((u, v, r)) = near {
                        for (i, vert) in part.vertices.iter().enumerate() {
                            if (vert.uv0[0] - u).hypot(vert.uv0[1] - v) <= r {
                                println!("      v{i}: pos=[{:.4}, {:.4}, {:.4}] uv0=[{:.3}, {:.3}] w={:?} ids={:?} w2={:?} ids2={:?}", vert.position[0], vert.position[1], vert.position[2], vert.uv0[0], vert.uv0[1], vert.bone_weight, vert.bone_id, vert.bone_weight2, vert.bone_id2);
                            }
                        }
                    }
                    let range = |f: &dyn Fn(&physis::model::Vertex) -> f32| {
                        part.vertices.iter().map(f).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), x| (lo.min(x), hi.max(x)))
                    };
                    println!(
                        "    uv0 u[{:.2},{:.2}] v[{:.2},{:.2}]  uv1 u[{:.2},{:.2}] v[{:.2},{:.2}]",
                        range(&|v| v.uv0[0]).0, range(&|v| v.uv0[0]).1, range(&|v| v.uv0[1]).0, range(&|v| v.uv0[1]).1,
                        range(&|v| v.uv1[0]).0, range(&|v| v.uv1[0]).1, range(&|v| v.uv1[1]).0, range(&|v| v.uv1[1]).1
                    );
                    let low: usize = part.vertices.iter().filter(|v| (v.bone_weight.iter().sum::<f32>() + v.bone_weight2.iter().sum::<f32>() - 1.0).abs() > 0.02).count();
                    let eight: usize = part.vertices.iter().filter(|v| v.bone_weight2.iter().any(|w| *w > 0.0)).count();
                    println!("    vertex elements {:?}; vertices whose weights do not sum to 1: {low}; with more than 4 influences: {eight}", mdl.vertex_elements(li * lod.parts.len() + pi));
                    if !part.shapes.is_empty() {
                        println!("    shapes: {}", part.shapes.iter().map(|s| format!("{} ({} changed)", s.name, s.morphed_vertices.iter().zip(&part.vertices).filter(|(a, b)| a.position != b.position).count())).collect::<Vec<_>>().join(", "));
                    }
                    for (si, sm) in part.submeshes.iter().enumerate() {
                        let attrs: Vec<&str> = mdl.attribute_names.iter().enumerate().filter(|(bit, _)| sm.attribute_index_mask & (1 << bit) != 0).map(|(_, n)| n.as_str()).collect();
                        if !attrs.is_empty() {
                            println!("    submesh {si}: {} tris, attributes {:?}", sm.index_count / 3, attrs);
                        }
                    }
                    let uv1_used = part.vertices.iter().any(|v| v.uv1 != [0.0, 0.0]);
                    if uv1_used {
                        println!("    uv1 present");
                    }
                    for t in part.indices.chunks_exact(3).filter(|t| {
                        let uv = |i: u16| part.vertices.get(i as usize).map(|v| v.uv0).unwrap_or([0.0; 2]);
                        let (a, b, c) = (uv(t[0]), uv(t[1]), uv(t[2]));
                        ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() < 1e-7
                    }).take(2) {
                        for &i in t {
                            let v = &part.vertices[i as usize];
                            println!("      degenerate tri vertex {i}: uv0 {:?} uv1 {:?} col {:?}", v.uv0, v.uv1, v.color);
                        }
                    }
                    println!(
                        "  part {pi}: {:?} verts={} indices={} tris={} material={} submeshes={} shapes={} bone_table={} bbox={:?}",
                        part.part_type,
                        part.vertices.len(),
                        part.indices.len(),
                        part.indices.len() / 3,
                        part.material_index,
                        part.submeshes.len(),
                        part.shapes.len(),
                        part.bone_table.len(),
                        bbox
                    );
                    for sm in &part.submeshes {
                        println!(
                            "    submesh: offset={} count={} attr_mask=0x{:x}",
                            sm.index_offset, sm.index_count, sm.attribute_index_mask
                        );
                    }
                    if let Some(v) = part.vertices.first() {
                        println!("    v0: pos={:?} n={:?} uv0={:?} uv1={:?} col={:?} bw={:?} bid={:?}",
                            v.position, v.normal, v.uv0, v.uv1, v.color, v.bone_weight, v.bone_id);
                    }
                }
            }
        }
        Command::Mtrl { path, dyes } => {
            let m = load_mtrl(source, &path)?;
            println!("{path}: shader={}", m.shader_package_name);
            println!("textures ({}):", m.texture_paths.len());
            for (i, t) in m.texture_paths.iter().enumerate() {
                println!("  [{i}] {t} [{}]", if source.exists(t) { "ok" } else { "MISSING" });
            }
            println!("samplers ({}):", m.samplers.len());
            for s in &m.samplers {
                println!(
                    "  usage=0x{:08X} flags=0x{:08X} texture_index={}",
                    s.texture_usage, s.flags, s.texture_index
                );
            }
            println!("shader keys ({}):", m.shader_keys.len());
            for k in &m.shader_keys {
                println!("  category=0x{:08X} value=0x{:08X}", k.category, k.value);
            }
            println!("constants: {}", m.constants.len());
            for c in &m.constants {
                println!("  id=0x{:08X} n={} values={:?}", c.id, c.num_values, &c.values[..c.num_values.min(4) as usize]);
            }
            match &m.color_table {
                Some(physis::mtrl::ColorTable::LegacyColorTable(t)) => println!("color table: legacy, {} rows", t.rows.len()),
                Some(physis::mtrl::ColorTable::DawntrailColorTable(t)) => {
                    println!("color table: dawntrail, {} rows", t.rows.len());
                    for (i, r) in t.rows.iter().enumerate() {
                        println!("  row {i}: diffuse={:?} spec={:?} rough={} metal={} emissive={:?} unk1={} unk2={} unk3={} sheen=({},{},{}) unk4={} unk5={} aniso={} unk6={} sphere={} unk7={} unk8={} shader={} tile={} tile_alpha={}", r.diffuse_color, r.specular_color, r.roughness, r.metalness, r.emissive_color, r.unknown1, r.unknown2, r.unknown3, r.sheen_rate, r.sheen_tint, r.sheen_aperture, r.unknown4, r.unknown5, r.anisotropy, r.unknown6, r.sphere_mask, r.unknown7, r.unknown8, r.shader_index, r.tile_set, r.tile_alpha);
                    }
                }
                Some(_) => println!("color table: opaque"),
                None => println!("color table: none"),
            }
            match &m.color_dye_table {
                Some(physis::mtrl::ColorDyeTable::DawntrailColorDyeTable(d)) => {
                    let dyed: Vec<String> = d
                        .rows
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| r.template != 0)
                        .map(|(i, r)| {
                            let mut flags = Vec::new();
                            for (on, name) in [(r.diffuse, "diffuse"), (r.specular, "specular"), (r.emissive, "emissive"), (r.scalar3, "scalar3"), (r.metalness, "metal"), (r.roughness, "rough"), (r.sheen_rate, "sheen"), (r.sheen_tint_rate, "sheen-tint"), (r.sheen_aperture, "sheen-aperture"), (r.anisotropy, "aniso"), (r.sphere_map_index, "sphere"), (r.sphere_map_mask, "sphere-mask")] {
                                if on {
                                    flags.push(name);
                                }
                            }
                            format!("row {i}: template {} channel {} [{}]", r.template, r.channel, flags.join(" "))
                        })
                        .collect();
                    println!("dye table: dawntrail, {} dyeable rows", dyed.len());
                    for l in dyed {
                        println!("  {l}");
                    }
                }
                Some(physis::mtrl::ColorDyeTable::LegacyColorDyeTable(d)) => {
                    let dyed: Vec<String> = d.rows.iter().enumerate().filter(|(_, r)| r.template != 0).map(|(i, r)| format!("row {i}: template {} diffuse {} specular {} emissive {} gloss {} power {}", r.template, r.diffuse, r.specular, r.emissive, r.gloss, r.specular_strength)).collect();
                    println!("dye table: legacy, {} dyeable rows", dyed.len());
                    for l in dyed {
                        println!("  {l}");
                    }
                }
                Some(_) => println!("dye table: opaque"),
                None => {}
            }
            if let Some(dyes) = dyes {
                use ffl_ff14_chara::meta::stm::{self, DyeTemplates, StmFile};
                let v: Vec<u8> = dyes.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                let dyes = [v.first().copied().unwrap_or(0), v.get(1).copied().unwrap_or(0)];
                let templates = DyeTemplates {
                    legacy: source.read(stm::LEGACY_PATH).map(|b| StmFile::parse(&b)).transpose()?,
                    dawntrail: source.read(stm::DAWNTRAIL_PATH).map(|b| StmFile::parse(&b)).transpose()?,
                };
                println!(
                    "staining templates: legacy {} entries, dawntrail {} entries",
                    templates.legacy.as_ref().map(|t| t.templates.len()).unwrap_or(0),
                    templates.dawntrail.as_ref().map(|t| t.templates.len()).unwrap_or(0)
                );
                if let Some(table) = &m.color_table {
                    let dyed = ffl_ff14::convert::apply_dyes(table, m.color_dye_table.as_ref(), dyes, &templates);
                    match (table, &dyed) {
                        (physis::mtrl::ColorTable::DawntrailColorTable(a), physis::mtrl::ColorTable::DawntrailColorTable(b)) => {
                            for (i, (ra, rb)) in a.rows.iter().zip(&b.rows).enumerate() {
                                if ra.diffuse_color != rb.diffuse_color || ra.specular_color != rb.specular_color || ra.emissive_color != rb.emissive_color || ra.roughness != rb.roughness || ra.metalness != rb.metalness {
                                    println!("  dyed row {i}: diffuse {:?} -> {:?} spec {:?} -> {:?} emissive {:?} -> {:?} rough {} -> {} metal {} -> {}", ra.diffuse_color, rb.diffuse_color, ra.specular_color, rb.specular_color, ra.emissive_color, rb.emissive_color, ra.roughness, rb.roughness, ra.metalness, rb.metalness);
                                }
                            }
                        }
                        (physis::mtrl::ColorTable::LegacyColorTable(a), physis::mtrl::ColorTable::LegacyColorTable(b)) => {
                            for (i, (ra, rb)) in a.rows.iter().zip(&b.rows).enumerate() {
                                if ra.diffuse_color != rb.diffuse_color || ra.specular_color != rb.specular_color {
                                    println!("  dyed row {i}: diffuse {:?} -> {:?} spec {:?} -> {:?}", ra.diffuse_color, rb.diffuse_color, ra.specular_color, rb.specular_color);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Command::Tex { path, out } => {
            let t = load_tex(source, &path)?;
            println!(
                "{path}: {:?} {}x{}x{} mips={} bytes={}",
                t.format, t.width, t.height, t.depth, t.mip_levels, t.data.len()
            );
            if let Some(rgba) = t.to_rgba() {
                let n = (rgba.len() / 4).max(1);
                let mut sums = [0u64; 4];
                let mut mins = [255u8; 4];
                let mut maxs = [0u8; 4];
                for px in rgba.chunks_exact(4) {
                    for c in 0..4 {
                        sums[c] += px[c] as u64;
                        mins[c] = mins[c].min(px[c]);
                        maxs[c] = maxs[c].max(px[c]);
                    }
                }
                println!("channels mean R={} G={} B={} A={}  min={:?} max={:?}", sums[0] / n as u64, sums[1] / n as u64, sums[2] / n as u64, sums[3] / n as u64, mins, maxs);
            }
            if let Some(out) = out {
                let rgba = t.to_rgba().ok_or_else(|| anyhow!("format {:?} not decodable", t.format))?;
                std::fs::write(&out, &rgba).with_context(|| format!("writing {}", out.display()))?;
                println!("wrote {} bytes of RGBA8 ({}x{}) to {}", rgba.len(), t.width, t.height, out.display());
            }
        }
    }
    Ok(())
}

fn bounds(vertices: &[physis::model::Vertex]) -> Option<([f32; 3], [f32; 3])> {
    let mut it = vertices.iter();
    let first = it.next()?.position;
    let (mut lo, mut hi) = (first, first);
    for v in it {
        for k in 0..3 {
            lo[k] = lo[k].min(v.position[k]);
            hi[k] = hi[k].max(v.position[k]);
        }
    }
    Some((lo, hi))
}

fn print_layer(layer: &Layer, assets: bool, indent: &str) {
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for o in &layer.objects {
        let kind = format!("{:?}", o.data);
        let kind = kind.split(['(', ' ', '{']).next().unwrap_or("?").to_string();
        *counts.entry(kind).or_default() += 1;
    }
    println!(
        "{indent}layer {:?} id={} festival={} housing={} objects={} {:?}",
        layer.header.name.value,
        layer.header.layer_id,
        layer.header.festival_id,
        layer.header.is_housing,
        layer.objects.len(),
        counts
    );
    if assets {
        for o in &layer.objects {
            match &o.data {
                LayerEntryData::BG(bg) => println!(
                    "{indent}  BG {} pos={:?} rot={:?} scl={:?} col={:?} vis={}",
                    bg.asset_path.value, o.transform.translation, o.transform.rotation, o.transform.scale, bg.collision_type, bg.is_visible
                ),
                LayerEntryData::SharedGroup(sg) => println!(
                    "{indent}  SG {} pos={:?} rot={:?} scl={:?}",
                    sg.asset_path.value, o.transform.translation, o.transform.rotation, o.transform.scale
                ),
                LayerEntryData::Sound(s) => println!(
                    "{indent}  Sound {} pos={:?} scl(inner,height,max)={:?} param={}",
                    if s.asset_path.value.is_empty() { "<obstruction probe>" } else { s.asset_path.value.as_str() },
                    o.transform.translation,
                    o.transform.scale,
                    s.sound_effect_param
                ),
                LayerEntryData::EnvSet(e) => println!(
                    "{indent}  EnvSet {:?} pos={:?} scl={:?} prio={} range={} reverb={} filter={} sound={}",
                    e.shape, o.transform.translation, o.transform.scale, e.priority, e.effective_range, e.reverb, e.filter, e.sound_asset_path.value
                ),
                LayerEntryData::MapRange(r) => println!(
                    "{indent}  MapRange {:?} pos={:?} scl={:?} prio={} spot={} bgm={} bgm_enabled={} zone_in_only={}",
                    r.parent_data.trigger_box_shape, o.transform.translation, o.transform.scale, r.parent_data.priority, r.place_name_spot, r.bgm, r.bgm_enabled, r.bgm_play_zone_in_only
                ),
                _ => {}
            }
        }
    }
}

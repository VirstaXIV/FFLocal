//! Character-related CLI commands.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::Subcommand;
use ffl_ff14_assets::{AssetSource, GameData};
use ffl_ff14_chara::appearance::{default_chara_dat_paths, default_gearset_paths};
use ffl_ff14_chara::gearset::GearsetSlot;
use ffl_ff14_chara::meta::{CmpFile, EqdpFile, ImcFile};
use ffl_ff14_chara::race::paths;
use ffl_ff14_chara::{AppearanceSource, CharaDatSource, CharacterResolver, GearSlot, GearsetSource, RaceCode, VanillaSource};

#[derive(Subcommand)]
pub enum CharaCommand {
    /// Parse a character-creator save (default: the first FFXIV_CHARA_*.dat found).
    Chara { path: Option<PathBuf> },
    /// Parse a GEARSET.DAT (default: the most recently modified character's) and list its sets.
    Gearset {
        path: Option<PathBuf>,
        /// Only print this set index.
        #[arg(long)]
        set: Option<u8>,
    },
    /// Resolve the full model/material/skeleton list for an appearance.
    ResolveCharacter {
        #[arg(long)]
        chara: Option<PathBuf>,
        #[arg(long)]
        gearset: Option<PathBuf>,
        #[arg(long)]
        set: Option<u8>,
        /// Skip gear entirely.
        #[arg(long)]
        no_gear: bool,
    },
    /// Dump human.cmp colours/scales for a chara save.
    Cmp { path: Option<PathBuf> },
    /// Dump EQDP bits for a race code (e.g. 801) and set id.
    Eqdp {
        race: u16,
        set: u16,
        #[arg(long)]
        accessory: bool,
    },
    /// Compare EQDP bits against model file existence for sets 0..N (bit semantics check).
    EqdpCheck {
        race: u16,
        #[arg(long, default_value_t = 300)]
        sets: u16,
    },
    /// Dump an IMC file's entries for a variant.
    Imc { path: String, variant: u16 },
}

fn chara_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    explicit
        .or_else(|| default_chara_dat_paths().into_iter().next())
        .ok_or_else(|| anyhow!("no FFXIV_CHARA_*.dat found; pass a path"))
}

fn gearset_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    explicit
        .or_else(|| default_gearset_paths().into_iter().next())
        .ok_or_else(|| anyhow!("no GEARSET.DAT found; pass a path"))
}

pub fn run(game: &GameData, cmd: CharaCommand) -> Result<()> {
    let source: &dyn AssetSource = game.source();
    match cmd {
        CharaCommand::Chara { path } => {
            let path = chara_path(path)?;
            let data = CharaDatSource::read(&path)?;
            let c = &data.customize;
            let race = RaceCode::from_customize(c.race, c.tribe, &c.gender);
            println!("{}: version {} comment {:?}", path.display(), data.version, data.comment);
            println!("race {:?} tribe {:?} gender {:?} -> {}", c.race, c.tribe, c.gender, race.map(|r| r.to_string()).unwrap_or("?".into()));
            println!("{c:#?}");
        }
        CharaCommand::Gearset { path, set } => {
            let path = gearset_path(path)?;
            let sets = GearsetSource::read(&path)?;
            println!("{}: current set {} ({} sets)", path.display(), sets.current, sets.sets.len());
            let items = game.excel.sheet("Item")?;
            for gs in &sets.sets {
                if let Some(s) = set
                    && gs.index != s
                {
                    continue;
                }
                println!("set {:2} {:?} job {} facewear {}", gs.index, gs.name, gs.class_job, gs.facewear);
                if set.is_some() || sets.sets.len() <= 3 {
                    for slot in GearsetSlot::ALL {
                        if let Some(it) = gs.item(slot) {
                            let name = items.string(it.item_id, "Name").unwrap_or_else(|_| "?".into());
                            let glam = if it.glamour_id != 0 {
                                format!(" glamour {} {:?}", it.glamour_id, items.string(it.glamour_id, "Name").unwrap_or_default())
                            } else {
                                String::new()
                            };
                            println!("  {:<12} {:>6}{} {:?}{}", format!("{slot:?}"), it.item_id, if it.hq { " HQ" } else { "   " }, name, glam);
                        }
                    }
                }
            }
        }
        CharaCommand::ResolveCharacter { chara, gearset, set, no_gear } => {
            let chara = CharaDatSource { path: chara_path(chara)? };
            let gearset = if no_gear {
                None
            } else {
                Some(GearsetSource { path: gearset_path(gearset)?, index: set })
            };
            let appearance = VanillaSource { chara, gearset }.load()?;
            let resolver = CharacterResolver::new(source, &game.excel);
            let set = resolver.resolve(&appearance)?;
            print!("{set}");
            println!("skeleton {}", if source.exists(&set.skeleton_path) { "ok" } else { "MISSING" });
            for n in &set.notes {
                println!("note: {n}");
            }
            let missing = set.missing();
            println!("{} missing files", missing.len());
            if !missing.is_empty() {
                std::process::exit(1);
            }
        }
        CharaCommand::Cmp { path } => {
            let path = chara_path(path)?;
            let data = CharaDatSource::read(&path)?;
            let c = &data.customize;
            let bytes = source.read(paths::CMP).ok_or_else(|| anyhow!("missing {}", paths::CMP))?;
            println!("{}: {} bytes", paths::CMP, bytes.len());
            let cmp = CmpFile::parse(&bytes)?;
            println!("skin      {} (idx {})", cmp.skin_color(c.tribe, &c.gender, c.skin_tone).hex(), c.skin_tone);
            println!("hair      {} (idx {})", cmp.hair_color(c.tribe, &c.gender, c.hair_tone).hex(), c.hair_tone);
            println!("highlight {} (idx {}, enabled {})", cmp.hair_highlight_color(c.highlights).hex(), c.highlights, c.enable_highlights);
            println!("eye R     {} (idx {})", cmp.eye_color(c.right_eye_color).hex(), c.right_eye_color);
            println!("eye L     {} (idx {})", cmp.eye_color(c.left_eye_color).hex(), c.left_eye_color);
            println!("lips      {} (raw {})", cmp.lip_color(c.lips_tone_fur_pattern).hex(), c.lips_tone_fur_pattern);
            println!("feature   {} (idx {})", cmp.feature_color(c.facial_feature_color).hex(), c.facial_feature_color);
            println!("scale     {:?}", cmp.scale(c.tribe));
            println!("height    {} -> scale {:.3}", c.height, cmp.height_scale(c.tribe, &c.gender, c.height));
        }
        CharaCommand::Eqdp { race, set, accessory } => {
            let path = paths::eqdp(RaceCode(race), accessory);
            let bytes = source.read(&path).ok_or_else(|| anyhow!("missing {path}"))?;
            let eqdp = EqdpFile::parse(&bytes)?;
            println!("{path}: entry for set {set} = 0b{:010b}", eqdp.entry(set));
            for slot in GearSlot::ALL {
                if slot.is_accessory() != accessory {
                    continue;
                }
                let (b1, b2) = eqdp.bits(set, slot);
                println!("  {:<7} bit1(material)={b1} bit2(model)={b2}", slot.name());
            }
        }
        CharaCommand::EqdpCheck { race, sets } => {
            let rc = RaceCode(race);
            let bytes = source.read(&paths::eqdp(rc, false)).ok_or_else(|| anyhow!("missing eqdp"))?;
            let eqdp = EqdpFile::parse(&bytes)?;
            let mut agree_b1 = 0;
            let mut agree_b2 = 0;
            let mut total = 0;
            for set in 0..sets {
                for slot in [GearSlot::Head, GearSlot::Body, GearSlot::Hands, GearSlot::Legs, GearSlot::Feet] {
                    let exists = source.exists(&paths::gear_mdl(rc, slot, set));
                    let (b1, b2) = eqdp.bits(set, slot);
                    total += 1;
                    if b1 == exists {
                        agree_b1 += 1;
                    }
                    if b2 == exists {
                        agree_b2 += 1;
                    }
                }
            }
            println!("{rc}: over {total} set/slot pairs, bit1 matches model existence {agree_b1}, bit2 matches {agree_b2}");
        }
        CharaCommand::Imc { path, variant } => {
            let bytes = source.read(&path).ok_or_else(|| anyhow!("missing {path}"))?;
            let imc = ImcFile::parse(&bytes)?;
            println!("{path}: {} variants", imc.variant_count());
            for part in 0..5 {
                if let Some(e) = imc.entry(part, variant) {
                    println!("  part {part}: {e:?}");
                }
            }
        }
    }
    Ok(())
}

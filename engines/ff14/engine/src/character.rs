//! Preset → `ffl_core::CharacterModel`: resolve, load, deform, materials and clips.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use ffl_core::{ActionCategory, ActionDef, AddonDef, AddonKind, AddonPlacement, AddonRule, AddonTransition, Attach, BoneData, CharacterModel, CharacterPart, CharacterPreset, Clip, ClipEvent, ClipEventKind, ClipPose, ContentReport, Foot, FootstepSet, Gait, GroundCondition, Locomotion, MaterialDesc, ModelData, SkeletonData, SoundCue, SurfaceKind, VoiceSet};
use ffl_ff14_assets::{AssetSource, GameData};
use ffl_ff14_assets::loaders::{load_mdl, load_mtrl, load_pbd, load_scd, load_sklb};
use ffl_ff14_chara::appearance::{default_chara_dat_paths, default_gearset_paths};
use ffl_ff14_chara::meta::atch::{self, AtchFile};
use ffl_ff14_chara::meta::CmpFile;
use ffl_ff14_chara::meta::stm::{self, DyeTemplates, StmFile};
use ffl_ff14_chara::meta::tmb::{TmbInfo, embedded_timelines};
use ffl_ff14_assets::music::scd_key;
use ffl_ff14_chara::resolver::item_models;
use ffl_ff14_chara::race::paths;
use ffl_ff14_chara::{AppearanceSource, CharaDatSource, CharacterResolver, GearsetSource, PartKind, VanillaSource};
use glam::{Mat4, Quat, Vec3};

use crate::convert::{CharacterColors, TextureCache, character_material, keyed, prepare_model_shaped, report_material};

const CLIP_FPS: f32 = 30.0;

/// Where a character's look comes from: the preset's own settings (imported, editable) or,
/// for presets made before settings-based presets existed, the game's save files.
pub enum AppearanceSpec {
    Settings(std::collections::BTreeMap<String, String>),
    Files { chara: PathBuf, gearset: Option<PathBuf>, set: Option<u8> },
}

pub struct CharacterRequest {
    pub appearance: AppearanceSpec,
    /// Mod packs this preset asks for (in addition to the globally enabled ones).
    pub mods: Vec<String>,
    /// `ClassJob` row: picks the stance/victory animation set ahead of the weapon type.
    pub job: Option<u32>,
}

/// Preset key holding the `;`-separated mod pack ids.
pub const MODS_KEY: &str = "mods";
/// Preset key holding the `ClassJob` row id.
pub const JOB_KEY: &str = "job";

impl CharacterRequest {
    /// Interpret a preset's settings (see `Ff14Engine::preset_fields`).
    pub fn from_preset(preset: &CharacterPreset) -> Result<Self> {
        let mods = preset
            .get(MODS_KEY)
            .map(|m| m.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
            .unwrap_or_default();
        let job = preset.get(JOB_KEY).and_then(|j| j.trim().parse::<u32>().ok()).filter(|j| *j != 0);
        if ffl_ff14_chara::preset::has_customize(&preset.settings) {
            return Ok(Self {
                appearance: AppearanceSpec::Settings(preset.settings.clone()),
                mods,
                job,
            });
        }
        let chara = preset
            .get("chara")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(|| default_chara_dat_paths().into_iter().next())
            .ok_or_else(|| anyhow!("no FFXIV_CHARA_*.dat found"))?;
        let use_gear = preset.get("gear").map(|v| v != "false" && v != "0").unwrap_or(true);
        let gearset = if use_gear {
            preset
                .get("gearset")
                .map(PathBuf::from)
                .filter(|p| p.is_file())
                .or_else(|| default_gearset_paths().into_iter().next())
        } else {
            None
        };
        let set = preset.get("set").and_then(|s| s.parse::<u8>().ok());
        Ok(Self {
            appearance: AppearanceSpec::Files { chara, gearset, set },
            mods,
            job,
        })
    }

    pub fn appearance(&self) -> Result<ffl_ff14_chara::Appearance> {
        match &self.appearance {
            AppearanceSpec::Settings(s) => ffl_ff14_chara::preset::SettingsSource(s).load(),
            AppearanceSpec::Files { chara, gearset, set } => VanillaSource {
                chara: CharaDatSource { path: chara.clone() },
                gearset: gearset.clone().map(|path| GearsetSource { path, index: *set }),
            }
            .load(),
        }
    }
}

/// `source` is the game plus whatever mod packs apply; every file read is recorded in the
/// model's content report with its origin.
pub fn load_character(game: &GameData, source: &dyn AssetSource, cache: &TextureCache, req: &CharacterRequest, name: &str) -> Result<CharacterModel> {
    let appearance = req.appearance()?;
    let resolver = CharacterResolver::new(source, &game.excel);
    let set = resolver.resolve(&appearance)?;
    let meta = source.meta_overrides();
    if !meta.is_empty() {
        tracing::debug!("mod metadata edits: {} eqdp, {} imc, {} est, {} attributes", meta.eqdp.len(), meta.imc.len(), meta.est.len(), meta.atr.len());
    }
    let mut notes = set.notes.clone();
    let mut content = ContentReport::default();
    content.push(&set.skeleton_path, source.origin(&set.skeleton_path));

    let skeleton = load_sklb(source, &set.skeleton_path).context("skeleton")?;
    let pbd = load_pbd(source, paths::PBD).ok();
    let cmp = source.read(paths::CMP).and_then(|b| CmpFile::parse(&b).ok());
    content.push(paths::PBD, source.origin(paths::PBD));
    content.push(paths::CMP, source.origin(paths::CMP));
    let c = &appearance.customize;
    let rgb = |v: [f32; 4]| [v[0], v[1], v[2]];
    // Advanced colours (Glamourer parameters, `color.*` preset keys) override the palette.
    let settings = match &req.appearance {
        AppearanceSpec::Settings(s) => Some(s),
        _ => None,
    };
    let param = |key: &str| settings.and_then(|s| s.get(key)).and_then(|v| ffl_ff14_assets::dalamud::parse_parameter(v));
    let rgb_of = |key: &str, palette: Option<[f32; 4]>, fallback: [f32; 3]| param(key).or(palette).map(rgb).unwrap_or(fallback);
    let lip = param("color.lip").or_else(|| cmp.as_ref().map(|m| m.lip_color(c.lips_tone_fur_pattern).to_f32()));
    let decal = param("color.decal").or_else(|| cmp.as_ref().map(|m| m.face_paint_color(c.face_paint_color).to_f32()));
    let colors = CharacterColors {
        skin: rgb_of("color.skin", cmp.as_ref().map(|m| m.skin_color(c.tribe, &c.gender, c.skin_tone).to_f32()), [0.8, 0.6, 0.5]),
        lip: lip.map(rgb).unwrap_or([0.6, 0.3, 0.3]),
        lip_strength: lip.map(|v| v[3]).unwrap_or(0.7),
        hair: rgb_of("color.hair", cmp.as_ref().map(|m| m.hair_color(c.tribe, &c.gender, c.hair_tone).to_f32()), [0.3, 0.2, 0.1]),
        highlight: rgb_of("color.highlight", cmp.as_ref().map(|m| m.hair_highlight_color(c.highlights).to_f32()), [0.5; 3]),
        highlights_enabled: c.enable_highlights,
        eye_left: rgb_of("color.eye_left", cmp.as_ref().map(|m| m.eye_color(c.left_eye_color).to_f32()), [0.3, 0.5, 0.3]),
        eye_right: rgb_of("color.eye_right", cmp.as_ref().map(|m| m.eye_color(c.right_eye_color).to_f32()), [0.3, 0.5, 0.3]),
        feature: rgb_of("color.feature", cmp.as_ref().map(|m| m.feature_color(c.facial_feature_color).to_f32()), [0.5; 3]),
        decal: decal.unwrap_or([0.0, 0.0, 0.0, 1.0]),
        decal_uv: [param("decal_uv_scale").map(|v| v[0]).unwrap_or(1.0), param("decal_uv_offset").map(|v| v[0]).unwrap_or(0.0)],
    };
    // Dyes: the staining templates the colour tables' dye entries point at.
    let templates = DyeTemplates {
        legacy: source.read(stm::LEGACY_PATH).and_then(|b| StmFile::parse(&b).map_err(|e| notes.push(format!("{}: {e:#}", stm::LEGACY_PATH))).ok()),
        dawntrail: source.read(stm::DAWNTRAIL_PATH).and_then(|b| StmFile::parse(&b).map_err(|e| notes.push(format!("{}: {e:#}", stm::DAWNTRAIL_PATH))).ok()),
    };
    for p in [stm::LEGACY_PATH, stm::DAWNTRAIL_PATH] {
        if source.exists(p) {
            content.push(p, source.origin(p));
        }
    }
    // Face paint: a single-channel decal projected through the face's second UV set.
    let decal_texture = (c.face_paint != 0).then(|| paths::face_decal(c.face_paint)).and_then(|p| {
        let t = cache.get(source, &p, false);
        if t.is_some() {
            content.push(&p, source.origin(&p));
        } else {
            notes.push(format!("no face paint texture {p}"));
        }
        t
    });
    let scale = cmp.as_ref().map(|m| m.height_scale(c.tribe, &c.gender, c.height)).unwrap_or(1.0);

    let mut parts = Vec::new();
    let mut weapon_parts: Vec<(PartKind, Option<u32>)> = Vec::new();
    for part in &set.parts {
        if !part.mdl_exists {
            notes.push(format!("missing model {}", part.mdl_path));
            continue;
        }
        let mdl = match load_mdl(source, &part.mdl_path) {
            Ok(m) => m,
            Err(err) => {
                notes.push(format!("{err:#}"));
                continue;
            }
        };
        content.push(&part.mdl_path, source.origin(&part.mdl_path));
        // Faces: the character-creator options are shape keys on the face model, and the
        // facial-feature marks are attribute submeshes (`atr_fv_a`..`g`); without either the
        // face is the neutral mesh with every mark shown.
        let face_shapes = if matches!(part.kind, PartKind::Face) { face_shape_keys(c, &mdl) } else { Vec::new() };
        let mut model = prepare_model_shaped(&keyed(source, &part.mdl_path), &mdl, 0, &face_shapes);
        if !face_shapes.is_empty() {
            notes.push(format!("face shapes {}", face_shapes.join(" ")));
        }
        let mut mask = if matches!(part.kind, PartKind::Face) { face_attribute_mask(c, &mdl) & part.attribute_mask } else { part.attribute_mask };
        // Mods switch named attributes globally (`atrx_*` on modded bodies hide body parts).
        if !meta.atr.is_empty() {
            for (bit, name) in mdl.attribute_names.iter().enumerate().take(16) {
                match meta.atr.get(name) {
                    Some(true) => mask |= 1 << bit,
                    Some(false) => mask &= !(1 << bit),
                    None => {}
                }
            }
        }
        if mask != 0xFFFF {
            apply_attribute_mask(&mut model, &mdl, mask);
        }
        let is_weapon = matches!(part.kind, PartKind::MainHand | PartKind::OffHand);
        if part.model_race != set.race
            && !is_weapon
            && let Some(pbd) = &pbd
        {
            match pbd.get_deform_matrices(set.race.0, part.model_race.0) {
                Some(m) => apply_racial_deform(&mut model, &m),
                None => notes.push(format!("no deform {} -> {} for {}", part.model_race, set.race, part.mdl_path)),
            }
        }
        let is_face = matches!(part.kind, PartKind::Face);
        model.materials = part
            .materials
            .iter()
            .map(|rm| match load_mtrl(source, &rm.mtrl_path) {
                Ok(m) => {
                    report_material(&mut content, source, &rm.mtrl_path, &m);
                    character_material(source, cache, &keyed(source, &rm.mtrl_path), &m, &colors, is_face, if is_face { decal_texture.clone() } else { None }, part.dyes, &templates)
                }
                Err(err) => {
                    notes.push(format!("{err:#}"));
                    MaterialDesc::unlit(&rm.mtrl_path, [1.0, 0.0, 1.0, 1.0])
                }
            })
            .collect();
        let attach = match part.kind {
            PartKind::MainHand => Attach::Bone("n_buki_r".into()),
            PartKind::OffHand => Attach::Bone("n_buki_l".into()),
            _ => Attach::Skinned,
        };
        let addon = match part.kind {
            PartKind::MainHand => Some(WEAPON_MAIN),
            PartKind::OffHand => Some(WEAPON_OFF),
            _ => None,
        };
        if matches!(part.kind, PartKind::MainHand | PartKind::OffHand) {
            weapon_parts.push((part.kind.clone(), part.item_id));
        }
        parts.push(CharacterPart {
            name: part.kind.name(),
            model,
            attach,
            addon: addon.map(str::to_string),
        });
    }

    // Weapon placements from the race's attach-offset table: entry 0 in hand, entry 1 sheathed.
    let atch = source.read(&paths::atch(set.race)).and_then(|b| AtchFile::parse(&b).map_err(|e| notes.push(format!("atch: {e:#}"))).ok());
    if source.exists(&paths::atch(set.race)) {
        content.push(&paths::atch(set.race), source.origin(&paths::atch(set.race)));
    }
    let main_item = weapon_parts.iter().find(|(k, _)| *k == PartKind::MainHand).and_then(|(_, id)| *id);
    let main_type = main_item.and_then(|id| item_models(&game.excel, id).ok()).and_then(|i| atch::main_hand_type(i.ui_category));
    // (attach type, mirrored): a two-handed weapon's secondary model without a type of its
    // own (the left fist of a pair of knuckles) uses the main type's entries on the other
    // side — the ATCH keeps entry 3 for the left hand, and the sheathed entry mirrors.
    let weapon_type = |kind: &PartKind| -> Option<(&'static str, bool)> {
        match kind {
            PartKind::MainHand => main_type.map(|t| (t, false)),
            PartKind::OffHand => {
                let item = weapon_parts.iter().find(|(k, _)| *k == PartKind::OffHand).and_then(|(_, id)| *id);
                if item.is_some() && item == main_item {
                    main_type.and_then(atch::sub_model_type).map(|t| (t, false)).or(main_type.map(|t| (t, true)))
                } else {
                    item.and_then(|id| item_models(&game.excel, id).ok()).and_then(|i| atch::off_hand_type(i.ui_category).or(atch::main_hand_type(i.ui_category))).map(|t| (t, false))
                }
            }
            _ => None,
        }
    };
    /// The other side's bone and transform: `_l` ↔ `_r`, X offset negated, the Y and Z Euler
    /// angles negated (a mirror across the body's plane).
    fn mirrored(entry: &ffl_ff14_chara::meta::atch::AtchEntry) -> ffl_ff14_chara::meta::atch::AtchEntry {
        let bone = if let Some(b) = entry.bone.strip_suffix("_l") {
            format!("{b}_r")
        } else if let Some(b) = entry.bone.strip_suffix("_r") {
            format!("{b}_l")
        } else {
            entry.bone.clone()
        };
        ffl_ff14_chara::meta::atch::AtchEntry {
            bone,
            scale: entry.scale,
            offset: [-entry.offset[0], entry.offset[1], entry.offset[2]],
            rotation: [entry.rotation[0], -entry.rotation[1], -entry.rotation[2]],
        }
    }
    let placement = |entry: &ffl_ff14_chara::meta::atch::AtchEntry, id: &str, name: &str| -> AddonPlacement {
        // Attach-offset Euler angles apply X first (verified: the other order put the
        // greatstaff head-down on the back).
        let q = Quat::from_euler(glam::EulerRot::ZYX, entry.rotation[2], entry.rotation[1], entry.rotation[0]);
        AddonPlacement {
            id: id.into(),
            name: name.into(),
            attach: Attach::Bone(entry.bone.clone()),
            offset: ffl_core::Trs {
                translation: entry.offset,
                rotation: [q.x, q.y, q.z, q.w],
                scale: [entry.scale; 3],
            },
        }
    };
    // Actions that keep the weapon drawn (battle stance, victory pose).
    let anim_set = animation_set(req.job, main_type);
    notes.push(format!("animation set: {anim_set:?} (job {:?}, weapon type {:?})", req.job, main_type));
    let actions = list_actions(game, set.race, anim_set.map(|s| s.stance));
    let active_actions: Vec<String> = actions
        .iter()
        .filter(|a| {
            let n = a.name.to_lowercase();
            n.contains("battle stance") || n == "victory" || n.contains("victory pose")
        })
        .map(|a| a.id.clone())
        .collect();
    let mut addons: Vec<AddonDef> = [(WEAPON_MAIN, "Main hand", PartKind::MainHand), (WEAPON_OFF, "Off hand", PartKind::OffHand)]
        .into_iter()
        .filter(|(id, _, _)| parts.iter().any(|p| p.addon.as_deref() == Some(id)))
        .map(|(id, name, kind)| {
            let mut placements = Vec::new();
            if let (Some(a), Some((t, mirror))) = (&atch, weapon_type(&kind)) {
                // Entry 3 is the "switch hand" state: the same weapon in the left hand.
                let drawn = if mirror {
                    a.entry(t, 3).filter(|e| !e.bone.is_empty()).cloned().or_else(|| a.entry(t, 0).map(mirrored))
                } else {
                    a.entry(t, 0).cloned()
                };
                if let Some(e) = drawn.filter(|e| !e.bone.is_empty()) {
                    placements.push(placement(&e, "drawn", "In hand"));
                }
                let stowed = a.entry(t, 1).map(|e| if mirror { mirrored(e) } else { e.clone() });
                if let Some(e) = stowed.filter(|e| !e.bone.is_empty()) {
                    placements.push(placement(&e, "sheathed", "Sheathed"));
                }
                notes.push(format!("{name}: attach type {t}{}, {} placements", if mirror { " (mirrored)" } else { "" }, placements.len()));
            } else {
                notes.push(format!("{name}: no attach type known (item {:?}); stays in hand, hidden when stowed", weapon_parts.iter().find(|(k, _)| *k == kind).and_then(|(_, i)| *i)));
            }
            let has = |p: &str| placements.iter().any(|x| x.id == p);
            AddonDef {
                id: id.into(),
                name: name.into(),
                kind: AddonKind::Weapon,
                enabled: true,
                // The game sheathes weapons for anything but idles, locomotion and stances.
                rules: vec![AddonRule::StowDuringActions],
                active: has("drawn").then(|| "drawn".to_string()),
                stowed: has("sheathed").then(|| "sheathed".to_string()),
                placements,
                active_actions: active_actions.clone(),
                activate: None,
                stow: None,
            }
        })
        .collect();

    let mut bones: Vec<BoneData> = skeleton
        .bones
        .iter()
        .map(|b| BoneData {
            name: b.name.clone(),
            parent: (b.parent_index >= 0).then_some(b.parent_index as usize),
            translation: b.position,
            rotation: b.rotation,
            scale: b.scale,
        })
        .collect();
    // The base skeleton has no face, hair or gear-specific bones: the extra skeleton tables
    // (`chara/xls/charadb/*.est`) name the skeleton set of this face, hairstyle, head gear
    // and body gear, each a sklb whose bones hang off base bones (j_kao for the face).
    // Without them every face bone collapsed onto the head: a flattened face with the bind
    // pose's open mouth.
    let extras = extra_skeletons(source, &set, &appearance.customize, &mut notes, &mut content);
    for (label, extra) in &extras {
        let added = merge_skeleton(&mut bones, extra);
        notes.push(format!("{label}: {added} bones"));
    }

    let mut clips = HashMap::new();
    // Packs authored for another skeleton (the c0101 base holds the weapon folders) go through
    // the mapper embedded in this character's skeleton.
    let retarget = Retarget::from_skeleton(&skeleton);
    let load_pack = |path: &str, wanted: Option<&[&str]>, clips: &mut HashMap<String, Clip>, notes: &mut Vec<String>| {
        let foreign = path_race(path) != Some(set.race.0);
        match (&retarget, foreign) {
            (Some(rt), true) => load_clips_with(source, path, clips, notes, wanted, Some(rt)),
            (None, true) => notes.push(format!("{path}: authored for another skeleton and this skeleton has no mapper; skipped")),
            _ => load_clips_with(source, path, clips, notes, wanted, None),
        }
    };
    for file in ["idle", "move_a"] {
        match resident_pap(source, set.race, "bt_common", file) {
            Some(path) => {
                content.push(&path, source.origin(&path));
                load_pack(&path, None, &mut clips, &mut notes);
            }
            None => notes.push(format!("no bt_common/resident/{file}.pap for {}", set.race)),
        }
    }
    // Battle mode (weapon drawn): `cbbm_*` idle and locomotion from the weapon folder.
    let mut armed_locomotion = None;
    if let Some(f) = anim_set.map(|s| s.movement) {
        for file in ["idle", "move_a"] {
            match resident_pap(source, set.race, f, file) {
                Some(path) => {
                    content.push(&path, source.origin(&path));
                    load_pack(&path, None, &mut clips, &mut notes);
                }
                None => notes.push(format!("no {f}/resident/{file}.pap for {} or its fallbacks", set.race)),
            }
        }
        let has = |n: &str| clips.contains_key(n).then(|| n.to_string());
        if has("cbbm_id0").is_some() {
            armed_locomotion = Some(Locomotion {
                idle: has("cbbm_id0"),
                walk: has("cbbm_01f_lp0"),
                run: has("cbbm_02f_lp0"),
                sprint: has("cbbm_sprint_lp0"),
                fall: has("cbbm_jump_2"),
                jump: has("cbbm_jump_1"),
                land: has("cbbm_jump_3"),
                ..Default::default()
            });
        }
    }
    // Draw / sheathe transitions live in the weapon folder's `sub.pap` (`cbbp_a_activ`,
    // `cbbp_a_deact`); most races only have them under c0101 (same bone order).
    if let Some(f) = anim_set.map(|s| s.draw) {
        // The timelines name the clips and the frame the weapon changes hands (C014).
        let mut timeline = |file: &str| -> Option<TmbInfo> {
            let path = format!("chara/action/battle/{file}.tmb");
            let bytes = source.read(&path)?;
            content.push(&path, source.origin(&path));
            Some(TmbInfo::parse(&bytes))
        };
        let start = timeline("battle_start");
        let end = timeline("battle_end");
        drop(timeline);
        let draw_clip = start.as_ref().and_then(|t| t.animation.clone()).unwrap_or_else(|| "cbbp_a_activ".into());
        let sheathe_clip = end.as_ref().and_then(|t| t.animation.clone()).unwrap_or_else(|| "cbbp_a_deact".into());
        match resident_pap(source, set.race, f, "sub") {
            Some(path) => {
                content.push(&path, source.origin(&path));
                load_pack(&path, Some(&[draw_clip.as_str(), sheathe_clip.as_str()]), &mut clips, &mut notes);
                let draw_at = start.as_ref().and_then(|t| t.main_hand_switch(true));
                let sheathe_at = end.as_ref().and_then(|t| t.main_hand_switch(false));
                for a in addons.iter_mut() {
                    a.activate = clips.contains_key(&draw_clip).then(|| AddonTransition {
                        clip: draw_clip.clone(),
                        switch_at: draw_at.unwrap_or(0.2),
                    });
                    a.stow = clips.contains_key(&sheathe_clip).then(|| AddonTransition {
                        clip: sheathe_clip.clone(),
                        switch_at: sheathe_at.unwrap_or(0.37),
                    });
                }
                notes.push(format!("weapon transitions from {path}: draw {draw_clip} at {draw_at:?} s, sheathe {sheathe_clip} at {sheathe_at:?} s"));
            }
            None => notes.push(format!("no {f}/resident/sub.pap for {} or its fallbacks", set.race)),
        }
    }
    // Verification aid: `FFL_TEST_RETARGET=1` also loads the base skeleton's normal idle and
    // run through the mapper as `cbnm_id0@base` / `cbnm_02f_lp0@base` (compare with the native
    // clips: same proportions, similar pose).
    if std::env::var_os("FFL_TEST_RETARGET").is_some() {
        match Retarget::from_skeleton(&skeleton) {
            Some(rt) => {
                let base = "chara/human/c0101/animation/a0001/bt_common/resident";
                let mut extra = HashMap::new();
                for file in ["idle", "move_a"] {
                    load_clips_with(source, &format!("{base}/{file}.pap"), &mut extra, &mut notes, Some(&["cbnm_id0", "cbnm_02f_lp0"]), Some(&rt));
                }
                for (n, c) in extra {
                    clips.insert(format!("{n}@base"), c);
                }
                notes.push(format!("retarget test: mapper from {:?}", rt.source_name));
            }
            None => notes.push("retarget test: skeleton has no mapper".into()),
        }
    }
    attach_standalone_events(source, &mut clips, &["cbnm_jump_3", "cbbm_jump_3"], "chara/action/normal/jump_landing.tmb", &mut content);
    // The face's resting expression and blink live in the face skeleton set's resident pack.
    let mut overlays = Vec::new();
    let mut blink = None;
    if let Some((face_set, face_skeleton)) = extras.iter().find_map(|(label, s)| label.strip_prefix("face skeleton f").and_then(|n| n.parse::<u16>().ok()).map(|n| (n, s))) {
        let path = format!("chara/human/c{}/animation/f{face_set:04}/resident/face.pap", set.race.code());
        if source.exists(&path) && std::env::var_os("FFL_NO_FACE_PAP").is_none() {
            content.push(&path, source.origin(&path));
            // Face packs index the face skeleton's bone order, not the base skeleton's.
            let bone_map: Vec<Option<usize>> = face_skeleton.bones.iter().map(|b| bones.iter().position(|x| x.name == b.name)).collect();
            load_clips_mapped(source, &path, &mut clips, &mut notes, Some(&["cfxf_base", "cfxl_lip_nor1", "cfxb_blink1"]), &bone_map);
            // The resting expression (identity deltas) and the closed-lips layer the game
            // keeps on an idle face (the bind pose's mouth is open).
            for n in ["cfxf_base"] {
                if let Some(c) = clips.get(n) {
                    notes.push(format!("face overlay {n}: {} tracks, additive {}", c.track_to_bone.len(), c.additive));
                    overlays.push(n.to_string());
                }
            }
            if clips.contains_key("cfxb_blink1") {
                blink = Some("cfxb_blink1".to_string());
            }
        } else {
            notes.push(format!("no face animation pack at {path}"));
        }
    }
    let has = |n: &str| clips.contains_key(n).then(|| n.to_string());
    let locomotion = Locomotion {
        idle: has("cbnm_id0"),
        walk: has("cbnm_01f_lp0"),
        run: has("cbnm_02f_lp0"),
        sprint: has("cbnm_sprint_lp0"),
        fall: has("cbnm_jump_2"),
        jump: has("cbnm_jump_1"),
        land: has("cbnm_jump_3"),
        // Swimming clips: the game keeps them in a resident pack this engine has not located
        // yet (see the notes); the runtime falls back to the idle/walk cycles in water.
        swim_idle: has("cbnm_swim_id0").or(has("cbnm_sw_id0")),
        swim_move: has("cbnm_swim_02f_lp0").or(has("cbnm_sw_02f_lp0")),
        swim_sprint: has("cbnm_swim_sprint_lp0"),
        dive_idle: has("cbnm_dive_id0"),
        dive_move: has("cbnm_dive_02f_lp0"),
    };

    let footsteps = footstep_set(game, source, set.race, appearance.visible_item_id(ffl_ff14_chara::gearset::GearsetSlot::Feet), &mut content, &mut notes);
    let voice = voice_set(game, source, c, &mut content, &mut notes);

    notes.push(content.summary());

    Ok(CharacterModel {
        engine: "ff14".into(),
        name: name.to_string(),
        skeleton: SkeletonData { bones },
        parts,
        clips,
        locomotion,
        armed_locomotion,
        overlays,
        blink,
        actions,
        addons,
        scale,
        height: 1.7 * scale,
        footsteps,
        voice,
        notes,
        content,
    })
}

const WEAPON_MAIN: &str = "weapon:main";
const WEAPON_OFF: &str = "weapon:off";

/// Animation folder of a weapon attach type (`stf` → `bt_rod_emp`): battle stances, victory
/// poses and battle-mode locomotion live there rather than in `bt_common`.
/// The animation folders (`chara/human/c{race}/animation/a0001/<folder>`) a character draws
/// its weapon-specific clips from. Most jobs use one folder for everything; Black Mage keeps
/// its stances in `bt_jst_sld` but moves and draws with `bt_stf_sld`, Ninja moves with
/// `bt_nin_nin` (VFXEditor `Select/SelectDataUtils.cs`: `JobAnimationIds`,
/// `JobMovementOverride`, `JobDrawOverride`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationSet {
    /// Battle stance, victory pose and other weapon emotes.
    pub stance: &'static str,
    /// Battle-mode idle and locomotion (`resident/idle.pap`, `move_a.pap`).
    pub movement: &'static str,
    /// Draw / sheathe transitions (`resident/sub.pap`).
    pub draw: &'static str,
}

impl AnimationSet {
    const fn same(folder: &'static str) -> Self {
        Self { stance: folder, movement: folder, draw: folder }
    }
}

/// Animation set by `ClassJob` row (classes share their job's set).
pub fn job_animation_set(class_job: u32) -> Option<AnimationSet> {
    Some(match class_job {
        1 | 19 => AnimationSet::same("bt_swd_sld"),  // GLA, PLD
        2 | 20 => AnimationSet::same("bt_clw_clw"),  // PGL, MNK
        3 | 21 => AnimationSet::same("bt_2ax_emp"),  // MRD, WAR
        4 | 22 => AnimationSet::same("bt_2sp_emp"),  // LNC, DRG
        5 | 23 => AnimationSet::same("bt_2bw_emp"),  // ARC, BRD
        6 | 24 => AnimationSet::same("bt_stf_sld"),  // CNJ, WHM
        7 | 25 => AnimationSet { stance: "bt_jst_sld", movement: "bt_stf_sld", draw: "bt_stf_sld" }, // THM, BLM
        26 | 27 | 28 => AnimationSet::same("bt_2bk_emp"), // ACN, SMN, SCH
        29 | 30 => AnimationSet { stance: "bt_dgr_dgr", movement: "bt_nin_nin", draw: "bt_dgr_dgr" }, // ROG, NIN
        31 => AnimationSet::same("bt_2gn_emp"), // MCH
        32 => AnimationSet::same("bt_2sw_emp"), // DRK
        33 => AnimationSet::same("bt_2gl_emp"), // AST
        34 => AnimationSet::same("bt_2kt_emp"), // SAM
        35 => AnimationSet::same("bt_2rp_emp"), // RDM
        36 => AnimationSet::same("bt_rod_emp"), // BLU
        37 => AnimationSet::same("bt_2gb_emp"), // GNB
        38 => AnimationSet::same("bt_chk_chk"), // DNC
        39 => AnimationSet::same("bt_2km_emp"), // RPR
        40 => AnimationSet::same("bt_2ff_emp"), // SGE
        41 => AnimationSet::same("bt_bld_bld"), // VPR
        42 => AnimationSet::same("bt_brs_plt"), // PCT
        _ => return None,
    })
}

/// Animation set by the main hand's attach type, for presets without a job.
pub fn weapon_animation_set(atch_type: &str) -> Option<AnimationSet> {
    Some(AnimationSet::same(match atch_type {
        "stf" | "2st" => "bt_stf_sld",
        "rod" => "bt_rod_emp",
        "2bk" => "bt_2bk_emp",
        "2gl" => "bt_2gl_emp",
        "2ff" => "bt_2ff_emp",
        "brs" => "bt_brs_plt",
        "2gn" => "bt_2gn_emp",
        "2bw" => "bt_2bw_emp",
        "chk" => "bt_chk_chk",
        "dgr" => "bt_dgr_dgr",
        "bld" | "bl2" => "bt_bld_bld",
        "2rp" => "bt_2rp_emp",
        "2gb" => "bt_2gb_emp",
        "2ax" => "bt_2ax_emp",
        "2sp" => "bt_2sp_emp",
        "2sw" => "bt_2sw_emp",
        "2kt" => "bt_2kt_emp",
        "2km" => "bt_2km_emp",
        "swd" | "sld" => "bt_swd_sld",
        "clw" => "bt_clw_clw",
        _ => return None,
    }))
}

/// Animation set: the job's first (stances and victory poses are per job, classes share
/// their job's), else the main-hand weapon's.
pub fn animation_set(job: Option<u32>, main_hand_type: Option<&str>) -> Option<AnimationSet> {
    job.and_then(job_animation_set).or_else(|| main_hand_type.and_then(weapon_animation_set))
}

/// Animation set for a preset's job and main-hand item.
pub fn preset_animation_set(game: &GameData, job: Option<u32>, appearance: &ffl_ff14_chara::Appearance) -> Option<AnimationSet> {
    let main = appearance
        .visible_item_id(ffl_ff14_chara::gearset::GearsetSlot::MainHand)
        .and_then(|id| item_models(&game.excel, id).ok())
        .and_then(|i| atch::main_hand_type(i.ui_category));
    animation_set(job, main)
}

/// The extra skeletons of a resolved character: (label, skeleton) for the face, the
/// hairstyle, the head gear and the body gear whose EST entry names a skeleton set.
fn extra_skeletons(
    source: &dyn AssetSource,
    set: &ffl_ff14_chara::resolver::CharacterModelSet,
    customize: &physis::savedata::chardat::CustomizeData,
    notes: &mut Vec<String>,
    content: &mut ContentReport,
) -> Vec<(String, physis::skeleton::Skeleton)> {
    use ffl_ff14_chara::meta::est::EstFile;
    use ffl_ff14_chara::race::GearSlot;
    use ffl_ff14_chara::resolver::PartKind;
    let race = set.race;
    let gear_set = |slot: GearSlot| set.parts.iter().find(|p| p.kind == PartKind::Gear(slot)).and_then(|p| p.set_id);
    let mut wanted: Vec<(&str, &str, char, Option<u16>)> = vec![
        ("faceskeletontemplate.est", "face", 'f', Some(customize.face as u16)),
        ("hairskeletontemplate.est", "hair", 'h', Some(customize.hair as u16)),
        ("extra_met.est", "met", 'm', gear_set(GearSlot::Head)),
        ("extra_top.est", "top", 't', gear_set(GearSlot::Body)),
    ];
    let overrides = source.meta_overrides();
    let mut out = Vec::new();
    for (file, dir, letter, id) in wanted.drain(..) {
        let Some(id) = id else {
            continue;
        };
        // A mod's EST edit (Penumbra slot names Face/Hair/Head/Body) wins over the table.
        let penumbra_slot = match dir {
            "face" => "Face",
            "hair" => "Hair",
            "met" => "Head",
            _ => "Body",
        };
        let skel = match overrides.est.get(&(race.0, penumbra_slot.to_string(), id)).copied().filter(|s| *s != 0) {
            Some(s) => s,
            None => {
                let table_path = format!("chara/xls/charadb/{file}");
                let Some(est) = source.read(&table_path).and_then(|b| EstFile::parse(&b).ok()) else {
                    notes.push(format!("no {file}"));
                    continue;
                };
                content.push(&table_path, source.origin(&table_path));
                let Some(skel) = est.skeleton(race, id) else {
                    continue;
                };
                skel
            }
        };
        let path = format!("chara/human/c{}/skeleton/{dir}/{letter}{skel:04}/skl_c{}{letter}{skel:04}.sklb", race.code(), race.code());
        match load_sklb(source, &path) {
            Ok(s) => {
                content.push(&path, source.origin(&path));
                out.push((format!("{dir} skeleton {letter}{skel:04}"), s));
            }
            Err(err) => notes.push(format!("{dir} skeleton {letter}{skel:04}: {err:#}")),
        }
    }
    out
}

/// Append the bones of an extra skeleton that the list does not have yet, parents resolved
/// by name (an extra skeleton repeats the base bone it hangs off, e.g. `j_kao`). Returns
/// how many bones were added.
fn merge_skeleton(bones: &mut Vec<BoneData>, extra: &physis::skeleton::Skeleton) -> usize {
    let mut added = 0;
    for b in &extra.bones {
        if bones.iter().any(|x| x.name == b.name) {
            continue;
        }
        let parent = (b.parent_index >= 0)
            .then(|| extra.bones.get(b.parent_index as usize).map(|p| p.name.clone()))
            .flatten()
            .and_then(|name| bones.iter().position(|x| x.name == name));
        bones.push(BoneData {
            name: b.name.clone(),
            parent,
            translation: b.position,
            rotation: b.rotation,
            scale: b.scale,
        });
        added += 1;
    }
    added
}

/// Pap path for an ActionTimeline key (`emote/joy` → `.../bt_common/emote/joy.pap`).
pub fn timeline_pap_path(race: ffl_ff14_chara::RaceCode, key: &str) -> String {
    format!("chara/human/c{}/animation/a0001/bt_common/{key}.pap", race.code())
}

/// A resident package (`idle`, `move_a`, `sub`, ...) of an animation folder, walking the race
/// fallback chain: most races only carry `bt_common`, weapon folders exist under c0101.
pub fn resident_pap(source: &dyn AssetSource, race: ffl_ff14_chara::RaceCode, folder: &str, file: &str) -> Option<String> {
    race.dependencies().into_iter().find_map(|r| {
        let path = format!("chara/human/c{}/animation/a0001/{folder}/resident/{file}.pap", r.code());
        source.exists(&path).then_some(path)
    })
}

/// The pap holding a timeline key: `bt_common` first, then the weapon folder, each through the
/// race fallback chain.
fn find_timeline_pap(source: &dyn AssetSource, race: ffl_ff14_chara::RaceCode, folder: Option<&str>, key: &str) -> Option<String> {
    let chain = race.dependencies();
    for r in &chain {
        let common = timeline_pap_path(*r, key);
        if source.exists(&common) {
            return Some(common);
        }
    }
    let f = folder?;
    for r in &chain {
        let path = format!("chara/human/c{}/animation/a0001/{f}/{key}.pap", r.code());
        if source.exists(&path) {
            return Some(path);
        }
    }
    None
}

/// Idle pose variants and emotes available to this race (and weapon folder).
pub fn list_actions(game: &GameData, race: ffl_ff14_chara::RaceCode, folder: Option<&str>) -> Vec<ActionDef> {
    let source = game.source();
    let mut out = Vec::new();
    out.push(ActionDef {
        id: "idle:default".into(),
        name: "Default idle".into(),
        category: ActionCategory::Idle,
        looped: true,
    });
    for n in 1..=12u32 {
        let key = format!("emote/pose{n:02}_loop");
        if source.exists(&timeline_pap_path(race, &key)) {
            out.push(ActionDef {
                id: format!("pose:{n}"),
                name: format!("Idle pose {n}"),
                category: ActionCategory::Idle,
                looped: true,
            });
        }
    }
    let (Ok(emotes), Ok(timelines)) = (game.excel.sheet("Emote"), game.excel.sheet("ActionTimeline")) else {
        return out;
    };
    for page in &emotes.sheet.pages {
        for entry in &page.entries {
            let row = entry.id;
            let Ok(name) = emotes.string(row, "Name") else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            // The first non-zero timeline is the standing animation.
            let mut chosen = None;
            for i in 0..7 {
                if let Ok(t) = emotes.integer(row, &format!("ActionTimeline[{i}]"))
                    && t != 0
                    && let Ok(key) = timelines.string(t as u32, "Key")
                    && !key.is_empty()
                    && find_timeline_pap(source, race, folder, &key).is_some()
                {
                    // `IsLoop` is set on nearly every emote timeline (it describes the pap's own
                    // looping), so only loop keys that say so plus the held stances.
                    let looped = key.contains("_loop") || key.ends_with("_lp") || key.starts_with("emote/battle");
                    chosen = Some((key, looped));
                    break;
                }
            }
            let Some((_, looped)) = chosen else {
                continue;
            };
            out.push(ActionDef {
                id: format!("emote:{row}"),
                name,
                category: ActionCategory::Emote,
                looped,
            });
        }
    }
    out
}

/// Load the clip behind an action id.
pub fn load_action(game: &GameData, race: ffl_ff14_chara::RaceCode, folder: Option<&str>, action_id: &str) -> Result<Clip> {
    let source = game.source();
    let path = if action_id == "idle:default" {
        timeline_pap_path(race, "resident/idle")
    } else if let Some(n) = action_id.strip_prefix("pose:") {
        timeline_pap_path(race, &format!("emote/pose{:02}_loop", n.parse::<u32>().unwrap_or(1)))
    } else if let Some(row) = action_id.strip_prefix("emote:") {
        let row: u32 = row.parse()?;
        let emotes = game.excel.sheet("Emote")?;
        let timelines = game.excel.sheet("ActionTimeline")?;
        let mut chosen = None;
        for i in 0..7 {
            if let Ok(t) = emotes.integer(row, &format!("ActionTimeline[{i}]"))
                && t != 0
                && let Ok(key) = timelines.string(t as u32, "Key")
                && let Some(path) = find_timeline_pap(source, race, folder, &key)
            {
                chosen = Some(path);
                break;
            }
        }
        chosen.ok_or_else(|| anyhow!("emote {row} has no animation for this race/weapon"))?
    } else {
        return Err(anyhow!("unknown action {action_id}"));
    };
    let mut clips = HashMap::new();
    let mut notes = Vec::new();
    let retarget = if path_race(&path) != Some(race.0) {
        let skeleton = load_sklb(source, &paths::skeleton(race)).context("skeleton")?;
        let rt = Retarget::from_skeleton(&skeleton).ok_or_else(|| anyhow!("{path}: authored for another skeleton and this skeleton has no mapper"))?;
        Some(rt)
    } else {
        None
    };
    load_clips_with(source, &path, &mut clips, &mut notes, None, retarget.as_ref());
    // Prefer the idle loop in the resident pack; otherwise the body clip, which is the one driving
    // the most bones (emote packs also carry small ear/tail/face clips of the same length).
    let clip = if action_id == "idle:default" {
        clips.remove("cbnm_id0")
    } else {
        clips
            .into_values()
            .max_by(|a, b| (a.track_to_bone.len(), a.duration).partial_cmp(&(b.track_to_bone.len(), b.duration)).unwrap_or(std::cmp::Ordering::Equal))
    };
    clip.ok_or_else(|| anyhow!("no clip in {path}: {}", notes.join("; ")))
}

/// Drop submeshes whose attributes are not enabled by the IMC mask.
/// Shape keys of a face model selected by the customize options: option 1 is the neutral
/// mesh, option N ≥ 2 picks the shape with letter N-2 of its group — brows (`shp_brw_*` ←
/// eyebrows, 5 shapes + neutral = the game's 6 options), eyes (`shp_eye_*` ← eye shape
/// without the small-iris bit), nose (`shp_nse_*`), mouth (`shp_mth_*` ← mouth without the
/// lipstick bit), cheeks (`shp_chk_*` ← jaw, 3 + neutral = 4 options). Options beyond the
/// shapes the model has keep the neutral mesh.
fn face_shape_keys(c: &physis::savedata::chardat::CustomizeData, mdl: &physis::model::MDL) -> Vec<String> {
    let available: Vec<&str> = mdl.lods.first().map(|l| l.parts.iter().flat_map(|p| p.shapes.iter().map(|s| s.name.as_str())).collect()).unwrap_or_default();
    let groups: [(&str, u8); 5] = [("brw", c.eyebrows), ("eye", c.eyes & 0x7F), ("nse", c.nose), ("mth", c.mouth & 0x7F), ("chk", c.jaw)];
    let mut out = Vec::new();
    for (group, option) in groups {
        if option < 2 {
            continue;
        }
        let name = format!("shp_{group}_{}", (b'a' + (option - 2).min(25)) as char);
        if available.contains(&name.as_str()) {
            out.push(name);
        }
    }
    out
}

/// Attribute mask of a face model: the facial-feature marks (`atr_fv_a`..`atr_fv_g`) follow
/// the customize bits; every other attribute stays visible.
fn face_attribute_mask(c: &physis::savedata::chardat::CustomizeData, mdl: &physis::model::MDL) -> u16 {
    let mut mask: u16 = 0xFFFF;
    for (bit, name) in mdl.attribute_names.iter().enumerate().take(16) {
        if let Some(letter) = name.strip_prefix("atr_fv_").and_then(|s| s.bytes().next())
            && (b'a'..=b'g').contains(&letter)
            && c.facial_features & (1 << (letter - b'a')) == 0
        {
            mask &= !(1 << bit);
        }
    }
    mask
}

fn apply_attribute_mask(model: &mut ModelData, mdl: &physis::model::MDL, mask: u16) {
    let Some(lod) = mdl.lods.first() else {
        return;
    };
    let mut pi = 0;
    for part in &lod.parts {
        if part.vertices.is_empty() || part.indices.len() < 3 {
            continue;
        }
        if let Some(pm) = model.meshes.get_mut(pi) {
            let base = part.submeshes.first().map(|s| s.index_offset as usize).unwrap_or(0);
            let mut keep: Vec<u32> = Vec::with_capacity(pm.indices.len());
            for sm in &part.submeshes {
                let visible = sm.attribute_index_mask == 0 || (sm.attribute_index_mask & mask as u32) == sm.attribute_index_mask;
                if visible {
                    let start = (sm.index_offset as usize).saturating_sub(base).min(pm.indices.len());
                    let end = (start + sm.index_count as usize).min(pm.indices.len());
                    keep.extend_from_slice(&pm.indices[start..end]);
                }
            }
            if !part.submeshes.is_empty() {
                pm.indices = keep;
            }
        }
        pi += 1;
    }
}

/// Apply composed racial deform matrices (per bone name) to a model authored for an ancestor race.
fn apply_racial_deform(model: &mut ModelData, deform: &physis::pbd::PreBoneDeformMatrices) {
    // Physis returns the character race's deformer first, then its ancestors'; compose ancestor first.
    let mut per_bone: HashMap<&str, Mat4> = HashMap::new();
    for b in deform.bones.iter().rev() {
        let d = &b.deform;
        let m = Mat4::from_cols_array(&[
            d[0], d[4], d[8], 0.0, //
            d[1], d[5], d[9], 0.0, //
            d[2], d[6], d[10], 0.0, //
            d[3], d[7], d[11], 1.0,
        ]);
        let entry = per_bone.entry(b.name.as_str()).or_insert(Mat4::IDENTITY);
        *entry = m * *entry;
    }
    for mesh in &mut model.meshes {
        let Some((joints, weights)) = &mesh.joints else {
            continue;
        };
        for (vi, (ji, jw)) in joints.iter().zip(weights.iter()).enumerate() {
            let p = Vec3::from(mesh.positions[vi]);
            let n = Vec3::from(mesh.normals[vi]);
            let mut p2 = Vec3::ZERO;
            let mut n2 = Vec3::ZERO;
            let mut total = 0.0;
            for k in 0..4 {
                let w = jw[k];
                if w <= 0.0 {
                    continue;
                }
                let bone_name = mesh
                    .bone_table
                    .get(ji[k] as usize)
                    .and_then(|bi| model.bone_names.get(*bi as usize))
                    .map(String::as_str)
                    .unwrap_or("");
                let m = per_bone.get(bone_name).copied().unwrap_or(Mat4::IDENTITY);
                p2 += m.transform_point3(p) * w;
                n2 += m.transform_vector3(n) * w;
                total += w;
            }
            if total > 0.0 {
                mesh.positions[vi] = (p2 / total).to_array();
                mesh.normals[vi] = n2.normalize_or_zero().to_array();
            }
        }
    }
}

/// Pre-sample every animation in a `.pap` into clips keyed by animation name.
/// Per source-bone retargeting onto the character's skeleton, from the skeleton mapper embedded
/// in the character's `.sklb`. Applied in local space: `rot_B = r * rot_A`,
/// `t_B = scale * t_A + t` (verified: reproduces the target rest pose from the base rest pose
/// with zero error for c0801 and c1101; `ffl-cli sklb <path>` prints the check).
pub struct Retarget {
    /// Indexed by source-skeleton bone: (target bone index, rotation, scale, translation).
    by_source_bone: Vec<Option<(usize, Quat, f32, Vec3)>>,
    pub source_name: String,
}

impl Retarget {
    /// Build from the mapper of `target` whose rest check is best (skeleton files carry a
    /// variant with and one without the rest-orientation corrections).
    pub fn from_skeleton(target: &physis::skeleton::Skeleton) -> Option<Retarget> {
        let target_index: HashMap<&str, usize> = target.bones.iter().enumerate().map(|(i, b)| (b.name.as_str(), i)).collect();
        let mut best: Option<(f32, Retarget)> = None;
        for m in &target.mappers {
            let mut err = 0.0f32;
            let mut by_source_bone: Vec<Option<(usize, Quat, f32, Vec3)>> = vec![None; m.source_bones.len()];
            for sm in &m.simple_mappings {
                let Some(a) = m.source_bones.iter().position(|b| b.name == sm.bone_a) else {
                    continue;
                };
                let Some(&b) = target_index.get(sm.bone_b.as_str()) else {
                    continue;
                };
                let r = Quat::from_array(sm.a_from_b_rotation).normalize();
                let t = Vec3::from(sm.a_from_b_translation);
                let sc = sm.a_from_b_scale[0];
                // Rest check against the mapper's own target skeleton.
                if let Some(tb) = m.target_bones.iter().find(|x| x.name == sm.bone_b) {
                    let qa = Quat::from_array(m.source_bones[a].rotation);
                    let qb = Quat::from_array(tb.rotation);
                    err = err.max(1.0 - (r * qa).dot(qb).abs());
                }
                by_source_bone[a] = Some((b, r, sc, t));
            }
            let candidate = Retarget {
                by_source_bone,
                source_name: m.source_name.clone(),
            };
            if best.as_ref().is_none_or(|(e, _)| err < *e) {
                best = Some((err, candidate));
            }
        }
        best.map(|(_, r)| r)
    }

    fn map(&self, source_bone: usize, pose: &ClipPose) -> Option<(usize, ClipPose)> {
        let (b, r, sc, t) = self.by_source_bone.get(source_bone).copied().flatten()?;
        let rot = (r * Quat::from_array(pose.rotation)).normalize();
        let tr = Vec3::from(pose.translation) * sc + t;
        Some((
            b,
            ClipPose {
                translation: tr.to_array(),
                rotation: rot.to_array(),
                scale: pose.scale,
            },
        ))
    }
}

/// Race code in an animation path (`chara/human/c0101/...` → 101).
pub fn path_race(path: &str) -> Option<u16> {
    let rest = path.strip_prefix("chara/human/c")?;
    rest.get(..4)?.parse().ok()
}

/// FF14 footstep files: `sound/foot/foot/fs_%s_%c_%c_%s.scd` (the template in the executable,
/// `ffxiv_dx11.exe` 2026.08.11 at 0x14215bec0; all 99 files verified). The client fills it as
/// material name, `m`/`f` (male/female; race 3 = Lalafell always `f`), `f`/`r` (dry / wet: the
/// wet flag is set for weather ids 7, 8 and 10 — Rain, Showers, Thunderstorms — and forced on
/// for the "mud" material 16, which plays as wet gravel), `shoes`/`boots` (the feet item's
/// footwear type 1..5 through a table `[0,0,0,1,1]`: types 4 and 5 are boots).
const FOOT_MATERIALS: [(SurfaceKind, &str); 12] = [
    (SurfaceKind::Dirt, "dart"),
    (SurfaceKind::Grass, "grass"),
    (SurfaceKind::Sand, "sand"),
    (SurfaceKind::Stone, "stone"),
    (SurfaceKind::Wood, "wood"),
    (SurfaceKind::Metal, "metal"),
    (SurfaceKind::Gravel, "gravel"),
    (SurfaceKind::Leaf, "leaf"),
    (SurfaceKind::Powder, "powder"),
    (SurfaceKind::Carpet, "carpet"),
    (SurfaceKind::Snow, "snow"),
    (SurfaceKind::Water, "water"),
];

/// Sound programs of a footstep file, selected by the client as `sound id − 1` when a `C042`
/// timeline entry carries a sound id, and by its walk/run state when the id is 0 (every
/// locomotion clip: `move_a.pap`, `move_b.pap`). Program 0 = walk (0.5–0.7 s heel-toe clips),
/// 1 = run (0.2–0.3 s single impacts), 3 = the landing (`jump_landing.tmb` sound id 4, one
/// entry), 2 = sound id 3 (unused by locomotion). Which of 0/1 the walk state picks is read
/// from the clip lengths (a run step lands every 0.33 s), not from the executable.
const FOOT_PROGRAM_WALK: usize = 0;
const FOOT_PROGRAM_RUN: usize = 1;
const FOOT_PROGRAM_LAND: usize = 3;

/// Whether a footwear item name reads as soft shoes (else boots). Heuristic.
pub fn is_shoes(item_name: &str) -> bool {
    let n = item_name.to_ascii_lowercase();
    ["shoe", "sandal", "slipper", "patten", "clog", "loafer", "pump", "espadrille", "geta", "zori", "thong", "sock", "moccasin"].iter().any(|k| n.contains(k))
}

/// The variations of a footstep bank as sound keys: the audio entries of sound program
/// `sound_id` (a random pick over three entries; see [`FOOT_PROGRAM_WALK`]). The program's
/// volume travels with the decoded sound (`SoundData::gain`).
/// Programs the file lacks fall back to program 0, and a file without programs to every entry.
fn scd_variations(source: &dyn AssetSource, path: &str, sound_id: usize, content: &mut ContentReport) -> Option<SoundCue> {
    let scd = load_scd(source, path).ok()?;
    let program = scd.sounds.get(sound_id).filter(|s| !s.audio.is_empty()).or_else(|| scd.sounds.first().filter(|s| !s.audio.is_empty()));
    let entries: Vec<&ffl_ff14_assets::scd::ScdEntry> = match program {
        Some(s) => s.audio.iter().filter_map(|&i| scd.entries.get(i).and_then(|e| e.as_ref())).collect(),
        None => scd.present().collect(),
    };
    let keys: Vec<String> = entries.iter().map(|e| scd_key(source, path, e.index)).collect();
    if keys.is_empty() {
        return None;
    }
    content.push(path, source.origin(path));
    Some(SoundCue::new(keys))
}

fn footstep_set(game: &GameData, source: &dyn AssetSource, race: ffl_ff14_chara::RaceCode, feet_item: Option<u32>, content: &mut ContentReport, notes: &mut Vec<String>) -> Option<FootstepSet> {
    // Lalafell (race code 1101/1201) always use the light `f` set, like the client.
    let light = race.is_female() || matches!(race.0, 1101 | 1201);
    let gender = if light { "f" } else { "m" };
    let feet_name = feet_item.and_then(|id| item_models(&game.excel, id).ok()).map(|i| i.name).unwrap_or_default();
    let kind = if feet_name.is_empty() || is_shoes(&feet_name) { "shoes" } else { "boots" };
    let mut set = FootstepSet { cues: Vec::new(), fallback_surface: SurfaceKind::Dirt };
    let programs = [(Gait::Walk, FOOT_PROGRAM_WALK), (Gait::Run, FOOT_PROGRAM_RUN), (Gait::Land, FOOT_PROGRAM_LAND)];
    for (surface, material) in FOOT_MATERIALS {
        for (condition, code) in [(GroundCondition::Dry, "f"), (GroundCondition::Wet, "r")] {
            let path = format!("sound/foot/foot/fs_{material}_{gender}_{code}_{kind}.scd");
            for (gait, program) in programs {
                if let Some(cue) = scd_variations(source, &path, program, content) {
                    set.push(surface, gait, condition, cue);
                }
            }
        }
    }
    // The mesh file has a single program.
    if let Some(cue) = scd_variations(source, "sound/foot/foot/fs_mesh_ex.scd", 0, content) {
        for (gait, _) in programs {
            set.push(SurfaceKind::Mesh, gait, GroundCondition::Dry, cue.clone());
        }
    }
    let banks = set.cues.len();
    notes.push(format!("footsteps: {} ({gender}, {kind}{})", set.summary(), if feet_name.is_empty() { String::new() } else { format!(" from \"{feet_name}\"") }));
    (banks > 0).then_some(set)
}

fn voice_set(game: &GameData, source: &dyn AssetSource, c: &physis::savedata::chardat::CustomizeData, content: &mut ContentReport, notes: &mut Vec<String>) -> Option<VoiceSet> {
    use ffl_ff14_chara::voice::{chara_make_voices, emote_voice_path, voice_bank, voice_bank_override};
    let row = chara_make_voices(&game.excel, c.tribe, &c.gender);
    let bank = match (&row, voice_bank_override()) {
        (_, Some(b)) => b,
        (Some(r), None) => voice_bank(c.voice, r),
        (None, None) => 0,
    };
    let mut lines = Vec::new();
    for line in 1..=260u32 {
        let path = emote_voice_path(line, bank);
        if source.exists(&path) {
            content.push(&path, source.origin(&path));
            lines.push((line, SoundCue::new(vec![scd_key(source, &path, 0)])));
        }
    }
    notes.push(format!(
        "voice: id {} bank {bank}{} ({} lines{})",
        c.voice,
        if voice_bank_override().is_some() { " (FFL_VOICE_BANK)" } else { "" },
        lines.len(),
        match &row {
            Some(r) => format!(", row voices {r:?}"),
            None => ", CharaMakeType voices not found".into(),
        }
    ));
    (!lines.is_empty()).then(|| VoiceSet { name: format!("voice {} bank {bank}", c.voice), lines })
}

/// Timeline entries → clip events (30 fps). Foot ids 34/35 are the client's left/right enum.
pub fn clip_events(t: &TmbInfo, source: &dyn ffl_ff14_assets::AssetSource) -> Vec<ClipEvent> {
    let mut events = Vec::new();
    for f in &t.footsteps {
        events.push(ClipEvent {
            time: f.frame as f32 / 30.0,
            kind: ClipEventKind::Footstep { foot: if f.foot_id == 35 { Foot::Right } else { Foot::Left }, variant: f.sound_id.max(0) as u32 },
        });
    }
    for v in t.voices.iter().filter(|v| v.sound_id > 0) {
        events.push(ClipEvent { time: v.frame as f32 / 30.0, kind: ClipEventKind::Voice { line: v.sound_id as u32 } });
    }
    for s in t.sounds.iter().filter(|s| !s.path.is_empty() && source.exists(&s.path)) {
        events.push(ClipEvent {
            time: s.frame as f32 / 30.0,
            kind: ClipEventKind::Sound { cue: SoundCue::new(vec![scd_key(source, &s.path, s.index.max(0) as usize)]), stop_at_end: s.looped != 0 },
        });
    }
    events.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
    events
}

/// Give clips without their own timeline the events of a standalone `.tmb` (the jump landing's
/// footsteps live in `chara/action/normal/jump_landing.tmb`, not in `move_a.pap`).
pub fn attach_standalone_events(source: &dyn ffl_ff14_assets::AssetSource, clips: &mut HashMap<String, Clip>, clip_names: &[&str], tmb_path: &str, content: &mut ContentReport) {
    let Some(bytes) = source.read(tmb_path) else { return };
    content.push(tmb_path, source.origin(tmb_path));
    let events = clip_events(&TmbInfo::parse(&bytes), source);
    for n in clip_names {
        if let Some(c) = clips.get_mut(*n)
            && c.events.is_empty()
        {
            c.events = events.clone();
        }
    }
}

pub fn load_clips(source: &dyn ffl_ff14_assets::AssetSource, path: &str, clips: &mut HashMap<String, Clip>, notes: &mut Vec<String>) {
    load_clips_with(source, path, clips, notes, None, None)
}

/// [`load_clips`] restricted to the named clips (no retarget).
pub fn load_clips_filtered(source: &dyn ffl_ff14_assets::AssetSource, path: &str, clips: &mut HashMap<String, Clip>, notes: &mut Vec<String>, wanted: Option<&[&str]>) {
    load_clips_with(source, path, clips, notes, wanted, None)
}

/// [`load_clips`] with an optional clip-name filter and an optional retarget (for a pap
/// authored on another skeleton, e.g. the c0101 base).
pub fn load_clips_with(
    source: &dyn ffl_ff14_assets::AssetSource,
    path: &str,
    clips: &mut HashMap<String, Clip>,
    notes: &mut Vec<String>,
    wanted: Option<&[&str]>,
    retarget: Option<&Retarget>,
) {
    load_clips_impl(source, path, clips, notes, wanted, retarget, None)
}

/// [`load_clips`] for a pack authored on another skeleton whose bones the character has by
/// name (a face pack indexes the face skeleton's own bone order): `bone_map[source bone]` =
/// the character's bone, tracks without one are dropped.
pub fn load_clips_mapped(
    source: &dyn ffl_ff14_assets::AssetSource,
    path: &str,
    clips: &mut HashMap<String, Clip>,
    notes: &mut Vec<String>,
    wanted: Option<&[&str]>,
    bone_map: &[Option<usize>],
) {
    load_clips_impl(source, path, clips, notes, wanted, None, Some(bone_map))
}

fn load_clips_impl(
    source: &dyn ffl_ff14_assets::AssetSource,
    path: &str,
    clips: &mut HashMap<String, Clip>,
    notes: &mut Vec<String>,
    wanted: Option<&[&str]>,
    retarget: Option<&Retarget>,
    bone_map: Option<&[Option<usize>]>,
) {
    let Some(mut bytes) = source.read(path) else {
        notes.push(format!("file not found: {path}"));
        return;
    };
    // The pap header and its Havok payload; a modded pack the reader cannot parse (Havok tag
    // files of another version) falls back to the game's pack.
    let parse = |bytes: &[u8]| {
        ffl_ff14_assets::loaders::guarded(path, || {
            let pap = <physis::pap::Pap as physis::ReadableFile>::from_existing(source.platform(), bytes)?;
            let root = physis::havok::HavokBinaryTagFileReader::read(pap.havok_data());
            let container = physis::havok::HavokAnimationContainer::new(root.find_object_by_type("hkaAnimationContainer"));
            Some((pap, container))
        })
    };
    let mut parsed = parse(&bytes);
    if parsed.is_none()
        && !matches!(source.origin(path), ffl_core::Provenance::Game)
        && let Some(vanilla) = source.read_vanilla(path)
    {
        parsed = parse(&vanilla);
        if parsed.is_some() {
            notes.push(format!("{path}: the modded animation pack could not be read; using the game's"));
            bytes = vanilla;
        }
    }
    let Some((pap, container)) = parsed else {
        notes.push(format!("failed to parse {path} ({} bytes)", bytes.len()));
        return;
    };
    // The timelines embedded after the Havok data carry footsteps, voice lines and sounds.
    let timelines: Vec<TmbInfo> = embedded_timelines(&bytes).into_iter().map(TmbInfo::parse).collect();
    for (i, binding) in container.bindings.iter().enumerate() {
        let name = pap
            .animations
            .get(i)
            .map(|a| a.name.trim_end_matches('\0').to_string())
            .unwrap_or_else(|| format!("{path}#{i}"));
        if let Some(w) = wanted
            && !w.contains(&name.as_str())
        {
            continue;
        }
        let duration = binding.animation.duration().max(1.0 / CLIP_FPS);
        // Include the final frame so one-shot clips can end on their true last pose.
        let frame_count = ((duration * CLIP_FPS).floor() as usize + 1).max(2);
        let source_bones: Vec<usize> = binding.transform_track_to_bone_indices.iter().map(|b| *b as usize).collect();
        // Tracks kept and the target bone of each (all of them without a retarget).
        let kept: Vec<(usize, usize)> = match (retarget, bone_map) {
            (None, Some(map)) => source_bones.iter().enumerate().filter_map(|(track, &sb)| map.get(sb).copied().flatten().map(|tb| (track, tb))).collect(),
            (None, None) => source_bones.iter().copied().enumerate().collect(),
            (Some(r), _) => source_bones
                .iter()
                .enumerate()
                .filter_map(|(track, &sb)| r.by_source_bone.get(sb).copied().flatten().map(|(tb, ..)| (track, tb)))
                .collect(),
        };
        let mut frames = Vec::with_capacity(frame_count);
        for f in 0..frame_count {
            let t = (f as f32 / CLIP_FPS).min(duration);
            let sampled = binding.animation.sample(t);
            let mut poses = Vec::with_capacity(kept.len());
            for &(track, _) in &kept {
                let x = &sampled[track];
                let pose = ClipPose {
                    translation: [x.translation[0], x.translation[1], x.translation[2]],
                    rotation: Quat::from_xyzw(x.rotation[0], x.rotation[1], x.rotation[2], x.rotation[3]).normalize().to_array(),
                    scale: [x.scale[0], x.scale[1], x.scale[2]],
                };
                poses.push(match retarget {
                    Some(r) => r.map(source_bones[track], &pose).map(|(_, p)| p).unwrap_or(pose),
                    None => pose,
                });
            }
            frames.push(poses);
        }
        // Timeline of this clip: by animation name, else by index when the pack has one
        // timeline per animation, else the pack's single timeline (emote packs carry one
        // timeline for the body/face/ear clips of the same emote).
        let timeline = timelines
            .iter()
            .find(|t| t.animation.as_deref() == Some(name.as_str()))
            .or_else(|| timelines.get(i).filter(|_| timelines.len() == container.bindings.len()))
            .or_else(|| timelines.first().filter(|_| timelines.len() == 1));
        let events = timeline.map(|t| clip_events(t, source)).unwrap_or_default();
        clips.insert(
            name.clone(),
            Clip {
                name,
                duration,
                fps: CLIP_FPS,
                frames,
                track_to_bone: kept.iter().map(|(_, tb)| *tb).collect(),
                events,
                additive: matches!(binding.blend_hint, physis::havok::HavokAnimationBlendHint::Additive) && std::env::var_os("FFL_FACE_ABSOLUTE").is_none(),
            },
        );
    }
}

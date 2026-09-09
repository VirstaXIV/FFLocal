//! Game-data checks for the sound side of the FF14 engine. They need an installed game, so
//! they are ignored by default: `cargo test -p ffl-ff14 --test sound -- --ignored --nocapture`.

use ffl_core::{ClipEventKind, Engine, SurfaceKind};
use ffl_ff14::Ff14Engine;

fn engine() -> Option<Ff14Engine> {
    Ff14Engine::open(None, None).ok()
}

#[test]
#[ignore = "needs the installed game"]
fn mist_has_music_and_emitters() {
    let Some(e) = engine() else { return };
    let scene = e.load_scene("339").unwrap();
    let music = scene.music.expect("Mist has territory music");
    assert!(music.day.as_deref().unwrap().contains("BGM_Field_Housing_Day"));
    assert!(music.night.as_deref().unwrap().contains("BGM_Field_Housing_Night"));
    assert!(scene.emitters.len() > 100, "{} emitters", scene.emitters.len());
    assert!(scene.emitters.iter().all(|em| em.positional && em.max_distance > em.inner_radius));
    let sound = e.load_sound(&scene.emitters[0].key).unwrap();
    assert!(sound.frames > 0);
    println!("mist: {} emitters, first {} = {:.2} s loop {:?}", scene.emitters.len(), scene.emitters[0].name, sound.duration_secs(), sound.loop_range);
}

#[test]
#[ignore = "needs the installed game"]
fn central_shroud_uses_music_regions() {
    let Some(e) = engine() else { return };
    let scene = e.load_scene("148").unwrap();
    assert!(scene.music.as_ref().is_none_or(|m| m.day.is_none()), "field zones have no territory day track");
    assert!(scene.music_regions.len() > 5, "{} regions", scene.music_regions.len());
    let gri = scene.music_regions.iter().filter(|r| r.music.day.as_deref().is_some_and(|k| k.contains("BGM_Field_Gri"))).count();
    assert!(gri > 0);
    println!("central shroud: {} music regions, {} with Gridania field themes", scene.music_regions.len(), gri);
}

#[test]
#[ignore = "needs the installed game"]
fn default_character_has_footsteps_voice_and_events() {
    let Some(e) = engine() else { return };
    let presets = e.default_presets().unwrap();
    let Some(preset) = presets.first() else {
        println!("no character saves to import");
        return;
    };
    let model = e.load_character(preset).unwrap();
    for n in &model.notes {
        if n.starts_with("footsteps") || n.starts_with("voice") {
            println!("{n}");
        }
    }
    let steps = model.footsteps.expect("footstep set");
    use ffl_core::{Gait, GroundCondition};
    let cue = steps.cue(SurfaceKind::Grass, Gait::Run, GroundCondition::Dry).expect("grass run cue");
    assert_eq!(cue.variations.len(), 3, "program 1 picks over three entries: {:?}", cue.variations);
    assert!(cue.variations[0].starts_with("ff14:scd:sound/foot/foot/fs_grass_"));
    assert!(cue.variations[0].contains("_f_"), "dry steps come from the `f` file: {:?}", cue.variations);
    let walk = steps.cue(SurfaceKind::Grass, Gait::Walk, GroundCondition::Dry).expect("grass walk cue");
    assert_ne!(walk.variations, cue.variations, "walk and run use different programs");
    let wet = steps.cue(SurfaceKind::Grass, Gait::Run, GroundCondition::Wet).expect("grass wet run cue");
    assert!(wet.variations[0].contains("_r_"), "wet steps come from the `r` file: {:?}", wet.variations);
    let land = steps.cue(SurfaceKind::Stone, Gait::Land, GroundCondition::Dry).expect("stone landing cue");
    assert_eq!(land.variations.len(), 1, "the landing program has one entry: {:?}", land.variations);
    let sound = e.load_sound(&cue.variations[0]).unwrap();
    assert!(sound.duration_secs() < 2.0 && sound.channels == 1);
    let walk = model.clips.get("cbnm_01f_lp0").expect("walk clip");
    let feet: Vec<_> = walk.events.iter().filter_map(|ev| match ev.kind {
        ClipEventKind::Footstep { foot, .. } => Some((ev.time, foot)),
        _ => None,
    }).collect();
    assert_eq!(feet.len(), 2, "{feet:?}");
    assert!((feet[0].0 - 0.3).abs() < 0.01 && (feet[1].0 - 0.8).abs() < 0.01, "{feet:?}");
    let land = model.clips.get("cbnm_jump_3").expect("land clip");
    assert_eq!(land.events.len(), 2, "{:?}", land.events);
    let voice = model.voice.expect("voice set");
    assert!(voice.line(47).is_some(), "laugh line");
    let laugh = e.load_action(preset, "emote:21").unwrap();
    assert!(laugh.events.iter().any(|ev| matches!(ev.kind, ClipEventKind::Voice { line: 47 })), "{:?}", laugh.events);
    println!("walk footsteps {feet:?}; landing {:?}; voice lines {}", land.events, voice.lines.len());
}

#[test]
#[ignore = "needs the installed game; prints the surface classification of a zone's materials"]
fn mist_surface_histogram() {
    let Some(e) = engine() else { return };
    let zone = std::env::var("FFL_TEST_ZONE").unwrap_or_else(|_| "339".into());
    let scene = e.load_scene(&zone).unwrap();
    let mut by_surface: std::collections::BTreeMap<String, usize> = Default::default();
    let mut unknown: std::collections::BTreeMap<String, usize> = Default::default();
    for key in scene.unique_model_keys() {
        let Ok(model) = e.load_model(key) else { continue };
        for m in &model.materials {
            *by_surface.entry(m.surface.label().to_string()).or_default() += 1;
            if m.surface == SurfaceKind::Unknown {
                let stem = m.key.rsplit('/').next().unwrap_or(&m.key).to_string();
                *unknown.entry(stem).or_default() += 1;
            }
        }
    }
    println!("zone {zone} materials by surface: {by_surface:?}");
    let mut u: Vec<_> = unknown.into_iter().collect();
    u.sort_by(|a, b| b.1.cmp(&a.1));
    for (stem, n) in u.iter().take(60) {
        println!("  unknown {n:>3}x {stem}");
    }
}

/// Probe: where is Mist's collision data and what do PCB polygon materials look like?
#[test]
#[ignore = "needs the installed game; prints PCB collision facts"]
fn mist_pcb_probe() {
    use ffl_ff14_assets::loaders::load_pcb;
    use ffl_ff14_assets::AssetSource;
    use physis::pcb::ResourceNode;
    let Some(game) = ffl_ff14_assets::GameData::open(None).ok() else { return };
    let source = game.source.clone();
    let base = "bg/ffxiv/sea_s1/hou/s1h1";
    for p in [format!("{base}/collision/tr0000.pcb"), format!("{base}/collision/list.pcb"), format!("{base}/level/s1h1.lcb")] {
        println!("{p}: {}", if source.exists(&p) { "exists" } else { "MISSING" });
    }
    fn walk(n: &ResourceNode, hist: &mut std::collections::BTreeMap<u64, usize>, tris: &mut usize) {
        for poly in &n.polygons {
            *hist.entry(poly.material).or_default() += 1;
            *tris += 1;
        }
        for c in &n.children {
            walk(c, hist, tris);
        }
    }
    if let Ok(pcb) = load_pcb(source.as_ref(), &format!("{base}/collision/tr0000.pcb")) {
        let mut hist = Default::default();
        let mut tris = 0;
        walk(&pcb.root_node, &mut hist, &mut tris);
        println!("tr0000.pcb: {tris} polygons, bounds {:?}..{:?}, materials {:?}", pcb.root_node.local_bounds.min, pcb.root_node.local_bounds.max, hist.iter().map(|(m, n)| (format!("{m:#x}"), *n)).collect::<Vec<_>>());
    }
    // Which placed models have their own collision file next to them?
    let graph = ffl_ff14_assets::zone::build_zone(source.as_ref(), &game.excel, 339).unwrap();
    let mut have = 0;
    let mut sample = Vec::new();
    let mut hist: std::collections::BTreeMap<u64, usize> = Default::default();
    let mut tris = 0;
    for m in graph.unique_model_paths() {
        let stem = m.rsplit('/').next().unwrap_or(m).trim_end_matches(".mdl");
        let candidates = [format!("{base}/collision/{stem}.pcb"), m.replace("/bgparts/", "/collision/").replace(".mdl", ".pcb"), m.replace(".mdl", ".pcb")];
        for c in candidates {
            if source.exists(&c) {
                have += 1;
                if sample.len() < 5 {
                    sample.push(c.clone());
                }
                if let Ok(pcb) = load_pcb(source.as_ref(), &c) {
                    walk(&pcb.root_node, &mut hist, &mut tris);
                }
                break;
            }
        }
    }
    println!("models with a collision file: {have} of {}; e.g. {sample:?}", graph.unique_model_paths().len());
    let mut top: Vec<_> = hist.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    println!("{tris} collision polygons; top materials: {:?}", top.iter().take(24).map(|(m, n)| (format!("{m:#x}"), *n)).collect::<Vec<_>>());
    let replace: Vec<_> = graph.models.iter().filter_map(|m| match &m.collision { ffl_ff14_assets::zone::Collision::Replace(p) => Some(p.clone()), _ => None }).collect();
    println!("{} placements with Replace collision, e.g. {:?}", replace.len(), replace.iter().take(3).collect::<Vec<_>>());
    let mismatched = graph.models.iter().filter(|m| match &m.collision {
        ffl_ff14_assets::zone::Collision::Replace(p) => {
            let stem = m.mdl_path.rsplit('/').next().unwrap_or("").trim_end_matches(".mdl");
            !p.ends_with(&format!("/{stem}.pcb"))
        }
        _ => false,
    }).count();
    println!("{mismatched} Replace paths differ from the model stem");
    let mut none = Vec::new();
    for m in graph.unique_model_paths() {
        let stem = m.rsplit('/').next().unwrap_or(m).trim_end_matches(".mdl");
        if !source.exists(&format!("{base}/collision/{stem}.pcb")) {
            none.push(stem.to_string());
        }
    }
    println!("{} models without a pcb: {:?}", none.len(), none);
    for p in ["tr0030", "tr0000", "0030", "tr0001", "terrain"] {
        println!("collision/{p}.pcb: {}", source.exists(&format!("{base}/collision/{p}.pcb")));
    }
    if let Ok(list) = ffl_ff14_assets::loaders::load::<physis::pcblist::PcbList>(source.as_ref(), &format!("{base}/collision/list.pcb")) {
        println!("list.pcb: {} entries, bounds {:?}..{:?}, first ids {:?}", list.entries.len(), list.bounds.min, list.bounds.max, list.entries.iter().take(6).map(|e| (e.mesh_id, e.bounds.min, e.bounds.max)).collect::<Vec<_>>());
    }
    let mut thist: std::collections::BTreeMap<u64, usize> = Default::default();
    let mut ttris = 0;
    let mut tfiles = 0;
    for id in 100..240u32 {
        let p = format!("{base}/collision/tr{id:04}.pcb");
        if let Ok(pcb) = load_pcb(source.as_ref(), &p) {
            tfiles += 1;
            walk(&pcb.root_node, &mut thist, &mut ttris);
        }
    }
    let mut ttop: Vec<_> = thist.into_iter().collect();
    ttop.sort_by(|a, b| b.1.cmp(&a.1));
    println!("terrain pieces tr0100..tr0239: {tfiles} files, {ttris} polygons, materials {:?}", ttop.iter().take(20).map(|(m, n)| (format!("{m:#x}"), *n)).collect::<Vec<_>>());
    if let Ok(lcb) = ffl_ff14_assets::loaders::load::<physis::lcb::Lcb>(source.as_ref(), &format!("{base}/level/s1h1.lcb")) {
        let n: usize = lcb.lccs.iter().map(|l| l.entries.len()).sum();
        println!("s1h1.lcb: {} lccs, {n} entries, first {:?}", lcb.lccs.len(), lcb.lccs.first().and_then(|l| l.entries.first()).map(|e| (e.instance_id, e.min, e.max)));
    }
}

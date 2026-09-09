//! Turn an [`Appearance`] into the concrete list of models, materials and skeleton to load.

use anyhow::{Result, anyhow};
use ffl_ff14_assets::loaders::load_mdl;
use ffl_ff14_assets::{AssetSource, ExcelCache};
use physis::race::Race;

use crate::appearance::Appearance;
use crate::gearset::GearsetSlot;
use crate::meta::{EqdpFile, ImcFile};
use crate::race::{BodyPart, GearSlot, RaceCode, paths};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartKind {
    Body,
    Face,
    Hair,
    Tail,
    Ears,
    Gear(GearSlot),
    MainHand,
    OffHand,
}

impl PartKind {
    pub fn name(&self) -> String {
        match self {
            PartKind::Body => "body".into(),
            PartKind::Face => "face".into(),
            PartKind::Hair => "hair".into(),
            PartKind::Tail => "tail".into(),
            PartKind::Ears => "ears".into(),
            PartKind::Gear(s) => s.name().into(),
            PartKind::MainHand => "mainhand".into(),
            PartKind::OffHand => "offhand".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedMaterial {
    /// Name as referenced by the model (`/mt_c0801b0001_a.mtrl`).
    pub name: String,
    pub mtrl_path: String,
    pub exists: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedPart {
    pub kind: PartKind,
    pub mdl_path: String,
    pub mdl_exists: bool,
    /// Race the model was authored for (may differ from the character's race).
    pub model_race: RaceCode,
    pub material_dir: String,
    pub materials: Vec<ResolvedMaterial>,
    /// IMC attribute mask (which `atr_*` submeshes to show); 0xFFFF = show all.
    pub attribute_mask: u16,
    /// Item id this part came from, if any.
    pub item_id: Option<u32>,
    /// Gear set id / weapon model id, for skeleton lookups.
    pub set_id: Option<u16>,
    /// The item's two dye channels (Stain rows, 0 = undyed).
    pub dyes: [u8; 2],
}

#[derive(Debug, Clone)]
pub struct CharacterModelSet {
    pub race: RaceCode,
    pub skeleton_path: String,
    pub parts: Vec<ResolvedPart>,
    pub notes: Vec<String>,
    /// Gear slots hidden by another item's EquipSlotCategory (-1 entries).
    pub hidden_slots: Vec<GearSlot>,
}

impl CharacterModelSet {
    pub fn missing(&self) -> Vec<String> {
        let mut out = Vec::new();
        for p in &self.parts {
            if !p.mdl_exists {
                out.push(p.mdl_path.clone());
            }
            for m in &p.materials {
                if !m.exists {
                    out.push(m.mtrl_path.clone());
                }
            }
        }
        out
    }
}

/// Item model ids packed into `Item.ModelMain` / `ModelSub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelId {
    pub set: u16,
    pub body: u16,
    pub variant: u16,
}

impl ModelId {
    pub fn equipment(v: u64) -> Self {
        Self {
            set: (v & 0xFFFF) as u16,
            body: 0,
            variant: ((v >> 16) & 0xFFFF) as u16,
        }
    }

    pub fn weapon(v: u64) -> Self {
        Self {
            set: (v & 0xFFFF) as u16,
            body: ((v >> 16) & 0xFFFF) as u16,
            variant: ((v >> 32) & 0xFFFF) as u16,
        }
    }
}

/// Which gear slots an EquipSlotCategory occupies (1) and blocks (-1).
#[derive(Debug, Default, Clone)]
pub struct SlotCategory {
    pub occupies: Vec<GearSlot>,
    pub blocks: Vec<GearSlot>,
    pub main_hand: bool,
    pub off_hand: bool,
}

pub fn slot_category(excel: &ExcelCache, category: u32) -> Result<SlotCategory> {
    let sheet = excel.sheet("EquipSlotCategory")?;
    let mut cat = SlotCategory::default();
    let fields: [(&str, Option<GearSlot>); 14] = [
        ("MainHand", None),
        ("OffHand", None),
        ("Head", Some(GearSlot::Head)),
        ("Body", Some(GearSlot::Body)),
        ("Gloves", Some(GearSlot::Hands)),
        ("Waist", None),
        ("Legs", Some(GearSlot::Legs)),
        ("Feet", Some(GearSlot::Feet)),
        ("Ears", Some(GearSlot::Ears)),
        ("Neck", Some(GearSlot::Neck)),
        ("Wrists", Some(GearSlot::Wrists)),
        ("FingerL", Some(GearSlot::RingLeft)),
        ("FingerR", Some(GearSlot::RingRight)),
        ("SoulCrystal", None),
    ];
    for (name, slot) in fields {
        let v = sheet.signed(category, name)?;
        match (name, v) {
            ("MainHand", 1) => cat.main_hand = true,
            ("OffHand", 1) => cat.off_hand = true,
            (_, 1) => cat.occupies.extend(slot),
            (_, -1) => cat.blocks.extend(slot),
            _ => {}
        }
    }
    Ok(cat)
}

pub struct ItemModels {
    pub name: String,
    pub main: ModelId,
    pub sub: ModelId,
    pub category: SlotCategory,
    pub is_weapon: bool,
    /// `ItemUICategory` row (weapon class, e.g. 9 = two-handed conjurer's arm).
    pub ui_category: u32,
}

pub fn item_models(excel: &ExcelCache, item_id: u32) -> Result<ItemModels> {
    let items = excel.sheet("Item")?;
    let name = items.string(item_id, "Name")?;
    let main_raw = items.integer(item_id, "ModelMain")?;
    let sub_raw = items.integer(item_id, "ModelSub")?;
    let category_id = items.integer(item_id, "EquipSlotCategory")? as u32;
    let ui_category = items.integer(item_id, "ItemUICategory").unwrap_or(0) as u32;
    let category = slot_category(excel, category_id)?;
    let is_weapon = category.main_hand || category.off_hand;
    Ok(ItemModels {
        name,
        main: if is_weapon { ModelId::weapon(main_raw) } else { ModelId::equipment(main_raw) },
        sub: if is_weapon { ModelId::weapon(sub_raw) } else { ModelId::equipment(sub_raw) },
        category,
        is_weapon,
        ui_category,
    })
}

/// What a model's material names should resolve against.
struct MaterialContext {
    character_race: RaceCode,
    material_race: RaceCode,
    /// Race the model file was authored for (its material names carry it).
    model_race: RaceCode,
    slot: Option<GearSlot>,
    variant: u16,
    material_dir: String,
}

pub struct CharacterResolver<'a> {
    pub source: &'a dyn AssetSource,
    pub excel: &'a ExcelCache,
    /// Metadata edits of the active mods (a modded set's EQDP/IMC/EST entries).
    pub overrides: ffl_ff14_assets::mods::MetaOverrides,
}

impl<'a> CharacterResolver<'a> {
    pub fn new(source: &'a dyn AssetSource, excel: &'a ExcelCache) -> Self {
        let overrides = source.meta_overrides();
        Self { source, excel, overrides }
    }

    fn eqdp(&self, race: RaceCode, accessory: bool) -> Option<EqdpFile> {
        let bytes = self.source.read(&paths::eqdp(race, accessory))?;
        EqdpFile::parse(&bytes).ok()
    }

    fn imc(&self, path: &str) -> Option<ImcFile> {
        let bytes = self.source.read(path)?;
        ImcFile::parse(&bytes).ok()
    }

    /// Resolve a material name referenced by a model (e.g. `/mt_c0201b0001_a.mtrl`) to a full
    /// path. Names encode the model's authored race and part kind; the kind letter decides the
    /// directory (skin materials live under `chara/human/<race>/obj/body/...` even when
    /// referenced by equipment).
    fn resolve_material_name(&self, name: &str, ctx: &MaterialContext) -> String {
        let file = name.trim_start_matches('/');
        // "mt_" + "c0201" + kind + "0001" + rest
        if let Some(rest) = file.strip_prefix("mt_") {
            let bytes = rest.as_bytes();
            if bytes.len() > 10 && bytes[0] == b'c' && bytes[1..5].iter().all(u8::is_ascii_digit) {
                let kind = bytes[5] as char;
                let id: u16 = rest[6..10].parse().unwrap_or(0);
                let tail = &rest[10..];
                let name_race = RaceCode(rest[1..5].parse().unwrap_or(101));
                let with_race = |race: RaceCode| format!("mt_c{}{kind}{id:04}{tail}", race.code());
                match kind {
                    'b' => {
                        // Skin: first race in the character's chain that has the material.
                        for race in ctx.character_race.dependencies() {
                            let dir = paths::body_part_material_dir(race, BodyPart::Body, id);
                            let candidate = format!("{dir}/{}", with_race(race));
                            if self.source.exists(&candidate) {
                                return candidate;
                            }
                        }
                        let dir = paths::body_part_material_dir(name_race, BodyPart::Body, id);
                        return format!("{dir}/{file}");
                    }
                    'e' | 'a' => {
                        // EQDP's material race first; then the race the model names (a mod
                        // that ships a c0201 model with c0201 materials while EQDP still
                        // points the material at c0101); then the chain.
                        let slot = ctx.slot.unwrap_or(if kind == 'a' { GearSlot::Ears } else { GearSlot::Body });
                        let dir = paths::gear_material_dir(slot, id, ctx.variant);
                        let first = format!("{dir}/{}", with_race(ctx.material_race));
                        if self.source.exists(&first) {
                            return first;
                        }
                        for race in std::iter::once(ctx.model_race).chain(ctx.character_race.dependencies()) {
                            let candidate = format!("{dir}/{}", with_race(race));
                            if self.source.exists(&candidate) {
                                return candidate;
                            }
                        }
                        return first;
                    }
                    'f' | 'h' | 't' | 'z' => {
                        let part = match kind {
                            'f' => BodyPart::Face,
                            'h' => BodyPart::Hair,
                            't' => BodyPart::Tail,
                            _ => BodyPart::Ears,
                        };
                        let dir = paths::body_part_material_dir(ctx.material_race, part, id);
                        let candidate = format!("{dir}/{}", with_race(ctx.material_race));
                        if self.source.exists(&candidate) {
                            return candidate;
                        }
                        // Fall back through the race chain.
                        for race in ctx.character_race.dependencies() {
                            let dir = paths::body_part_material_dir(race, part, id);
                            let candidate = format!("{dir}/{}", with_race(race));
                            if self.source.exists(&candidate) {
                                return candidate;
                            }
                        }
                        return format!("{dir}/{file}");
                    }
                    _ => {}
                }
            }
            if bytes.len() > 10 && bytes[0] == b'w' {
                return format!("{}/{file}", ctx.material_dir);
            }
        }
        if name.starts_with('/') {
            format!("{}{name}", ctx.material_dir)
        } else {
            name.to_string()
        }
    }

    /// Materials referenced by a model, resolved to full paths.
    fn materials(&self, mdl_path: &str, ctx: &MaterialContext, notes: &mut Vec<String>) -> Vec<ResolvedMaterial> {
        let mdl = match load_mdl(self.source, mdl_path) {
            Ok(m) => m,
            Err(err) => {
                notes.push(format!("{err:#}"));
                return Vec::new();
            }
        };
        mdl.material_names
            .iter()
            .map(|name| {
                let mtrl_path = self.resolve_material_name(name, ctx);
                let exists = self.source.exists(&mtrl_path);
                ResolvedMaterial {
                    name: name.clone(),
                    mtrl_path,
                    exists,
                }
            })
            .collect()
    }

    fn body_part(
        &self,
        race: RaceCode,
        material_race: RaceCode,
        part: BodyPart,
        id: u16,
        kind: PartKind,
        notes: &mut Vec<String>,
    ) -> ResolvedPart {
        let mdl_path = paths::body_part_mdl(race, part, id);
        let material_dir = paths::body_part_material_dir(material_race, part, id);
        let mdl_exists = self.source.exists(&mdl_path);
        let ctx = MaterialContext {
            character_race: race,
            material_race,
            model_race: race,
            slot: None,
            variant: 1,
            material_dir: material_dir.clone(),
        };
        let materials = if mdl_exists {
            self.materials(&mdl_path, &ctx, notes)
        } else {
            Vec::new()
        };
        ResolvedPart {
            kind,
            mdl_path,
            mdl_exists,
            model_race: race,
            material_dir,
            materials,
            attribute_mask: 0xFFFF,
            item_id: None,
            dyes: [0, 0],
            set_id: Some(id),
        }
    }

    /// Resolve one gear slot of a set: EQDP race fallback, IMC variant, materials.
    fn gear_part(
        &self,
        race: RaceCode,
        slot: GearSlot,
        model: ModelId,
        item_id: u32,
        notes: &mut Vec<String>,
    ) -> ResolvedPart {
        let accessory = slot.is_accessory();
        let chain = race.dependencies();
        let mut model_race = *chain.last().unwrap();
        let mut material_race = model_race;
        let mut found_model = false;
        let mut found_material = false;
        for &candidate in &chain {
            // A mod's EQDP edit for this race/set/slot replaces the game's entry.
            let (b_material, b_model) = match self.overrides.eqdp.get(&(candidate.0, model.set, slot.penumbra_name().to_string())) {
                Some(entry) => (entry & 1 != 0, entry & 2 != 0),
                None => {
                    let Some(eqdp) = self.eqdp(candidate, accessory) else {
                        continue;
                    };
                    eqdp.bits(model.set, slot)
                }
            };
            if !found_model && b_model {
                model_race = candidate;
                found_model = true;
            }
            if !found_material && b_material {
                material_race = candidate;
                found_material = true;
            }
            if found_model && found_material {
                break;
            }
        }
        // Verify against the file system; EQDP can be wrong for some sets.
        let mut mdl_path = paths::gear_mdl(model_race, slot, model.set);
        if !self.source.exists(&mdl_path) {
            for &candidate in &chain {
                let p = paths::gear_mdl(candidate, slot, model.set);
                if self.source.exists(&p) {
                    notes.push(format!(
                        "{}: eqdp said {model_race} but the model exists for {candidate}",
                        slot.name()
                    ));
                    model_race = candidate;
                    mdl_path = p;
                    break;
                }
            }
        }
        let imc_path = paths::gear_imc(slot, model.set);
        let mut attribute_mask = 0xFFFF;
        let mut variant = model.variant;
        let object = if accessory { "Accessory" } else { "Equipment" };
        if let Some((material_id, mask)) = self.overrides.imc.get(&(object.to_string(), model.set, model.variant, slot.penumbra_name().to_string())) {
            variant = *material_id as u16;
            attribute_mask = *mask;
        } else {
            match self.imc(&imc_path) {
                Some(imc) => match imc.entry(slot.imc_part(), model.variant) {
                    Some(entry) => {
                        variant = entry.material_id as u16;
                        attribute_mask = entry.attribute_mask;
                    }
                    None => notes.push(format!(
                        "{imc_path}: no entry for part {} variant {}",
                        slot.imc_part(),
                        model.variant
                    )),
                },
                None => notes.push(format!("{imc_path}: missing")),
            }
        }
        let material_dir = paths::gear_material_dir(slot, model.set, variant);
        let mdl_exists = self.source.exists(&mdl_path);
        let ctx = MaterialContext {
            character_race: race,
            material_race,
            model_race,
            slot: Some(slot),
            variant,
            material_dir: material_dir.clone(),
        };
        let materials = if mdl_exists {
            self.materials(&mdl_path, &ctx, notes)
        } else {
            Vec::new()
        };
        ResolvedPart {
            kind: PartKind::Gear(slot),
            mdl_path,
            mdl_exists,
            model_race,
            material_dir,
            materials,
            attribute_mask,
            item_id: Some(item_id),
            dyes: [0, 0],
            set_id: Some(model.set),
        }
    }

    fn weapon_part(&self, kind: PartKind, model: ModelId, item_id: u32, notes: &mut Vec<String>) -> Option<ResolvedPart> {
        if model.set == 0 {
            return None;
        }
        let mdl_path = paths::weapon_mdl(model.set, model.body);
        let imc_path = paths::weapon_imc(model.set, model.body);
        let mut variant = model.variant;
        let mut attribute_mask = 0xFFFF;
        if let Some(imc) = self.imc(&imc_path) {
            if let Some(entry) = imc.entry(0, model.variant) {
                variant = entry.material_id as u16;
                attribute_mask = entry.attribute_mask;
            }
        } else {
            notes.push(format!("{imc_path}: missing"));
        }
        let material_dir = paths::weapon_material_dir(model.set, model.body, variant);
        let mdl_exists = self.source.exists(&mdl_path);
        let ctx = MaterialContext {
            character_race: RaceCode::MIDLANDER_MALE,
            material_race: RaceCode::MIDLANDER_MALE,
            model_race: RaceCode::MIDLANDER_MALE,
            slot: None,
            variant,
            material_dir: material_dir.clone(),
        };
        let materials = if mdl_exists {
            self.materials(&mdl_path, &ctx, notes)
        } else {
            Vec::new()
        };
        Some(ResolvedPart {
            kind,
            mdl_path,
            mdl_exists,
            model_race: RaceCode::MIDLANDER_MALE,
            material_dir,
            materials,
            attribute_mask,
            item_id: Some(item_id),
            dyes: [0, 0],
            set_id: Some(model.set),
        })
    }

    pub fn resolve(&self, appearance: &Appearance) -> Result<CharacterModelSet> {
        let c = &appearance.customize;
        let race = RaceCode::from_customize(c.race, c.tribe, &c.gender)
            .ok_or_else(|| anyhow!("unsupported race/tribe/gender {:?}/{:?}/{:?}", c.race, c.tribe, c.gender))?;
        let mut notes = appearance.notes.clone();
        let mut parts = Vec::new();

        // Customization parts. (There is no separate body model: the e0000 smallclothes
        // gear pieces are the naked body and reference the skin materials.)
        parts.push(self.body_part(race, race, BodyPart::Face, c.face as u16, PartKind::Face, &mut notes));
        let hair_id = c.hair as u16;
        let hair_material_race = race.hair_material_race(hair_id);
        parts.push(self.body_part(race, hair_material_race, BodyPart::Hair, hair_id, PartKind::Hair, &mut notes));
        match c.race {
            Race::Miqote | Race::AuRa | Race::Hrothgar => {
                let tail = c.race_feature_type.max(1) as u16;
                let p = self.body_part(race, race, BodyPart::Tail, tail, PartKind::Tail, &mut notes);
                if p.mdl_exists {
                    parts.push(p);
                } else {
                    notes.push(format!("no tail model {}", p.mdl_path));
                }
            }
            Race::Viera => {
                let ears = c.race_feature_type.max(1) as u16;
                parts.push(self.body_part(race, race, BodyPart::Ears, ears, PartKind::Ears, &mut notes));
            }
            _ => {}
        }

        // Equipment.
        let mut hidden_slots: Vec<GearSlot> = Vec::new();
        let mut gear: Vec<(GearSlot, ModelId, u32, [u8; 2])> = Vec::new();
        let gear_slots = [
            GearsetSlot::Head,
            GearsetSlot::Body,
            GearsetSlot::Hands,
            GearsetSlot::Legs,
            GearsetSlot::Feet,
            GearsetSlot::Ears,
            GearsetSlot::Neck,
            GearsetSlot::Wrists,
            GearsetSlot::RingLeft,
            GearsetSlot::RingRight,
        ];
        for gs in gear_slots {
            let Some(item_id) = appearance.visible_item_id(gs) else {
                continue;
            };
            let item = match item_models(self.excel, item_id) {
                Ok(i) => i,
                Err(err) => {
                    notes.push(format!("item {item_id}: {err:#}"));
                    continue;
                }
            };
            notes.push(format!(
                "{:?}: item {item_id} {:?} set {} variant {}",
                gs, item.name, item.main.set, item.main.variant
            ));
            for slot in &item.category.occupies {
                // Rings: the category says FingerL/FingerR both; keep the one matching the gearset slot.
                let wanted = match gs {
                    GearsetSlot::RingLeft => *slot == GearSlot::RingLeft || !item.category.occupies.contains(&GearSlot::RingLeft),
                    GearsetSlot::RingRight => *slot == GearSlot::RingRight || !item.category.occupies.contains(&GearSlot::RingRight),
                    _ => true,
                };
                if wanted {
                    gear.push((*slot, item.main, item_id, appearance.item(gs).map(|it| it.stains()).unwrap_or([0, 0])));
                }
            }
            for slot in &item.category.blocks {
                if !hidden_slots.contains(slot) {
                    hidden_slots.push(*slot);
                }
            }
        }
        // Body slots without gear get the smallclothes (set 0) model.
        for slot in [GearSlot::Head, GearSlot::Body, GearSlot::Hands, GearSlot::Legs, GearSlot::Feet] {
            if !gear.iter().any(|(s, _, _, _)| *s == slot) && slot != GearSlot::Head {
                gear.push((slot, ModelId { set: 0, body: 0, variant: 1 }, 0, [0, 0]));
            }
        }
        for (slot, model, item_id, dyes) in gear {
            if hidden_slots.contains(&slot) {
                notes.push(format!("{} hidden by another item", slot.name()));
                continue;
            }
            let mut part = self.gear_part(race, slot, model, item_id, &mut notes);
            part.dyes = dyes;
            parts.push(part);
        }

        // Weapons.
        let dyed = |mut p: ResolvedPart, gs: GearsetSlot| {
            p.dyes = appearance.item(gs).map(|it| it.stains()).unwrap_or([0, 0]);
            p
        };
        if let Some(item_id) = appearance.visible_item_id(GearsetSlot::MainHand) {
            match item_models(self.excel, item_id) {
                Ok(item) => {
                    notes.push(format!("main hand: item {item_id} {:?} {:?}", item.name, item.main));
                    parts.extend(self.weapon_part(PartKind::MainHand, item.main, item_id, &mut notes).map(|p| dyed(p, GearsetSlot::MainHand)));
                    if appearance.visible_item_id(GearsetSlot::OffHand).is_none() {
                        parts.extend(self.weapon_part(PartKind::OffHand, item.sub, item_id, &mut notes).map(|p| dyed(p, GearsetSlot::MainHand)));
                    }
                }
                Err(err) => notes.push(format!("main hand item {item_id}: {err:#}")),
            }
        }
        if let Some(item_id) = appearance.visible_item_id(GearsetSlot::OffHand) {
            match item_models(self.excel, item_id) {
                Ok(item) => {
                    notes.push(format!("off hand: item {item_id} {:?} {:?}", item.name, item.main));
                    parts.extend(self.weapon_part(PartKind::OffHand, item.main, item_id, &mut notes).map(|p| dyed(p, GearsetSlot::OffHand)));
                }
                Err(err) => notes.push(format!("off hand item {item_id}: {err:#}")),
            }
        }

        Ok(CharacterModelSet {
            race,
            skeleton_path: paths::skeleton(race),
            parts,
            notes,
            hidden_slots,
        })
    }
}

impl std::fmt::Display for CharacterModelSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "race {}  skeleton {}", self.race, self.skeleton_path)?;
        for p in &self.parts {
            writeln!(
                f,
                "[{:<9}] {} {}  (model race {}, attr mask 0x{:03x})",
                p.kind.name(),
                if p.mdl_exists { "ok     " } else { "MISSING" },
                p.mdl_path,
                p.model_race,
                p.attribute_mask
            )?;
            for m in &p.materials {
                writeln!(f, "            {} {}", if m.exists { "ok     " } else { "MISSING" }, m.mtrl_path)?;
            }
        }
        Ok(())
    }
}

impl CharacterModelSet {
    pub fn context_note(&self) -> String {
        self.notes.join("\n")
    }
}


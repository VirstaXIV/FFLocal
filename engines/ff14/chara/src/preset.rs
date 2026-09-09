//! Character presets as plain settings: every customize value and equipped item as a
//! `key = value` string, so a preset is a self-contained FFLocal configuration rather than a
//! pointer into the game's save files. "Import from game" fills these from `FFXIV_CHARA_*.dat`
//! and `GEARSET.DAT`; afterwards the game files are not needed to describe the character.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use physis::race::{Gender, Race, Tribe};
use physis::savedata::chardat::CustomizeData;

use crate::appearance::{Appearance, AppearanceSource};
use crate::gearset::{GearsetItem, GearsetSlot, SLOT_COUNT};

/// Customize keys in the order they are stored.
pub const CUSTOMIZE_KEYS: [&str; 27] = [
    "race",
    "tribe",
    "gender",
    "age",
    "height",
    "face",
    "hair",
    "highlights",
    "skin_tone",
    "right_eye_color",
    "hair_tone",
    "highlight_tone",
    "facial_features",
    "facial_feature_color",
    "eyebrows",
    "left_eye_color",
    "eyes",
    "nose",
    "jaw",
    "mouth",
    "lips_tone",
    "race_feature_size",
    "race_feature_type",
    "bust",
    "face_paint",
    "face_paint_color",
    "voice",
];

/// Item keys per gear-set slot (soul crystal and waist are not rendered).
pub const ITEM_KEYS: [(GearsetSlot, &str); 12] = [
    (GearsetSlot::MainHand, "item.mainhand"),
    (GearsetSlot::OffHand, "item.offhand"),
    (GearsetSlot::Head, "item.head"),
    (GearsetSlot::Body, "item.body"),
    (GearsetSlot::Hands, "item.hands"),
    (GearsetSlot::Legs, "item.legs"),
    (GearsetSlot::Feet, "item.feet"),
    (GearsetSlot::Ears, "item.ears"),
    (GearsetSlot::Neck, "item.neck"),
    (GearsetSlot::Wrists, "item.wrists"),
    (GearsetSlot::RingLeft, "item.ring_left"),
    (GearsetSlot::RingRight, "item.ring_right"),
];

/// Dye keys per slot (`dye.<slot>` first channel, `dye2.<slot>` second), Stain sheet rows.
pub fn dye_keys(item_key: &str) -> (String, String) {
    let slot = item_key.trim_start_matches("item.");
    (format!("dye.{slot}"), format!("dye2.{slot}"))
}

/// True when the settings describe a character themselves (as opposed to pointing at files).
pub fn has_customize(settings: &BTreeMap<String, String>) -> bool {
    settings.contains_key("race")
}

pub fn customize_to_settings(c: &CustomizeData, out: &mut BTreeMap<String, String>) {
    let mut set = |k: &str, v: u8| {
        out.insert(k.to_string(), v.to_string());
    };
    set("race", c.race as u8);
    set("tribe", c.tribe as u8);
    set("gender", c.gender.clone() as u8);
    set("age", c.age);
    set("height", c.height);
    set("face", c.face);
    set("hair", c.hair);
    set("highlights", u8::from(c.enable_highlights));
    set("skin_tone", c.skin_tone);
    set("right_eye_color", c.right_eye_color);
    set("hair_tone", c.hair_tone);
    set("highlight_tone", c.highlights);
    set("facial_features", c.facial_features);
    set("facial_feature_color", c.facial_feature_color);
    set("eyebrows", c.eyebrows);
    set("left_eye_color", c.left_eye_color);
    set("eyes", c.eyes);
    set("nose", c.nose);
    set("jaw", c.jaw);
    set("mouth", c.mouth);
    set("lips_tone", c.lips_tone_fur_pattern);
    set("race_feature_size", c.race_feature_size);
    set("race_feature_type", c.race_feature_type);
    set("bust", c.bust);
    set("face_paint", c.face_paint);
    set("face_paint_color", c.face_paint_color);
    set("voice", c.voice);
}

fn u8_setting(s: &BTreeMap<String, String>, key: &str, default: u8) -> u8 {
    s.get(key).and_then(|v| v.trim().parse::<i64>().ok()).map(|v| v.clamp(0, 255) as u8).unwrap_or(default)
}

/// Customize data from settings (`None` when the settings do not carry a character).
pub fn customize_from_settings(s: &BTreeMap<String, String>) -> Result<Option<CustomizeData>> {
    if !has_customize(s) {
        return Ok(None);
    }
    let d = CustomizeData::default();
    let race = Race::from_repr(u8_setting(s, "race", 1)).ok_or_else(|| anyhow!("unknown race {:?}", s.get("race")))?;
    let tribe = Tribe::from_repr(u8_setting(s, "tribe", 1)).ok_or_else(|| anyhow!("unknown tribe {:?}", s.get("tribe")))?;
    let gender = Gender::from_repr(u8_setting(s, "gender", 0)).ok_or_else(|| anyhow!("unknown gender {:?}", s.get("gender")))?;
    Ok(Some(CustomizeData {
        race,
        tribe,
        gender,
        age: u8_setting(s, "age", d.age),
        height: u8_setting(s, "height", d.height),
        face: u8_setting(s, "face", d.face),
        hair: u8_setting(s, "hair", d.hair),
        enable_highlights: u8_setting(s, "highlights", 0) != 0,
        skin_tone: u8_setting(s, "skin_tone", d.skin_tone),
        right_eye_color: u8_setting(s, "right_eye_color", d.right_eye_color),
        hair_tone: u8_setting(s, "hair_tone", d.hair_tone),
        highlights: u8_setting(s, "highlight_tone", d.highlights),
        facial_features: u8_setting(s, "facial_features", d.facial_features),
        facial_feature_color: u8_setting(s, "facial_feature_color", d.facial_feature_color),
        eyebrows: u8_setting(s, "eyebrows", d.eyebrows),
        left_eye_color: u8_setting(s, "left_eye_color", d.left_eye_color),
        eyes: u8_setting(s, "eyes", d.eyes),
        nose: u8_setting(s, "nose", d.nose),
        jaw: u8_setting(s, "jaw", d.jaw),
        mouth: u8_setting(s, "mouth", d.mouth),
        lips_tone_fur_pattern: u8_setting(s, "lips_tone", d.lips_tone_fur_pattern),
        race_feature_size: u8_setting(s, "race_feature_size", d.race_feature_size),
        race_feature_type: u8_setting(s, "race_feature_type", d.race_feature_type),
        bust: u8_setting(s, "bust", d.bust),
        face_paint: u8_setting(s, "face_paint", d.face_paint),
        face_paint_color: u8_setting(s, "face_paint_color", d.face_paint_color),
        voice: u8_setting(s, "voice", d.voice),
    }))
}

/// Store the *visible* item of each slot (glamour wins over the equipped item).
pub fn items_to_settings(items: &[Option<GearsetItem>; SLOT_COUNT], out: &mut BTreeMap<String, String>) {
    for (slot, key) in ITEM_KEYS {
        let id = items[slot as usize].map(|it| if it.glamour_id != 0 { it.glamour_id } else { it.item_id }).unwrap_or(0);
        out.insert(key.to_string(), if id == 0 { String::new() } else { id.to_string() });
        let stains = items[slot as usize].map(|it| it.stains()).unwrap_or([0, 0]);
        let (k1, k2) = dye_keys(key);
        out.insert(k1, if stains[0] == 0 { String::new() } else { stains[0].to_string() });
        out.insert(k2, if stains[1] == 0 { String::new() } else { stains[1].to_string() });
    }
}

pub fn items_from_settings(s: &BTreeMap<String, String>) -> [Option<GearsetItem>; SLOT_COUNT] {
    let mut items: [Option<GearsetItem>; SLOT_COUNT] = Default::default();
    for (slot, key) in ITEM_KEYS {
        if let Some(id) = s.get(key).and_then(|v| v.trim().parse::<u32>().ok())
            && id != 0
        {
            let (k1, k2) = dye_keys(key);
            let stain = |k: &str| s.get(k).and_then(|v| v.trim().parse::<u8>().ok()).unwrap_or(0);
            items[slot as usize] = Some(GearsetItem {
                item_id: id,
                hq: false,
                glamour_id: 0,
                dye: GearsetItem::pack_stains([stain(&k1), stain(&k2)]),
            });
        }
    }
    items
}

/// An appearance described entirely by preset settings.
pub struct SettingsSource<'a>(pub &'a BTreeMap<String, String>);

impl AppearanceSource for SettingsSource<'_> {
    fn load(&self) -> Result<Appearance> {
        let customize = customize_from_settings(self.0)?.ok_or_else(|| anyhow!("preset has no character data (import it from the game first)"))?;
        Ok(Appearance {
            customize,
            items: items_from_settings(self.0),
            notes: vec!["appearance from preset settings".into()],
        })
    }
}

pub fn race_name(r: Race) -> &'static str {
    match r {
        Race::Hyur => "Hyur",
        Race::Elezen => "Elezen",
        Race::Lalafell => "Lalafell",
        Race::Miqote => "Miqo'te",
        Race::Roegadyn => "Roegadyn",
        Race::AuRa => "Au Ra",
        Race::Hrothgar => "Hrothgar",
        Race::Viera => "Viera",
    }
}

pub fn tribe_name(t: Tribe) -> &'static str {
    match t {
        Tribe::Midlander => "Midlander",
        Tribe::Highlander => "Highlander",
        Tribe::Wildwood => "Wildwood",
        Tribe::Duskwight => "Duskwight",
        Tribe::Plainsfolk => "Plainsfolk",
        Tribe::Dunesfolk => "Dunesfolk",
        Tribe::Seeker => "Seeker of the Sun",
        Tribe::Keeper => "Keeper of the Moon",
        Tribe::SeaWolf => "Sea Wolf",
        Tribe::Hellsguard => "Hellsguard",
        Tribe::Raen => "Raen",
        Tribe::Xaela => "Xaela",
        Tribe::Hellion => "Helion",
        Tribe::Lost => "The Lost",
        Tribe::Rava => "Rava",
        Tribe::Veena => "Veena",
    }
}

/// The two tribes of a race.
pub fn tribes_of(r: Race) -> [Tribe; 2] {
    match r {
        Race::Hyur => [Tribe::Midlander, Tribe::Highlander],
        Race::Elezen => [Tribe::Wildwood, Tribe::Duskwight],
        Race::Lalafell => [Tribe::Plainsfolk, Tribe::Dunesfolk],
        Race::Miqote => [Tribe::Seeker, Tribe::Keeper],
        Race::Roegadyn => [Tribe::SeaWolf, Tribe::Hellsguard],
        Race::AuRa => [Tribe::Raen, Tribe::Xaela],
        Race::Hrothgar => [Tribe::Hellion, Tribe::Lost],
        Race::Viera => [Tribe::Rava, Tribe::Veena],
    }
}

pub const ALL_RACES: [Race; 8] = [Race::Hyur, Race::Elezen, Race::Lalafell, Race::Miqote, Race::Roegadyn, Race::AuRa, Race::Hrothgar, Race::Viera];

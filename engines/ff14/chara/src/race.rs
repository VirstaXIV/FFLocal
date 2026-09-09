//! Race codes (`c0801` etc.), the model fallback chain, and character asset path builders.
//!
//! Path formats follow Penumbra.GameData `GamePaths.cs`; the fallback chain follows its
//! `GenderRace.Fallback()`.

use physis::race::{Gender, Race, Tribe};

/// A gender/race model code such as 101 (Midlander male) or 801 (Miqo'te female).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RaceCode(pub u16);

impl RaceCode {
    pub const MIDLANDER_MALE: RaceCode = RaceCode(101);
    pub const MIDLANDER_FEMALE: RaceCode = RaceCode(201);
    pub const HIGHLANDER_MALE: RaceCode = RaceCode(301);
    pub const HIGHLANDER_FEMALE: RaceCode = RaceCode(401);
    pub const ELEZEN_MALE: RaceCode = RaceCode(501);
    pub const ELEZEN_FEMALE: RaceCode = RaceCode(601);
    pub const MIQOTE_MALE: RaceCode = RaceCode(701);
    pub const MIQOTE_FEMALE: RaceCode = RaceCode(801);
    pub const ROEGADYN_MALE: RaceCode = RaceCode(901);
    pub const ROEGADYN_FEMALE: RaceCode = RaceCode(1001);
    pub const LALAFELL_MALE: RaceCode = RaceCode(1101);
    pub const LALAFELL_FEMALE: RaceCode = RaceCode(1201);
    pub const AURA_MALE: RaceCode = RaceCode(1301);
    pub const AURA_FEMALE: RaceCode = RaceCode(1401);
    pub const HROTHGAR_MALE: RaceCode = RaceCode(1501);
    pub const HROTHGAR_FEMALE: RaceCode = RaceCode(1601);
    pub const VIERA_MALE: RaceCode = RaceCode(1701);
    pub const VIERA_FEMALE: RaceCode = RaceCode(1801);

    /// Four-digit code as used in paths (`0801`).
    pub fn code(self) -> String {
        format!("{:04}", self.0)
    }

    pub fn from_customize(race: Race, tribe: Tribe, gender: &Gender) -> Option<RaceCode> {
        let female = *gender == Gender::Female;
        let base = match (race, tribe) {
            (Race::Hyur, Tribe::Midlander) => 101,
            (Race::Hyur, Tribe::Highlander) => 301,
            (Race::Elezen, _) => 501,
            (Race::Miqote, _) => 701,
            (Race::Roegadyn, _) => 901,
            (Race::Lalafell, _) => 1101,
            (Race::AuRa, _) => 1301,
            (Race::Hrothgar, _) => 1501,
            (Race::Viera, _) => 1701,
            _ => return None,
        };
        Some(RaceCode(if female { base + 100 } else { base }))
    }

    pub fn is_female(self) -> bool {
        (self.0 / 100) % 2 == 0
    }

    /// The model race the game falls back to when this race has no dedicated model.
    pub fn fallback(self) -> Option<RaceCode> {
        match self {
            RaceCode::MIDLANDER_MALE => None,
            RaceCode::MIDLANDER_FEMALE => Some(RaceCode::MIDLANDER_MALE),
            RaceCode::HROTHGAR_MALE => Some(RaceCode::ROEGADYN_MALE),
            RaceCode::LALAFELL_FEMALE => Some(RaceCode::LALAFELL_MALE),
            _ if self.is_female() => Some(RaceCode::MIDLANDER_FEMALE),
            _ => Some(RaceCode::MIDLANDER_MALE),
        }
    }

    /// This race followed by its fallback chain, ending in Midlander male.
    pub fn dependencies(self) -> Vec<RaceCode> {
        let mut out = vec![self];
        let mut cur = self;
        while let Some(next) = cur.fallback() {
            if out.contains(&next) {
                break;
            }
            out.push(next);
            cur = next;
        }
        out
    }

    /// Penumbra `MaterialHandling.GetGameGenderRace`: which race's hair *materials* are used.
    pub fn hair_material_race(self, hair_id: u16) -> RaceCode {
        if self == RaceCode::HROTHGAR_MALE || self == RaceCode::HROTHGAR_FEMALE {
            return self;
        }
        let midlander = if self.is_female() {
            RaceCode::MIDLANDER_FEMALE
        } else {
            RaceCode::MIDLANDER_MALE
        };
        match hair_id {
            101..=115 => {
                if self == RaceCode::MIQOTE_MALE || self == RaceCode::MIQOTE_FEMALE {
                    self
                } else {
                    midlander
                }
            }
            116..=200 => midlander,
            _ => self,
        }
    }
}

impl std::fmt::Display for RaceCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "c{:04}", self.0)
    }
}

/// Equipment slots that have their own models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GearSlot {
    Head,
    Body,
    Hands,
    Legs,
    Feet,
    Ears,
    Neck,
    Wrists,
    RingRight,
    RingLeft,
}

impl GearSlot {
    pub const ALL: [GearSlot; 10] = [
        GearSlot::Head,
        GearSlot::Body,
        GearSlot::Hands,
        GearSlot::Legs,
        GearSlot::Feet,
        GearSlot::Ears,
        GearSlot::Neck,
        GearSlot::Wrists,
        GearSlot::RingRight,
        GearSlot::RingLeft,
    ];

    pub fn is_accessory(self) -> bool {
        matches!(
            self,
            GearSlot::Ears | GearSlot::Neck | GearSlot::Wrists | GearSlot::RingRight | GearSlot::RingLeft
        )
    }

    /// Penumbra's slot name in its metadata edits.
    pub fn penumbra_name(self) -> &'static str {
        match self {
            GearSlot::Head => "Head",
            GearSlot::Body => "Body",
            GearSlot::Hands => "Hands",
            GearSlot::Legs => "Legs",
            GearSlot::Feet => "Feet",
            GearSlot::Ears => "Ears",
            GearSlot::Neck => "Neck",
            GearSlot::Wrists => "Wrists",
            GearSlot::RingRight => "RFinger",
            GearSlot::RingLeft => "LFinger",
        }
    }

    /// Path suffix (`met`, `top`, ...).
    pub fn suffix(self) -> &'static str {
        match self {
            GearSlot::Head => "met",
            GearSlot::Body => "top",
            GearSlot::Hands => "glv",
            GearSlot::Legs => "dwn",
            GearSlot::Feet => "sho",
            GearSlot::Ears => "ear",
            GearSlot::Neck => "nek",
            GearSlot::Wrists => "wrs",
            GearSlot::RingRight => "rir",
            GearSlot::RingLeft => "ril",
        }
    }

    /// Bit offset in an EQDP entry (equipment and accessory files share the layout).
    pub fn eqdp_offset(self) -> u16 {
        match self {
            GearSlot::Head | GearSlot::Ears => 0,
            GearSlot::Body | GearSlot::Neck => 2,
            GearSlot::Hands | GearSlot::Wrists => 4,
            GearSlot::Legs | GearSlot::RingRight => 6,
            GearSlot::Feet | GearSlot::RingLeft => 8,
        }
    }

    /// Part index in an IMC file.
    pub fn imc_part(self) -> usize {
        match self {
            GearSlot::Head | GearSlot::Ears => 0,
            GearSlot::Body | GearSlot::Neck => 1,
            GearSlot::Hands | GearSlot::Wrists => 2,
            GearSlot::Legs | GearSlot::RingRight => 3,
            GearSlot::Feet | GearSlot::RingLeft => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            GearSlot::Head => "head",
            GearSlot::Body => "body",
            GearSlot::Hands => "hands",
            GearSlot::Legs => "legs",
            GearSlot::Feet => "feet",
            GearSlot::Ears => "ears",
            GearSlot::Neck => "neck",
            GearSlot::Wrists => "wrists",
            GearSlot::RingRight => "ring_r",
            GearSlot::RingLeft => "ring_l",
        }
    }
}

/// Character customization body parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BodyPart {
    Body,
    Face,
    Hair,
    Tail,
    Ears,
}

impl BodyPart {
    /// (`obj/<dir>`, abbreviation, model suffix)
    fn parts(self) -> (&'static str, &'static str, &'static str) {
        match self {
            BodyPart::Body => ("body", "b", "top"),
            BodyPart::Face => ("face", "f", "fac"),
            BodyPart::Hair => ("hair", "h", "hir"),
            BodyPart::Tail => ("tail", "t", "til"),
            BodyPart::Ears => ("zear", "z", "zer"),
        }
    }
}

/// Path builders.
pub mod paths {
    use super::{BodyPart, GearSlot, RaceCode};

    /// Attach offsets (weapon placements) for a race.
    pub fn atch(race: RaceCode) -> String {
        format!("chara/xls/attachOffset/c{}.atch", race.code())
    }

    pub fn skeleton(race: RaceCode) -> String {
        format!(
            "chara/human/c{0}/skeleton/base/b0001/skl_c{0}b0001.sklb",
            race.code()
        )
    }

    /// Extra skeleton (`hair`, `face`, `met`, `top`) with the EST skeleton id.
    pub fn extra_skeleton(race: RaceCode, slot: &str, id: u16) -> String {
        let c = slot.chars().next().unwrap_or('h');
        format!(
            "chara/human/c{0}/skeleton/{slot}/{c}{id:04}/skl_c{0}{c}{id:04}.sklb",
            race.code()
        )
    }

    pub fn body_part_mdl(race: RaceCode, part: BodyPart, id: u16) -> String {
        let (dir, abbr, suffix) = part.parts();
        format!(
            "chara/human/c{0}/obj/{dir}/{abbr}{id:04}/model/c{0}{abbr}{id:04}_{suffix}.mdl",
            race.code()
        )
    }

    /// Material directory for a customization part (faces have no variant folder).
    pub fn body_part_material_dir(race: RaceCode, part: BodyPart, id: u16) -> String {
        let (dir, abbr, _) = part.parts();
        match part {
            BodyPart::Face => format!("chara/human/c{}/obj/{dir}/{abbr}{id:04}/material", race.code()),
            _ => format!(
                "chara/human/c{}/obj/{dir}/{abbr}{id:04}/material/v0001",
                race.code()
            ),
        }
    }

    pub fn gear_mdl(race: RaceCode, slot: GearSlot, set: u16) -> String {
        if slot.is_accessory() {
            format!(
                "chara/accessory/a{set:04}/model/c{}a{set:04}_{}.mdl",
                race.code(),
                slot.suffix()
            )
        } else {
            format!(
                "chara/equipment/e{set:04}/model/c{}e{set:04}_{}.mdl",
                race.code(),
                slot.suffix()
            )
        }
    }

    pub fn gear_material_dir(slot: GearSlot, set: u16, variant: u16) -> String {
        if slot.is_accessory() {
            format!("chara/accessory/a{set:04}/material/v{variant:04}")
        } else {
            format!("chara/equipment/e{set:04}/material/v{variant:04}")
        }
    }

    pub fn gear_imc(slot: GearSlot, set: u16) -> String {
        if slot.is_accessory() {
            format!("chara/accessory/a{set:04}/a{set:04}.imc")
        } else {
            format!("chara/equipment/e{set:04}/e{set:04}.imc")
        }
    }

    pub fn eqdp(race: RaceCode, accessory: bool) -> String {
        if accessory {
            format!(
                "chara/xls/charadb/accessorydeformerparameter/c{}.eqdp",
                race.code()
            )
        } else {
            format!(
                "chara/xls/charadb/equipmentdeformerparameter/c{}.eqdp",
                race.code()
            )
        }
    }

    pub fn weapon_mdl(set: u16, body: u16) -> String {
        format!("chara/weapon/w{set:04}/obj/body/b{body:04}/model/w{set:04}b{body:04}.mdl")
    }

    pub fn weapon_material_dir(set: u16, body: u16, variant: u16) -> String {
        format!("chara/weapon/w{set:04}/obj/body/b{body:04}/material/v{variant:04}")
    }

    pub fn weapon_imc(set: u16, body: u16) -> String {
        format!("chara/weapon/w{set:04}/obj/body/b{body:04}/b{body:04}.imc")
    }

    pub fn weapon_skeleton(set: u16) -> String {
        format!("chara/weapon/w{set:04}/skeleton/base/b0001/skl_w{set:04}b0001.sklb")
    }

    pub const PBD: &str = "chara/xls/boneDeformer/human.pbd";
    pub const CMP: &str = "chara/xls/charamake/human.cmp";

    /// Face paint decal `n` (customize `face_paint`, 0 = none): a BC4 front projection.
    pub fn face_decal(n: u8) -> String {
        format!("chara/common/texture/decal_face/_decal_{n}.tex")
    }
    pub const EST_HEAD: &str = "chara/xls/charadb/extra_met.est";
    pub const EST_BODY: &str = "chara/xls/charadb/extra_top.est";
    pub const EST_HAIR: &str = "chara/xls/charadb/hairskeletontemplate.est";
    pub const EST_FACE: &str = "chara/xls/charadb/faceskeletontemplate.est";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_chains() {
        assert_eq!(
            RaceCode::MIQOTE_FEMALE.dependencies(),
            vec![RaceCode::MIQOTE_FEMALE, RaceCode::MIDLANDER_FEMALE, RaceCode::MIDLANDER_MALE]
        );
        assert_eq!(
            RaceCode::HROTHGAR_MALE.dependencies(),
            vec![RaceCode::HROTHGAR_MALE, RaceCode::ROEGADYN_MALE, RaceCode::MIDLANDER_MALE]
        );
        assert_eq!(
            RaceCode::LALAFELL_FEMALE.dependencies(),
            vec![RaceCode::LALAFELL_FEMALE, RaceCode::LALAFELL_MALE, RaceCode::MIDLANDER_MALE]
        );
    }

    #[test]
    fn codes() {
        assert_eq!(
            RaceCode::from_customize(Race::Lalafell, Tribe::Dunesfolk, &Gender::Female),
            Some(RaceCode::LALAFELL_FEMALE)
        );
        assert_eq!(RaceCode::VIERA_MALE.code(), "1701");
        assert!(RaceCode::AURA_FEMALE.is_female());
        assert!(!RaceCode::AURA_MALE.is_female());
    }
}

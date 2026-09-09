//! Player-character file tables: which DAT file ids hold a race's skeleton and motions, and
//! how `(race, slot, model id)` becomes a file id. The tables are the client's own
//! (`FFXiMain.dll`, as ported by xi-tools / Atom0s' `common_geartables.php` and baked into
//! xi-model-viewer's `modelids.js`); the base ids match the TDW viewer's `pcdat` table.
//!
//! A slot's table is a list of `(first file id, count)` groups; the model id counts across the
//! groups, so a group of `(7368, 256)` maps model 0 → 7368 and a `(0, 64)` group reserves 64
//! model ids without files.

use std::collections::BTreeMap;

use ffl_core::CharacterPreset;

/// The client's look race numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Race {
    HumeMale = 1,
    HumeFemale = 2,
    ElvaanMale = 3,
    ElvaanFemale = 4,
    TarutaruMale = 5,
    TarutaruFemale = 6,
    Mithra = 7,
    Galka = 8,
}

impl Race {
    pub const ALL: [Race; 8] = [
        Race::HumeMale,
        Race::HumeFemale,
        Race::ElvaanMale,
        Race::ElvaanFemale,
        Race::TarutaruMale,
        Race::TarutaruFemale,
        Race::Mithra,
        Race::Galka,
    ];

    pub fn from_id(id: u8) -> Option<Race> {
        Race::ALL.into_iter().find(|r| *r as u8 == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Race::HumeMale => "Hume ♂",
            Race::HumeFemale => "Hume ♀",
            Race::ElvaanMale => "Elvaan ♂",
            Race::ElvaanFemale => "Elvaan ♀",
            Race::TarutaruMale => "Tarutaru ♂",
            Race::TarutaruFemale => "Tarutaru ♀",
            Race::Mithra => "Mithra",
            Race::Galka => "Galka",
        }
    }

    /// Index into the per-race tables below.
    fn index(self) -> usize {
        self as usize - 1
    }

    /// File id of the base DAT: the skeleton and the basic motions (lower body); `+1` holds
    /// the upper-body motions, `+3`/`+4` the waist packs.
    pub fn base_file(self) -> u32 {
        [7072, 10248, 13424, 16600, 19776, 19776, 23176, 26352][self.index()]
    }

    /// First emote motion DAT (eight routines per file, the waist parts six files later).
    pub fn emote_file(self) -> u32 {
        [10056, 13232, 16408, 19584, 22760, 22984, 26160, 29336][self.index()]
    }

    /// Number of emote schedule DATs after `emote_file`.
    pub const EMOTE_FILES: u32 = 5;

    /// Battle motion DAT (draw/sheathe, battle idle, engaged upper-body walk/run, attacks)
    /// per weapon animation type (the weapon `info` byte 3, 0..15); xi-model-viewer
    /// `characters.json` `battleByType` resolved to file ids on the Steam install.
    pub fn battle_file(self, weapon_type: u8) -> u32 {
        const T: [[u32; 16]; 8] = [
            [9672, 9673, 9674, 9675, 9676, 9677, 9678, 9679, 9680, 9672, 9682, 9672, 9672, 9672, 9672, 9672],
            [12848, 12849, 12850, 12851, 12852, 12853, 12854, 12855, 12848, 12848, 12858, 12848, 12848, 12848, 12848, 12848],
            [16024, 16025, 16026, 16027, 16028, 16029, 16030, 16031, 16032, 16033, 16034, 16024, 16024, 16024, 16024, 16024],
            [19200, 19201, 19202, 19203, 19200, 19205, 19206, 19207, 19200, 19200, 19210, 19200, 19200, 19200, 19200, 19200],
            [22376, 22377, 22378, 22379, 22380, 22381, 22376, 22376, 22376, 22376, 22386, 22376, 22376, 22376, 22376, 22376],
            [22376, 22377, 22378, 22379, 22380, 22381, 22376, 22376, 22376, 22376, 22386, 22376, 22376, 22376, 22376, 22376],
            [25776, 25777, 25778, 25779, 25780, 25781, 25782, 25783, 25784, 25776, 25786, 25776, 25776, 25776, 25776, 25776],
            [28952, 28953, 28954, 28955, 28956, 28957, 28958, 28959, 28960, 28952, 28962, 28952, 28952, 28952, 28952, 28952],
        ];
        T[self.index()][(weapon_type as usize).min(15)]
    }

    /// The waist (`btl2`) companion of a battle DAT: the client's `MotionBFileNo` tables
    /// (viewer file numbers `folder × 1000 + file`): main packs start at `base` and the
    /// skirt block follows `num` files later (`2 × num` for bodies whose `info` byte 9 is 2).
    pub fn battle_skirt_number(self, battle_number: u32, waist_variant: u8) -> Option<u32> {
        const BASE: [u32; 8] = [32013, 36117, 41084, 46057, 51019, 51019, 56014, 60112];
        const NUM: [u32; 8] = [9, 8, 10, 6, 6, 6, 9, 8];
        let (base, num) = (BASE[self.index()], NUM[self.index()]);
        let k = if waist_variant == 2 { 2 } else { 1 };
        (battle_number >= base && battle_number < base + num).then(|| battle_number + k * num)
    }

    /// Entry of a footstep sound group that plays for this race: the zone's `SeSep` sections
    /// are named `<race><surface><variant>` (`0111`, `0711`, ...) and each race's section
    /// points at its own entry of the shared `se100..se140` bank (a second entry 10 higher is
    /// the alternate). Elvaan have no sections of their own and take the Hume entries.
    pub fn footstep_entry(self) -> u32 {
        match self {
            Race::HumeMale | Race::ElvaanMale => 1,
            Race::HumeFemale | Race::ElvaanFemale => 6,
            Race::TarutaruMale => 5,
            Race::TarutaruFemale => 3,
            Race::Mithra => 2,
            Race::Galka => 4,
        }
    }

    /// Approximate standing height in metres (for the collider).
    pub fn height(self) -> f32 {
        match self {
            Race::HumeMale | Race::HumeFemale | Race::Mithra => 1.75,
            Race::ElvaanMale | Race::ElvaanFemale => 1.95,
            Race::TarutaruMale | Race::TarutaruFemale => 1.0,
            Race::Galka => 2.2,
        }
    }
}

/// Equipment slots in look-string order, with the preset key of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Face,
    Head,
    Body,
    Hands,
    Legs,
    Feet,
    Main,
    Sub,
    Range,
}

impl Slot {
    pub const ALL: [Slot; 9] = [Slot::Face, Slot::Head, Slot::Body, Slot::Hands, Slot::Legs, Slot::Feet, Slot::Main, Slot::Sub, Slot::Range];
    pub const GEAR: [Slot; 8] = [Slot::Head, Slot::Body, Slot::Hands, Slot::Legs, Slot::Feet, Slot::Main, Slot::Sub, Slot::Range];

    pub fn key(self) -> &'static str {
        match self {
            Slot::Face => "face",
            Slot::Head => "head",
            Slot::Body => "body",
            Slot::Hands => "hands",
            Slot::Legs => "legs",
            Slot::Feet => "feet",
            Slot::Main => "main",
            Slot::Sub => "sub",
            Slot::Range => "range",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Slot::Face => "Face",
            Slot::Head => "Head",
            Slot::Body => "Body",
            Slot::Hands => "Hands",
            Slot::Legs => "Legs",
            Slot::Feet => "Feet",
            Slot::Main => "Main hand",
            Slot::Sub => "Off hand",
            Slot::Range => "Ranged",
        }
    }

    pub fn from_key(key: &str) -> Option<Slot> {
        Slot::ALL.into_iter().find(|s| s.key() == key)
    }

    pub fn is_weapon(self) -> bool {
        matches!(self, Slot::Main | Slot::Sub | Slot::Range)
    }
}

type Groups = &'static [(u32, u32)];

/// `(face, head, body, hands, legs, feet, main, sub, range)` groups per race.
fn tables(race: Race) -> [Groups; 9] {
    match race {
        Race::HumeMale => [
            &[(7080, 32)],
            &[(7112, 256), (63323, 48), (63371, 16), (71247, 256), (98787, 32), (102961, 64)],
            &[(7368, 256), (63387, 48), (63435, 16), (71503, 256), (98819, 32), (103025, 64)],
            &[(7624, 256), (63451, 48), (63499, 16), (71759, 256), (98851, 32), (103089, 64)],
            &[(7880, 256), (63515, 48), (63563, 16), (72015, 256), (98883, 32), (103153, 64)],
            &[(8136, 256), (63579, 48), (63627, 16), (72271, 256), (98915, 32), (103217, 64)],
            &[(8392, 512), (63643, 128), (72527, 256), (107301, 300), (0, 64)],
            &[(41199, 512), (66459, 128), (81999, 256), (105201, 300), (0, 64)],
            &[(9416, 256)],
        ],
        Race::HumeFemale => [
            &[(10256, 32)],
            &[(10288, 256), (63771, 48), (63819, 16), (72783, 256), (98947, 32), (103281, 64)],
            &[(10544, 256), (63835, 48), (63883, 16), (73039, 256), (98979, 32), (103345, 64)],
            &[(10800, 256), (63899, 48), (63947, 16), (73295, 256), (99011, 32), (103409, 64)],
            &[(11056, 256), (63963, 48), (64011, 16), (73551, 256), (99043, 32), (103473, 64)],
            &[(11312, 256), (64027, 48), (64075, 16), (73807, 256), (99075, 32), (103537, 64)],
            &[(11568, 512), (64091, 128), (74063, 256), (107601, 300), (0, 64)],
            &[(42479, 512), (66587, 128), (82255, 256), (105501, 300), (0, 64)],
            &[(12592, 256)],
        ],
        Race::ElvaanMale => [
            &[(13432, 32)],
            &[(13464, 256), (64219, 48), (64267, 16), (74319, 256), (99107, 32), (103601, 64)],
            &[(13720, 256), (64283, 48), (64331, 16), (74575, 256), (99139, 32), (103665, 64)],
            &[(13976, 256), (64347, 48), (64395, 16), (74831, 256), (99171, 32), (103729, 64)],
            &[(14232, 256), (64411, 48), (64459, 16), (75087, 256), (99203, 32), (103793, 64)],
            &[(14488, 256), (64475, 48), (64523, 16), (75343, 256), (99235, 32), (103857, 64)],
            &[(14744, 512), (64539, 128), (75599, 256), (107901, 300), (0, 64)],
            &[(43759, 512), (66715, 128), (82511, 256), (105801, 300), (0, 64)],
            &[(15768, 256)],
        ],
        Race::ElvaanFemale => [
            &[(16608, 32)],
            &[(16640, 256), (64667, 48), (64715, 16), (75855, 256), (99267, 32), (103921, 64)],
            &[(16896, 256), (64731, 48), (64779, 16), (76111, 256), (99299, 32), (103985, 64)],
            &[(17152, 256), (64795, 48), (64843, 16), (76367, 256), (99331, 32), (104049, 64)],
            &[(17408, 256), (64859, 48), (64907, 16), (76623, 256), (99363, 32), (104113, 64)],
            &[(17664, 256), (64923, 48), (64971, 16), (76879, 256), (99395, 32), (104177, 64)],
            &[(17920, 512), (64987, 128), (77135, 256), (108201, 300), (0, 64)],
            &[(45039, 512), (66843, 128), (82767, 256), (106101, 300), (0, 64)],
            &[(18944, 256)],
        ],
        Race::TarutaruMale => [
            &[(19784, 32)],
            &[(19816, 256), (65115, 48), (65163, 16), (77391, 256), (99427, 32), (104241, 64)],
            &[(20072, 256), (65179, 48), (65227, 16), (77647, 256), (99459, 32), (104305, 64)],
            &[(20328, 256), (65243, 48), (65291, 16), (77903, 256), (99491, 32), (104369, 64)],
            &[(20584, 256), (65307, 48), (65355, 16), (78159, 256), (99523, 32), (104433, 64)],
            &[(20840, 256), (65371, 48), (65419, 16), (78415, 256), (99555, 32), (104497, 64)],
            &[(21096, 512), (65435, 128), (78671, 256), (108501, 300), (0, 64)],
            &[(46319, 512), (66971, 128), (83023, 256), (106401, 300), (0, 64)],
            &[(22120, 256)],
        ],
        Race::TarutaruFemale => [
            &[(22952, 32)],
            &[(19816, 256), (65115, 48), (65171, 16), (77391, 256), (99443, 32), (104241, 64)],
            &[(20072, 256), (65179, 48), (65235, 16), (77647, 256), (99475, 32), (104305, 64)],
            &[(20328, 256), (65243, 48), (65299, 16), (77903, 256), (99507, 32), (104369, 64)],
            &[(20584, 256), (65307, 48), (65363, 16), (78159, 256), (99539, 32), (104433, 64)],
            &[(20840, 256), (65371, 48), (65427, 16), (78415, 256), (99571, 32), (104497, 64)],
            &[(21096, 512), (65435, 128), (78671, 256), (108501, 300), (0, 64)],
            &[(46319, 512), (66971, 128), (83023, 256), (106401, 300), (0, 64)],
            &[(22120, 256)],
        ],
        Race::Mithra => [
            &[(23184, 32)],
            &[(23216, 256), (65563, 48), (65611, 16), (78927, 256), (99587, 32), (104561, 64)],
            &[(23472, 256), (65627, 48), (65675, 16), (79183, 256), (99619, 32), (104625, 64)],
            &[(23728, 256), (65691, 48), (65739, 16), (79439, 256), (99651, 32), (104689, 64)],
            &[(23984, 256), (65755, 48), (65803, 16), (79695, 256), (99683, 32), (104753, 64)],
            &[(24240, 256), (65819, 48), (65867, 16), (79951, 256), (99715, 32), (104817, 64)],
            &[(24496, 512), (65883, 128), (80207, 256), (108801, 300), (0, 64)],
            &[(47599, 512), (67099, 128), (83279, 256), (106701, 300), (0, 64)],
            &[(25520, 256)],
        ],
        Race::Galka => [
            &[(26360, 32)],
            &[(26392, 256), (66011, 48), (66059, 16), (80463, 256), (99747, 32), (104881, 64)],
            &[(26648, 256), (66075, 48), (66123, 16), (80719, 256), (99779, 32), (104945, 64)],
            &[(26904, 256), (66139, 48), (66187, 16), (80975, 256), (99811, 32), (105009, 64)],
            &[(27160, 256), (66203, 48), (66251, 16), (81231, 256), (99843, 32), (105073, 64)],
            &[(27416, 256), (66267, 48), (66315, 16), (81487, 256), (99875, 32), (105137, 64)],
            &[(27672, 512), (66331, 128), (81743, 256), (109101, 300), (0, 64)],
            &[(48879, 512), (67227, 128), (83535, 256), (107001, 300), (0, 64)],
            &[(28696, 256)],
        ],
    }
}

/// File id of a model in a slot for a race (`None` for reserved or out-of-range ids).
pub fn model_file(race: Race, slot: Slot, model: u16) -> Option<u32> {
    let groups = tables(race)[Slot::ALL.iter().position(|s| *s == slot)?];
    let mut cursor = 0u32;
    for (base, count) in groups {
        let m = model as u32;
        if m >= cursor && m < cursor + count {
            return (*base != 0).then_some(base + (m - cursor));
        }
        cursor += count;
    }
    None
}

/// Every `(model id, file id)` a slot's table maps for a race.
pub fn slot_models(race: Race, slot: Slot) -> Vec<(u16, u32)> {
    let groups = tables(race)[Slot::ALL.iter().position(|s| *s == slot).unwrap_or(0)];
    let mut out = Vec::new();
    let mut cursor = 0u32;
    for (base, count) in groups {
        if *base != 0 {
            for i in 0..*count {
                out.push(((cursor + i) as u16, base + i));
            }
        }
        cursor += count;
    }
    out
}

/// The preset keys of an FFXI character.
pub const RACE_KEY: &str = "race";
pub const FACE_KEY: &str = "face";
pub const SIZE_KEY: &str = "size";

pub fn gear_key(slot: Slot) -> String {
    format!("model.{}", slot.key())
}

/// A character's look: race, face and one model id per equipment slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Look {
    pub race: Race,
    pub face: u16,
    /// 0 small, 1 medium, 2 large (not rendered yet).
    pub size: u8,
    pub gear: BTreeMap<Slot, u16>,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            race: Race::HumeMale,
            face: 0,
            size: 1,
            gear: BTreeMap::new(),
        }
    }
}

impl Look {
    pub fn from_preset(preset: &CharacterPreset) -> Look {
        let get_u16 = |k: &str| preset.get(k).and_then(|v| v.trim().parse::<u16>().ok()).unwrap_or(0);
        let race = preset.get(RACE_KEY).and_then(|v| v.trim().parse::<u8>().ok()).and_then(Race::from_id).unwrap_or(Race::HumeMale);
        let mut gear = BTreeMap::new();
        for slot in Slot::GEAR {
            gear.insert(slot, get_u16(&gear_key(slot)));
        }
        Look {
            race,
            face: get_u16(FACE_KEY),
            size: preset.get(SIZE_KEY).and_then(|v| v.trim().parse::<u8>().ok()).unwrap_or(1).min(2),
            gear,
        }
    }

    pub fn model(&self, slot: Slot) -> u16 {
        self.gear.get(&slot).copied().unwrap_or(0)
    }
}

impl std::cmp::PartialOrd for Slot {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for Slot {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}

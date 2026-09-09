//! A loaded character: skeleton, parts and animation clips, engine-agnostic.

use std::collections::HashMap;

use crate::mesh::ModelData;

#[derive(Debug, Clone)]
pub struct BoneData {
    pub name: String,
    pub parent: Option<usize>,
    pub translation: [f32; 3],
    /// Quaternion x, y, z, w.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

#[derive(Debug, Clone, Default)]
pub struct SkeletonData {
    pub bones: Vec<BoneData>,
}

impl SkeletonData {
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.name == name)
    }
}

/// How a part follows the skeleton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attach {
    /// Skinned to the skeleton by bone name.
    Skinned,
    /// Rigidly parented to one bone (weapons, accessories).
    Bone(String),
    /// For placements of skinned addons: the skeleton bone `bone` is re-parented onto the
    /// bone `onto` with an identity local transform (its own animation tracks are ignored
    /// while it is), the way FFXI hangs a drawn weapon's grip joint off the hand joint.
    /// `Attach::Skinned` as the other placement restores the bone's own parent.
    Reparent { bone: String, onto: String },
}

#[derive(Debug, Clone)]
pub struct CharacterPart {
    pub name: String,
    pub model: ModelData,
    pub attach: Attach,
    /// Addon this part belongs to (see [`AddonDef`]); `None` for the character's own body/gear.
    pub addon: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddonKind {
    Weapon,
    Accessory,
    Effect,
    Other,
}

/// Engine-agnostic rules the runtime applies to an addon's visibility and placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddonRule {
    /// Hidden while an emote plays.
    HideDuringEmotes,
    /// Hidden while the character stands idle.
    HideWhileIdle,
    /// Hidden while the character moves.
    HideWhileMoving,
    /// Moved to its stowed placement (or hidden when it has none) while any action other than
    /// idles and locomotion plays, except the actions listed in [`AddonDef::active_actions`].
    StowDuringActions,
}

/// A place an addon can sit: in the hand, sheathed on the back or hip, holstered.
#[derive(Debug, Clone, PartialEq)]
pub struct AddonPlacement {
    /// Engine-scoped id (`drawn`, `sheathed`).
    pub id: String,
    pub name: String,
    pub attach: Attach,
    /// Offset from the attach bone (translation, rotation, scale).
    pub offset: crate::scene::Trs,
}

/// Something attached to a character that is not the character itself: weapons, tools, effects.
/// The runtime can toggle addons on and off, move them between placements and applies rules.
#[derive(Debug, Clone)]
pub struct AddonDef {
    /// Engine-scoped id (e.g. `weapon:main`).
    pub id: String,
    pub name: String,
    pub kind: AddonKind,
    /// Shown by default.
    pub enabled: bool,
    pub rules: Vec<AddonRule>,
    /// Where the addon can be placed; empty means "wherever its parts attach".
    pub placements: Vec<AddonPlacement>,
    /// Placement id while in use (drawn weapon).
    pub active: Option<String>,
    /// Placement id while stowed (sheathed weapon); `None` hides the addon when stowed.
    pub stowed: Option<String>,
    /// Action ids that keep the addon active despite [`AddonRule::StowDuringActions`].
    pub active_actions: Vec<String>,
    /// Clip played when the addon goes to its active placement (drawing a weapon).
    pub activate: Option<AddonTransition>,
    /// Clip played when the addon goes to its stowed placement (sheathing).
    pub stow: Option<AddonTransition>,
}

/// A character clip that accompanies a placement change, and when during it the addon
/// actually changes placement (seconds from the clip start).
#[derive(Debug, Clone, PartialEq)]
pub struct AddonTransition {
    pub clip: String,
    pub switch_at: f32,
}

impl AddonDef {
    pub fn placement(&self, id: &str) -> Option<&AddonPlacement> {
        self.placements.iter().find(|p| p.id == id)
    }

    /// Whether the runtime can move it between an active and a stowed placement.
    pub fn can_stow(&self) -> bool {
        self.active.is_some() && self.stowed.is_some()
    }
}

/// One sampled bone pose.
#[derive(Debug, Clone, Copy)]
pub struct ClipPose {
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Foot {
    Left,
    Right,
}

/// Something that happens at a point in a clip (the game's animation timeline entries).
#[derive(Debug, Clone, PartialEq)]
pub enum ClipEventKind {
    /// A foot lands; the sound depends on the surface under the character.
    /// `variant` is the engine's footstep sound id (0 = normal step).
    Footstep { foot: Foot, variant: u32 },
    /// The character speaks a voice line (see [`CharacterModel::voice`]).
    Voice { line: u32 },
    /// A specific sound plays.
    Sound { cue: crate::sound::SoundCue, stop_at_end: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClipEvent {
    /// Seconds from the clip start.
    pub time: f32,
    pub kind: ClipEventKind,
}

/// A pre-sampled animation clip: `frames[frame][track]`, tracks map to skeleton bone indices.
#[derive(Debug, Clone)]
pub struct Clip {
    pub name: String,
    pub duration: f32,
    pub fps: f32,
    pub frames: Vec<Vec<ClipPose>>,
    pub track_to_bone: Vec<usize>,
    /// Timeline events (footsteps, voice lines, sounds), sorted by time.
    pub events: Vec<ClipEvent>,
    /// Poses are deltas on top of the reference pose (FFXIV face expressions), not absolute
    /// local poses: translation adds, rotation multiplies.
    pub additive: bool,
}

/// What kind of action an [`ActionDef`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionCategory {
    /// A standing pose that replaces the idle loop.
    Idle,
    /// A one-shot or looping emote played on request.
    Emote,
    Other,
}

/// An animation a character can perform on request (idle variants, emotes). Clips are loaded
/// on demand through [`crate::Engine::load_action`].
#[derive(Debug, Clone)]
pub struct ActionDef {
    /// Engine-scoped id (e.g. `emote:42`).
    pub id: String,
    pub name: String,
    pub category: ActionCategory,
    /// Loops until replaced (idle poses, dances) instead of playing once.
    pub looped: bool,
}

/// Which clip names drive the standard locomotion states.
#[derive(Debug, Clone, Default)]
pub struct Locomotion {
    pub idle: Option<String>,
    pub walk: Option<String>,
    pub run: Option<String>,
    pub sprint: Option<String>,
    pub fall: Option<String>,
    pub jump: Option<String>,
    pub land: Option<String>,
    /// Treading water at the surface.
    pub swim_idle: Option<String>,
    /// Swimming forward at the surface.
    pub swim_move: Option<String>,
    pub swim_sprint: Option<String>,
    /// Hovering under water.
    pub dive_idle: Option<String>,
    /// Swimming under water.
    pub dive_move: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CharacterModel {
    pub engine: String,
    pub name: String,
    pub skeleton: SkeletonData,
    pub parts: Vec<CharacterPart>,
    pub clips: HashMap<String, Clip>,
    pub locomotion: Locomotion,
    /// Locomotion used while a weapon addon is drawn (battle stance); falls back to
    /// `locomotion` per missing clip.
    pub armed_locomotion: Option<Locomotion>,
    /// Clips always applied on top of the locomotion, for the bones they animate (a face's
    /// resting expression): looping, in sync with the character's clock.
    pub overlays: Vec<String>,
    /// One-shot clip played every few seconds over the overlays (a blink).
    pub blink: Option<String>,
    /// Actions available for this character (loaded lazily).
    pub actions: Vec<ActionDef>,
    /// Addons (weapons, ...) whose parts carry `CharacterPart::addon`.
    pub addons: Vec<AddonDef>,
    /// Uniform scale applied to the whole character.
    pub scale: f32,
    /// Approximate standing height in metres (for the collider).
    pub height: f32,
    /// Bone the runtime should treat as the visual root (feet at origin).
    pub notes: Vec<String>,
    /// Footstep sounds by surface and gait (see [`ClipEventKind::Footstep`]).
    pub footsteps: Option<crate::sound::FootstepSet>,
    /// Voice lines for [`ClipEventKind::Voice`].
    pub voice: Option<crate::sound::VoiceSet>,
    /// Where every asset came from (protected game files vs shareable mods).
    pub content: crate::content::ContentReport,
}

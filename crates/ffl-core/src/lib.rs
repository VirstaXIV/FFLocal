//! Engine-agnostic contracts. Everything the runtime (Bevy) and the launcher consume goes
//! through these types; game-specific crates ("engines") produce them.
//!
//! An *engine* here is an asset provider for one game or content source: FFXIV, FFXI, a Unity
//! or Unreal export, a mod pack. Characters and scenes from different engines can be combined in
//! one world because they all reduce to the same plain data.

pub mod character;
pub mod content;
pub mod engine;
pub mod material;
pub mod mesh;
pub mod preset;
pub mod scene;
pub mod sound;

pub use character::{ActionCategory, ActionDef, AddonDef, AddonKind, AddonPlacement, AddonRule, AddonTransition, Attach, BoneData, CharacterModel, CharacterPart, Clip, ClipEvent, ClipEventKind, ClipPose, Foot, Locomotion, SkeletonData};
pub use content::{AssetOrigin, ContentReport, Provenance};
pub use engine::{ArchiveReport, InstallCandidate, CatalogEntry, ContentPolicy, Engine, EngineInfo, FieldKind, ImportSource, MapInfo, ModInfo, ModProfile, PresetField};
pub use material::{MAX_PARAMS, MAX_TEXTURE_SLOTS, MaterialDesc, ShaderDef, TextureData, TextureFormat, TextureRef, TextureSlotDef, builtin};
pub use mesh::{CollisionMesh, MeshData, ModelData};
pub use preset::{CharacterPreset, Library};
pub use scene::{CollisionHint, Environment, LightDesc, Node, NodeKind, Scene, Trs};
pub use sound::{AmbientEmitter, ClockSpec, FootstepCue, FootstepSet, Gait, GroundCondition, MusicRegion, MusicSet, MusicSlot, RegionShape, SoundCategory, SoundCue, SoundData, SoundEncoding, SurfaceKind, VoiceSet, wav_bytes};

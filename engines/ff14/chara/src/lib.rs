//! FFLocal character layer: vanilla appearance sources, race/path logic, metadata parsers and
//! the resolver that turns an appearance into concrete game assets. No engine dependency.

pub mod appearance;
pub mod gearset;
pub mod meta;
pub mod preset;
pub mod race;
pub mod resolver;
pub mod voice;

pub use appearance::{Appearance, AppearanceSource, CharaDatSource, GearsetSource, VanillaSource};
pub use race::{BodyPart, GearSlot, RaceCode};
pub use resolver::{CharacterModelSet, CharacterResolver, PartKind, ResolvedPart};

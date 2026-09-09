//! The hub: where a character and a world are chosen before entering, plus the settings.
//!
//! This crate holds the *model* ([`Hub`]) and its egui *views* ([`ui`]). It knows nothing about
//! Bevy: a runtime hands it the registered engines, calls the view functions with an egui
//! context each frame and reacts to the [`HubEvent`]s that come back. Any other host (a wgpu
//! window, a future Unity or Unreal client through an egui bridge) can drive the same code.

pub mod hub;
pub mod settings;
pub mod ui;

pub use hub::{ActionEvent, ActionsView, Hub, HubEvent, LookupState, PresetEditor, SoundActivity, SoundMonitor, SoundSource, WorldRequest};
pub use settings::{AudioSettings, GameSettings, GraphicsSettings, UiSettings, Settings};

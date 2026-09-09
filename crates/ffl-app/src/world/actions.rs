//! Engine-agnostic actions: idle-pose variants and emotes listed by the engine, loaded on demand
//! and played through the character animator. The "Actions" egui window drives it.

use std::sync::Arc;

use anyhow::Result as AnyResult;
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, poll_once};
use bevy_egui::{EguiContexts, EguiPrimaryContextPass};
use ffl_core::{ActionCategory, ActionDef, AddonDef, AddonRule, CharacterPreset, Clip, Engine};

use crate::app::{AppState, Engines, RuntimeOptions, WorldRequest};
use crate::world::character::CharacterAnimator;
use crate::world::player::{CharacterRig, Player};

/// Clip name under which an action's clip is stored in the animator.
pub fn action_clip_name(id: &str) -> String {
    format!("action:{id}")
}

#[derive(Resource, Default)]
pub struct ActionRuntime {
    pub actions: Vec<ActionDef>,
    pub preset: Option<CharacterPreset>,
    pub selected_idle: String,
    pub filter: String,
    pub status: String,
    pending: Option<(String, Task<AnyResult<Clip>>)>,
    /// What to do once the pending clip arrives.
    queued: Option<PlayRequest>,
    /// `--action` already fired for this character.
    startup_done: bool,
}

#[derive(Clone)]
struct PlayRequest {
    id: String,
    category: ActionCategory,
    looped: bool,
}

impl ActionRuntime {
    /// Called when a character is attached: remember its actions and preset.
    pub fn reset(&mut self, actions: Vec<ActionDef>, preset: Option<CharacterPreset>) {
        self.actions = actions;
        self.preset = preset;
        self.selected_idle = "idle:default".into();
        self.pending = None;
        self.queued = None;
        self.startup_done = false;
        self.status.clear();
    }

    /// Find an action by id or (case-insensitive) name.
    pub fn find(&self, id_or_name: &str) -> Option<ActionDef> {
        let q = id_or_name.to_lowercase();
        self.actions.iter().find(|a| a.id == id_or_name || a.name.to_lowercase() == q).cloned()
    }
}

/// Marks a mesh entity as part of an addon (weapon, ...).
#[derive(Component)]
pub struct AddonMember {
    pub addon: String,
    /// Placement the mesh currently sits at (`None` = the part's own attach).
    pub placement: Option<String>,
}

/// Addons of the attached character, whether the user has them switched on and which ones the
/// user stowed by hand (Z).
#[derive(Resource, Default)]
pub struct AddonState {
    pub addons: Vec<(AddonDef, bool)>,
    pub stowed_manual: Vec<String>,
    /// A draw/sheathe in progress: the placement switches `remaining` seconds into the clip.
    pub pending: Option<PendingToggle>,
    /// Addons whose placement re-parented a skeleton bone (`Attach::Reparent`): addon id →
    /// (bone entity, its original parent entity, bone index).
    pub reparented: std::collections::HashMap<String, (Entity, Entity, usize)>,
}

#[derive(Debug, Clone)]
pub struct PendingToggle {
    pub stow: bool,
    pub remaining: f32,
}


/// What an addon should look like this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct AddonWanted {
    pub visible: bool,
    /// Placement id, `None` = the part's own attach.
    pub placement: Option<String>,
}

impl AddonState {
    pub fn reset(&mut self, defs: &[AddonDef], overrides: &[(String, bool)], sheathed: bool) {
        self.addons = defs
            .iter()
            .map(|d| {
                let on = overrides.iter().rev().find(|(id, _)| *id == d.id).map(|(_, v)| *v).unwrap_or(d.enabled);
                (d.clone(), on)
            })
            .collect();
        self.stowed_manual = if sheathed { defs.iter().filter(|d| d.can_stow()).map(|d| d.id.clone()).collect() } else { Vec::new() };
        self.reparented.clear();
        self.pending = None;
    }

    fn stowable(&self) -> Vec<String> {
        self.addons.iter().filter(|(d, _)| d.can_stow()).map(|(d, _)| d.id.clone()).collect()
    }

    /// Whether the next toggle stows (true) or draws (false); `None` when nothing is stowable.
    pub fn next_toggle_stows(&self) -> Option<bool> {
        let stowable = self.stowable();
        if stowable.is_empty() {
            return None;
        }
        Some(!stowable.iter().all(|id| self.stowed_manual.contains(id)))
    }

    /// Move every stowable addon (weapons) between drawn and stowed now.
    pub fn set_stowed(&mut self, stow: bool) {
        self.stowed_manual = if stow { self.stowable() } else { Vec::new() };
    }

    /// The transition (clip + switch time) for a toggle, from the first addon that has one.
    pub fn transition(&self, stow: bool) -> Option<ffl_core::AddonTransition> {
        self.addons.iter().find_map(|(d, _)| if stow { d.stow.clone() } else { d.activate.clone() })
    }

    /// Resolve visibility and placement from the toggles, rules and the character's state.
    /// `action` is the id of the action currently playing (emote), if any.
    pub fn wanted(&self, id: &str, action: Option<&str>, moving: bool) -> AddonWanted {
        let Some((def, on)) = self.addons.iter().find(|(d, _)| d.id == id) else {
            return AddonWanted {
                visible: true,
                placement: None,
            };
        };
        if !on {
            return AddonWanted {
                visible: false,
                placement: def.active.clone(),
            };
        }
        let emoting = action.is_some();
        let mut visible = true;
        let mut stow = self.stowed_manual.contains(&def.id);
        for r in &def.rules {
            match r {
                AddonRule::HideDuringEmotes => visible &= !emoting,
                AddonRule::HideWhileIdle => visible &= moving,
                AddonRule::HideWhileMoving => visible &= !moving,
                AddonRule::StowDuringActions => {
                    if let Some(a) = action
                        && !def.active_actions.iter().any(|x| x == a)
                    {
                        stow = true;
                    }
                }
            }
        }
        let placement = if stow {
            match &def.stowed {
                Some(p) => Some(p.clone()),
                None => {
                    visible = false;
                    def.active.clone()
                }
            }
        } else {
            def.active.clone()
        };
        AddonWanted { visible, placement }
    }
}

pub struct ActionsPlugin;

impl Plugin for ActionsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ActionRuntime>()
            .init_resource::<AddonState>()
            .add_systems(
                Update,
                (poll_action_load, startup_action, stow_hotkey, apply_addons).chain().run_if(in_state(AppState::World)),
            )
            .add_systems(EguiPrimaryContextPass, actions_window.run_if(in_state(AppState::World)));
    }
}

fn request_play(
    runtime: &mut ActionRuntime,
    engines: &Engines,
    request: &WorldRequest,
    animator: &mut CharacterAnimator,
    action: &ActionDef,
) {
    let req = PlayRequest {
        id: action.id.clone(),
        category: action.category,
        looped: action.looped,
    };
    let clip = action_clip_name(&action.id);
    if animator.clips.contains_key(&clip) {
        apply_play(runtime, animator, &req);
        return;
    }
    if runtime.pending.as_ref().is_some_and(|(id, _)| *id == action.id) {
        runtime.queued = Some(req);
        return;
    }
    let Some(preset) = runtime.preset.clone().or_else(|| request.character.clone()) else {
        runtime.status = "no character preset".into();
        return;
    };
    let Some(engine) = engines.get(&preset.engine) else {
        runtime.status = format!("unknown engine {}", preset.engine);
        return;
    };
    let engine: Arc<dyn Engine> = engine;
    let id = action.id.clone();
    runtime.status = format!("loading {}", action.name);
    runtime.queued = Some(req);
    let task_id = id.clone();
    runtime.pending = Some((id, AsyncComputeTaskPool::get().spawn(async move { engine.load_action(&preset, &task_id) })));
}

fn apply_play(runtime: &mut ActionRuntime, animator: &mut CharacterAnimator, req: &PlayRequest) {
    let clip = action_clip_name(&req.id);
    match req.category {
        ActionCategory::Idle => {
            runtime.selected_idle = req.id.clone();
            animator.idle_override = if req.id == "idle:default" { None } else { Some(clip) };
            animator.emote = None;
        }
        _ => animator.start_emote(clip, req.looped),
    }
    runtime.status.clear();
}

fn poll_action_load(mut runtime: ResMut<ActionRuntime>, mut rigs: Query<&mut CharacterAnimator, With<CharacterRig>>) {
    let Some((id, task)) = runtime.pending.as_mut() else {
        return;
    };
    let Some(result) = block_on(poll_once(task)) else {
        return;
    };
    let id = id.clone();
    runtime.pending = None;
    match result {
        Ok(clip) => {
            let Ok(mut animator) = rigs.single_mut() else {
                return;
            };
            info!("action {id}: clip {:.2}s, {} frames", clip.duration, clip.frames.len());
            if id == "idle:default" {
                // The default idle is already the locomotion clip; nothing to insert.
            } else {
                Arc::make_mut(&mut animator.clips).insert(action_clip_name(&id), clip);
            }
            if let Some(req) = runtime.queued.take()
                && req.id == id
            {
                apply_play(&mut runtime, &mut animator, &req);
            }
        }
        Err(err) => {
            error!("action {id} failed: {err:#}");
            runtime.status = format!("{id}: {err:#}");
            runtime.queued = None;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn actions_window(
    mut contexts: EguiContexts,
    mut runtime: ResMut<ActionRuntime>,
    mut addons: ResMut<AddonState>,
    engines: Res<Engines>,
    request: Res<WorldRequest>,
    hub: Res<crate::app::HubState>,
    mut rigs: Query<&mut CharacterAnimator, With<CharacterRig>>,
) -> bevy::prelude::Result {
    let ctx = contexts.ctx_mut()?;
    let Ok(mut animator) = rigs.single_mut() else {
        return Ok(());
    };
    if !hub.0.settings.ui.actions || (runtime.actions.is_empty() && addons.addons.is_empty()) {
        return Ok(());
    }
    let character = request.character.as_ref().map(|p| p.name.clone()).unwrap_or_default();
    let engine = request.character.as_ref().map(|p| hub.0.engine_name(&p.engine)).unwrap_or_default();
    let playing = animator
        .emote
        .as_ref()
        .and_then(|(clip, _)| runtime.actions.iter().find(|a| action_clip_name(&a.id) == *clip))
        .map(|a| a.id.clone());
    let ActionRuntime {
        actions,
        selected_idle,
        filter,
        status,
        ..
    } = &mut *runtime;
    let events = ffl_hub::ui::actions_window(ctx, ffl_hub::ActionsView {
        character: &character,
        engine: &engine,
        status,
        addons: &mut addons.addons,
        actions,
        selected_idle,
        playing: playing.as_deref(),
        filter,
    });
    for event in events {
        match event {
            ffl_hub::ActionEvent::Stop => animator.emote = None,
            ffl_hub::ActionEvent::Play(a) => request_play(&mut runtime, &engines, &request, &mut animator, &a),
        }
    }
    Ok(())
}

/// `--action`: play the requested action once the character has its action list.
fn startup_action(
    opts: Res<RuntimeOptions>,
    engines: Res<Engines>,
    request: Res<WorldRequest>,
    mut runtime: ResMut<ActionRuntime>,
    mut rigs: Query<&mut CharacterAnimator, With<CharacterRig>>,
) {
    if runtime.startup_done || runtime.actions.is_empty() {
        return;
    }
    let Some(wanted) = opts.action.as_deref() else {
        runtime.startup_done = true;
        return;
    };
    let Ok(mut animator) = rigs.single_mut() else {
        return;
    };
    runtime.startup_done = true;
    match runtime.find(wanted) {
        Some(a) => {
            info!("startup action {} ({})", a.name, a.id);
            request_play(&mut runtime, &engines, &request, &mut animator, &a);
        }
        None => {
            let names: Vec<&str> = runtime.actions.iter().map(|a| a.name.as_str()).collect();
            warn!("no action named {wanted}; available: {}", names.join(", "));
        }
    }
}

/// Z: draw / sheathe. Plays the transition clip (when the engine provided one) and switches
/// the placement part-way through it; `--toggle-sheathe-at` presses Z for verification runs.
fn stow_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    opts: Res<RuntimeOptions>,
    mode: Res<crate::camera::CameraMode>,
    ui_focus: Res<crate::app::UiFocus>,
    mut addons: ResMut<AddonState>,
    players: Query<&Player>,
    mut rigs: Query<&mut CharacterAnimator, With<CharacterRig>>,
    mut auto_fired: Local<bool>,
) {
    let auto = opts.toggle_sheathe_at.is_some_and(|t| !*auto_fired && players.single().ok().is_some_and(|p| p.age >= t));
    let pressed = (*mode == crate::camera::CameraMode::Orbit && !ui_focus.keyboard && keys.just_pressed(KeyCode::KeyZ)) || auto;
    if pressed && addons.pending.is_none() {
        if auto {
            *auto_fired = true;
        }
        if let Some(stow) = addons.next_toggle_stows() {
            match (addons.transition(stow), rigs.single_mut().ok()) {
                (Some(t), Some(mut animator)) if animator.clips.contains_key(&t.clip) => {
                    animator.start_emote(t.clip.clone(), false);
                    addons.pending = Some(PendingToggle { stow, remaining: t.switch_at });
                    info!("{} ({}, switch at {:.2}s)", if stow { "sheathing" } else { "drawing" }, t.clip, t.switch_at);
                }
                _ => {
                    addons.set_stowed(stow);
                    info!("weapons {}", if stow { "sheathed" } else { "drawn" });
                }
            }
        }
    }
    if let Some(p) = addons.pending.as_mut() {
        p.remaining -= time.delta_secs();
        if p.remaining <= 0.0 {
            let stow = p.stow;
            addons.pending = None;
            addons.set_stowed(stow);
        }
    }
}

/// Show, hide and move addon meshes from the toggles, rules and the character's state.
#[allow(clippy::too_many_arguments)]
fn apply_addons(
    mut commands: Commands,
    mut addons: ResMut<AddonState>,
    mut animators: Query<&mut CharacterAnimator, With<CharacterRig>>,
    players: Query<&Player>,
    bones: Query<(Entity, &crate::world::character::Bone, &ChildOf)>,
    mut members: Query<(Entity, &mut AddonMember, &mut Visibility, &ChildOf)>,
) {
    if addons.addons.is_empty() {
        return;
    }
    let bones_by_name = |name: &str| bones.iter().find(|(_, b, _)| b.name == name);
    let action = animators
        .single()
        .ok()
        .and_then(|a| a.emote.as_ref())
        .and_then(|(clip, _)| clip.strip_prefix("action:").map(str::to_string));
    let moving = players.single().ok().is_some_and(|p| p.moving);
    // An action that sheathes the weapon leaves it sheathed (the game makes you redraw).
    if let Some(a) = action.as_deref() {
        let sticky: Vec<String> = addons
            .addons
            .iter()
            .filter(|(d, _)| d.can_stow() && d.rules.contains(&AddonRule::StowDuringActions) && !d.active_actions.iter().any(|x| x == a))
            .map(|(d, _)| d.id.clone())
            .collect();
        for id in sticky {
            if !addons.stowed_manual.contains(&id) {
                addons.stowed_manual.push(id);
            }
        }
    }
    // Battle stance while any weapon is drawn.
    let armed = addons
        .addons
        .iter()
        .any(|(d, on)| *on && d.kind == ffl_core::AddonKind::Weapon && d.can_stow() && !addons.stowed_manual.contains(&d.id));
    if let Ok(mut a) = animators.single_mut()
        && a.armed != armed
    {
        a.armed = armed;
    }
    // Placements that re-parent a skeleton bone (drawn FFXI weapons): apply once per addon.
    let defs: Vec<AddonDef> = addons.addons.iter().map(|(d, _)| d.clone()).collect();
    for def in &defs {
        let wanted = addons.wanted(&def.id, action.as_deref(), moving);
        let target = wanted.placement.as_deref().and_then(|p| def.placement(p)).map(|p| p.attach.clone());
        match target {
            Some(ffl_core::Attach::Reparent { bone, onto }) => {
                if addons.reparented.contains_key(&def.id) {
                    continue;
                }
                let (Some((bone_entity, bone_info, parent)), Some((onto_entity, _, _))) = (bones_by_name(&bone), bones_by_name(&onto)) else {
                    debug!("{}: re-parent bones {bone} -> {onto} not spawned yet", def.id);
                    continue;
                };
                info!("{}: bone {bone} re-parented onto {onto}", def.id);
                commands.entity(bone_entity).insert((ChildOf(onto_entity), Transform::IDENTITY));
                addons.reparented.insert(def.id.clone(), (bone_entity, parent.parent(), bone_info.index));
                if let Ok(mut a) = animators.single_mut()
                    && !a.pinned.contains(&bone_info.index)
                {
                    a.pinned.push(bone_info.index);
                }
            }
            _ => {
                if let Some((bone_entity, original, index)) = addons.reparented.remove(&def.id) {
                    info!("{}: bone restored to its own parent", def.id);
                    commands.entity(bone_entity).insert(ChildOf(original));
                    if let Ok(mut a) = animators.single_mut() {
                        a.pinned.retain(|i| *i != index);
                    }
                }
            }
        }
    }
    for (entity, mut member, mut vis, parent) in &mut members {
        let wanted = addons.wanted(&member.addon, action.as_deref(), moving);
        let visibility = if wanted.visible { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != visibility {
            *vis = visibility;
        }
        if wanted.placement == member.placement || wanted.placement.is_none() {
            continue;
        }
        let Some(def) = addons.addons.iter().map(|(d, _)| d).find(|d| d.id == member.addon) else {
            continue;
        };
        let Some(target) = wanted.placement.as_deref().and_then(|p| def.placement(p)) else {
            continue;
        };
        let ffl_core::Attach::Bone(bone_name) = &target.attach else {
            // Skinned / re-parenting placements leave the mesh where it is.
            member.placement = wanted.placement.clone();
            continue;
        };
        let Some((bone, _, _)) = bones.iter().find(|(_, b, _)| &b.name == bone_name) else {
            warn!("{}: placement bone {bone_name} not in the skeleton", member.addon);
            member.placement = wanted.placement.clone();
            continue;
        };
        // Re-attach instantly: easing between bones made the weapon tumble through the air.
        let to = crate::convert::trs_to_transform(&target.offset);
        let _ = parent;
        debug!("{}: placement {:?} -> {:?} ({bone_name})", member.addon, member.placement, wanted.placement);
        commands.entity(entity).insert((ChildOf(bone), to));
        member.placement = wanted.placement.clone();
    }
}


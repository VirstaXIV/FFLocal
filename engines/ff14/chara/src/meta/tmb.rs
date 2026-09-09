//! Timeline (`.tmb`) reader for the few entries the character runtime needs. Layout per
//! VFXEditor (`Formats/TmbFormat`): `TMLB` header (magic, u32 size, u32 count), then entries,
//! each `magic, u32 size, i16 id, i16 time (frames)` followed by fields. `C010` = Animation
//! (u32 duration frames, u32 unk, u32 flags, f32 start, f32 end, u32 name offset relative to
//! the entry start + 8, i.e. past magic and size), `C014` = Weapon Position (u32 enabled, u32 unk, u32 ATCH state, u32
//! object: 0 main hand, 1 off hand). ATCH state 1 = drawn (`Drawn_State0`), 0 = stowed
//! (`Stowed_State1`). `C042` = Footstep (u32 enabled, u32 unk, i32 foot id: 34 left / 35 right,
//! i32 sound id: 0 normal, 4 on the jump landing), `C053` = Voiceline (i32 unk, i32 unk, i16 bind
//! point, i16 sound id = the emote voice line, i16 unk, u16 flags: bit 0 stop on movement),
//! `C063` = Sound (i32 loop, i32 unk, u32 path offset relative to entry + 8, i32 sound index,
//! u8 position flags, u8 bind id).
//!
//! Locomotion and emote timelines live *inside* the `.pap` (`TMLB` blocks after the Havok data,
//! see [`embedded_timelines`]); the standalone `chara/action/**.tmb` files of the same clips
//! carry only the animation entry.

#[derive(Debug, Clone, PartialEq)]
pub struct WeaponEvent {
    pub frame: u16,
    pub drawn: bool,
    pub off_hand: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FootstepEvent {
    pub frame: u16,
    /// 34 = left foot, 35 = right foot (client enum, not a bone index).
    pub foot_id: i32,
    pub sound_id: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VoiceEvent {
    pub frame: u16,
    pub bind: i16,
    pub sound_id: i16,
    pub flags: u16,
}

impl VoiceEvent {
    pub fn stop_on_movement(&self) -> bool {
        self.flags & 1 != 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SoundEvent {
    pub frame: u16,
    pub path: String,
    pub index: i32,
    pub looped: i32,
    pub position_flags: u8,
    pub bind: u8,
}

#[derive(Debug, Clone, Default)]
pub struct TmbInfo {
    /// Clip name of the first animation entry.
    pub animation: Option<String>,
    /// Its duration in frames (30 fps).
    pub animation_frames: u32,
    pub weapon_events: Vec<WeaponEvent>,
    pub footsteps: Vec<FootstepEvent>,
    pub voices: Vec<VoiceEvent>,
    pub sounds: Vec<SoundEvent>,
    /// Every entry magic seen, for diagnostics.
    pub entry_magics: Vec<String>,
}

/// The `TMLB` timeline blocks embedded at the end of a `.pap`, in file order (one per
/// animation in the packs seen so far; match them to clips by their `C010` name).
pub fn embedded_timelines(pap: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if pap.len() < 26 || &pap[0..4] != b"pap " {
        return out;
    }
    let tmb_offset = i32::from_le_bytes([pap[22], pap[23], pap[24], pap[25]]).max(0) as usize;
    let mut off = tmb_offset.min(pap.len());
    while let Some(rel) = pap[off..].windows(4).position(|w| w == b"TMLB") {
        let start = off + rel;
        if start + 8 > pap.len() {
            break;
        }
        let size = u32::from_le_bytes([pap[start + 4], pap[start + 5], pap[start + 6], pap[start + 7]]) as usize;
        if size < 12 || start + size > pap.len() {
            break;
        }
        out.push(&pap[start..start + size]);
        off = start + size;
    }
    out
}

impl TmbInfo {
    pub fn parse(d: &[u8]) -> Self {
        let mut info = TmbInfo::default();
        if d.len() < 12 || &d[0..4] != b"TMLB" {
            return info;
        }
        let u32_at = |o: usize| u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
        let u16_at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]);
        let mut off = 12;
        while off + 12 <= d.len() {
            let magic = &d[off..off + 4];
            let size = u32_at(off + 4) as usize;
            if size < 12 || off + size > d.len() {
                break;
            }
            let time = u16_at(off + 10);
            info.entry_magics.push(String::from_utf8_lossy(magic).to_string());
            match magic {
                b"C010" if size >= 40 => {
                    let frames = u32_at(off + 12);
                    let name_off = u32_at(off + 32) as usize;
                    let start = off + 8 + name_off;
                    if start < d.len() {
                        let end = d[start..].iter().position(|b| *b == 0).map(|e| start + e).unwrap_or(d.len());
                        let name = String::from_utf8_lossy(&d[start..end]).to_string();
                        if info.animation.is_none() && !name.is_empty() {
                            info.animation = Some(name);
                            info.animation_frames = frames;
                        }
                    }
                }
                b"C042" if size >= 28 => {
                    let enabled = u32_at(off + 12) != 0;
                    if enabled {
                        info.footsteps.push(FootstepEvent { frame: time, foot_id: u32_at(off + 20) as i32, sound_id: u32_at(off + 24) as i32 });
                    }
                }
                b"C053" if size >= 28 => {
                    info.voices.push(VoiceEvent { frame: time, bind: u16_at(off + 20) as i16, sound_id: u16_at(off + 22) as i16, flags: u16_at(off + 26) });
                }
                b"C063" if size >= 32 => {
                    let path_off = u32_at(off + 20) as usize;
                    let start = off + 8 + path_off;
                    let path = if path_off > 0 && start < d.len() {
                        let end = d[start..].iter().position(|b| *b == 0).map(|e| start + e).unwrap_or(d.len());
                        String::from_utf8_lossy(&d[start..end]).to_string()
                    } else {
                        String::new()
                    };
                    info.sounds.push(SoundEvent {
                        frame: time,
                        path,
                        index: u32_at(off + 24) as i32,
                        looped: u32_at(off + 12) as i32,
                        position_flags: d[off + 28],
                        bind: d[off + 29],
                    });
                }
                b"C014" if size >= 28 => {
                    let enabled = u32_at(off + 12) != 0;
                    let state = u32_at(off + 20);
                    let object = u32_at(off + 24);
                    if enabled {
                        info.weapon_events.push(WeaponEvent {
                            frame: time,
                            drawn: state == 1,
                            off_hand: object == 1,
                        });
                    }
                }
                _ => {}
            }
            off += size;
        }
        info
    }

    /// Seconds (30 fps) of the earliest main-hand event of the given kind.
    pub fn main_hand_switch(&self, drawn: bool) -> Option<f32> {
        self.weapon_events.iter().filter(|e| e.drawn == drawn && !e.off_hand).map(|e| e.frame).min().map(|f| f as f32 / 30.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> Option<Vec<u8>> {
        let dir = std::env::var("FFL_TEST_SCD_DIR").ok()?;
        std::fs::read(format!("{dir}/{name}")).ok()
    }

    #[test]
    #[ignore = "needs FFL_TEST_SCD_DIR with extracted game files"]
    fn walk_cycle_has_two_footsteps() {
        let Some(pap) = scratch("move_a.pap") else { return };
        let blocks = embedded_timelines(&pap);
        assert_eq!(blocks.len(), 8);
        let infos: Vec<TmbInfo> = blocks.iter().map(|b| TmbInfo::parse(b)).collect();
        let walk = infos.iter().find(|i| i.animation.as_deref() == Some("cbnm_01f_lp0")).unwrap();
        assert_eq!(walk.animation_frames, 30);
        assert_eq!(walk.footsteps, vec![FootstepEvent { frame: 9, foot_id: 34, sound_id: 0 }, FootstepEvent { frame: 24, foot_id: 35, sound_id: 0 }]);
        let run = infos.iter().find(|i| i.animation.as_deref() == Some("cbnm_02f_lp0")).unwrap();
        assert_eq!(run.footsteps.iter().map(|f| f.frame).collect::<Vec<_>>(), vec![7, 17]);
    }

    #[test]
    #[ignore = "needs FFL_TEST_SCD_DIR with extracted game files"]
    fn laugh_has_a_voice_line() {
        let Some(pap) = scratch("laugh_st.pap") else { return };
        let blocks = embedded_timelines(&pap);
        let info = TmbInfo::parse(blocks[0]);
        assert_eq!(info.voices.len(), 1);
        assert_eq!(info.voices[0].sound_id, 47);
        assert!(info.voices[0].stop_on_movement());
    }

    #[test]
    #[ignore = "needs FFL_TEST_SCD_DIR with extracted game files"]
    fn landing_timeline() {
        let Some(tmb) = scratch("normal_jump_landing.tmb") else { return };
        let info = TmbInfo::parse(&tmb);
        assert_eq!(info.footsteps.iter().map(|f| (f.frame, f.foot_id, f.sound_id)).collect::<Vec<_>>(), vec![(2, 35, 4), (3, 34, 4)]);
    }
}

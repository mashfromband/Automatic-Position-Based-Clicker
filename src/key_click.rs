//! Physical key presses mapped to one mouse click in a selected game window.
use crate::error::{ClickerError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[cfg(windows)]
pub mod native;

pub const MAX_BINDINGS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameIdentity {
    pub executable: String,
    pub class: String,
    pub title: String,
}
impl GameIdentity {
    pub fn same_game(&self, other: &Self) -> bool {
        self.executable.eq_ignore_ascii_case(&other.executable)
            && self.class.eq_ignore_ascii_case(&other.class)
    }
    fn valid(&self) -> bool {
        !self.executable.is_empty()
            && !self.class.is_empty()
            && self.executable.encode_utf16().count() <= 32768
            && self.class.encode_utf16().count() <= 256
            && self.title.encode_utf16().count() <= 512
            && [&self.executable, &self.class, &self.title]
                .iter()
                .all(|s| !s.contains('\0'))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPoint {
    pub game: GameIdentity,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
impl ClientPoint {
    pub fn fits(&self, game: &GameIdentity, width: i32, height: i32) -> bool {
        self.game.same_game(game)
            && self.width == width
            && self.height == height
            && self.x >= 0
            && self.y >= 0
            && self.x < width
            && self.y < height
    }
    fn valid(&self) -> bool {
        self.game.valid()
            && self.width > 0
            && self.height > 0
            && self.fits(&self.game, self.width, self.height)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyBinding {
    pub key: Option<u32>,
    pub position: Option<ClientPoint>,
    pub right: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyClickSettings {
    pub enabled: bool,
    pub last_target: Option<GameIdentity>,
    pub bindings: Vec<KeyBinding>,
}
impl KeyClickSettings {
    pub fn validate(&self) -> Result<(), ClickerError> {
        let mut used = BTreeSet::new();
        if self.bindings.len() > MAX_BINDINGS
            || self.last_target.as_ref().is_some_and(|g| !g.valid())
        {
            return Err(ErrorCode::KeyMapping.into());
        }
        for binding in &self.bindings {
            if binding
                .key
                .is_some_and(|k| !supported_key(k) || !used.insert(k))
                || binding.position.as_ref().is_some_and(|p| !p.valid())
            {
                return Err(ErrorCode::KeyMapping.into());
            }
        }
        Ok(())
    }
    pub fn add_wasd(&mut self) -> bool {
        let missing: Vec<_> = [b'W', b'A', b'S', b'D']
            .into_iter()
            .map(u32::from)
            .filter(|k| !self.bindings.iter().any(|b| b.key == Some(*k)))
            .collect();
        if self.bindings.len() + missing.len() > MAX_BINDINGS {
            return false;
        }
        self.bindings
            .extend(missing.into_iter().map(|key| KeyBinding {
                key: Some(key),
                ..KeyBinding::default()
            }));
        true
    }
}

pub fn supported_key(key: u32) -> bool {
    matches!(key,0x08|0x09|0x0d|0x13|0x14|0x20..=0x28|0x2c..=0x2e|0x30..=0x39|0x41..=0x5a|0x5d|0x60..=0x87|0x90|0x91|0xba..=0xc0|0xdb..=0xdf|0xe2)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub suppress: bool,
    pub click: Option<usize>,
}

#[derive(Default)]
pub struct KeyCapture {
    pending: Option<(u32, u32)>,
}
impl KeyCapture {
    pub fn event(&mut self, key: u32, down: bool, mods: u32) -> (bool, Option<(u32, u32)>) {
        if matches!(key,0x10..=0x12|0x5b|0x5c|0xa0..=0xa5) {
            return (false, None);
        }
        if down {
            if key == 0x1b {
                self.pending = None;
                return (true, Some((0, key)));
            }
            if self.pending.is_none() {
                self.pending = Some((mods, key));
            }
            (true, None)
        } else if self.pending.is_some_and(|(_, pending)| pending == key) {
            (false, self.pending.take())
        } else {
            (false, None)
        }
    }
}

pub struct KeyState {
    down: [bool; 256],
    consumed: [bool; 256],
}
impl Default for KeyState {
    fn default() -> Self {
        Self {
            down: [false; 256],
            consumed: [false; 256],
        }
    }
}
impl KeyState {
    pub fn seed_down(&mut self, keys: impl IntoIterator<Item = u32>) {
        for key in keys {
            if let Some(value) = self.down.get_mut(key as usize) {
                *value = true;
            }
        }
    }
    /// Focus/config changes never turn an already held key into a fresh press.
    pub fn event(
        &mut self,
        key: u32,
        down: bool,
        injected: bool,
        binding: Option<usize>,
        blocked: bool,
    ) -> Decision {
        if injected || key >= 256 {
            return Decision::default();
        }
        let key = key as usize;
        if !down {
            self.down[key] = false;
            return Decision {
                suppress: std::mem::take(&mut self.consumed[key]),
                click: None,
            };
        }
        if std::mem::replace(&mut self.down[key], true) {
            return Decision {
                suppress: self.consumed[key],
                click: None,
            };
        }
        if !blocked {
            if let Some(index) = binding {
                self.consumed[key] = true;
                return Decision {
                    suppress: true,
                    click: Some(index),
                };
            }
        }
        Decision::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_capture_finishes_on_release_and_keeps_the_original_modifiers() {
        let mut capture = KeyCapture::default();
        assert_eq!(capture.event(b'W' as u32, true, 6), (true, None));
        assert_eq!(capture.event(b'W' as u32, true, 0), (true, None));
        assert_eq!(capture.event(b'A' as u32, false, 0), (false, None));
        assert_eq!(
            capture.event(b'W' as u32, false, 0),
            (false, Some((6, b'W' as u32)))
        );
        assert_eq!(capture.event(0x1b, true, 0), (true, Some((0, 0x1b))));
    }
    #[test]
    fn a_physical_press_clicks_once_until_release_even_after_focus_changes() {
        let mut state = KeyState::default();
        assert_eq!(
            state.event(b'W' as u32, true, false, Some(0), false),
            Decision {
                suppress: true,
                click: Some(0)
            }
        );
        for _ in 0..30 {
            assert_eq!(
                state.event(b'W' as u32, true, false, Some(0), false),
                Decision {
                    suppress: true,
                    click: None
                }
            );
        }
        assert_eq!(
            state.event(b'W' as u32, true, false, None, false),
            Decision {
                suppress: true,
                click: None
            }
        );
        assert_eq!(
            state.event(b'W' as u32, false, false, None, false),
            Decision {
                suppress: true,
                click: None
            }
        );
        assert_eq!(
            state.event(b'W' as u32, true, false, Some(0), false).click,
            Some(0)
        );
    }
    #[test]
    fn simultaneous_wasd_are_independent_and_other_apps_keep_their_keys() {
        let mut state = KeyState::default();
        for (index, key) in [b'W', b'A', b'S', b'D'].into_iter().enumerate() {
            assert_eq!(
                state
                    .event(key as u32, true, false, Some(index), false)
                    .click,
                Some(index)
            );
        }
        assert_eq!(
            state.event(b'E' as u32, true, false, None, false),
            Decision::default()
        );
        state.event(b'W' as u32, false, false, None, false);
        assert_eq!(
            state.event(b'W' as u32, true, false, None, false),
            Decision::default()
        );
        assert_eq!(
            state.event(b'W' as u32, true, false, Some(0), false),
            Decision::default()
        );
    }
    #[test]
    fn injection_modifiers_and_keys_held_before_enable_do_not_trigger_clicks() {
        let mut state = KeyState::default();
        state.seed_down([b'W' as u32]);
        assert_eq!(
            state.event(b'W' as u32, true, false, Some(0), false),
            Decision::default()
        );
        assert_eq!(
            state.event(b'A' as u32, true, true, Some(1), false),
            Decision::default()
        );
        assert_eq!(
            state.event(b'A' as u32, true, false, Some(1), true),
            Decision::default()
        );
        assert_eq!(
            state.event(b'A' as u32, false, false, Some(1), false),
            Decision::default()
        );
        assert_eq!(
            state.event(b'A' as u32, true, false, Some(1), false).click,
            Some(1)
        );
    }
    #[test]
    fn wasd_preset_keeps_existing_locations_and_rejects_duplicate_keys() {
        let mut settings = KeyClickSettings::default();
        assert!(settings.add_wasd());
        settings.bindings[0].right = true;
        assert!(settings.add_wasd());
        assert_eq!(settings.bindings.len(), 4);
        assert!(settings.bindings[0].right);
        settings.bindings.push(settings.bindings[0].clone());
        assert!(settings.validate().is_err());
        assert!(!supported_key(0x1b));
        assert!(!supported_key(0x11));
        assert!(supported_key(b'W' as u32));
    }
    #[test]
    fn positions_follow_the_selected_game_and_detect_size_changes() {
        let game = GameIdentity {
            executable: "C:/Games/example.exe".into(),
            class: "GameWindow".into(),
            title: "Example".into(),
        };
        let point = ClientPoint {
            game: game.clone(),
            x: 100,
            y: 200,
            width: 1280,
            height: 900,
        };
        assert!(point.valid());
        assert!(point.fits(&game, 1280, 900));
        assert!(!point.fits(&game, 1920, 1080));
        let other = GameIdentity {
            executable: "C:/Other/game.exe".into(),
            ..game
        };
        assert!(!point.fits(&other, 1280, 900));
    }
}

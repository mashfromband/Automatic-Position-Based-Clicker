//! Physical keys mapped to game controls in a selected window.
use crate::error::{ClickerError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[cfg(windows)]
pub mod native;

pub const MAX_BINDINGS: usize = 16;
pub const MAX_PROFILES: usize = 64;
pub const MIN_STICK_RADIUS_PX: u32 = 4;
pub const MAX_STICK_RADIUS_PX: u32 = 2048;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressBehavior {
    #[default]
    Click,
    Hold,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMethod {
    #[default]
    Mouse,
    Touch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovementMode {
    #[default]
    Buttons,
    Stick,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StickSettings {
    pub center: Option<ClientPoint>,
    pub radius_px: u32,
}
impl Default for StickSettings {
    fn default() -> Self {
        Self {
            center: None,
            radius_px: 80,
        }
    }
}
impl StickSettings {
    /// The entire movement circle must fit inside the recorded client area.
    pub fn configured(&self) -> bool {
        let Some(center) = &self.center else {
            return false;
        };
        let radius = i64::from(self.radius_px);
        self.validate().is_ok()
            && i64::from(center.x) - radius >= 0
            && i64::from(center.y) - radius >= 0
            && i64::from(center.x) + radius < i64::from(center.width)
            && i64::from(center.y) + radius < i64::from(center.height)
    }

    fn validate(&self) -> Result<(), ClickerError> {
        if !(MIN_STICK_RADIUS_PX..=MAX_STICK_RADIUS_PX).contains(&self.radius_px)
            || self.center.as_ref().is_some_and(|point| !point.valid())
        {
            return Err(ErrorCode::StickRange.into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyBinding {
    pub key: Option<u32>,
    pub position: Option<ClientPoint>,
    pub right: bool,
    pub behavior: PressBehavior,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameProfile {
    pub id: u64,
    pub name: String,
    pub last_target: Option<GameIdentity>,
    pub bindings: Vec<KeyBinding>,
    pub movement_mode: MovementMode,
    pub input_method: InputMethod,
    pub stick: StickSettings,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyClickSettings {
    pub enabled: bool,
    pub last_target: Option<GameIdentity>,
    pub bindings: Vec<KeyBinding>,
    pub movement_mode: MovementMode,
    pub input_method: InputMethod,
    pub stick: StickSettings,
    pub profiles: Vec<GameProfile>,
    pub selected_profile: Option<u64>,
}
impl KeyClickSettings {
    pub fn validate(&self) -> Result<(), ClickerError> {
        validate_controls(&self.last_target, &self.bindings, &self.stick)?;
        if self.profiles.len() > MAX_PROFILES {
            return Err(ErrorCode::ProfileLimit.into());
        }
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for profile in &self.profiles {
            if profile.id == 0
                || !ids.insert(profile.id)
                || !names.insert(profile_name_key(&profile.name)?)
            {
                return Err(ErrorCode::ProfileName.into());
            }
            validate_controls(&profile.last_target, &profile.bindings, &profile.stick)?;
        }
        if self.selected_profile.is_some_and(|id| !ids.contains(&id)) {
            return Err(ErrorCode::ProfileName.into());
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
                behavior: PressBehavior::Hold,
                ..KeyBinding::default()
            }));
        true
    }

    pub fn save_profile(&mut self, name: &str) -> Result<u64, ClickerError> {
        let name_key = profile_name_key(name)?;
        let name = name.trim().to_owned();
        self.update_profiles(|settings| {
            if settings.profiles.iter().any(|profile| {
                Some(profile.id) != settings.selected_profile
                    && profile.name.trim().to_lowercase() == name_key
            }) {
                return Err(ErrorCode::ProfileName.into());
            }
            let id = if let Some(id) = settings.selected_profile {
                id
            } else {
                settings.available_profile_id()?
            };
            let profile = settings.current_profile(id, name);
            if let Some(index) = settings.profiles.iter().position(|p| p.id == id) {
                settings.profiles[index] = profile;
            } else {
                settings.profiles.push(profile);
            }
            settings.selected_profile = Some(id);
            Ok(id)
        })
    }

    pub fn select_profile(&mut self, id: u64) -> Result<(), ClickerError> {
        self.update_profiles(|settings| {
            if !settings.profiles.iter().any(|profile| profile.id == id) {
                return Err(ErrorCode::ProfileName.into());
            }
            settings.preserve_current_profile()?;
            let profile = settings
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
                .ok_or(ErrorCode::ProfileName)?;
            settings.apply_profile(profile);
            Ok(())
        })
    }

    pub fn new_profile(&mut self) -> Result<(), ClickerError> {
        self.update_profiles(|settings| {
            settings.preserve_current_profile()?;
            if settings.profiles.len() >= MAX_PROFILES {
                return Err(ErrorCode::ProfileLimit.into());
            }
            settings.reset_current_profile();
            Ok(())
        })
    }

    pub fn delete_profile(&mut self, id: u64) -> Result<(), ClickerError> {
        self.update_profiles(|settings| {
            let index = settings
                .profiles
                .iter()
                .position(|profile| profile.id == id)
                .ok_or(ErrorCode::ProfileName)?;
            if settings.selected_profile == Some(id) {
                settings.profiles.remove(index);
                let next = settings
                    .profiles
                    .get(index.min(settings.profiles.len().saturating_sub(1)))
                    .cloned();
                if let Some(profile) = next {
                    settings.apply_profile(profile);
                } else {
                    settings.reset_current_profile();
                }
            } else {
                settings.sync_current_profile()?;
                settings.profiles.remove(index);
            }
            Ok(())
        })
    }

    pub fn sync_selected_profile(&mut self) -> Result<(), ClickerError> {
        self.update_profiles(Self::sync_current_profile)
    }

    pub fn selected_profile_name(&self) -> Option<&str> {
        let id = self.selected_profile?;
        self.profiles
            .iter()
            .find(|profile| profile.id == id)
            .map(|profile| profile.name.as_str())
    }

    /// Checks recorded coordinates; the controller also checks the live window.
    pub fn has_configured_action(&self) -> bool {
        if validate_controls(&self.last_target, &self.bindings, &self.stick).is_err() {
            return false;
        }
        let Some(target) = &self.last_target else {
            return false;
        };
        let stick_ready = self.movement_mode == MovementMode::Stick
            && self.stick.configured()
            && self
                .stick
                .center
                .as_ref()
                .is_some_and(|center| center.game.same_game(target));
        stick_ready
            || self.bindings.iter().any(|binding| {
                binding.key.is_some_and(|key| {
                    self.movement_mode != MovementMode::Stick
                        || !matches!(key, 0x57 | 0x41 | 0x53 | 0x44)
                }) && binding
                    .position
                    .as_ref()
                    .is_some_and(|point| point.fits(target, point.width, point.height))
            })
    }

    fn current_profile(&self, id: u64, name: String) -> GameProfile {
        GameProfile {
            id,
            name,
            last_target: self.last_target.clone(),
            bindings: self.bindings.clone(),
            movement_mode: self.movement_mode,
            input_method: self.input_method,
            stick: self.stick.clone(),
        }
    }

    fn apply_profile(&mut self, profile: GameProfile) {
        self.last_target = profile.last_target;
        self.bindings = profile.bindings;
        self.movement_mode = profile.movement_mode;
        self.input_method = profile.input_method;
        self.stick = profile.stick;
        self.selected_profile = Some(profile.id);
        self.enabled = false;
    }

    fn reset_current_profile(&mut self) {
        self.last_target = None;
        self.bindings.clear();
        self.movement_mode = MovementMode::Buttons;
        self.input_method = InputMethod::Touch;
        self.stick = StickSettings::default();
        self.selected_profile = None;
        self.enabled = false;
    }

    fn has_draft_data(&self) -> bool {
        self.last_target.is_some()
            || !self.bindings.is_empty()
            || self.movement_mode != MovementMode::Buttons
            || self.stick != StickSettings::default()
    }

    fn sync_current_profile(&mut self) -> Result<(), ClickerError> {
        if let Some(id) = self.selected_profile {
            let index = self
                .profiles
                .iter()
                .position(|profile| profile.id == id)
                .ok_or(ErrorCode::ProfileName)?;
            self.profiles[index] = self.current_profile(id, self.profiles[index].name.clone());
        }
        Ok(())
    }

    fn preserve_current_profile(&mut self) -> Result<(), ClickerError> {
        if self.selected_profile.is_some() {
            return self.sync_current_profile();
        }
        // Neither input method makes an otherwise empty editor a draft.
        if !self.has_draft_data() {
            return Ok(());
        }
        let id = self.available_profile_id()?;
        // There is always an unused automatic name among these candidates.
        let name = (1..=MAX_PROFILES + 1)
            .map(|number| format!("Settings {number}"))
            .find(|name| {
                !self.profiles.iter().any(|profile| {
                    profile_name_key(&profile.name).ok() == profile_name_key(name).ok()
                })
            })
            .ok_or(ErrorCode::ProfileLimit)?;
        self.profiles.push(self.current_profile(id, name));
        Ok(())
    }

    fn available_profile_id(&self) -> Result<u64, ClickerError> {
        if self.profiles.len() >= MAX_PROFILES {
            return Err(ErrorCode::ProfileLimit.into());
        }
        (1..=MAX_PROFILES as u64 + 1)
            .find(|id| !self.profiles.iter().any(|profile| profile.id == *id))
            .ok_or_else(|| ErrorCode::ProfileLimit.into())
    }

    fn update_profiles<T>(
        &mut self,
        update: impl FnOnce(&mut Self) -> Result<T, ClickerError>,
    ) -> Result<T, ClickerError> {
        self.validate()?;
        let mut next = self.clone();
        let result = update(&mut next)?;
        next.validate()?;
        *self = next;
        Ok(result)
    }
}

fn validate_controls(
    target: &Option<GameIdentity>,
    bindings: &[KeyBinding],
    stick: &StickSettings,
) -> Result<(), ClickerError> {
    if bindings.len() > MAX_BINDINGS || target.as_ref().is_some_and(|game| !game.valid()) {
        return Err(ErrorCode::KeyMapping.into());
    }
    let mut used = BTreeSet::new();
    for binding in bindings {
        if binding
            .key
            .is_some_and(|key| !supported_key(key) || !used.insert(key))
            || binding
                .position
                .as_ref()
                .is_some_and(|point| !point.valid())
            || binding.label.contains('\0')
        {
            return Err(ErrorCode::KeyMapping.into());
        }
    }
    stick.validate()
}

fn profile_name_key(name: &str) -> Result<String, ClickerError> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(ErrorCode::ProfileName.into());
    }
    Ok(name.trim().to_lowercase())
}

pub fn supported_key(key: u32) -> bool {
    matches!(key,0x08|0x09|0x0d|0x10|0x13|0x14|0x20..=0x28|0x2c..=0x2e|0x30..=0x39|0x41..=0x5a|0x5d|0x60..=0x87|0x90|0x91|0xba..=0xc0|0xdb..=0xdf|0xe2)
}

pub fn normalized_game_key(key: u32) -> u32 {
    match key {
        0xa0 | 0xa1 => 0x10,
        _ => key,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub suppress: bool,
    pub click: Option<usize>,
}

#[derive(Default)]
pub struct KeyCapture {
    pending: Option<(u32, u32)>,
    binding_shifts: [bool; 3],
    binding_shift_pending: bool,
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

    pub fn event_for_binding(
        &mut self,
        key: u32,
        down: bool,
        mods: u32,
    ) -> (bool, Option<(u32, u32)>) {
        const MOD_SHIFT: u32 = 4;
        if key == 0x1b && down {
            self.binding_shifts = [false; 3];
            self.binding_shift_pending = false;
            return self.event(key, down, mods);
        }
        let shift_index = match key {
            0x10 => Some(0),
            0xa0 => Some(1),
            0xa1 => Some(2),
            _ => None,
        };
        if let Some(index) = shift_index {
            if down {
                if !self.binding_shifts.iter().any(|held| *held) {
                    self.binding_shift_pending = self.pending.is_none() && mods & !MOD_SHIFT == 0;
                }
                self.binding_shifts[index] = true;
                return (false, None);
            }
            let was_down = std::mem::take(&mut self.binding_shifts[index]);
            if was_down && !self.binding_shifts.iter().any(|held| *held) {
                let standalone =
                    std::mem::take(&mut self.binding_shift_pending) && mods & !MOD_SHIFT == 0;
                if standalone {
                    return (false, Some((0, 0x10)));
                }
            }
            return (false, None);
        }
        if down {
            self.binding_shift_pending = false;
        }
        self.event(key, down, mods)
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
        let mut observed = [false; 256];
        for key in keys {
            if let Some(value) = observed.get_mut(key as usize) {
                *value = true;
            }
        }
        for (index, down) in observed.into_iter().enumerate() {
            // Suppressed downs may be logically up in Windows. Preserve them until
            // their physical UP; clearing them could turn a repeat into a new click.
            if !self.consumed[index] {
                self.down[index] = down;
            } else if down {
                self.down[index] = true;
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

    fn game() -> GameIdentity {
        GameIdentity {
            executable: "C:/Games/example.exe".into(),
            class: "GameWindow".into(),
            title: "Example".into(),
        }
    }

    fn point() -> ClientPoint {
        ClientPoint {
            game: game(),
            x: 300,
            y: 300,
            width: 1280,
            height: 900,
        }
    }

    fn configured_settings() -> KeyClickSettings {
        KeyClickSettings {
            last_target: Some(game()),
            bindings: vec![KeyBinding {
                key: Some(b'W' as u32),
                position: Some(point()),
                right: true,
                behavior: PressBehavior::Click,
                label: "Move".into(),
            }],
            ..KeyClickSettings::default()
        }
    }

    fn assert_profile_error(
        settings: &mut KeyClickSettings,
        expected: ErrorCode,
        operation: impl FnOnce(&mut KeyClickSettings) -> Result<(), ClickerError>,
    ) {
        let original = settings.clone();
        assert_eq!(operation(settings).unwrap_err().code, expected);
        assert_eq!(*settings, original);
    }

    #[test]
    fn old_bindings_keep_click_and_mouse_while_new_profiles_start_with_touch() {
        let old: KeyClickSettings = serde_json::from_str(
            r#"{"enabled":true,"bindings":[{"key":87,"position":null,"right":true}]}"#,
        )
        .unwrap();
        assert!(old.enabled);
        assert!(old.bindings[0].right);
        assert_eq!(old.bindings[0].behavior, PressBehavior::Click);
        assert_eq!(old.bindings[0].label, "");
        assert_eq!(old.input_method, InputMethod::Mouse);
        assert_eq!(old.movement_mode, MovementMode::Buttons);
        assert_eq!(old.stick, StickSettings::default());
        assert!(old.profiles.is_empty());
        assert_eq!(old.selected_profile, None);
        old.validate().unwrap();

        let mut fresh = KeyClickSettings::default();
        fresh.new_profile().unwrap();
        assert_eq!(fresh.input_method, InputMethod::Touch);
        assert!(!fresh.enabled);
        assert!(fresh.bindings.is_empty());
        assert!(fresh.profiles.is_empty());
        let id = fresh.save_profile("New game").unwrap();
        assert_eq!(fresh.profiles[0].id, id);
        assert_eq!(fresh.profiles[0].input_method, InputMethod::Touch);
        let old_profile: GameProfile =
            serde_json::from_str(r#"{"id":8,"name":"Old game"}"#).unwrap();
        assert_eq!(old_profile.input_method, InputMethod::Mouse);
    }

    #[test]
    fn control_enums_and_profiles_round_trip_without_execution_state() {
        let mut settings = configured_settings();
        settings.enabled = true;
        settings.input_method = InputMethod::Touch;
        settings.movement_mode = MovementMode::Stick;
        settings.stick.center = Some(point());
        settings.bindings[0].behavior = PressBehavior::Hold;
        settings.bindings[0].label = "移動".into();
        settings.save_profile("Game").unwrap();
        let json = serde_json::to_value(&settings).unwrap();
        let profile = json["profiles"][0].as_object().unwrap();
        assert_eq!(profile.len(), 7);
        for field in [
            "id",
            "name",
            "last_target",
            "bindings",
            "movement_mode",
            "input_method",
            "stick",
        ] {
            assert!(profile.contains_key(field), "{field}");
        }
        assert!(!profile.contains_key("enabled"));
        assert!(!profile.contains_key("hwnd"));
        assert_eq!(json["input_method"], "touch");
        assert_eq!(json["movement_mode"], "stick");
        assert_eq!(json["bindings"][0]["behavior"], "hold");
        let loaded: KeyClickSettings = serde_json::from_value(json).unwrap();
        assert_eq!(loaded, settings);
        loaded.validate().unwrap();
    }

    #[test]
    fn wasd_adds_holds_without_changing_existing_clicks_or_partial_rows() {
        let mut settings = configured_settings();
        let original = settings.bindings[0].clone();
        settings.bindings.push(KeyBinding::default());
        assert!(settings.add_wasd());
        assert_eq!(settings.bindings.len(), 5);
        assert_eq!(settings.bindings[0], original);
        assert_eq!(settings.bindings[1], KeyBinding::default());
        for (binding, key) in settings.bindings[2..].iter().zip([b'A', b'S', b'D']) {
            assert_eq!(binding.key, Some(key as u32));
            assert_eq!(binding.behavior, PressBehavior::Hold);
            assert!(binding.position.is_none());
        }
        settings
            .bindings
            .resize(MAX_BINDINGS, KeyBinding::default());
        settings.bindings[2].key = None;
        let original = settings.clone();
        assert!(!settings.add_wasd());
        assert_eq!(settings, original);
    }

    #[test]
    fn stick_allows_an_unset_center_but_checks_the_complete_circle_before_use() {
        let mut stick = StickSettings::default();
        stick.validate().unwrap();
        assert!(!stick.configured());
        stick.center = Some(ClientPoint {
            x: 80,
            y: 80,
            width: 161,
            height: 161,
            ..point()
        });
        assert!(stick.configured());
        for (x, y) in [(79, 80), (81, 80), (80, 79), (80, 81)] {
            let center = stick.center.as_mut().unwrap();
            center.x = x;
            center.y = y;
            stick.validate().unwrap();
            assert!(!stick.configured(), "{x}, {y}");
        }
        stick.center = Some(ClientPoint {
            x: i32::MAX - 1,
            y: i32::MAX - 1,
            width: i32::MAX,
            height: i32::MAX,
            ..point()
        });
        assert!(!stick.configured());
        for radius in [0, 3, 2049, u32::MAX] {
            stick.radius_px = radius;
            assert_eq!(stick.validate().unwrap_err().code, ErrorCode::StickRange);
            assert!(!stick.configured());
        }
        for radius in [4, 2048] {
            stick.radius_px = radius;
            stick.center = Some(ClientPoint {
                x: radius as i32,
                y: radius as i32,
                width: (radius * 2 + 1) as i32,
                height: (radius * 2 + 1) as i32,
                ..point()
            });
            assert!(stick.configured());
        }
    }

    #[test]
    fn incomplete_controls_can_be_saved_but_only_valid_actions_are_ready() {
        let mut settings = KeyClickSettings {
            last_target: Some(game()),
            movement_mode: MovementMode::Stick,
            bindings: vec![KeyBinding {
                key: Some(b'W' as u32),
                ..KeyBinding::default()
            }],
            ..KeyClickSettings::default()
        };
        settings.save_profile("In progress").unwrap();
        settings.validate().unwrap();
        assert!(!settings.has_configured_action());
        settings.bindings[0].position = Some(point());
        assert!(!settings.has_configured_action());
        settings.movement_mode = MovementMode::Buttons;
        assert!(settings.has_configured_action());
        settings.bindings[0].key = None;
        assert!(!settings.has_configured_action());
        settings.movement_mode = MovementMode::Stick;
        settings.stick.center = Some(point());
        assert!(settings.has_configured_action());
        settings.stick.center.as_mut().unwrap().x = 0;
        settings.validate().unwrap();
        assert!(!settings.has_configured_action());
        settings.stick.center = None;
        settings.bindings[0].key = Some(0x20);
        assert!(settings.has_configured_action());
        settings.bindings[0].position.as_mut().unwrap().x = 1280;
        assert!(!settings.has_configured_action());
        settings.bindings[0].position = Some(point());
        settings.last_target.as_mut().unwrap().executable = "C:/Other/game.exe".into();
        assert!(!settings.has_configured_action());
        settings.last_target = None;
        assert!(!settings.has_configured_action());
    }

    #[test]
    fn switching_profiles_preserves_edits_and_stops_the_live_configuration() {
        let mut settings = configured_settings();
        let first = settings.save_profile("First").unwrap();
        settings.enabled = true;
        settings.bindings[0].label = "Edited first".into();
        settings.new_profile().unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.selected_profile_name(), None);
        assert_eq!(settings.profiles[0].bindings[0].label, "Edited first");
        assert_eq!(settings.input_method, InputMethod::Touch);
        assert_eq!(settings.stick, StickSettings::default());
        assert_eq!(settings.last_target, None);
        assert!(settings.bindings.is_empty());
        settings.last_target = Some(game());
        settings.stick.center = Some(point());
        settings.stick.radius_px = 110;
        settings.movement_mode = MovementMode::Stick;
        let second = settings.save_profile("Second").unwrap();
        assert_ne!(first, second);
        settings.enabled = true;
        settings.stick.radius_px = 120;
        settings.select_profile(first).unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.selected_profile_name(), Some("First"));
        assert_eq!(settings.input_method, InputMethod::Mouse);
        assert_eq!(settings.movement_mode, MovementMode::Buttons);
        assert_eq!(settings.bindings[0].label, "Edited first");
        assert!(settings.bindings[0].right);
        assert_eq!(settings.bindings[0].behavior, PressBehavior::Click);
        settings.select_profile(second).unwrap();
        assert_eq!(settings.stick.center, Some(point()));
        assert_eq!(settings.stick.radius_px, 120);
        assert_eq!(settings.input_method, InputMethod::Touch);
        assert_eq!(settings.movement_mode, MovementMode::Stick);
        settings.enabled = true;
        settings.stick.radius_px = 130;
        settings.select_profile(second).unwrap();
        assert_eq!(settings.stick.radius_px, 130);
        assert!(!settings.enabled);
    }

    #[test]
    fn saving_and_syncing_update_one_profile_without_persisting_enabled() {
        let mut settings = configured_settings();
        let id = settings.save_profile("  First  ").unwrap();
        settings.enabled = true;
        settings.bindings[0].behavior = PressBehavior::Hold;
        settings.sync_selected_profile().unwrap();
        assert!(settings.enabled);
        assert_eq!(
            settings.profiles[0].bindings[0].behavior,
            PressBehavior::Hold
        );
        assert_eq!(settings.save_profile("Renamed").unwrap(), id);
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.selected_profile_name(), Some("Renamed"));
        settings.select_profile(id).unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.bindings[0].behavior, PressBehavior::Hold);
    }

    #[test]
    fn profile_failures_leave_both_saved_and_current_data_unchanged() {
        let mut settings = configured_settings();
        let first = settings.save_profile("First").unwrap();
        settings.new_profile().unwrap();
        settings.bindings.push(KeyBinding {
            label: "Still being edited".into(),
            ..KeyBinding::default()
        });
        settings.enabled = true;
        for name in [
            "",
            "  ",
            "\n",
            "bad\nname",
            "bad\tname",
            "bad\0name",
            " first ",
        ] {
            assert_profile_error(&mut settings, ErrorCode::ProfileName, |settings| {
                settings.save_profile(name).map(|_| ())
            });
        }
        for id in [0, first + 50, u64::MAX] {
            assert_profile_error(&mut settings, ErrorCode::ProfileName, |settings| {
                settings.select_profile(id)
            });
            assert_profile_error(&mut settings, ErrorCode::ProfileName, |settings| {
                settings.delete_profile(id)
            });
        }
        settings.save_profile("Second").unwrap();
        settings.bindings[0].label = "Unsaved edit".into();
        assert_profile_error(&mut settings, ErrorCode::ProfileName, |settings| {
            settings.save_profile("FIRST").map(|_| ())
        });
        settings.selected_profile = Some(999);
        assert_profile_error(&mut settings, ErrorCode::ProfileName, |settings| {
            settings.sync_selected_profile()
        });
    }

    #[test]
    fn unnamed_legacy_and_partial_drafts_are_archived_before_new_or_switch() {
        let mut settings = configured_settings();
        let legacy = settings.bindings.clone();
        settings.new_profile().unwrap();
        assert_eq!(settings.profiles.len(), 1);
        assert!(!settings.profiles[0].name.is_empty());
        assert_eq!(settings.profiles[0].bindings, legacy);
        assert_eq!(settings.profiles[0].input_method, InputMethod::Mouse);
        let legacy_id = settings.profiles[0].id;
        for _ in 0..8 {
            settings.new_profile().unwrap();
            assert_eq!(settings.profiles.len(), 1);
        }
        let partial = KeyBinding {
            label: "Aim".into(),
            ..KeyBinding::default()
        };
        settings.bindings.push(partial.clone());
        settings.select_profile(legacy_id).unwrap();
        assert_eq!(settings.profiles.len(), 2);
        let draft_id = settings.profiles[1].id;
        assert_ne!(legacy_id, draft_id);
        assert_eq!(settings.profiles[1].bindings, vec![partial.clone()]);
        assert_eq!(settings.bindings, legacy);
        settings.select_profile(draft_id).unwrap();
        assert_eq!(settings.bindings, vec![partial]);
        assert_eq!(settings.input_method, InputMethod::Touch);
        assert!(!settings.has_configured_action());
        settings.new_profile().unwrap();
        settings.input_method = InputMethod::Mouse;
        settings.select_profile(legacy_id).unwrap();
        assert_eq!(settings.profiles.len(), 2);
    }

    #[test]
    fn automatic_profile_names_and_ids_avoid_existing_values() {
        let mut settings = configured_settings();
        settings.profiles.push(GameProfile {
            id: u64::MAX,
            name: "settings 1".into(),
            ..GameProfile::default()
        });
        settings.select_profile(u64::MAX).unwrap();
        assert_eq!(settings.profiles.len(), 2);
        let archived = settings.profiles.iter().find(|p| p.id != u64::MAX).unwrap();
        assert_ne!(archived.id, 0);
        assert_eq!(archived.name, "Settings 2");
        assert_eq!(archived.bindings, configured_settings().bindings);
        settings.validate().unwrap();
    }

    #[test]
    fn profile_limit_never_discards_a_draft_and_existing_profiles_remain_editable() {
        let mut settings = configured_settings();
        settings.profiles = (1..=MAX_PROFILES as u64)
            .map(|id| GameProfile {
                id,
                name: format!("Game {id}"),
                ..GameProfile::default()
            })
            .collect();
        type Operation = fn(&mut KeyClickSettings) -> Result<(), ClickerError>;
        let operations: [Operation; 3] = [
            KeyClickSettings::new_profile,
            |settings: &mut KeyClickSettings| settings.select_profile(1),
            |settings: &mut KeyClickSettings| settings.save_profile("Another").map(|_| ()),
        ];
        for operation in operations {
            assert_profile_error(&mut settings, ErrorCode::ProfileLimit, operation);
        }
        settings.selected_profile = Some(1);
        settings.bindings[0].label = "At capacity".into();
        assert_eq!(settings.save_profile("Renamed").unwrap(), 1);
        settings.sync_selected_profile().unwrap();
        assert_eq!(settings.profiles.len(), MAX_PROFILES);
        settings.delete_profile(2).unwrap();
        assert_eq!(settings.profiles.len(), MAX_PROFILES - 1);
        settings.new_profile().unwrap();
        settings.bindings.push(KeyBinding::default());
        settings.select_profile(1).unwrap();
        assert_eq!(settings.profiles.len(), MAX_PROFILES);
        assert_eq!(settings.bindings[0].label, "At capacity");
    }

    #[test]
    fn deleting_an_inactive_profile_keeps_the_active_edit() {
        let mut settings = configured_settings();
        let first = settings.save_profile("First").unwrap();
        settings.new_profile().unwrap();
        let second = settings.save_profile("Second").unwrap();
        settings.select_profile(first).unwrap();
        settings.enabled = true;
        settings.bindings[0].label = "Keep this edit".into();
        settings.delete_profile(second).unwrap();
        assert!(settings.enabled);
        assert_eq!(settings.selected_profile, Some(first));
        assert_eq!(settings.profiles.len(), 1);
        assert!(!settings.profiles.iter().any(|profile| profile.id == second));
        assert_eq!(settings.profiles[0].bindings[0].label, "Keep this edit");
        assert_eq!(settings.bindings[0].label, "Keep this edit");
        assert_eq!(settings.bindings[0].position, Some(point()));
        assert_eq!(settings.last_target, Some(game()));
        assert!(settings.bindings[0].right);
        assert_eq!(settings.bindings[0].behavior, PressBehavior::Click);
    }

    #[test]
    fn an_empty_touch_editor_never_uses_profile_slots() {
        let mut settings = KeyClickSettings::default();
        for _ in 0..MAX_PROFILES + 1 {
            settings.new_profile().unwrap();
            assert!(settings.profiles.is_empty());
            assert_eq!(settings.selected_profile, None);
            assert_eq!(settings.input_method, InputMethod::Touch);
        }
        let id = settings.save_profile("Empty saved profile").unwrap();
        settings.new_profile().unwrap();
        for _ in 0..8 {
            settings.new_profile().unwrap();
            assert_eq!(settings.profiles.len(), 1);
        }
        settings.select_profile(id).unwrap();
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.selected_profile, Some(id));
    }

    #[test]
    fn deleting_the_last_profile_resets_the_editor_and_never_archives_deleted_data() {
        let mut settings = configured_settings();
        settings.movement_mode = MovementMode::Stick;
        settings.stick.center = Some(point());
        settings.stick.radius_px = 100;
        let deleted = settings.save_profile("Delete this profile").unwrap();
        settings.enabled = true;
        settings.bindings[0].label = "Delete this edit too".into();
        settings.delete_profile(deleted).unwrap();
        assert_eq!(
            settings,
            KeyClickSettings {
                input_method: InputMethod::Touch,
                ..KeyClickSettings::default()
            }
        );
        settings.sync_selected_profile().unwrap();
        for _ in 0..8 {
            settings.new_profile().unwrap();
            assert!(settings.profiles.is_empty());
            assert!(settings.bindings.is_empty());
            assert_eq!(settings.selected_profile, None);
        }
        assert_profile_error(&mut settings, ErrorCode::ProfileName, |settings| {
            settings.select_profile(deleted)
        });
    }

    #[test]
    fn deletion_at_capacity_allows_new_and_switch_without_resurrecting_the_deleted_id() {
        for deleted in [1, MAX_PROFILES as u64 / 2, MAX_PROFILES as u64] {
            let mut settings = configured_settings();
            settings.profiles = (1..=MAX_PROFILES as u64)
                .map(|id| GameProfile {
                    id,
                    name: format!("Game {id}"),
                    ..GameProfile::default()
                })
                .collect();
            settings.selected_profile = Some(deleted);
            settings.enabled = true;
            settings.bindings[0].label = "Explicitly deleted edit".into();
            settings.delete_profile(deleted).unwrap();
            let next = if deleted == MAX_PROFILES as u64 {
                deleted - 1
            } else {
                deleted + 1
            };
            assert_eq!(settings.selected_profile, Some(next));
            assert!(!settings.enabled);
            assert!(settings.bindings.is_empty());
            assert_eq!(settings.last_target, None);
            assert_eq!(settings.movement_mode, MovementMode::Buttons);
            assert_eq!(settings.input_method, InputMethod::Mouse);
            let remaining = settings.profiles.clone();
            for _ in 0..8 {
                settings.new_profile().unwrap();
                assert_eq!(settings.profiles, remaining);
                assert_eq!(settings.profiles.len(), MAX_PROFILES - 1);
                assert!(
                    !settings
                        .profiles
                        .iter()
                        .any(|profile| profile.id == deleted)
                );
            }
            settings.select_profile(next).unwrap();
            assert_eq!(settings.profiles, remaining);
            settings.new_profile().unwrap();
            settings.bindings.push(KeyBinding {
                key: Some(0x20),
                label: "Replacement".into(),
                ..KeyBinding::default()
            });
            settings.save_profile("Replacement").unwrap();
            assert_eq!(settings.profiles.len(), MAX_PROFILES);
            assert!(!settings.profiles.iter().any(|profile| {
                profile
                    .bindings
                    .iter()
                    .any(|binding| binding.label == "Explicitly deleted edit")
            }));
        }
    }

    #[test]
    fn a_full_profile_list_rejects_new_and_can_recover_an_existing_unnamed_draft() {
        let mut settings = configured_settings();
        settings.profiles = (1..=MAX_PROFILES as u64)
            .map(|id| GameProfile {
                id,
                name: format!("Game {id}"),
                ..GameProfile::default()
            })
            .collect();
        settings.selected_profile = Some(1);
        settings.bindings[0].label = "Keep at capacity".into();
        for _ in 0..8 {
            assert_profile_error(&mut settings, ErrorCode::ProfileLimit, |settings| {
                settings.new_profile()
            });
            assert_eq!(settings.profiles.len(), MAX_PROFILES);
            assert_eq!(settings.bindings[0].label, "Keep at capacity");
            assert_eq!(settings.selected_profile, Some(1));
        }
        settings.sync_selected_profile().unwrap();
        settings.select_profile(1).unwrap();
        assert_eq!(settings.bindings[0].label, "Keep at capacity");
        // Recover data saved by an earlier implementation that allowed New at
        // capacity; deleting an inactive profile must leave this editor intact.
        settings.selected_profile = None;
        settings.bindings = vec![KeyBinding {
            label: "Unfinished new draft".into(),
            ..KeyBinding::default()
        }];
        assert_profile_error(&mut settings, ErrorCode::ProfileLimit, |settings| {
            settings.new_profile()
        });
        assert_profile_error(&mut settings, ErrorCode::ProfileLimit, |settings| {
            settings.select_profile(1)
        });
        let draft = settings.bindings.clone();
        settings.delete_profile(2).unwrap();
        assert_eq!(settings.bindings, draft);
        assert_eq!(settings.selected_profile, None);
        settings.select_profile(1).unwrap();
        assert_eq!(settings.profiles.len(), MAX_PROFILES);
        assert_eq!(settings.bindings[0].label, "Keep at capacity");
        assert!(
            settings
                .profiles
                .iter()
                .any(|profile| profile.bindings == draft)
        );
    }

    #[test]
    fn saved_profiles_are_validated_even_when_they_are_not_selected() {
        let mut settings = configured_settings();
        settings.save_profile("First").unwrap();
        settings.profiles.push(settings.profiles[0].clone());
        assert_eq!(
            settings.validate().unwrap_err().code,
            ErrorCode::ProfileName
        );
        settings.profiles[1].id = 2;
        settings.profiles[1].name = " first ".into();
        assert_eq!(
            settings.validate().unwrap_err().code,
            ErrorCode::ProfileName
        );
        settings.profiles[1].name = "Second".into();
        let duplicate = settings.profiles[1].bindings[0].clone();
        settings.profiles[1].bindings.push(duplicate);
        assert_eq!(settings.validate().unwrap_err().code, ErrorCode::KeyMapping);
        settings.profiles[1].bindings.pop();
        settings.profiles[1].stick.radius_px = 3;
        assert_eq!(settings.validate().unwrap_err().code, ErrorCode::StickRange);
        settings.profiles[1].stick.radius_px = 80;
        settings.profiles[1].id = 0;
        assert_eq!(
            settings.validate().unwrap_err().code,
            ErrorCode::ProfileName
        );
    }

    #[test]
    fn binding_capture_accepts_standalone_shift_only_on_its_last_release() {
        for key in [0x10, 0xa0, 0xa1] {
            let mut capture = KeyCapture::default();
            assert_eq!(capture.event_for_binding(key, false, 0), (false, None));
            assert_eq!(capture.event_for_binding(key, true, 0), (false, None));
            assert_eq!(capture.event_for_binding(key, true, 4), (false, None));
            assert_eq!(
                capture.event_for_binding(key, false, 4),
                (false, Some((0, 0x10)))
            );
            assert_eq!(capture.event_for_binding(key, false, 0), (false, None));
        }
        let mut capture = KeyCapture::default();
        assert_eq!(capture.event_for_binding(0xa0, true, 0), (false, None));
        assert_eq!(capture.event_for_binding(0xa1, true, 4), (false, None));
        assert_eq!(capture.event_for_binding(0xa0, false, 4), (false, None));
        assert_eq!(
            capture.event_for_binding(0xa1, false, 4),
            (false, Some((0, 0x10)))
        );
    }

    #[test]
    fn binding_capture_does_not_report_a_shift_chord_as_standalone_shift() {
        for release_shift_first in [true, false] {
            let mut capture = KeyCapture::default();
            capture.event_for_binding(0xa0, true, 0);
            assert_eq!(
                capture.event_for_binding(b'W' as u32, true, 4),
                (true, None)
            );
            if release_shift_first {
                assert_eq!(capture.event_for_binding(0xa0, false, 4), (false, None));
            }
            assert_eq!(
                capture.event_for_binding(b'W' as u32, false, 0),
                (false, Some((4, b'W' as u32)))
            );
            if !release_shift_first {
                assert_eq!(capture.event_for_binding(0xa0, false, 4), (false, None));
            }
        }
        let mut capture = KeyCapture::default();
        capture.event_for_binding(b'W' as u32, true, 0);
        capture.event_for_binding(0xa0, true, 0);
        assert_eq!(
            capture.event_for_binding(b'W' as u32, false, 4),
            (false, Some((0, b'W' as u32)))
        );
        assert_eq!(capture.event_for_binding(0xa0, false, 4), (false, None));
    }

    #[test]
    fn binding_capture_rejects_modifier_chords_and_escape_cancels_shift() {
        for (modifier, flag) in [(0x11, 2), (0x12, 1), (0x5b, 8)] {
            let mut capture = KeyCapture::default();
            capture.event_for_binding(0xa0, true, flag);
            assert_eq!(
                capture.event_for_binding(0xa0, false, flag | 4),
                (false, None)
            );
            capture.event_for_binding(0xa0, true, 0);
            capture.event_for_binding(modifier, true, 4);
            capture.event_for_binding(modifier, false, flag | 4);
            assert_eq!(capture.event_for_binding(0xa0, false, 4), (false, None));
        }
        let mut capture = KeyCapture::default();
        capture.event_for_binding(0xa0, true, 0);
        assert_eq!(
            capture.event_for_binding(0x1b, true, 4),
            (true, Some((0, 0x1b)))
        );
        assert_eq!(capture.event_for_binding(0xa0, false, 4), (false, None));
        capture.event_for_binding(0xa1, true, 0);
        assert_eq!(
            capture.event_for_binding(0xa1, false, 4),
            (false, Some((0, 0x10)))
        );
    }

    #[test]
    fn shortcut_capture_keeps_modifier_keys_out_and_game_keys_normalize_only_shift() {
        let mut capture = KeyCapture::default();
        for key in [
            0x10, 0x11, 0x12, 0x5b, 0x5c, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5,
        ] {
            assert_eq!(capture.event(key, true, 0), (false, None));
            assert_eq!(capture.event(key, false, 0), (false, None));
        }
        assert_eq!(capture.event(b'W' as u32, true, 6), (true, None));
        assert_eq!(
            capture.event(b'W' as u32, false, 0),
            (false, Some((6, b'W' as u32)))
        );
        for key in [0x10, 0xa0, 0xa1] {
            assert_eq!(normalized_game_key(key), 0x10);
            assert!(supported_key(normalized_game_key(key)));
        }
        for key in [0x11, 0x12, 0x5b, 0x5c, 0xa2, 0xa3, 0xa4, 0xa5] {
            assert_eq!(normalized_game_key(key), key);
            assert!(!supported_key(key));
        }
        assert_eq!(normalized_game_key(b'W' as u32), b'W' as u32);
    }

    #[test]
    fn reseeding_releases_unconsumed_keys_but_keeps_a_suppressed_hold() {
        let mut state = KeyState::default();
        state.event(b'W' as u32, true, false, Some(0), false);
        state.event(b'A' as u32, true, false, None, false);
        state.seed_down([]);
        assert_eq!(
            state.event(b'W' as u32, true, false, Some(0), false).click,
            None
        );
        assert_eq!(
            state.event(b'A' as u32, true, false, Some(1), false).click,
            Some(1)
        );
        state.event(b'W' as u32, false, false, Some(0), false);
        assert_eq!(
            state.event(b'W' as u32, true, false, Some(0), false).click,
            Some(0)
        );
    }
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

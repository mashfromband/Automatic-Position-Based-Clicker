use super::super::{
    ClientPoint, InputMethod, KeyClickSettings, MovementMode, PressBehavior, StickSettings,
    normalized_game_key,
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub(super) const SHIFT: u32 = 0x10;
pub(super) const STICK: usize = 256;
const PRESS_TIME: Duration = Duration::from_millis(50);
const RELEASE_TIME: Duration = Duration::from_millis(50);
const MAX_PRESS_AGE: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Binding {
    pub key: Option<u32>,
    pub position: Option<ClientPoint>,
    pub right: bool,
    pub behavior: PressBehavior,
}

/// Only fields that affect input belong here; the profile book is UI/storage state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ActiveSettings {
    pub bindings: Vec<Binding>,
    pub movement_mode: MovementMode,
    pub input_method: InputMethod,
    pub stick: StickSettings,
    pub operation_hotkeys: Vec<(u32, u32)>,
}
impl From<&KeyClickSettings> for ActiveSettings {
    fn from(settings: &KeyClickSettings) -> Self {
        Self {
            bindings: settings
                .bindings
                .iter()
                .map(|binding| Binding {
                    key: binding.key.map(normalized_game_key),
                    position: binding.position.clone(),
                    right: binding.right,
                    behavior: binding.behavior,
                })
                .collect(),
            movement_mode: settings.movement_mode,
            input_method: settings.input_method,
            stick: settings.stick.clone(),
            operation_hotkeys: Vec::new(),
        }
    }
}
impl ActiveSettings {
    pub fn has_modern_controls(&self) -> bool {
        self.input_method == InputMethod::Touch
            || self.movement_mode == MovementMode::Stick
            || self.bindings.iter().any(|binding| {
                binding.behavior == PressBehavior::Hold || binding.key == Some(SHIFT)
            })
    }
    pub fn handles(&self, key: u32) -> bool {
        let key = normalized_game_key(key);
        if self.movement_mode == MovementMode::Stick && wasd(key) {
            return self.stick.center.is_some();
        }
        self.bindings
            .iter()
            .any(|binding| binding.key == Some(key) && binding.position.is_some())
    }
}

fn wasd(key: u32) -> bool {
    matches!(key, 0x57 | 0x41 | 0x53 | 0x44)
}
fn shortcut_key(key: u32) -> bool {
    matches!(key, 0x11 | 0x12 | 0x5b | 0x5c | 0xa2..=0xa5)
}
fn shift_key(key: u32) -> bool {
    matches!(key, SHIFT | 0xa0 | 0xa1)
}

#[derive(Clone)]
pub(super) struct SourceSnapshot {
    down: [bool; 256],
    presses: [u64; 256],
    pressed_at: [Option<Instant>; 256],
    press_order: [u64; 256],
    pub shortcut: bool,
}

pub(super) struct RouteContext {
    pub ready: bool,
    pub alt: bool,
}

/// Physical state and accepted state are separate. A focus/modifier change disarms
/// held keys until their physical UP; it cannot manufacture a new press on resume.
pub(super) struct SourceKeyState {
    physical: [bool; 256],
    accepted: [bool; 256],
    consumed: [bool; 256],
    presses: [u64; 256],
    pressed_at: [Option<Instant>; 256],
    press_order: [u64; 256],
    next_order: u64,
}
impl Default for SourceKeyState {
    fn default() -> Self {
        Self {
            physical: [false; 256],
            accepted: [false; 256],
            consumed: [false; 256],
            presses: [0; 256],
            pressed_at: [None; 256],
            press_order: [0; 256],
            next_order: 0,
        }
    }
}
impl SourceKeyState {
    fn shift_down(&self) -> bool {
        [SHIFT, 0xa0, 0xa1]
            .into_iter()
            .any(|key| self.physical[key as usize])
    }
    fn shortcut_down(&self) -> bool {
        (0..256).any(|key| shortcut_key(key) && self.physical[key as usize])
    }
    pub fn shortcut_active(&self) -> bool {
        self.shortcut_down()
    }
    pub fn route(
        &mut self,
        settings: &ActiveSettings,
        key: u32,
        down: bool,
        injected: bool,
        context: RouteContext,
        now: Instant,
    ) -> bool {
        if injected || key >= 256 {
            return false;
        }
        if !context.ready {
            self.suspend();
        }
        let modifiers =
            u32::from(
                context.alt || self.physical[0x12] || self.physical[0xa4] || self.physical[0xa5],
            ) | (u32::from(self.physical[0x11] || self.physical[0xa2] || self.physical[0xa3]) << 1)
                | (u32::from(self.shift_down()) << 2)
                | (u32::from(self.physical[0x5b] || self.physical[0x5c]) << 3);
        let reserved = down
            && settings
                .operation_hotkeys
                .iter()
                .any(|(mods, registered)| *registered == key && *mods & 0x0f == modifiers);
        if reserved {
            self.suspend();
            // A matching repeat must reach RegisterHotKey even if this physical
            // key originally started an unmodified game control.
            self.consumed[key as usize] = false;
        }
        // Legacy all-Click Mouse mappings keep their original shortcut priority.
        // Shift remains ordinary OS input in both the legacy and modern routes.
        let legacy_shift = !settings.has_modern_controls() && self.shift_down();
        self.event(
            key,
            down,
            false,
            context.ready && settings.handles(key),
            !context.ready || context.alt || legacy_shift || reserved,
            now,
        )
    }
    pub fn suspend(&mut self) {
        self.accepted.fill(false);
        // Pending taps must not survive losing the target or entering a shortcut.
        self.pressed_at.fill(None);
    }
    pub fn seed_down(&mut self, keys: impl IntoIterator<Item = u32>) {
        let mut observed = [false; 256];
        for key in keys.into_iter().filter(|key| *key < 256) {
            observed[key as usize] = true;
        }
        for (key, down) in observed.into_iter().enumerate() {
            // A suppressed key can be logically UP in GetAsyncKeyState. Keep it
            // physical-down until its hook UP has balanced the suppressed DOWN.
            if !self.consumed[key] || down {
                self.physical[key] = down;
            }
        }
        // Generic Shift is redundant when either side was reported.
        if self.physical[0xa0] || self.physical[0xa1] {
            self.physical[SHIFT as usize] = false;
        }
        if self.physical[0xa2] || self.physical[0xa3] {
            self.physical[0x11] = false;
        }
        if self.physical[0xa4] || self.physical[0xa5] {
            self.physical[0x12] = false;
        }
        self.suspend();
    }
    pub fn snapshot(&self) -> SourceSnapshot {
        SourceSnapshot {
            down: self.accepted,
            presses: self.presses,
            pressed_at: self.pressed_at,
            press_order: self.press_order,
            shortcut: self.shortcut_down(),
        }
    }
    /// Returns whether to suppress this physical event. Shift always reaches the
    /// OS, including both sides, even when the combined Shift key is also bound.
    pub fn event(
        &mut self,
        key: u32,
        down: bool,
        injected: bool,
        handled: bool,
        blocked: bool,
        now: Instant,
    ) -> bool {
        if injected || key >= 256 {
            return false;
        }
        let was_shift = self.shift_down();
        let index = key as usize;
        let repeated = self.physical[index];
        self.physical[index] = down;
        if shortcut_key(key) {
            if down {
                self.suspend();
            }
            return false;
        }
        let logical = normalized_game_key(key) as usize;
        if shift_key(key) {
            if !self.shift_down() {
                self.accepted[SHIFT as usize] = false;
            }
            if down && !was_shift && handled && !blocked && !self.shortcut_down() {
                self.accepted[SHIFT as usize] = true;
                self.record_press(SHIFT as usize, now);
            }
            return false;
        }
        if !down {
            self.accepted[logical] = false;
            return std::mem::take(&mut self.consumed[index]);
        }
        if repeated {
            if self.shortcut_down() {
                self.consumed[index] = false;
                return false;
            }
            return self.consumed[index];
        }
        if handled && !blocked && !self.shortcut_down() {
            self.accepted[logical] = true;
            self.consumed[index] = true;
            self.record_press(logical, now);
            return true;
        }
        false
    }
    fn record_press(&mut self, key: usize, now: Instant) {
        self.next_order = self.next_order.wrapping_add(1);
        self.press_order[key] = self.next_order;
        self.presses[key] = self.presses[key].wrapping_add(1);
        self.pressed_at[key] = Some(now);
    }
}

#[derive(Clone, Debug)]
pub(super) struct Action {
    pub id: usize,
    pub position: ClientPoint,
    pub start: Option<ClientPoint>,
    pub right: bool,
    pub pulse: bool,
}
enum PulsePhase {
    Pending(Instant),
    Pressing(Instant),
    Releasing,
    Cooling(Instant),
}
struct Pulse {
    action: Action,
    phase: PulsePhase,
    order: u64,
}
pub(super) struct Planner {
    seen: [u64; 256],
    pulses: BTreeMap<usize, Pulse>,
}
impl Default for Planner {
    fn default() -> Self {
        Self {
            seen: [0; 256],
            pulses: BTreeMap::new(),
        }
    }
}
impl Planner {
    pub fn can_start(&self, id: usize, now: Instant) -> bool {
        match self.pulses.get(&id).map(|pulse| &pulse.phase) {
            Some(PulsePhase::Pending(created)) => {
                now.saturating_duration_since(*created) <= MAX_PRESS_AGE
            }
            Some(PulsePhase::Cooling(_) | PulsePhase::Releasing) => false,
            _ => true,
        }
    }
    pub fn reset(&mut self, source: &SourceSnapshot) {
        self.seen = source.presses;
        self.pulses.clear();
    }
    pub fn started(&mut self, id: usize, now: Instant) {
        if let Some(pulse) = self.pulses.get_mut(&id) {
            pulse.phase = PulsePhase::Pressing(now + PRESS_TIME);
        }
    }
    pub fn reject(&mut self, id: usize, now: Instant) {
        if let Some(pulse) = self.pulses.get_mut(&id) {
            pulse.phase = PulsePhase::Cooling(now + RELEASE_TIME);
        }
    }
    pub fn released(&mut self, id: usize, now: Instant) {
        if let Some(pulse) = self.pulses.get_mut(&id) {
            pulse.phase = PulsePhase::Cooling(now + RELEASE_TIME);
        }
    }
    pub fn actions(
        &mut self,
        settings: &ActiveSettings,
        source: &SourceSnapshot,
        now: Instant,
    ) -> Vec<Action> {
        self.pulses.retain(|_, pulse| match pulse.phase {
            PulsePhase::Pending(created) => now.saturating_duration_since(created) <= MAX_PRESS_AGE,
            PulsePhase::Pressing(until) if now >= until => {
                pulse.phase = PulsePhase::Releasing;
                true
            }
            PulsePhase::Cooling(until) => now < until,
            _ => true,
        });
        let mut result = Vec::new();
        let direction =
            |key: u32, opposite: u32| source.down[key as usize] && !source.down[opposite as usize];
        for (id, binding) in settings.bindings.iter().enumerate() {
            let Some(key) = binding.key.filter(|key| *key < 256) else {
                continue;
            };
            if settings.movement_mode == MovementMode::Stick && wasd(key) {
                continue;
            }
            let Some(position) = binding.position.clone() else {
                continue;
            };
            let opposed = match key {
                0x57 => source.down[0x53],
                0x53 => source.down[0x57],
                0x41 => source.down[0x44],
                0x44 => source.down[0x41],
                _ => false,
            };
            if opposed {
                if let Some(pulse) = self.pulses.get_mut(&id) {
                    match pulse.phase {
                        PulsePhase::Pending(_) => {
                            pulse.phase = PulsePhase::Cooling(now + RELEASE_TIME)
                        }
                        PulsePhase::Pressing(_) => pulse.phase = PulsePhase::Releasing,
                        _ => {}
                    }
                }
                continue;
            }
            let allowed = match key {
                0x57 => direction(key, 0x53),
                0x53 => direction(key, 0x57),
                0x41 => direction(key, 0x44),
                0x44 => direction(key, 0x41),
                _ => source.down[key as usize],
            };
            let action = Action {
                id,
                position,
                start: None,
                right: binding.right,
                pulse: binding.behavior == PressBehavior::Click,
            };
            if !action.pulse {
                if allowed {
                    result.push(action);
                }
            } else if let Some(created) = source.pressed_at[key as usize].filter(|at| {
                source.presses[key as usize] != self.seen[key as usize]
                    && !self.pulses.contains_key(&id)
                    && now.saturating_duration_since(*at) <= MAX_PRESS_AGE
            }) {
                self.pulses.insert(
                    id,
                    Pulse {
                        action,
                        phase: PulsePhase::Pending(created),
                        order: source.press_order[key as usize],
                    },
                );
            }
        }
        if settings.movement_mode == MovementMode::Stick {
            if let Some(center) = settings.stick.center.as_ref() {
                let x = i32::from(source.down[0x44]) - i32::from(source.down[0x41]);
                let y = i32::from(source.down[0x53]) - i32::from(source.down[0x57]);
                if x != 0 || y != 0 {
                    let length = ((x * x + y * y) as f64).sqrt();
                    let radius = settings.stick.radius_px as f64;
                    let mut position = center.clone();
                    // Truncation keeps the quantized diagonal within the radius.
                    position.x += (x as f64 * radius / length) as i32;
                    position.y += (y as f64 * radius / length) as i32;
                    result.insert(
                        0,
                        Action {
                            id: STICK,
                            position,
                            start: Some(center.clone()),
                            right: false,
                            pulse: false,
                        },
                    );
                }
            }
        }
        let mut pulses: Vec<_> = self
            .pulses
            .values()
            .filter(|pulse| {
                matches!(
                    pulse.phase,
                    PulsePhase::Pending(_) | PulsePhase::Pressing(_)
                )
            })
            .collect();
        pulses.sort_unstable_by_key(|pulse| pulse.order);
        result.extend(pulses.into_iter().map(|pulse| pulse.action.clone()));
        self.seen = source.presses;
        result
    }
}

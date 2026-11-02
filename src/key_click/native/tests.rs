use super::controls::{SHIFT, STICK};
use super::injection::*;
use super::*;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Condvar, mpsc},
};
use windows_sys::Win32::UI::Input::Pointer::*;

#[derive(Clone, Debug, PartialEq, Eq)]
struct MousePacket {
    flags: u32,
    point: Option<Pixel>,
    tag: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct TouchPacket {
    id: u32,
    point: Pixel,
    flags: u32,
}
#[derive(Clone, Debug)]
enum Event {
    Mouse(MousePacket),
    Touch(Vec<TouchPacket>),
}
enum MouseFault {
    Partial(usize),
    PanicAfter(usize),
}
enum TouchFault {
    Fail,
    Retry,
    PanicAfter,
}
struct FakeSink {
    desktop: Desktop,
    state: MouseState,
    events: Vec<Event>,
    touch_attempts: Vec<Vec<TouchPacket>>,
    mouse_faults: VecDeque<MouseFault>,
    touch_faults: VecDeque<TouchFault>,
    initialize_failure: bool,
    initialize_calls: usize,
    contacts: BTreeMap<u32, (Pixel, bool)>,
}
impl Default for FakeSink {
    fn default() -> Self {
        Self {
            desktop: Desktop {
                left: -2000,
                top: -100,
                width: 4000,
                height: 2000,
            },
            state: MouseState {
                position: Pixel { x: -100, y: 40 },
                physical: PhysicalMouse::default(),
            },
            events: Vec::new(),
            touch_attempts: Vec::new(),
            mouse_faults: VecDeque::new(),
            touch_faults: VecDeque::new(),
            initialize_failure: false,
            initialize_calls: 0,
            contacts: BTreeMap::new(),
        }
    }
}
impl FakeSink {
    fn apply_mouse(&mut self, packet: &INPUT) {
        assert_eq!(packet.r#type, INPUT_MOUSE);
        let packet = unsafe { packet.Anonymous.mi };
        assert_eq!(packet.mouseData, 0);
        assert_eq!(packet.time, 0);
        let point = if packet.dwFlags & MOUSEEVENTF_MOVE != 0 {
            assert_eq!(
                packet.dwFlags & (MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK),
                MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK
            );
            let point = Pixel {
                x: self.desktop.left
                    + (packet.dx as i64 * self.desktop.width as i64 / 65536) as i32,
                y: self.desktop.top
                    + (packet.dy as i64 * self.desktop.height as i64 / 65536) as i32,
            };
            self.state.position = point;
            Some(point)
        } else {
            None
        };
        self.events.push(Event::Mouse(MousePacket {
            flags: packet.dwFlags,
            point,
            tag: packet.dwExtraInfo,
        }));
    }
    fn mouse_packets(&self) -> Vec<&MousePacket> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Mouse(packet) => Some(packet),
                _ => None,
            })
            .collect()
    }
    fn touch_frames(&self) -> Vec<&Vec<TouchPacket>> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Touch(frame) => Some(frame),
                _ => None,
            })
            .collect()
    }
    fn downs(&self) -> usize {
        self.mouse_packets()
            .iter()
            .filter(|packet| packet.flags & (MOUSEEVENTF_LEFTDOWN | MOUSEEVENTF_RIGHTDOWN) != 0)
            .count()
            + self
                .touch_frames()
                .iter()
                .flat_map(|frame| frame.iter())
                .filter(|packet| packet.flags & POINTER_FLAG_DOWN != 0)
                .count()
    }
    fn ups(&self) -> usize {
        self.mouse_packets()
            .iter()
            .filter(|packet| packet.flags & (MOUSEEVENTF_LEFTUP | MOUSEEVENTF_RIGHTUP) != 0)
            .count()
            + self
                .touch_frames()
                .iter()
                .flat_map(|frame| frame.iter())
                .filter(|packet| packet.flags & POINTER_FLAG_UP != 0)
                .count()
    }
}
impl Sink for FakeSink {
    fn desktop(&mut self) -> Result<Desktop, ClickerError> {
        Ok(self.desktop)
    }
    fn mouse_state(&mut self) -> Result<MouseState, ClickerError> {
        Ok(self.state)
    }
    fn mouse(&mut self, packets: &[INPUT]) -> Result<(), MouseFailure> {
        let fault = self.mouse_faults.pop_front();
        let inserted = match fault {
            Some(MouseFault::Partial(n) | MouseFault::PanicAfter(n)) => n.min(packets.len()),
            None => packets.len(),
        };
        for packet in &packets[..inserted] {
            self.apply_mouse(packet);
        }
        match fault {
            Some(MouseFault::PanicAfter(_)) => panic!("mouse backend fixture"),
            Some(MouseFault::Partial(_)) => Err(MouseFailure {
                inserted,
                error: ErrorCode::InputSend.into(),
            }),
            None => Ok(()),
        }
    }
    fn initialize_touch(&mut self) -> Result<(), ClickerError> {
        self.initialize_calls += 1;
        if self.initialize_failure {
            Err(ErrorCode::InputInitialize.into())
        } else {
            Ok(())
        }
    }
    fn touch(&mut self, packets: &[POINTER_TOUCH_INFO]) -> Result<(), TouchFailure> {
        self.touch_attempts.push(
            packets
                .iter()
                .map(|packet| TouchPacket {
                    id: packet.pointerInfo.pointerId,
                    point: packet.pointerInfo.ptPixelLocation.into(),
                    flags: packet.pointerInfo.pointerFlags,
                })
                .collect(),
        );
        let fault = self.touch_faults.pop_front();
        if matches!(fault, Some(TouchFault::Fail | TouchFault::Retry)) {
            return Err(TouchFailure {
                retry: matches!(fault, Some(TouchFault::Retry)),
                error: ErrorCode::InputSend.into(),
            });
        }
        let mut frame = Vec::new();
        for packet in packets {
            assert_eq!(packet.pointerInfo.pointerType, PT_TOUCH);
            assert_eq!(packet.pointerInfo.dwTime, 0);
            assert_eq!(packet.pointerInfo.PerformanceCount, 0);
            assert_eq!(packet.touchMask, 0);
            let pointer = &packet.pointerInfo;
            let point = Pixel::from(pointer.ptPixelLocation);
            frame.push(TouchPacket {
                id: pointer.pointerId,
                point,
                flags: pointer.pointerFlags,
            });
        }
        for id in self.contacts.keys() {
            assert!(
                frame.iter().any(|packet| packet.id == *id),
                "a contact was omitted from a full frame"
            );
        }
        for packet in &frame {
            if packet.flags & POINTER_FLAG_DOWN != 0 {
                assert_eq!(
                    packet.flags,
                    POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT
                );
                assert!(
                    self.contacts
                        .insert(packet.id, (packet.point, false))
                        .is_none()
                );
            } else if packet.flags & POINTER_FLAG_UPDATE != 0 {
                assert_eq!(
                    packet.flags,
                    POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT
                );
                // An API failure can leave a DOWN's outcome unknown. Cleanup's
                // UPDATE/UP still belongs to that attempted contact ID.
                self.contacts.insert(packet.id, (packet.point, true));
            } else {
                assert_eq!(packet.flags, POINTER_FLAG_UP);
                assert_eq!(
                    self.contacts.remove(&packet.id),
                    Some((packet.point, true)),
                    "UP did not match the last UPDATE"
                );
            }
        }
        self.events.push(Event::Touch(frame));
        if matches!(fault, Some(TouchFault::PanicAfter)) {
            panic!("touch backend fixture");
        }
        Ok(())
    }
}

fn identity() -> GameIdentity {
    GameIdentity {
        executable: "C:/Games/example.exe".into(),
        class: "GameWindow".into(),
        title: "Example".into(),
    }
}
fn point(x: i32, y: i32) -> ClientPoint {
    ClientPoint {
        game: identity(),
        x,
        y,
        width: 1000,
        height: 800,
    }
}
fn settings(method: InputMethod, mode: MovementMode, behavior: PressBehavior) -> KeyClickSettings {
    KeyClickSettings {
        input_method: method,
        movement_mode: mode,
        stick: StickSettings {
            center: Some(point(200, 200)),
            radius_px: 80,
        },
        bindings: [
            b'W' as u32,
            b'A' as u32,
            b'S' as u32,
            b'D' as u32,
            0x20,
            SHIFT,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, key)| KeyBinding {
            key: Some(key),
            position: Some(point(100 + index as i32 * 30, 150)),
            behavior: if index >= 4 {
                PressBehavior::Click
            } else {
                behavior
            },
            ..KeyBinding::default()
        })
        .collect(),
        ..KeyClickSettings::default()
    }
}
struct Harness {
    settings: ActiveSettings,
    source: SourceKeyState,
    runtime: Runtime<FakeSink>,
    start: Instant,
}
impl Harness {
    fn new(method: InputMethod, mode: MovementMode, behavior: PressBehavior) -> Self {
        Self {
            settings: ActiveSettings::from(&settings(method, mode, behavior)),
            source: SourceKeyState::default(),
            runtime: Runtime::new(FakeSink::default()),
            start: Instant::now(),
        }
    }
    fn key(&mut self, key: u32, down: bool, at: u64) -> bool {
        self.source.route(
            &self.settings,
            key,
            down,
            false,
            RouteContext {
                ready: true,
                alt: false,
            },
            self.start + Duration::from_millis(at),
        )
    }
    fn frame(&mut self, at: u64) -> Result<(), ClickerError> {
        self.runtime.frame(
            &self.settings,
            &self.source.snapshot(),
            self.start + Duration::from_millis(at),
            |point| {
                Ok(Pixel {
                    x: point.x,
                    y: point.y,
                })
            },
            || true,
        )
    }
    fn stop(&mut self) -> Result<(), ClickerError> {
        self.source.suspend();
        self.runtime.halt(&self.source.snapshot())
    }
}

#[test]
fn physical_hold_repeat_diagonal_opposites_and_release_reach_touch_frames() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    assert!(h.key(b'W' as u32, true, 0));
    h.frame(0).unwrap();
    for at in 1..30 {
        assert!(h.key(b'W' as u32, true, at));
        h.frame(at).unwrap();
    }
    assert_eq!(h.runtime.sink.downs(), 1);
    assert!(h.key(b'D' as u32, true, 30));
    h.frame(30).unwrap();
    assert_eq!(h.runtime.sink.contacts.len(), 2);
    let w = h.runtime.sink.touch_frames()[0][0].id;
    assert!(h.key(b'D' as u32, false, 40));
    h.frame(40).unwrap();
    h.frame(48).unwrap();
    assert_eq!(h.runtime.sink.contacts.len(), 1);
    assert!(h.runtime.sink.contacts.contains_key(&w));
    h.key(b'S' as u32, true, 50);
    h.frame(50).unwrap();
    assert!(h.runtime.sink.contacts.is_empty());
    h.key(b'S' as u32, false, 60);
    h.frame(60).unwrap();
    assert_eq!(h.runtime.sink.contacts.len(), 1);
    h.key(b'W' as u32, false, 70);
    h.frame(70).unwrap();
    h.frame(78).unwrap();
    assert!(h.runtime.sink.contacts.is_empty());
    let downs = h.runtime.sink.downs();
    h.frame(200).unwrap();
    assert_eq!(h.runtime.sink.downs(), downs);
    assert_eq!(h.runtime.sink.downs(), h.runtime.sink.ups());
}

#[test]
fn stick_starts_at_center_reuses_contact_and_releases_at_the_last_update() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Stick,
        PressBehavior::Click,
    );
    h.key(b'W' as u32, true, 0);
    h.key(b'D' as u32, true, 0);
    h.frame(0).unwrap();
    let first = h.runtime.sink.touch_frames()[0][0].clone();
    assert_eq!(first.id, STICK as u32 + 1);
    assert_eq!(first.point, Pixel { x: 200, y: 200 });
    h.frame(8).unwrap();
    let diagonal = h.runtime.sink.touch_frames()[1][0].clone();
    assert_eq!(diagonal.id, first.id);
    let dx = diagonal.point.x - 200;
    let dy = diagonal.point.y - 200;
    assert!(dx > 0 && dy < 0 && dx * dx + dy * dy <= 80 * 80);
    h.key(b'D' as u32, false, 16);
    h.frame(16).unwrap();
    let up = h.runtime.sink.touch_frames()[2][0].clone();
    assert_eq!(up.point, Pixel { x: 200, y: 120 });
    h.key(b'W' as u32, false, 24);
    h.frame(24).unwrap();
    let release = h.runtime.sink.touch_frames()[3][0].clone();
    assert_eq!(release.id, first.id);
    assert_eq!(release.point, up.point);
    assert_eq!(release.flags, POINTER_FLAG_UP);
    assert_eq!(h.runtime.sink.downs(), 1);
}

#[test]
fn additional_tap_keeps_movement_contact_and_observes_press_and_release_time() {
    let mut h = Harness::new(InputMethod::Touch, MovementMode::Stick, PressBehavior::Hold);
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    h.frame(8).unwrap();
    let movement = STICK as u32 + 1;
    h.key(0x20, true, 10);
    h.frame(10).unwrap();
    h.key(0x20, false, 11);
    h.frame(59).unwrap();
    assert_eq!(h.runtime.sink.contacts.len(), 2);
    h.frame(60).unwrap();
    assert_eq!(h.runtime.sink.contacts.len(), 1);
    assert!(h.runtime.sink.contacts.contains_key(&movement));
    assert_eq!(h.runtime.completed, 1);
    h.key(0x20, true, 70);
    h.frame(70).unwrap();
    h.key(0x20, false, 71);
    h.frame(110).unwrap();
    assert_eq!(
        h.runtime.sink.downs(),
        2,
        "a press during cooldown must not queue a later tap"
    );
    h.key(0x20, true, 111);
    h.frame(111).unwrap();
    assert_eq!(h.runtime.sink.downs(), 3);
    h.stop().unwrap();
    assert!(h.runtime.sink.contacts.is_empty());
}

#[test]
fn fast_taps_coalesce_and_long_holds_never_accumulate_clicks() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    for at in 0..20 {
        h.key(0x20, true, at * 2);
        h.key(0x20, false, at * 2 + 1);
    }
    h.frame(40).unwrap();
    h.frame(48).unwrap();
    h.frame(90).unwrap();
    assert_eq!(h.runtime.sink.downs(), 1);
    h.key(0x20, true, 150);
    h.frame(150).unwrap();
    for at in 151..250 {
        h.key(0x20, true, at);
        h.frame(at).unwrap();
    }
    assert_eq!(h.runtime.sink.downs(), 2);
    h.key(0x20, false, 260);
    h.frame(260).unwrap();
    h.frame(500).unwrap();
    assert_eq!(h.runtime.sink.downs(), 2);
}

#[test]
fn shift_union_runs_an_additional_action_and_preserves_both_os_keys() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    h.settings.bindings[5].behavior = PressBehavior::Hold;
    assert!(!h.key(0xa0, true, 0));
    h.frame(0).unwrap();
    assert!(h.key(b'W' as u32, true, 1));
    h.frame(1).unwrap();
    assert_eq!(h.runtime.sink.contacts.len(), 2);
    assert!(!h.key(0xa1, true, 2));
    h.frame(2).unwrap();
    assert!(!h.key(0xa0, false, 3));
    h.frame(3).unwrap();
    assert_eq!(h.runtime.sink.contacts.len(), 2);
    assert!(!h.key(0xa1, false, 4));
    h.frame(4).unwrap();
    assert_eq!(h.runtime.sink.contacts.len(), 1);
    assert_eq!(h.runtime.sink.downs(), 2);
    h.stop().unwrap();
}

#[test]
fn ctrl_alt_and_win_shortcuts_pass_through_and_disarm_already_held_keys() {
    for modifier in [0x11, 0xa2, 0xa3, 0x12, 0xa4, 0xa5, 0x5b, 0x5c] {
        let mut h = Harness::new(
            InputMethod::Touch,
            MovementMode::Buttons,
            PressBehavior::Hold,
        );
        assert!(!h.key(modifier, true, 0));
        assert!(!h.key(b'W' as u32, true, 1));
        h.frame(1).unwrap();
        assert!(!h.key(b'W' as u32, false, 2));
        assert!(!h.key(modifier, false, 3));
        h.frame(3).unwrap();
        assert_eq!(h.runtime.sink.downs(), 0);
        h.key(b'W' as u32, true, 4);
        h.frame(4).unwrap();
        h.key(modifier, true, 5);
        h.frame(5).unwrap();
        assert!(h.runtime.sink.contacts.is_empty());
        h.key(modifier, false, 6);
        h.frame(6).unwrap();
        h.key(b'W' as u32, true, 7);
        h.frame(7).unwrap();
        assert_eq!(h.runtime.sink.downs(), 1);
        assert!(h.key(b'W' as u32, false, 8));
        h.key(b'W' as u32, true, 9);
        h.frame(9).unwrap();
        assert_eq!(h.runtime.sink.downs(), 2);
    }
}

#[test]
fn shortcut_repeat_after_a_consumed_key_reaches_the_os_with_a_matching_up() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    assert!(h.key(b'W' as u32, true, 0));
    h.frame(0).unwrap();
    assert!(!h.key(0x11, true, 1));
    h.frame(1).unwrap();
    assert!(!h.key(b'W' as u32, true, 2));
    assert!(!h.key(b'W' as u32, false, 3));
    assert!(!h.key(0x11, false, 4));
    h.frame(4).unwrap();
    assert_eq!(h.runtime.sink.downs(), 1);
    assert!(h.runtime.sink.contacts.is_empty());
}

#[test]
fn both_button_axes_cancel_and_a_pending_opposite_tap_is_not_replayed() {
    for behavior in [PressBehavior::Hold, PressBehavior::Click] {
        let mut h = Harness::new(InputMethod::Touch, MovementMode::Buttons, behavior);
        h.key(b'A' as u32, true, 0);
        h.frame(0).unwrap();
        h.frame(8).unwrap();
        h.key(b'D' as u32, true, 9);
        h.frame(9).unwrap();
        assert!(h.runtime.sink.contacts.is_empty());
        h.key(b'A' as u32, false, 10);
        h.key(b'D' as u32, false, 10);
        h.frame(100).unwrap();
        assert_eq!(h.runtime.sink.downs(), 1);
    }
}

#[test]
fn focus_loss_and_reseed_do_not_replay_held_or_injected_input() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    h.source.seed_down([b'W' as u32]);
    assert!(!h.key(b'W' as u32, true, 0));
    h.frame(0).unwrap();
    assert_eq!(h.runtime.sink.downs(), 0);
    h.key(b'W' as u32, false, 1);
    h.key(b'W' as u32, true, 2);
    h.frame(2).unwrap();
    h.runtime
        .frame(
            &h.settings,
            &h.source.snapshot(),
            h.start + Duration::from_millis(3),
            |_| panic!("unfocused resolver"),
            || false,
        )
        .unwrap();
    h.source.seed_down([]); // Suppressed W remains physical-down until the hook UP.
    assert!(h.key(b'W' as u32, true, 4));
    h.frame(4).unwrap();
    assert_eq!(h.runtime.sink.downs(), 1);
    assert!(
        !h.source
            .event(b'D' as u32, true, true, true, false, h.start)
    );
    h.frame(5).unwrap();
    assert_eq!(h.runtime.sink.downs(), 1);
    h.key(b'W' as u32, false, 6);
    h.key(b'W' as u32, true, 7);
    h.frame(7).unwrap();
    assert_eq!(h.runtime.sink.downs(), 2);
}

#[test]
fn mouse_preserves_its_first_hold_and_warns_when_other_positions_or_taps_conflict() {
    let mut h = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    h.key(b'D' as u32, true, 1);
    h.key(0x20, true, 1);
    h.frame(1).unwrap();
    assert_eq!(h.runtime.sink.downs(), 1);
    assert_eq!(h.runtime.sink.ups(), 0);
    assert_eq!(h.runtime.sink.state.position, Pixel { x: 100, y: 150 });
    assert_eq!(
        h.runtime.warning.as_ref().unwrap().code,
        ErrorCode::InputCombination
    );
    h.key(0x20, false, 2);
    h.key(b'D' as u32, false, 2);
    h.frame(100).unwrap();
    assert_eq!(
        h.runtime.sink.downs(),
        1,
        "a rejected additional tap must never start later"
    );
    h.stop().unwrap();
    assert_eq!(h.runtime.sink.ups(), 1);
}

#[test]
fn legacy_right_click_keeps_fifty_ms_down_and_fifty_ms_release_before_restore() {
    let mut h = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    h.settings.bindings[0].right = true;
    let original = h.runtime.sink.state.position;
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    h.key(b'W' as u32, false, 1);
    h.frame(49).unwrap();
    assert_eq!(h.runtime.sink.ups(), 0);
    h.frame(50).unwrap();
    assert_eq!(h.runtime.sink.ups(), 1);
    assert_ne!(h.runtime.sink.state.position, original);
    h.frame(99).unwrap();
    assert_ne!(h.runtime.sink.state.position, original);
    h.frame(100).unwrap();
    assert_eq!(h.runtime.sink.state.position, original);
    let packets = h.runtime.sink.mouse_packets();
    assert_eq!(packets[1].flags, MOUSEEVENTF_RIGHTDOWN);
    assert_eq!(packets[2].flags, MOUSEEVENTF_RIGHTUP);
    assert!(
        packets
            .iter()
            .all(|packet| packet.tag == crate::engine::KEY_CLICK_TAG)
    );
    assert_eq!(h.runtime.completed, 1);
}

#[test]
fn frame_polling_games_observe_each_mouse_press_and_release_at_the_registered_position() {
    for hz in [20, 30, 60, 120, 240] {
        let period = 1_000_000_000u64 / hz;
        for phase in [0, period / 4, period / 2, period - 1] {
            for right in [false, true] {
                let mut h = Harness::new(
                    InputMethod::Mouse,
                    MovementMode::Buttons,
                    PressBehavior::Click,
                );
                h.settings.bindings[4].right = right;
                let target = Pixel { x: 220, y: 150 };
                let mut position = h.runtime.sink.state.position;
                let mut down = false;
                let mut previous_down = false;
                let mut presses = 0;
                let mut releases = 0;
                let mut next_sample = phase;
                for at in 0..1450u64 {
                    while next_sample < at * 1_000_000 {
                        if position == target {
                            presses += usize::from(down && !previous_down);
                            releases += usize::from(!down && previous_down);
                        }
                        previous_down = down;
                        next_sample += period;
                    }
                    if at < 1320 && at % 110 == 0 {
                        h.key(0x20, true, at);
                    } else if at < 1320 && at % 110 == 1 {
                        h.key(0x20, false, at);
                    }
                    h.frame(at).unwrap();
                    for event in std::mem::take(&mut h.runtime.sink.events) {
                        let Event::Mouse(packet) = event else {
                            panic!("Mouse mode emitted a touch packet");
                        };
                        if let Some(point) = packet.point {
                            position = point;
                        }
                        if packet.flags & (MOUSEEVENTF_LEFTDOWN | MOUSEEVENTF_RIGHTDOWN) != 0 {
                            down = true;
                        }
                        if packet.flags & (MOUSEEVENTF_LEFTUP | MOUSEEVENTF_RIGHTUP) != 0 {
                            down = false;
                        }
                    }
                }
                assert_eq!((presses, releases), (12, 12), "{hz}Hz, phase {phase}");
                assert!(!down);
                assert_eq!(h.runtime.completed, 12);
                assert_eq!(position, Pixel { x: -100, y: 40 });
            }
        }
    }
}

#[test]
fn mouse_stick_drags_from_center_without_repressing_and_respects_opposites() {
    let mut h = Harness::new(
        InputMethod::Mouse,
        MovementMode::Stick,
        PressBehavior::Click,
    );
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    assert_eq!(h.runtime.sink.state.position, Pixel { x: 200, y: 200 });
    h.frame(8).unwrap();
    assert_eq!(h.runtime.sink.state.position, Pixel { x: 200, y: 120 });
    h.key(b'D' as u32, true, 16);
    h.frame(16).unwrap();
    assert_eq!(h.runtime.sink.downs(), 1);
    h.key(b'S' as u32, true, 24);
    h.key(b'A' as u32, true, 24);
    h.frame(24).unwrap();
    assert_eq!(h.runtime.sink.ups(), 1);
}

#[test]
fn physical_mouse_press_is_not_owned_or_released_and_user_movement_is_not_restored() {
    for right in [false, true] {
        let mut h = Harness::new(
            InputMethod::Mouse,
            MovementMode::Buttons,
            PressBehavior::Hold,
        );
        h.runtime.sink.state.physical = PhysicalMouse {
            left: !right,
            right,
            activity: 1,
        };
        h.key(b'W' as u32, true, 0);
        h.frame(0).unwrap();
        h.stop().unwrap();
        assert!(h.runtime.sink.events.is_empty());
    }
    let mut h = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    let moved = Pixel { x: 900, y: 300 };
    h.runtime.sink.state.position = moved;
    h.runtime.sink.state.physical.activity += 1;
    h.stop().unwrap();
    assert_eq!(h.runtime.sink.state.position, moved);
    assert_eq!(h.runtime.sink.ups(), 1);
    assert_eq!(h.runtime.sink.mouse_packets().last().unwrap().point, None);
}

#[test]
fn physical_takeover_during_a_synthetic_hold_never_releases_the_physical_button() {
    let mut h = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    h.runtime.sink.state.physical.left = true;
    h.runtime.sink.state.physical.activity += 1;
    h.stop().unwrap();
    assert_eq!(h.runtime.sink.ups(), 0);
    h.stop().unwrap();
    assert_eq!(h.runtime.sink.ups(), 0);
}

#[test]
fn mouse_motion_disarms_the_current_control_instead_of_fighting_the_user() {
    let mut h = Harness::new(InputMethod::Mouse, MovementMode::Stick, PressBehavior::Hold);
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    h.frame(8).unwrap();
    let moved = Pixel { x: 600, y: 600 };
    h.runtime.sink.state.position = moved;
    h.runtime.sink.state.physical.activity += 1;
    h.frame(16).unwrap();
    h.frame(24).unwrap();
    assert_eq!(h.runtime.sink.state.position, moved);
    assert_eq!(h.runtime.sink.downs(), 1);
    h.key(b'W' as u32, false, 25);
    h.frame(25).unwrap();
    h.key(b'W' as u32, true, 26);
    h.frame(26).unwrap();
    assert_eq!(h.runtime.sink.downs(), 2);
}

#[test]
fn one_pixel_quantization_is_not_a_physical_takeover_and_restores_after_release() {
    let mut h = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    h.runtime.sink.state.position.x += 1;
    h.runtime.sink.state.position.y -= 1;
    h.frame(8).unwrap();
    assert_eq!(h.runtime.sink.downs(), 1);
    assert_eq!(h.runtime.sink.ups(), 0);
    assert!(h.runtime.warning.is_none());
    h.key(b'W' as u32, false, 9);
    h.frame(9).unwrap();
    assert_eq!(h.runtime.sink.ups(), 1);
    assert_eq!(h.runtime.sink.state.position, Pixel { x: -100, y: 40 });
}

#[test]
fn ordinary_mouse_clicks_wait_in_order_but_expired_and_cancelled_taps_do_not_replay() {
    let mut h = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    h.key(b'W' as u32, true, 0);
    h.key(0x20, true, 0);
    for at in [0, 8, 50, 99] {
        h.frame(at).unwrap();
    }
    assert_eq!(h.runtime.sink.downs(), 1);
    h.frame(100).unwrap();
    assert_eq!(h.runtime.sink.downs(), 2);
    assert!(h.runtime.warning.is_none());
    h.frame(150).unwrap();
    h.frame(200).unwrap();
    assert_eq!(h.runtime.completed, 2);
    let pressed: Vec<_> = h
        .runtime
        .sink
        .mouse_packets()
        .windows(2)
        .filter(|pair| pair[1].flags & MOUSEEVENTF_LEFTDOWN != 0)
        .map(|pair| pair[0].point.unwrap())
        .collect();
    assert_eq!(
        pressed,
        [Pixel { x: 100, y: 150 }, Pixel { x: 220, y: 150 }]
    );

    let mut stopped = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    stopped.key(b'W' as u32, true, 0);
    stopped.key(0x20, true, 0);
    stopped.frame(0).unwrap();
    stopped.stop().unwrap();
    stopped.frame(500).unwrap();
    assert_eq!(stopped.runtime.sink.downs(), 1);

    let mut expired = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    expired.key(b'W' as u32, true, 0);
    expired.key(0x20, true, 0);
    expired.frame(0).unwrap();
    expired.frame(300).unwrap();
    expired.frame(350).unwrap();
    assert_eq!(expired.runtime.sink.downs(), 1);
}

#[test]
fn registered_shift_hotkeys_take_priority_without_disabling_other_shift_movement() {
    for method in [InputMethod::Mouse, InputMethod::Touch] {
        for mode in [MovementMode::Buttons, MovementMode::Stick] {
            let mut h = Harness::new(method, mode, PressBehavior::Hold);
            h.settings
                .bindings
                .retain(|binding| binding.key != Some(SHIFT));
            h.settings.operation_hotkeys = vec![(MOD_SHIFT, b'W' as u32), (MOD_SHIFT, 0x20)];
            assert!(!h.key(0xa0, true, 0));
            h.frame(0).unwrap();
            let before = h.runtime.sink.downs();
            assert!(!h.key(b'W' as u32, true, 1));
            assert!(!h.key(b'W' as u32, true, 2));
            h.frame(2).unwrap();
            assert_eq!(h.runtime.sink.downs(), before);
            assert!(!h.key(b'W' as u32, false, 3));
            assert!(!h.key(0x20, true, 4));
            assert!(!h.key(0x20, false, 5));
            h.frame(5).unwrap();
            assert_eq!(h.runtime.sink.downs(), before);
            assert!(h.key(b'D' as u32, true, 6));
            h.frame(6).unwrap();
            assert_eq!(h.runtime.sink.downs(), before + 1);
            h.key(b'D' as u32, false, 7);
            h.frame(7).unwrap();
            h.frame(8).unwrap();
            h.settings.operation_hotkeys.clear();
            assert!(h.key(b'W' as u32, true, 9));
            h.frame(9).unwrap();
            assert_eq!(h.runtime.sink.downs(), before + 2);
            h.stop().unwrap();
        }
    }
}

#[test]
fn no_owned_input_means_stop_sends_no_mouse_or_touch_up() {
    for method in [InputMethod::Mouse, InputMethod::Touch] {
        let mut h = Harness::new(method, MovementMode::Buttons, PressBehavior::Hold);
        h.stop().unwrap();
        h.stop().unwrap();
        assert!(h.runtime.sink.events.is_empty());
    }
}

#[test]
fn touch_right_action_warns_without_releasing_movement_or_changing_saved_mouse_data() {
    let mut h = Harness::new(InputMethod::Touch, MovementMode::Stick, PressBehavior::Hold);
    h.settings.bindings[4].right = true;
    let saved = h.settings.clone();
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    h.frame(8).unwrap();
    h.key(0x20, true, 9);
    h.frame(9).unwrap();
    h.frame(100).unwrap();
    assert_eq!(h.runtime.sink.downs(), 1);
    assert_eq!(h.runtime.sink.ups(), 0);
    assert_eq!(
        h.runtime.warning.as_ref().unwrap().code,
        ErrorCode::InputRightButton
    );
    assert_eq!(h.settings, saved);
}

#[test]
fn rejected_additional_actions_are_never_resolved_or_allowed_to_interrupt_movement() {
    for method in [InputMethod::Touch, InputMethod::Mouse] {
        let mut h = Harness::new(method, MovementMode::Stick, PressBehavior::Hold);
        h.settings.bindings[4].right = true;
        h.key(b'W' as u32, true, 0);
        h.frame(0).unwrap();
        h.frame(8).unwrap();
        h.key(0x20, true, 9);
        h.runtime
            .frame(
                &h.settings,
                &h.source.snapshot(),
                h.start + Duration::from_millis(9),
                |point| {
                    assert_ne!(
                        point.x, 220,
                        "an unsupported additional action reached target resolution"
                    );
                    Ok(Pixel {
                        x: point.x,
                        y: point.y,
                    })
                },
                || true,
            )
            .unwrap();
        assert_eq!(h.runtime.sink.downs(), 1);
        assert_eq!(h.runtime.sink.ups(), 0);
        assert_eq!(
            h.runtime.warning.as_ref().unwrap().code,
            if method == InputMethod::Touch {
                ErrorCode::InputRightButton
            } else {
                ErrorCode::InputCombination
            }
        );
        h.stop().unwrap();
    }
}

#[test]
fn missing_touch_initialization_does_not_attempt_contacts_or_cleanup_unowned_input() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    h.runtime.sink.initialize_failure = true;
    h.key(b'W' as u32, true, 0);
    assert_eq!(h.frame(0).unwrap_err().code, ErrorCode::InputInitialize);
    h.stop().unwrap();
    assert!(h.runtime.sink.events.is_empty());
    assert_eq!(h.runtime.sink.initialize_calls, 1);
}

#[test]
fn partial_mouse_batch_releases_only_a_down_that_could_have_been_inserted() {
    for inserted in 0..=2 {
        let mut h = Harness::new(
            InputMethod::Mouse,
            MovementMode::Buttons,
            PressBehavior::Hold,
        );
        h.runtime
            .sink
            .mouse_faults
            .push_back(MouseFault::Partial(inserted));
        h.key(b'W' as u32, true, 0);
        assert_eq!(h.frame(0).unwrap_err().code, ErrorCode::InputSend);
        assert_eq!(h.runtime.sink.ups(), usize::from(inserted == 2));
        let count = h.runtime.sink.events.len();
        h.stop().unwrap();
        assert_eq!(h.runtime.sink.events.len(), count);
    }
}

#[test]
fn backend_panics_and_resolution_failures_cleanup_started_mouse_and_touch() {
    let mut mouse = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    mouse
        .runtime
        .sink
        .mouse_faults
        .push_back(MouseFault::PanicAfter(2));
    mouse.key(b'W' as u32, true, 0);
    assert_eq!(mouse.frame(0).unwrap_err().code, ErrorCode::WorkerPanic);
    assert_eq!(mouse.runtime.sink.ups(), 1);
    let mut touch = Harness::new(InputMethod::Touch, MovementMode::Stick, PressBehavior::Hold);
    touch
        .runtime
        .sink
        .touch_faults
        .push_back(TouchFault::PanicAfter);
    touch.key(b'W' as u32, true, 0);
    assert_eq!(touch.frame(0).unwrap_err().code, ErrorCode::WorkerPanic);
    assert_eq!(touch.runtime.sink.ups(), 1);
    assert!(touch.runtime.sink.contacts.is_empty());
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    let result = h.runtime.frame(
        &h.settings,
        &h.source.snapshot(),
        h.start + Duration::from_millis(8),
        |_| Err(ErrorCode::ScreenChanged.into()),
        || true,
    );
    assert_eq!(result.unwrap_err().code, ErrorCode::ScreenChanged);
    assert!(h.runtime.sink.contacts.is_empty());
}

#[test]
fn touch_retry_keeps_last_successful_position_and_starts_pulse_time_at_actual_down() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    h.runtime
        .sink
        .touch_faults
        .extend([TouchFault::Retry, TouchFault::Retry, TouchFault::Retry]);
    h.key(0x20, true, 0);
    h.frame(0).unwrap();
    assert_eq!(h.runtime.sink.downs(), 0);
    h.frame(20).unwrap();
    h.frame(28).unwrap();
    h.frame(69).unwrap();
    assert_eq!(h.runtime.sink.ups(), 0);
    h.frame(70).unwrap();
    assert_eq!(h.runtime.sink.ups(), 1);
    let mut stick = Harness::new(InputMethod::Touch, MovementMode::Stick, PressBehavior::Hold);
    stick.key(b'W' as u32, true, 0);
    stick.frame(0).unwrap();
    stick.frame(8).unwrap();
    let last = stick.runtime.sink.contacts[&(STICK as u32 + 1)].0;
    stick.runtime.sink.touch_faults.push_back(TouchFault::Retry);
    stick.key(b'D' as u32, true, 9);
    stick.frame(9).unwrap();
    let attempts = &stick.runtime.sink.touch_attempts;
    assert_eq!(attempts[attempts.len() - 2], attempts[attempts.len() - 1]);
    let last_update = stick.runtime.sink.contacts[&(STICK as u32 + 1)].0;
    assert_ne!(last_update, last);
    stick.key(b'W' as u32, false, 10);
    stick.key(b'D' as u32, false, 10);
    stick.frame(10).unwrap();
    assert_eq!(
        stick.runtime.sink.touch_frames().last().unwrap()[0].point,
        last_update
    );
}

#[test]
fn a_released_hold_after_a_not_ready_down_never_sends_up_or_a_deferred_down() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    h.runtime
        .sink
        .touch_faults
        .extend([TouchFault::Retry, TouchFault::Retry, TouchFault::Retry]);
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    h.key(b'W' as u32, false, 1);
    h.frame(8).unwrap();
    h.stop().unwrap();
    assert!(h.runtime.sink.events.is_empty());
}

#[test]
fn a_repeatedly_rejected_live_frame_stops_at_the_last_successful_touch_position() {
    let mut h = Harness::new(InputMethod::Touch, MovementMode::Stick, PressBehavior::Hold);
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    h.frame(8).unwrap();
    let last = h.runtime.sink.contacts[&(STICK as u32 + 1)].0;
    h.runtime
        .sink
        .touch_faults
        .extend([TouchFault::Retry, TouchFault::Retry, TouchFault::Retry]);
    h.key(b'D' as u32, true, 9);
    assert_eq!(h.frame(9).unwrap_err().code, ErrorCode::InputSend);
    assert!(h.runtime.sink.contacts.is_empty());
    let attempts = &h.runtime.sink.touch_attempts[2..5];
    assert!(attempts.windows(2).all(|pair| pair[0] == pair[1]));
    let up = &h.runtime.sink.touch_frames().last().unwrap()[0];
    assert_eq!(up.flags, POINTER_FLAG_UP);
    assert_eq!(up.point, last);
}

#[test]
fn stale_taps_are_dropped_and_display_changes_release_active_contacts() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    h.key(0x20, true, 0);
    h.key(0x20, false, 1);
    h.frame(300).unwrap();
    assert_eq!(h.runtime.sink.downs(), 0);
    h.key(0x20, true, 301);
    h.frame(301).unwrap();
    h.runtime.sink.desktop.width += 200;
    assert_eq!(h.frame(310).unwrap_err().code, ErrorCode::ScreenChanged);
    assert!(h.runtime.sink.contacts.is_empty());
}

#[test]
fn touch_send_failure_and_failed_release_are_cleaned_up_before_stop_returns() {
    let mut h = Harness::new(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    h.key(b'W' as u32, true, 0);
    h.frame(0).unwrap();
    h.frame(8).unwrap();
    h.runtime.sink.touch_faults.push_back(TouchFault::Fail);
    assert_eq!(h.frame(16).unwrap_err().code, ErrorCode::InputSend);
    assert!(h.runtime.sink.contacts.is_empty());
    let mut mouse = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    mouse.key(b'W' as u32, true, 0);
    mouse.frame(0).unwrap();
    mouse
        .runtime
        .sink
        .mouse_faults
        .push_back(MouseFault::Partial(0));
    let _ = mouse.stop();
    assert_eq!(mouse.runtime.sink.ups(), 1);
    let count = mouse.runtime.sink.events.len();
    mouse.stop().unwrap();
    assert_eq!(mouse.runtime.sink.events.len(), count);
}

#[test]
fn labels_and_profile_book_are_not_part_of_the_active_input_snapshot() {
    let mut settings = settings(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    );
    let initial = ActiveSettings::from(&settings);
    settings.bindings[0].label = "Up".into();
    settings.enabled = true;
    settings.selected_profile = Some(9);
    settings.profiles.push(GameProfile {
        id: 9,
        name: "Example".into(),
        last_target: settings.last_target.clone(),
        bindings: settings.bindings.clone(),
        movement_mode: settings.movement_mode,
        input_method: settings.input_method,
        stick: settings.stick.clone(),
    });
    assert_eq!(ActiveSettings::from(&settings), initial);
    settings.bindings[0].right = true;
    assert_ne!(ActiveSettings::from(&settings), initial);
}

#[test]
fn modern_controls_predicate_uses_only_the_features_that_opt_into_shift() {
    let mut legacy = settings(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    legacy.bindings.retain(|binding| binding.key != Some(SHIFT));
    assert!(!ActiveSettings::from(&legacy).has_modern_controls());
    for variant in 0..5 {
        let mut settings = legacy.clone();
        match variant {
            0 => settings.input_method = InputMethod::Touch,
            1 => settings.movement_mode = MovementMode::Stick,
            2 => settings.bindings[0].behavior = PressBehavior::Hold,
            3 => settings.bindings.push(KeyBinding {
                key: Some(SHIFT),
                ..KeyBinding::default()
            }),
            _ => settings.bindings.push(KeyBinding {
                key: Some(0xa1),
                ..KeyBinding::default()
            }),
        }
        assert!(ActiveSettings::from(&settings).has_modern_controls());
    }
    legacy.bindings[0].label = "Movement".into();
    legacy.enabled = true;
    assert!(!ActiveSettings::from(&legacy).has_modern_controls());
}

#[test]
fn router_preserves_legacy_shift_shortcuts_and_allows_shift_with_modern_controls() {
    let mut legacy = settings(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    legacy.bindings.retain(|binding| binding.key != Some(SHIFT));
    let mut h = Harness::new(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Click,
    );
    h.settings = ActiveSettings::from(&legacy);
    assert!(!h.key(0xa0, true, 0));
    assert!(!h.key(b'W' as u32, true, 1));
    h.frame(1).unwrap();
    assert!(!h.key(b'W' as u32, false, 2));
    assert!(!h.key(0xa0, false, 3));
    h.frame(3).unwrap();
    assert_eq!(h.runtime.sink.downs(), 0);
    assert!(h.key(b'W' as u32, true, 4));
    h.frame(4).unwrap();
    assert!(!h.key(0xa1, true, 5));
    h.frame(5).unwrap();
    h.frame(53).unwrap();
    assert_eq!(h.runtime.sink.ups(), 0);
    h.frame(54).unwrap();
    assert_eq!(h.runtime.sink.ups(), 1);
    for variant in 0..4 {
        let mut settings = legacy.clone();
        match variant {
            0 => settings.input_method = InputMethod::Touch,
            1 => settings.movement_mode = MovementMode::Stick,
            2 => settings.bindings[0].behavior = PressBehavior::Hold,
            _ => settings.bindings.push(KeyBinding {
                key: Some(SHIFT),
                ..KeyBinding::default()
            }),
        }
        let mut h = Harness::new(
            InputMethod::Mouse,
            MovementMode::Buttons,
            PressBehavior::Click,
        );
        h.settings = ActiveSettings::from(&settings);
        assert!(!h.key(0xa0, true, 0));
        assert!(h.key(b'W' as u32, true, 1));
        h.frame(1).unwrap();
        assert_eq!(h.runtime.sink.downs(), 1);
        assert!(!h.key(0xa0, false, 2));
        h.stop().unwrap();
    }
}

#[test]
fn seeded_generic_modifier_aliases_do_not_stick_after_the_side_key_is_released() {
    for (generic, side) in [(0x11, 0xa2), (0x12, 0xa5)] {
        let mut h = Harness::new(
            InputMethod::Touch,
            MovementMode::Buttons,
            PressBehavior::Hold,
        );
        h.source.seed_down([generic, side]);
        assert!(!h.key(b'W' as u32, true, 0));
        assert!(!h.key(b'W' as u32, false, 1));
        assert!(!h.key(side, false, 2));
        assert!(h.key(b'W' as u32, true, 3));
        h.frame(3).unwrap();
        assert_eq!(h.runtime.sink.downs(), 1);
    }
}

#[test]
fn target_point_rejection_prevents_new_down_and_releases_only_the_last_owned_contact() {
    let mut h = Harness::new(InputMethod::Touch, MovementMode::Stick, PressBehavior::Hold);
    h.key(b'W' as u32, true, 0);
    let result = h.runtime.frame(
        &h.settings,
        &h.source.snapshot(),
        h.start,
        |_| Err(ErrorCode::PointOutside.into()),
        || true,
    );
    assert_eq!(result.unwrap_err().code, ErrorCode::PointOutside);
    assert!(h.runtime.sink.events.is_empty());
    h.key(b'W' as u32, false, 1);
    h.key(b'W' as u32, true, 2);
    h.frame(2).unwrap();
    h.frame(10).unwrap();
    let last = h.runtime.sink.contacts[&(STICK as u32 + 1)].0;
    let result = h.runtime.frame(
        &h.settings,
        &h.source.snapshot(),
        h.start + Duration::from_millis(11),
        |_| Err(ErrorCode::PointOutside.into()),
        || true,
    );
    assert_eq!(result.unwrap_err().code, ErrorCode::PointOutside);
    let frame = h.runtime.sink.touch_frames().last().unwrap().to_vec();
    assert_eq!(frame.len(), 1);
    assert_eq!(frame[0].point, last);
    assert_eq!(frame[0].flags, POINTER_FLAG_UP);
    assert_eq!(h.runtime.sink.downs(), 1);
}

#[test]
fn focus_loss_during_resolution_disarms_the_live_source_before_focus_returns() {
    let shared = Shared::new(
        FakeSink::default(),
        Arc::new(Mutex::new(PhysicalMouse::default())),
    );
    let active = ActiveSettings::from(&settings(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    ));
    let snapshot = Arc::new(Snapshot {
        source: active.clone(),
        settings: active,
        target: None,
        size: Some((1000, 800)),
    });
    *shared.snapshot.write().unwrap() = snapshot.clone();
    shared.active.store(true, Ordering::Release);
    shared.desired.store(true, Ordering::Release);
    shared
        .source
        .lock()
        .unwrap()
        .event(b'W' as u32, true, false, true, false, Instant::now());
    let foreground = AtomicBool::new(true);
    shared.process(
        &snapshot,
        Instant::now(),
        || foreground.load(Ordering::Acquire),
        |point| {
            foreground.store(false, Ordering::Release);
            Ok(Pixel {
                x: point.x,
                y: point.y,
            })
        },
    );
    foreground.store(true, Ordering::Release);
    shared.process(
        &snapshot,
        Instant::now(),
        || true,
        |_| panic!("held key was replayed after focus returned"),
    );
    assert!(shared.output.lock().unwrap().sink.events.is_empty());
    assert!(shared.error.lock().unwrap().is_none());
}

struct GateSink {
    fake: FakeSink,
    gate: Arc<(Mutex<(bool, bool)>, Condvar)>,
}

#[test]
fn transient_target_rejection_releases_then_requires_a_fresh_press_without_a_toggle() {
    for code in [
        ErrorCode::KeyTarget,
        ErrorCode::PointOutside,
        ErrorCode::ScreenChanged,
    ] {
        let shared = Shared::new(
            FakeSink::default(),
            Arc::new(Mutex::new(PhysicalMouse::default())),
        );
        let active = ActiveSettings::from(&settings(
            InputMethod::Touch,
            MovementMode::Buttons,
            PressBehavior::Hold,
        ));
        let snapshot = Arc::new(Snapshot {
            source: active.clone(),
            settings: active,
            target: None,
            size: Some((1000, 800)),
        });
        *shared.snapshot.write().unwrap() = snapshot.clone();
        shared.active.store(true, Ordering::Release);
        shared.desired.store(true, Ordering::Release);
        let now = Instant::now();
        shared
            .source
            .lock()
            .unwrap()
            .event(b'W' as u32, true, false, true, false, now);
        let resolve = |point: &ClientPoint| {
            Ok(Pixel {
                x: point.x,
                y: point.y,
            })
        };
        shared.process(&snapshot, now, || true, resolve);
        assert_eq!(shared.output.lock().unwrap().sink.downs(), 1);
        shared.process(
            &snapshot,
            now + Duration::from_millis(8),
            || true,
            |_| Err(code.clone().into()),
        );
        assert_eq!(shared.error.lock().unwrap().take().unwrap().code, code);
        assert!(shared.active.load(Ordering::Acquire));
        assert!(shared.output.lock().unwrap().sink.contacts.is_empty());
        shared
            .source
            .lock()
            .unwrap()
            .event(b'W' as u32, true, false, true, false, now);
        shared.process(
            &snapshot,
            now + Duration::from_millis(16),
            || true,
            |_| panic!("old physical hold was replayed"),
        );
        shared
            .source
            .lock()
            .unwrap()
            .event(b'W' as u32, false, false, true, false, now);
        shared
            .source
            .lock()
            .unwrap()
            .event(b'W' as u32, true, false, true, false, now);
        shared.process(&snapshot, now + Duration::from_millis(24), || true, resolve);
        assert_eq!(shared.output.lock().unwrap().sink.downs(), 2);
        shared.pause();
    }
}

#[test]
fn an_obsolete_snapshot_does_not_disarm_the_newly_accepted_key_state() {
    let shared = Shared::new(
        FakeSink::default(),
        Arc::new(Mutex::new(PhysicalMouse::default())),
    );
    let active = ActiveSettings::from(&settings(
        InputMethod::Touch,
        MovementMode::Buttons,
        PressBehavior::Hold,
    ));
    let current = Arc::new(Snapshot {
        source: active.clone(),
        settings: active,
        target: None,
        size: Some((1000, 800)),
    });
    *shared.snapshot.write().unwrap() = current.clone();
    shared.active.store(true, Ordering::Release);
    shared.desired.store(true, Ordering::Release);
    shared
        .source
        .lock()
        .unwrap()
        .event(b'W' as u32, true, false, true, false, Instant::now());
    shared.process(
        &Arc::new(Snapshot::default()),
        Instant::now(),
        || true,
        |_| panic!("obsolete snapshot resolved an input"),
    );
    shared.process(
        &current,
        Instant::now(),
        || true,
        |point| {
            Ok(Pixel {
                x: point.x,
                y: point.y,
            })
        },
    );
    assert_eq!(shared.output.lock().unwrap().sink.downs(), 1);
    shared.pause();
    assert_eq!(shared.output.lock().unwrap().sink.ups(), 1);
}
impl Sink for GateSink {
    fn desktop(&mut self) -> Result<Desktop, ClickerError> {
        self.fake.desktop()
    }
    fn mouse_state(&mut self) -> Result<MouseState, ClickerError> {
        self.fake.mouse_state()
    }
    fn mouse(&mut self, packets: &[INPUT]) -> Result<(), MouseFailure> {
        if packets.len() == 2 {
            let (lock, changed) = &*self.gate;
            let mut state = lock.lock().unwrap();
            state.0 = true;
            changed.notify_all();
            while !state.1 {
                state = changed.wait(state).unwrap();
            }
        }
        self.fake.mouse(packets)
    }
    fn initialize_touch(&mut self) -> Result<(), ClickerError> {
        self.fake.initialize_touch()
    }
    fn touch(&mut self, packets: &[POINTER_TOUCH_INFO]) -> Result<(), TouchFailure> {
        self.fake.touch(packets)
    }
}

#[test]
fn pause_is_a_release_barrier_for_an_in_flight_send_and_rejects_stale_frames() {
    let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
    let shared = Arc::new(Shared::new(
        GateSink {
            fake: FakeSink::default(),
            gate: gate.clone(),
        },
        Arc::new(Mutex::new(PhysicalMouse::default())),
    ));
    let active = ActiveSettings::from(&settings(
        InputMethod::Mouse,
        MovementMode::Buttons,
        PressBehavior::Hold,
    ));
    let snapshot = Arc::new(Snapshot {
        source: active.clone(),
        settings: active,
        target: None,
        size: Some((1000, 800)),
    });
    *shared.snapshot.write().unwrap() = snapshot.clone();
    shared.desired.store(true, Ordering::Release);
    shared.active.store(true, Ordering::Release);
    shared
        .source
        .lock()
        .unwrap()
        .event(b'W' as u32, true, false, true, false, Instant::now());
    let worker_shared = shared.clone();
    let worker_snapshot = snapshot.clone();
    let worker = thread::spawn(move || {
        worker_shared.process(
            &worker_snapshot,
            Instant::now(),
            || true,
            |point| {
                Ok(Pixel {
                    x: point.x,
                    y: point.y,
                })
            },
        )
    });
    let (lock, changed) = &*gate;
    let state = lock.lock().unwrap();
    let (state, timeout) = changed
        .wait_timeout_while(state, Duration::from_secs(5), |state| !state.0)
        .unwrap();
    let entered = state.0 && !timeout.timed_out();
    drop(state);
    let pause_shared = shared.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let pause = thread::spawn(move || {
        pause_shared.pause();
        done_tx.send(()).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.active.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::yield_now();
    }
    let invalidated = !shared.active.load(Ordering::Acquire);
    let pending = done_rx.try_recv().is_err();
    {
        let mut state = lock.lock().unwrap();
        state.1 = true;
        changed.notify_all();
    }
    worker.join().unwrap();
    pause.join().unwrap();
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(entered && invalidated && pending);
    shared.process(
        &snapshot,
        Instant::now(),
        || true,
        |_| panic!("paused frame resolved an input"),
    );
    let output = shared.output.lock().unwrap();
    assert_eq!(output.sink.fake.downs(), 1);
    assert_eq!(output.sink.fake.ups(), 1);
    assert!(shared.error.lock().unwrap().is_none());
}

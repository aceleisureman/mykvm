use super::*;
use crate::linux_input::wayland::test_support;
use reis::{eis as server, request::EisRequest};

#[tokio::test]
async fn readiness_waits_for_keyboard_and_resumed_pointer_regions() {
    let (mut server, connection, mut events) = test_support::MockEis::connect().await;
    let mut input = test_support::input(connection);
    test_support::event(&mut input, &mut events).await.unwrap();
    server.requests();
    server.add_pointer();
    while input.active.is_empty() {
        test_support::event(&mut input, &mut events).await.unwrap();
    }
    assert!(input.prepare().is_err());
    server.add_keyboard();
    while input.prepare().is_err() {
        test_support::event(&mut input, &mut events).await.unwrap();
    }
    assert_eq!(input.pointers.len(), 1);
}

#[tokio::test]
async fn protocol_keys_drags_wheel_frames_and_hidpi_coordinates() {
    let (mut server, mut input, _events) = test_support::ready().await;
    let key = InputCommand::Key {
        key_code: 0xA3,
        down: true,
    };
    input.inject(7, &key).unwrap();
    input.inject(7, &key).unwrap(); // repeat is compositor-owned, not a second press
    input
        .inject(
            7,
            &InputCommand::MouseButton {
                button: MouseButton::Left,
                down: true,
                x: 100,
                y: 200,
            },
        )
        .unwrap();
    input
        .inject(
            7,
            &InputCommand::MouseMove {
                x: 1919,
                y: 1079,
                drag_button: Some(MouseButton::Left),
            },
        )
        .unwrap();
    input
        .inject(
            7,
            &InputCommand::Scroll {
                delta_x: 1,
                delta_y: 2,
            },
        )
        .unwrap();
    input
        .inject(
            7,
            &InputCommand::Key {
                key_code: 0xA3,
                down: false,
            },
        )
        .unwrap();
    input
        .inject(
            7,
            &InputCommand::Key {
                key_code: 0xA3,
                down: false,
            },
        )
        .unwrap(); // unknown up ignored
    input
        .inject(
            7,
            &InputCommand::MouseButton {
                button: MouseButton::Left,
                down: false,
                x: 1919,
                y: 1079,
            },
        )
        .unwrap();
    let requests = server.requests();
    let keys: Vec<_> = requests
        .iter()
        .filter_map(|request| match request {
            EisRequest::KeyboardKey(key) => Some(key),
            _ => None,
        })
        .collect();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].key, 97); // right Ctrl, evdev (not X11 +8)
    assert_eq!(keys[0].state, server::keyboard::KeyState::Press);
    assert_eq!(keys[1].state, server::keyboard::KeyState::Released);
    assert_eq!(keys[0].device, keys[1].device);
    assert!(keys.iter().all(|key| key.time > 0));
    let buttons: Vec<_> = requests
        .iter()
        .filter_map(|request| match request {
            EisRequest::Button(button) => Some(button),
            _ => None,
        })
        .collect();
    assert_eq!(buttons.len(), 2);
    assert_eq!(buttons[0].button, 272);
    assert_eq!(buttons[0].state, server::button::ButtonState::Press);
    assert_eq!(buttons[1].state, server::button::ButtonState::Released);
    assert_eq!(buttons[0].device, buttons[1].device);
    assert!(requests.iter().any(|request| matches!(request, EisRequest::PointerMotionAbsolute(motion) if motion.dx_absolute == 1919.0 && motion.dy_absolute == 1079.0)));
    assert!(requests.iter().any(|request| matches!(request, EisRequest::ScrollDiscrete(scroll) if scroll.discrete_dx == 120 && scroll.discrete_dy == -240)));
    let frames: Vec<_> = requests
        .iter()
        .filter_map(|request| match request {
            EisRequest::Frame(frame) => Some(frame.time),
            _ => None,
        })
        .collect();
    assert!(!frames.is_empty());
    assert!(frames.windows(2).all(|pair| pair[0] <= pair[1]));
}

#[tokio::test]
async fn disconnect_and_controller_switch_release_only_the_owning_connection() {
    let (mut server, mut input, _events) = test_support::ready().await;
    input
        .inject(
            7,
            &InputCommand::Key {
                key_code: 0x10,
                down: true,
            },
        )
        .unwrap();
    server.requests();
    input.disconnected(99).unwrap();
    assert!(server.requests().is_empty());
    input
        .inject(
            8,
            &InputCommand::Key {
                key_code: 0x11,
                down: true,
            },
        )
        .unwrap();
    let requests = server.requests();
    assert!(requests.iter().any(|request| matches!(request, EisRequest::KeyboardKey(key) if key.key == 42 && key.state == server::keyboard::KeyState::Released)));
    input.disconnected(7).unwrap(); // old socket closes after replacement
    assert!(server.requests().is_empty());
    input.disconnected(8).unwrap();
    assert!(server.requests().iter().any(|request| matches!(request, EisRequest::KeyboardKey(key) if key.key == 29 && key.state == server::keyboard::KeyState::Released)));
    input.disconnected(8).unwrap();
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn cleanup_never_emits_on_a_paused_or_removed_device() {
    for remove in [false, true] {
        let (mut server, mut input, mut events) = test_support::ready().await;
        input
            .inject(
                1,
                &InputCommand::Key {
                    key_code: 0x41,
                    down: true,
                },
            )
            .unwrap();
        input
            .inject(
                1,
                &InputCommand::MouseButton {
                    button: MouseButton::Left,
                    down: true,
                    x: 10,
                    y: 20,
                },
            )
            .unwrap();
        server.requests();
        let keyboard = server.keyboard.as_ref().unwrap();
        if remove {
            keyboard.remove();
        } else {
            keyboard.paused();
        }
        server.flush();
        test_support::event(&mut input, &mut events).await.unwrap();
        assert!(input.prepare().is_err());
        drop(input);
        let requests = server.requests();
        assert!(!requests
            .iter()
            .any(|request| matches!(request, EisRequest::KeyboardKey(_))));
        assert!(requests.iter().any(|request| matches!(request, EisRequest::Button(button) if button.state == server::button::ButtonState::Released)));
        assert!(!requests.iter().any(|request| matches!(request, EisRequest::DeviceStopEmulating(stop) if stop.device.name() == Some("keyboard"))));
    }
}

#[tokio::test]
async fn pause_resumes_the_same_device_with_neutral_input_and_a_new_sequence() {
    let (mut server, mut input, mut events) = test_support::ready().await;
    let sequence = input.sequence;
    let key = InputCommand::Key {
        key_code: 0x41,
        down: true,
    };
    input.inject(1, &key).unwrap();
    input
        .inject(
            1,
            &InputCommand::MouseButton {
                button: MouseButton::Left,
                down: true,
                x: 10,
                y: 20,
            },
        )
        .unwrap();
    server.requests();
    server.keyboard.as_ref().unwrap().paused();
    server.flush();
    test_support::event(&mut input, &mut events).await.unwrap();
    assert!(input.is_paused());
    assert!(input.prepare().is_err());
    assert!(input.keys.is_empty() && input.buttons.is_empty());
    assert!(input.controller.is_none());
    assert!(input.pointers.is_empty());
    assert!(input.pointer_device.is_none());
    let cleanup = server.requests();
    assert!(cleanup.iter().any(
        |r| matches!(r, EisRequest::Button(b) if b.state == server::button::ButtonState::Released)
    ));
    assert!(!cleanup.iter().any(|r| matches!(
        r,
        EisRequest::KeyboardKey(_) | EisRequest::DeviceStopEmulating(_)
    )));

    // Pause gates the entire session, including otherwise still-active devices.
    assert!(input.inject(1, &key).is_err());
    assert!(input
        .inject(
            1,
            &InputCommand::MouseMove {
                x: 30,
                y: 40,
                drag_button: None,
            },
        )
        .is_err());
    assert!(server.requests().is_empty());

    server.keyboard.as_ref().unwrap().resumed();
    server.flush();
    test_support::event(&mut input, &mut events).await.unwrap();
    input.prepare().unwrap();
    assert!(!input.is_paused());
    let resumed = server.requests();
    let starts: Vec<_> = resumed
        .iter()
        .filter_map(|r| match r {
            EisRequest::DeviceStartEmulating(start) => Some(start),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 1);
    assert_eq!(&starts[0].device, server.keyboard.as_ref().unwrap());
    assert!(starts[0].sequence > sequence);
    // Resumption never replays the held key. A fresh key-down is required.
    assert!(!resumed
        .iter()
        .any(|r| matches!(r, EisRequest::KeyboardKey(_) | EisRequest::Button(_))));
    input.inject(1, &key).unwrap();
    let pressed = server.requests();
    assert_eq!(pressed.iter().filter(|r| matches!(r, EisRequest::KeyboardKey(k) if k.state == server::keyboard::KeyState::Press)).count(), 1);
}

#[tokio::test]
async fn every_original_paused_device_must_resume_before_input_can_continue() {
    let (mut server, mut input, mut events) = test_support::ready().await;
    let mapping = input.pointers[0].0.position(1919, 1079);
    let pointer = input.pointers[0].1.clone();
    server.pointer.as_ref().unwrap().paused();
    server.keyboard.as_ref().unwrap().paused();
    server.flush();
    test_support::event(&mut input, &mut events).await.unwrap();
    test_support::event(&mut input, &mut events).await.unwrap();
    assert_eq!(input.paused.len(), 2);
    assert!(input.prepare().is_err());
    server.pointer.as_ref().unwrap().resumed();
    server.flush();
    test_support::event(&mut input, &mut events).await.unwrap();
    assert!(input.prepare().is_err());
    assert!(input.pointers.is_empty());
    server.keyboard.as_ref().unwrap().resumed();
    server.flush();
    test_support::event(&mut input, &mut events).await.unwrap();
    input.prepare().unwrap();
    assert!(!input.is_paused());
    assert_eq!(input.pointers[0].1, pointer);
    assert_eq!(input.pointers[0].0.position(1919, 1079), mapping);
}

#[tokio::test]
async fn replacing_a_paused_device_cannot_bypass_removal_or_reuse_its_mapping() {
    let (mut server, mut input, mut events) = test_support::ready().await;
    let original = server.pointer.as_ref().unwrap().clone();
    original.paused();
    server.flush();
    test_support::event(&mut input, &mut events).await.unwrap();
    server.add_pointer();
    test_support::event(&mut input, &mut events).await.unwrap(); // added
    test_support::event(&mut input, &mut events).await.unwrap(); // resumed
    assert!(input.is_paused());
    assert!(input.prepare().is_err());
    original.remove();
    server.flush();
    let error = test_support::event(&mut input, &mut events)
        .await
        .unwrap_err();
    assert!(error.contains("was removed"));
    assert!(error.contains("restart MyKVM"));
}

#[tokio::test]
async fn removed_keyboard_recovers_without_replaying_input_or_changing_pointer_geometry() {
    for pause_first in [false, true] {
        let (mut server, mut input, mut events) = test_support::ready().await;
        let pointer = input.pointers[0].1.clone();
        let position = input.pointers[0].0.position(1919, 1079);
        let original = input.device::<ei::Keyboard>().unwrap();
        let sequence = input.sequence;
        let key = InputCommand::Key {
            key_code: 0x41,
            down: true,
        };
        input.inject(1, &key).unwrap();
        input
            .inject(
                1,
                &InputCommand::MouseButton {
                    button: MouseButton::Left,
                    down: true,
                    x: 10,
                    y: 20,
                },
            )
            .unwrap();
        server.requests();
        if pause_first {
            server.keyboard.as_ref().unwrap().paused();
            server.flush();
            test_support::event(&mut input, &mut events).await.unwrap();
        }
        server.keyboard.as_ref().unwrap().remove();
        server.flush();
        assert_eq!(
            test_support::event(&mut input, &mut events).await.unwrap(),
            EventEffect::ResetInput
        );
        // Capabilities are gone by DeviceRemoved; the earlier snapshot is what
        // allows only keyboard-only replacement, not arbitrary removed devices.
        assert!(original.interface::<ei::Keyboard>().is_none());
        assert!(input.waiting_for_keyboard && input.is_paused());
        assert!(!input.keyboard_only_devices.contains(&original));
        assert!(input.prepare().is_err());
        assert!(input.keys.is_empty() && input.buttons.is_empty());
        assert!(input.controller.is_none() && input.pointer_device.is_none());
        assert!(input.pointers.is_empty());
        assert!(input.inject(1, &key).is_err());
        assert!(input
            .inject(
                1,
                &InputCommand::MouseMove {
                    x: 30,
                    y: 40,
                    drag_button: None,
                }
            )
            .is_err());
        let cleanup = server.requests();
        assert!(cleanup.iter().any(|r| matches!(r, EisRequest::Button(b) if b.state == server::button::ButtonState::Released)));
        assert!(!cleanup.iter().any(|r| matches!(
            r,
            EisRequest::KeyboardKey(_) | EisRequest::DeviceStopEmulating(_)
        )));

        server.add_keyboard();
        test_support::event(&mut input, &mut events).await.unwrap(); // added, not resumed
        assert!(input.is_paused());
        assert!(input.prepare().is_err());
        test_support::event(&mut input, &mut events).await.unwrap(); // resumed
        input.prepare().unwrap();
        assert!(!input.is_paused());
        assert_eq!(input.pointers[0].1, pointer);
        assert_eq!(input.pointers[0].0.position(1919, 1079), position);
        let requests = server.requests();
        assert!(requests
            .iter()
            .any(|r| matches!(r, EisRequest::DeviceStartEmulating(start)
            if &start.device == server.keyboard.as_ref().unwrap() && start.sequence > sequence)));
        assert!(!requests
            .iter()
            .any(|r| matches!(r, EisRequest::KeyboardKey(_) | EisRequest::Button(_))));
        // Releasing a key held on the old keyboard must not send a release to
        // its replacement. Only a fresh press may reach the new device.
        input
            .inject(
                1,
                &InputCommand::Key {
                    key_code: 0x41,
                    down: false,
                },
            )
            .unwrap();
        assert!(server.requests().is_empty());
        input.inject(1, &key).unwrap();
        let requests = server.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|r| matches!(r, EisRequest::KeyboardKey(k)
            if &k.device == server.keyboard.as_ref().unwrap()
                && k.key == 30 && k.state == server::keyboard::KeyState::Press))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn replacement_keyboard_cannot_resume_an_original_paused_pointer() {
    let (mut server, mut input, mut events) = test_support::ready().await;
    server.pointer.as_ref().unwrap().paused();
    server.keyboard.as_ref().unwrap().remove();
    server.flush();
    test_support::event(&mut input, &mut events).await.unwrap();
    test_support::event(&mut input, &mut events).await.unwrap();
    server.add_keyboard();
    test_support::event(&mut input, &mut events).await.unwrap();
    test_support::event(&mut input, &mut events).await.unwrap();
    assert!(!input.waiting_for_keyboard);
    assert!(input.is_paused());
    assert!(input.prepare().is_err());
    server.pointer.as_ref().unwrap().resumed();
    server.flush();
    test_support::event(&mut input, &mut events).await.unwrap();
    input.prepare().unwrap();
}

#[tokio::test]
async fn an_already_resumed_replacement_still_requires_an_input_reset_barrier() {
    let (mut server, mut input, mut events) = test_support::ready().await;
    let original = server.keyboard.as_ref().unwrap().clone();
    server.add_keyboard();
    test_support::event(&mut input, &mut events).await.unwrap();
    test_support::event(&mut input, &mut events).await.unwrap();
    input
        .inject(
            1,
            &InputCommand::Key {
                key_code: 0x41,
                down: true,
            },
        )
        .unwrap();
    server.requests();
    original.remove();
    server.flush();
    assert_eq!(
        test_support::event(&mut input, &mut events).await.unwrap(),
        EventEffect::ResetInput
    );
    assert!(input.keys.is_empty() && input.controller.is_none());
    assert!(!input.is_paused());
    input.prepare().unwrap();
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn a_keyboard_from_an_unbound_seat_cannot_end_replacement_waiting() {
    let (mut server, mut input, mut events) = test_support::ready().await;
    server.keyboard.as_ref().unwrap().remove();
    server.flush();
    test_support::event(&mut input, &mut events).await.unwrap();
    server.add_foreign_keyboard();
    for _ in 0..3 {
        test_support::event(&mut input, &mut events).await.unwrap();
    }
    assert!(input.waiting_for_keyboard);
    assert!(input.prepare().is_err());
    assert!(server.requests().is_empty());
    server.add_keyboard();
    test_support::event(&mut input, &mut events).await.unwrap();
    test_support::event(&mut input, &mut events).await.unwrap();
    input.prepare().unwrap();
}

#[tokio::test]
async fn removing_a_keyboard_with_other_capabilities_still_fails_closed() {
    let (mut server, mut input, mut events) = test_support::ready().await;
    server.add_keyboard_with_button();
    test_support::event(&mut input, &mut events).await.unwrap();
    test_support::event(&mut input, &mut events).await.unwrap();
    input.prepare().unwrap();
    server.keyboard.as_ref().unwrap().remove();
    server.flush();
    let error = test_support::event(&mut input, &mut events)
        .await
        .unwrap_err();
    assert!(error.contains("was removed"));
    assert!(error.contains("restart MyKVM"));
    assert!(!input.waiting_for_keyboard);
}

#[tokio::test]
async fn normal_shutdown_releases_held_inputs_before_stopping_emulation() {
    let (mut server, mut input, _events) = test_support::ready().await;
    input
        .inject(
            1,
            &InputCommand::Key {
                key_code: 0x41,
                down: true,
            },
        )
        .unwrap();
    input
        .inject(
            1,
            &InputCommand::MouseButton {
                button: MouseButton::Middle,
                down: true,
                x: 10,
                y: 20,
            },
        )
        .unwrap();
    server.requests();
    drop(input);
    let requests = server.requests();
    let key_up = requests.iter().position(|request| matches!(request, EisRequest::KeyboardKey(key) if key.state == server::keyboard::KeyState::Released)).unwrap();
    let button_up = requests.iter().position(|request| matches!(request, EisRequest::Button(button) if button.state == server::button::ButtonState::Released)).unwrap();
    let stop = requests
        .iter()
        .position(|request| matches!(request, EisRequest::DeviceStopEmulating(_)))
        .unwrap();
    assert!(key_up < stop && button_up < stop);
}

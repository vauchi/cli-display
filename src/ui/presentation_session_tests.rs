// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: GPL-3.0-or-later

use super::{CommandReducer, run_with_io};
use vauchi_core::{
    AccessibilitySpec, ActionSpec, ActionTone, Command, ContextBar, Event, FilePickPurpose,
    InteractionId, PresentationNode, PresentationTextStyle, PresentationTokens, StandardShortcut,
    SurfaceId, SurfaceLayout, SurfaceSpec,
};

fn action(id: &str, label: &str) -> ActionSpec {
    ActionSpec {
        interaction_id: InteractionId::new(id).unwrap(),
        label: label.into(),
        accessibility_label: label.into(),
        icon_token: None,
        enabled: true,
        tone: ActionTone::Standard,
        shortcut: Some(StandardShortcut::ActivatePrimary),
    }
}

fn surface(revision: u64, title: &str) -> SurfaceSpec {
    SurfaceSpec {
        surface_id: SurfaceId::new("welcome").unwrap(),
        revision,
        title: title.into(),
        subtitle: Some("Prepared by Core".into()),
        accessibility_label: title.into(),
        layout: SurfaceLayout::Scroll,
        tokens: PresentationTokens {
            spacing_small: 1,
            spacing_medium: 2,
            spacing_large: 3,
            corner_radius: 0,
            minimum_target_size: 1,
            colors: crate::ui::test_colors::test_colors(),
            fonts: None,
        },
        nodes: vec![PresentationNode::Text {
            id: None,
            content: "Hello".into(),
            style: PresentationTextStyle::Body,
            accessibility: AccessibilitySpec::label("Hello"),
        }],
    }
}

struct FakeReducer {
    initial: Vec<Command>,
}

impl CommandReducer for FakeReducer {
    type Error = std::convert::Infallible;

    fn initial_commands(&mut self) -> Result<Vec<Command>, Self::Error> {
        Ok(std::mem::take(&mut self.initial))
    }

    fn dispatch(&mut self, event: Event) -> Result<Vec<Command>, Self::Error> {
        if matches!(event, Event::PresentationEnvironmentChanged { .. }) {
            return Ok(Vec::new());
        }
        assert!(matches!(
            event,
            Event::ActionActivated { interaction_id, .. }
                if interaction_id.as_str() == "opaque-token"
        ));
        Ok(vec![Command::PerformNativeBack])
    }
}

// @internal
#[test]
fn generic_session_exits_only_when_core_requests_native_back() {
    let mut reducer = FakeReducer {
        initial: vec![
            Command::ReplaceSurface {
                surface: surface(1, "Session"),
            },
            Command::SetContextBar {
                surface_id: SurfaceId::new("welcome").unwrap(),
                revision: 1,
                bar: Box::new(ContextBar {
                    primary: Some(action("opaque-token", "Continue")),
                    ..ContextBar::default()
                }),
            },
        ],
    };
    let mut input = std::io::Cursor::new(b"1\n");
    let mut output = Vec::new();

    run_with_io(&mut reducer, &mut input, &mut output).expect("generic session");

    assert!(String::from_utf8(output).unwrap().contains("Session"));
}

struct EnvironmentRecordingReducer {
    events: Vec<Event>,
}

impl CommandReducer for EnvironmentRecordingReducer {
    type Error = std::convert::Infallible;

    fn initial_commands(&mut self) -> Result<Vec<Command>, Self::Error> {
        Ok(vec![
            Command::ReplaceSurface {
                surface: surface(1, "Session"),
            },
            Command::SetContextBar {
                surface_id: SurfaceId::new("welcome").unwrap(),
                revision: 1,
                bar: Box::new(ContextBar {
                    primary: Some(action("opaque-token", "Continue")),
                    ..ContextBar::default()
                }),
            },
        ])
    }

    fn dispatch(&mut self, event: Event) -> Result<Vec<Command>, Self::Error> {
        self.events.push(event);
        Ok(vec![Command::PerformNativeBack])
    }
}

// @internal
#[test]
fn session_reports_the_terminal_environment_before_the_first_prompt() {
    let mut reducer = EnvironmentRecordingReducer { events: Vec::new() };
    let mut input = std::io::Cursor::new(b"1\n");
    let mut output = Vec::new();

    run_with_io(&mut reducer, &mut input, &mut output).expect("environment-reporting session");

    assert_eq!(reducer.events.len(), 1);
    let Event::PresentationEnvironmentChanged {
        available_width,
        available_height,
        input_modes,
        color_scheme,
        pinned_actions,
        ..
    } = &reducer.events[0]
    else {
        panic!("expected an environment report, got {:?}", reducer.events);
    };
    assert_eq!((*available_width, *available_height), (80, 24));
    assert_eq!(input_modes, &vec![vauchi_core::InputMode::Keyboard]);
    assert_eq!(*color_scheme, None);
    assert!(*pinned_actions);
    assert!(output.is_empty(), "Core closed the session before a prompt");
}

struct EnvironmentRefusingReducer {
    activated: Vec<Event>,
}

impl CommandReducer for EnvironmentRefusingReducer {
    type Error = String;

    fn initial_commands(&mut self) -> Result<Vec<Command>, Self::Error> {
        EnvironmentRecordingReducer { events: Vec::new() }
            .initial_commands()
            .map_err(|never| match never {})
    }

    fn dispatch(&mut self, event: Event) -> Result<Vec<Command>, Self::Error> {
        if matches!(event, Event::PresentationEnvironmentChanged { .. }) {
            return Err("environment refused while locked".to_owned());
        }
        self.activated.push(event);
        Ok(vec![Command::PerformNativeBack])
    }
}

// The report only tells Core what the terminal draws; a Core that refuses
// it keeps the answers in the content, so the session goes on.
// @internal
#[test]
fn a_refused_environment_report_does_not_end_the_session() {
    let mut reducer = EnvironmentRefusingReducer {
        activated: Vec::new(),
    };
    let mut input = std::io::Cursor::new(b"1\n");
    let mut output = Vec::new();

    run_with_io(&mut reducer, &mut input, &mut output).expect("session survives the refusal");

    assert_eq!(
        reducer.activated,
        vec![Event::ActionActivated {
            surface_id: SurfaceId::new("welcome").unwrap(),
            interaction_id: vauchi_core::InteractionId::new("opaque-token").unwrap(),
        }]
    );
}

struct FilePickReducer {
    expected_bytes: Vec<u8>,
    expected_filename: String,
}

impl CommandReducer for FilePickReducer {
    type Error = std::convert::Infallible;

    fn initial_commands(&mut self) -> Result<Vec<Command>, Self::Error> {
        Ok(vec![Command::FilePickFromUser {
            accepted_mime_types: vec!["application/octet-stream".into()],
            accepted_extensions: vec!["vbk".into()],
            max_bytes: None,
            purpose: FilePickPurpose::ImportBackup,
        }])
    }

    fn dispatch(&mut self, event: Event) -> Result<Vec<Command>, Self::Error> {
        if matches!(event, Event::PresentationEnvironmentChanged { .. }) {
            return Ok(Vec::new());
        }
        assert_eq!(
            event,
            Event::FilePickedFromUser {
                bytes: self.expected_bytes.clone(),
                filename: self.expected_filename.clone(),
            }
        );
        Ok(vec![Command::PerformNativeBack])
    }
}

// @internal
#[test]
fn generic_session_adapts_a_core_file_pick_to_terminal_input() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("backup.vauchi");
    let bytes = b"encrypted backup".to_vec();
    std::fs::write(&path, &bytes).expect("test backup");
    let mut reducer = FilePickReducer {
        expected_bytes: bytes,
        expected_filename: "backup.vauchi".into(),
    };
    let mut input = std::io::Cursor::new(format!("{}\n", path.display()));
    let mut output = Vec::new();

    run_with_io(&mut reducer, &mut input, &mut output).expect("file-pick session");

    assert!(
        String::from_utf8(output)
            .expect("utf-8 terminal output")
            .contains("File path")
    );
}

struct WakeupReducer {
    first_wakeup: Command,
    observed: Vec<Event>,
}

impl CommandReducer for WakeupReducer {
    type Error = std::convert::Infallible;

    fn initial_commands(&mut self) -> Result<Vec<Command>, Self::Error> {
        Ok(vec![
            Command::ReplaceSurface {
                surface: surface(1, "Restoring backup"),
            },
            self.first_wakeup.clone(),
        ])
    }

    fn dispatch(&mut self, event: Event) -> Result<Vec<Command>, Self::Error> {
        if matches!(event, Event::PresentationEnvironmentChanged { .. }) {
            return Ok(Vec::new());
        }
        self.observed.push(event);
        Ok(vec![Command::PerformNativeBack])
    }
}

// Core asks for a wakeup at once when the next thing to run is its own
// work, such as a restore behind "Restoring backup…" (vauchi/private#273).
// The line session has no timer, so it reports that one straight away.
// @internal
#[test]
fn a_wakeup_core_asks_for_at_once_is_reported_before_the_next_prompt() {
    let mut reducer = WakeupReducer {
        first_wakeup: Command::schedule_wakeup(0, 1, 1, Some(0)),
        observed: Vec::new(),
    };
    let mut input = std::io::Cursor::new(b"");
    let mut output = Vec::new();

    run_with_io(&mut reducer, &mut input, &mut output).expect("session ends on native back");

    assert_eq!(reducer.observed, vec![Event::WakeupFired]);
}

// @internal
#[test]
fn a_later_wakeup_is_not_reported_at_once() {
    let mut reducer = WakeupReducer {
        first_wakeup: Command::schedule_wakeup(30, 90, 30, None),
        observed: Vec::new(),
    };
    let mut input = std::io::Cursor::new(b"");
    let mut output = Vec::new();

    run_with_io(&mut reducer, &mut input, &mut output).expect("session ends on native back");

    assert!(
        !reducer.observed.contains(&Event::WakeupFired),
        "a 30 s heartbeat is not fired early: {:?}",
        reducer.observed
    );
}

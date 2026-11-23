// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Display;
use std::io::{self, BufRead, IsTerminal, Write};

use vauchi_app::ui::AppEngine;
use vauchi_core::{Command, Event, InputMode, MotionPreference};

use super::{
    OfferedInputs, PresentationState, SecretReader, prompt_event, prompt_event_with,
    render_to_string,
};

const FALLBACK_TERMINAL_SIZE: (u32, u32) = (80, 24);

/// Reads a password or PIN at the terminal without echoing it
/// (vauchi/private#630).
struct TerminalSecrets;

impl SecretReader for TerminalSecrets {
    fn read_secret(&mut self, label: &str) -> io::Result<String> {
        dialoguer::Password::new()
            .with_prompt(format!("  {label}"))
            .allow_empty_password(true)
            .interact()
            .map_err(io::Error::other)
    }
}

pub trait CommandReducer {
    type Error: Display;

    fn initial_commands(&mut self) -> Result<Vec<Command>, Self::Error>;
    fn dispatch(&mut self, event: Event) -> Result<Vec<Command>, Self::Error>;
}

impl CommandReducer for AppEngine {
    type Error = vauchi_app::ui::AppPresentationError;

    fn initial_commands(&mut self) -> Result<Vec<Command>, Self::Error> {
        self.initial_commands()
    }

    fn dispatch(&mut self, event: Event) -> Result<Vec<Command>, Self::Error> {
        self.dispatch(event)
    }
}

pub fn run_engine(engine: &mut AppEngine) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let (mut input, mut output) = (stdin.lock(), stdout.lock());
    if stdin.is_terminal() {
        run_session(
            engine,
            &mut input,
            &mut output,
            Some(&mut TerminalSecrets),
            terminal_size(),
        )
    } else {
        run_with_io(engine, &mut input, &mut output)
    }
    .map_err(Into::into)
}

/// Piped input is scripted, so Core gets the same 80x24 every run.
pub fn run_with_io(
    reducer: &mut impl CommandReducer,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> io::Result<()> {
    run_session(reducer, input, output, None, FALLBACK_TERMINAL_SIZE)
}

fn terminal_size() -> (u32, u32) {
    if !io::stdout().is_terminal() {
        return FALLBACK_TERMINAL_SIZE;
    }
    let (rows, columns) = console::Term::stdout().size();
    (u32::from(columns), u32::from(rows))
}

fn presentation_environment(available_width: u32, available_height: u32) -> Event {
    let reduced_motion = std::env::var("VAUCHI_REDUCED_MOTION")
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
    Event::PresentationEnvironmentChanged {
        available_width,
        available_height,
        input_modes: vec![InputMode::Keyboard],
        motion: if reduced_motion {
            MotionPreference::Reduced
        } else {
            MotionPreference::Full
        },
        color_scheme: None,
        pinned_actions: true,
    }
}

fn run_session(
    reducer: &mut impl CommandReducer,
    input: &mut impl BufRead,
    output: &mut impl Write,
    mut secrets: Option<&mut dyn SecretReader>,
    (available_width, available_height): (u32, u32),
) -> io::Result<()> {
    let mut state = PresentationState::default();
    let mut offered = OfferedInputs::default();
    let mut commands = reducer.initial_commands().map_err(reducer_error)?;
    // The report only says what this terminal draws; refused, Core keeps
    // the answers in the content, so the session goes on without it.
    if let Ok(reported) =
        reducer.dispatch(presentation_environment(available_width, available_height))
    {
        commands.extend(reported);
    }
    loop {
        let effects = state.apply(&commands);
        if state.native_back_requested() {
            return Ok(());
        }
        if let Some(event) = execute_effects(&effects, input, output)? {
            commands = reducer.dispatch(event).map_err(reducer_error)?;
            continue;
        }
        write!(output, "{}", render_to_string(&state))?;
        output.flush()?;
        let event = match secrets.as_deref_mut() {
            Some(reader) => prompt_event_with(&state, &mut offered, input, output, reader)?,
            None => prompt_event(&state, &mut offered, input, output)?,
        };
        commands = reducer.dispatch(event).map_err(reducer_error)?;
    }
}

fn reducer_error(error: impl Display) -> io::Error {
    io::Error::other(format!("Core reducer rejected event: {error}"))
}

fn execute_effects(
    effects: &[Command],
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> io::Result<Option<Event>> {
    for effect in effects {
        match effect {
            Command::PresentAlert { alert } => {
                writeln!(output, "  {}: {}", alert.title, alert.message)?;
            }
            Command::ShowToast { toast } => {
                writeln!(output, "  {}", toast.message)?;
            }
            Command::OpenExternalUrl { url } => {
                writeln!(output, "  Open: {url}")?;
            }
            Command::PostNotification { notification } => {
                writeln!(output, "  {}: {}", notification.title, notification.body)?;
            }
            Command::QrRequestScan => {
                write!(output, "  QR data > ")?;
                output.flush()?;
                let mut data = String::new();
                input.read_line(&mut data)?;
                return Ok(Some(Event::QrScanned {
                    data: data.trim().to_string(),
                }));
            }
            Command::FilePickFromUser { .. } => {
                write!(output, "  File path (blank to cancel) > ")?;
                output.flush()?;
                let mut path = String::new();
                input.read_line(&mut path)?;
                let path = path.trim();
                if path.is_empty() {
                    return Ok(Some(Event::FilePickCancelledByUser));
                }
                let bytes = std::fs::read(path)?;
                let filename = std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string();
                return Ok(Some(Event::FilePickedFromUser { bytes, filename }));
            }
            Command::ResetApplication => {
                writeln!(output, "  Application state reset.")?;
            }
            // Core asks for a wakeup at once when its own work runs next,
            // such as a restore behind "Restoring backup…"
            // (vauchi/private#273); this session has no timer for later ones.
            Command::ScheduleWakeup {
                delay_millis: 0, ..
            } => return Ok(Some(Event::WakeupFired)),
            unsupported => {
                return Ok(Some(Event::HardwareUnavailable {
                    transport: unsupported.variant_name().to_string(),
                }));
            }
        }
    }
    Ok(None)
}

// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: GPL-3.0-or-later

//! The terminal host loop. An app assembles Core and its hardware behind
//! [`Host`]; the terminal it drives implements [`TerminalIo`]. The loop
//! moved here from linux-app so every app with a terminal target runs the
//! same one (ADR-078 step 4, vauchi/private#1234).

use std::{error::Error, io, ops::ControlFlow, time::Duration};

use vauchi_contract::{Command, Event};

/// How long one input wait lasts, so background commands are never held up.
const INPUT_WAIT: Duration = Duration::from_millis(50);

/// What the app assembles: Core with its hardware.
pub trait Host {
    type Error: Error + Send + Sync + 'static;

    /// Hand Core one event.
    fn dispatch(&mut self, event: Event) -> Result<(), Self::Error>;

    /// Move what the hardware observed to Core; true when anything moved.
    fn poll(&mut self) -> Result<bool, Self::Error>;

    /// The display commands Core produced since the last call.
    fn take_commands(&mut self) -> Vec<Command>;
}

pub trait TerminalIo {
    /// Apply a batch and render it; break when the display requests exit.
    fn render(&mut self, commands: &[Command]) -> io::Result<ControlFlow<()>>;

    /// Return input or exit, waiting no longer than `timeout` when idle.
    /// An empty batch means the wait expired without input.
    fn wait_input(&mut self, timeout: Duration) -> io::Result<ControlFlow<(), Vec<Event>>>;

    /// Restore native terminal state before the host's teardown can block.
    fn restore(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Run until the display or the input ends. The host is owned for the loop's
/// lifetime and dropped after the terminal is restored, on every exit path.
pub fn run_terminal<H: Host>(host: H, terminal: &mut impl TerminalIo) -> io::Result<()> {
    run_terminal_with_shutdown(host, terminal, || false)
}

/// As [`run_terminal`], also ending when `shutdown_requested` turns true
/// between bounded waits.
pub fn run_terminal_with_shutdown<H: Host>(
    mut host: H,
    terminal: &mut impl TerminalIo,
    shutdown_requested: impl Fn() -> bool,
) -> io::Result<()> {
    let mut guard = RestoreTerminal {
        terminal,
        active: true,
    };
    let result = run_loop(&mut host, guard.terminal, shutdown_requested);
    guard.active = false;
    let restored = guard.terminal.restore();
    // Dropped only now: its hardware teardown may wait on a native backend.
    drop(host);
    result.and(restored)
}

struct RestoreTerminal<'a, T: TerminalIo> {
    terminal: &'a mut T,
    active: bool,
}

impl<T: TerminalIo> Drop for RestoreTerminal<'_, T> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.terminal.restore();
        }
    }
}

fn run_loop<H: Host>(
    host: &mut H,
    terminal: &mut impl TerminalIo,
    shutdown_requested: impl Fn() -> bool,
) -> io::Result<()> {
    // Core schedules its next wakeup only when one fires (ADR-044 Am2a):
    // the host fires the first, or no heartbeat ever runs.
    host.dispatch(Event::WakeupFired)
        .map_err(io::Error::other)?;
    while !shutdown_requested() {
        host.poll().map_err(io::Error::other)?;
        let commands = host.take_commands();
        if !commands.is_empty() {
            let rendered = terminal.render(&commands);
            if shutdown_requested() || rendered?.is_break() {
                return Ok(());
            }
        }
        let input = terminal.wait_input(INPUT_WAIT);
        if shutdown_requested() {
            return Ok(());
        }
        match input? {
            ControlFlow::Break(()) => return Ok(()),
            ControlFlow::Continue(events) => {
                for event in events {
                    host.dispatch(event).map_err(io::Error::other)?;
                }
            }
        }
    }
    Ok(())
}

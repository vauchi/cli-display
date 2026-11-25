// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: GPL-3.0-or-later

//! The terminal host loop against a scripted host and console: what an app
//! assembles (Core with its hardware) is the host, the display shell is the
//! console. Moved from linux-app, which now assembles it (ADR-078 step 4,
//! vauchi/private#1234).

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    fmt, io,
    ops::ControlFlow,
    rc::Rc,
    time::Duration,
};

use vauchi_cli_display::host::{
    TerminalHost, TerminalIo, run_terminal, run_terminal_with_shutdown,
};
use vauchi_contract::{Command, Event};

#[derive(Debug)]
struct HostFailed;

impl fmt::Display for HostFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("host failed")
    }
}

impl std::error::Error for HostFailed {}

/// A host whose command batches the test scripts, one per turn of the loop.
#[derive(Default)]
struct ScriptedHost {
    batches: VecDeque<Vec<Command>>,
    dispatched: Rc<RefCell<Vec<Event>>>,
    fail_dispatch: bool,
    _dropped_after_restore: Option<DropProbe>,
}

/// Records, when the host is dropped, whether the terminal was already restored.
struct DropProbe {
    restored: Rc<Cell<bool>>,
    observed: Rc<Cell<Option<bool>>>,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.observed.set(Some(self.restored.get()));
    }
}

impl TerminalHost for ScriptedHost {
    type Error = HostFailed;

    fn dispatch(&mut self, event: Event) -> Result<(), HostFailed> {
        if self.fail_dispatch {
            return Err(HostFailed);
        }
        self.dispatched.borrow_mut().push(event);
        Ok(())
    }

    fn poll(&mut self) -> Result<bool, HostFailed> {
        Ok(!self.batches.is_empty())
    }

    fn take_commands(&mut self) -> Vec<Command> {
        self.batches.pop_front().unwrap_or_default()
    }
}

struct Console {
    reads: VecDeque<io::Result<ControlFlow<(), Vec<Event>>>>,
    batches: Vec<Vec<Command>>,
    waits: Vec<Duration>,
    stop_after_render: bool,
    restored: Rc<Cell<bool>>,
    shutdown_on_wait: Option<Rc<Cell<bool>>>,
}

impl Console {
    fn reading(reads: Vec<io::Result<ControlFlow<(), Vec<Event>>>>) -> Self {
        Self {
            reads: reads.into(),
            batches: vec![],
            waits: vec![],
            stop_after_render: false,
            restored: Rc::default(),
            shutdown_on_wait: None,
        }
    }
}

impl TerminalIo for Console {
    fn render(&mut self, commands: &[Command]) -> io::Result<ControlFlow<()>> {
        self.batches.push(commands.to_vec());
        Ok(if self.stop_after_render {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        })
    }

    fn wait_input(&mut self, timeout: Duration) -> io::Result<ControlFlow<(), Vec<Event>>> {
        self.waits.push(timeout);
        if let Some(shutdown) = &self.shutdown_on_wait {
            shutdown.set(true);
        }
        self.reads
            .pop_front()
            .expect("the loop must stop once input ends")
    }

    fn restore(&mut self) -> io::Result<()> {
        self.restored.set(true);
        Ok(())
    }
}

// @internal — Core schedules its next wakeup only when one fires, so without
// the host's first one no heartbeat runs (vauchi/private#1297).
#[test]
fn the_first_wakeup_is_fired_once_before_any_wait() {
    let dispatched = Rc::new(RefCell::new(vec![]));
    let host = ScriptedHost {
        dispatched: dispatched.clone(),
        ..ScriptedHost::default()
    };
    let mut console = Console::reading(vec![Ok(ControlFlow::Break(()))]);

    run_terminal(host, &mut console).unwrap();

    assert_eq!(*dispatched.borrow(), vec![Event::WakeupFired]);
    assert_eq!(console.waits.len(), 1);
}

// @internal — waiting for terminal input must not hold back what the host
// has for the display; every wait is bounded.
#[test]
fn what_the_host_has_is_rendered_before_each_wait() {
    let host = ScriptedHost {
        batches: VecDeque::from([
            vec![Command::PerformNativeBack],
            vec![Command::ResetApplication],
        ]),
        ..ScriptedHost::default()
    };
    let mut console = Console::reading(vec![
        Ok(ControlFlow::Continue(vec![])),
        Ok(ControlFlow::Break(())),
    ]);

    run_terminal(host, &mut console).unwrap();

    assert_eq!(
        console.batches,
        vec![
            vec![Command::PerformNativeBack],
            vec![Command::ResetApplication]
        ]
    );
    assert_eq!(console.waits, vec![Duration::from_millis(50); 2]);
}

// @internal — input reaches the host unchanged, and quitting reads no more.
#[test]
fn input_is_dispatched_unchanged_and_quit_reads_no_more() {
    let dispatched = Rc::new(RefCell::new(vec![]));
    let host = ScriptedHost {
        dispatched: dispatched.clone(),
        ..ScriptedHost::default()
    };
    let typed = Event::QrScanned {
        data: "typed".into(),
    };
    let mut console = Console::reading(vec![
        Ok(ControlFlow::Continue(vec![typed.clone()])),
        Ok(ControlFlow::Break(())),
    ]);

    run_terminal(host, &mut console).unwrap();

    assert_eq!(*dispatched.borrow(), vec![Event::WakeupFired, typed]);
    assert_eq!(console.waits.len(), 2);
}

// @internal — an input failure returns at once, with its kind, instead of
// spinning on EOF.
#[test]
fn an_input_failure_ends_the_loop_with_its_kind() {
    let mut console = Console::reading(vec![Err(io::ErrorKind::BrokenPipe.into())]);

    let error = run_terminal(ScriptedHost::default(), &mut console).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(console.waits.len(), 1);
}

// @internal — a host that cannot take an event ends the loop with an error
// and leaves the terminal restored.
#[test]
fn a_host_failure_ends_the_loop_and_restores_the_terminal() {
    let host = ScriptedHost {
        fail_dispatch: true,
        ..ScriptedHost::default()
    };
    let mut console = Console::reading(vec![]);

    let error = run_terminal(host, &mut console).unwrap_err();

    assert_eq!(error.to_string(), "host failed");
    assert!(console.restored.get());
    assert!(console.waits.is_empty());
}

// @internal — a display that finished must not enter another input wait.
#[test]
fn a_finished_display_stops_before_reading_input() {
    let host = ScriptedHost {
        batches: VecDeque::from([vec![Command::PerformNativeBack]]),
        ..ScriptedHost::default()
    };
    let mut console = Console::reading(vec![]);
    console.stop_after_render = true;

    run_terminal(host, &mut console).unwrap();

    assert_eq!(console.batches, vec![vec![Command::PerformNativeBack]]);
    assert!(console.waits.is_empty());
}

// @internal — a shutdown request (a signal, a console close) ends the loop
// after the wait it arrived in, without dispatching that wait's input.
#[test]
fn a_shutdown_request_ends_the_loop_without_dispatching_further_input() {
    let dispatched = Rc::new(RefCell::new(vec![]));
    let host = ScriptedHost {
        dispatched: dispatched.clone(),
        ..ScriptedHost::default()
    };
    let shutdown = Rc::new(Cell::new(false));
    let mut console = Console::reading(vec![Ok(ControlFlow::Continue(vec![Event::WakeupFired]))]);
    console.shutdown_on_wait = Some(shutdown.clone());

    run_terminal_with_shutdown(host, &mut console, || shutdown.get()).unwrap();

    assert_eq!(*dispatched.borrow(), vec![Event::WakeupFired]);
    assert_eq!(console.waits.len(), 1);
    assert!(console.restored.get());
}

// @internal — restoring the console precedes dropping the host, whose
// hardware teardown may wait on a native backend.
#[test]
fn the_terminal_is_restored_before_the_host_is_dropped() {
    for case in ["quit", "io-error", "panic"] {
        let reads = match case {
            "quit" => vec![Ok(ControlFlow::Break(()))],
            "io-error" => vec![Err(io::ErrorKind::BrokenPipe.into())],
            _ => vec![],
        };
        let mut console = Console::reading(reads);
        let observed = Rc::new(Cell::new(None));
        let host = ScriptedHost {
            _dropped_after_restore: Some(DropProbe {
                restored: console.restored.clone(),
                observed: observed.clone(),
            }),
            ..ScriptedHost::default()
        };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_terminal(host, &mut console)
        }));
        let outcome = match result {
            Ok(Ok(())) => "quit",
            Ok(Err(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
                "io-error"
            }
            Err(_) => "panic",
        };

        assert_eq!(outcome, case);
        assert_eq!(observed.get(), Some(true), "{case}");
    }
}

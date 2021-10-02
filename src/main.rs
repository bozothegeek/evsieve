// SPDX-License-Identifier: GPL-2.0-or-later

// Allowed because useless default implementations are dead lines of code.
#![allow(clippy::new_without_default)]

// Allowed because the key "" is a canonically valid key, and comparing a key to "" is more
// idiomatic than asking whether a key is empty.
#![allow(clippy::comparison_to_empty)]

// Allowed because nested ifs allow for more-readable code.
#![allow(clippy::collapsible_if)]

// Allowed because the matches! macro is not supported in Rust 1.41.1, under which evsieve must compile.
#![allow(clippy::match_like_matches_macro)]

// Disallowed for code uniformity.
#![warn(clippy::explicit_iter_loop)]

pub mod event;
pub mod key;
pub mod map;
pub mod domain;
pub mod state;
pub mod signal;
pub mod utils;
pub mod error;
pub mod capability;
pub mod range;
pub mod stream;
pub mod ecodes;
pub mod sysexit;
pub mod hook;
pub mod predevice;
pub mod print;
pub mod subprocess;
pub mod daemon;

pub mod io {
    pub mod input;
    pub mod epoll;
    pub mod output;
    pub mod loopback;
    pub mod internal_pipe;
}

pub mod persist {
    pub mod inotify;
    pub mod blueprint;
    pub mod subsystem;
}

pub mod arguments {
    pub mod hook;
    pub mod parser;
    pub mod input;
    pub mod output;
    pub mod lib;
    pub mod map;
    pub mod toggle;
    pub mod print;
}

pub mod bindings {
    #[allow(warnings)]
    pub mod libevdev;
}

#[macro_use]
extern crate lazy_static;

use std::os::unix::prelude::{AsRawFd, RawFd};

use error::{InterruptError, RuntimeError, Context};
use io::epoll::{Epoll, FileIndex, Message};
use io::input::InputDevice;
use signal::{SigMask, SignalFd};

fn main() {
    let result = run_and_interpret_exit_code();
    subprocess::terminate_all();
    std::process::exit(result)
}

fn run_and_interpret_exit_code() -> i32 {
    let result = std::panic::catch_unwind(run);

    match result {
        Ok(Ok(())) => 0,
        // A RuntimeError happened.
        Ok(Err(error)) => {
            eprintln!("{}", error);
            1
        },
        // A panic happened.
        Err(_) => {
            eprintln!("Internal error: a panic happened. This is a bug.");
            1
        },
    }
}

enum Pollable {
    InputDevice(InputDevice),
    SignalFd(SignalFd),
}
impl AsRawFd for Pollable {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Pollable::InputDevice(device) => device.as_raw_fd(),
            Pollable::SignalFd(fd) => fd.as_raw_fd(),
        }
    }
}

fn run() -> Result<(), RuntimeError> {
    let args: Vec<String> = std::env::args().collect();
    if arguments::parser::check_help_and_version(&args) {
        daemon::notify_ready();
        return Ok(());
    }

    let (mut setup, input_devices) = arguments::parser::implement(args)?;
    let mut epoll = Epoll::new()?;
    for device in input_devices {
        unsafe { epoll.add_file(Pollable::InputDevice(device))? };
    }

    let signal_fd = SignalFd::new(SigMask::new().fill().del(libc::SIGCHLD));
    unsafe { epoll.add_file(Pollable::SignalFd(signal_fd))? };
    let _signal_block = signal::block();

    daemon::notify_ready();

    'mainloop: loop {
        let messages = match epoll.poll() {
            Ok(res) => res,
            Err(InterruptError {}) => return Ok(()),
        };

        for message in messages {
            match message {
                Message::Ready(index) => {
                    let file = &mut epoll[index];
                    match file {
                        Pollable::InputDevice(device) => {
                            match device.poll() {
                                Ok(events) => for event in events {
                                    stream::run(&mut setup, event);
                                },
                                Err(error) => {
                                    error.print_err();
                                    handle_broken_file(&mut epoll, index);
                                }
                            }
                        },
                        Pollable::SignalFd(fd) => {
                            match fd.read_raw() {
                                Ok(siginfo) => {
                                    let signal_no = siginfo.ssi_signo as i32;
                                    match signal_no {
                                        libc::SIGINT | libc::SIGTERM | libc::SIGHUP => {
                                            break 'mainloop Ok(());
                                        },
                                        // TODO: handle other signals
                                        _ => {},
                                    }
                                },
                                Err(error) => match error.kind() {
                                    std::io::ErrorKind::Interrupted => continue,
                                    // TODO
                                    _ => panic!("Internal error: signalfd broken."),
                                }
                            }
                        }
                    }
                },
                Message::Broken(index) => {
                    handle_broken_file(&mut epoll, index);
                }
            }
        }
    }
}

fn handle_broken_file(epoll: &mut Epoll<Pollable>, index: FileIndex) {
    let broken_device = epoll.remove(index);
    if let Pollable::InputDevice(device) = broken_device {
        eprintln!("The device {} has been disconnected.", device.path().display());
    }
}
// SPDX-License-Identifier: GPL-2.0-or-later

use crate::error::{InterruptError, SystemError, RuntimeError};
use crate::event::Event;
use crate::sysexit;
use crate::error::Context;
use crate::signal;
use std::collections::HashMap;
use std::os::unix::io::{AsRawFd, RawFd};

/// Like a file descriptor, that identifies a file registered in this Epoll.
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub struct FileIndex(u64);

/// The epoll is responsible for detecting which input devices have events available.
/// The evsieve program spends most of its time waiting on Epoll::poll, which waits until
/// some input device has events available.
/// 
/// It also keeps track of when input devices unexpectedly close.
pub struct Epoll {
    fd: RawFd,
    files: HashMap<FileIndex, Box<dyn Pollable>>,
    /// A counter, so every file registered can get an unique index in the files map.
    counter: u64,
    /// Ensures that no signals are delivered during inopportune moments while an Epoll exists.
    /// Epoll::poll() should be the only moment during which signals are allowed to be delivered.
    signal_block: std::sync::Arc<signal::SignalBlock>,
}

/// Represents all messages files added to the Epoll may return.
pub enum Message {
    Event(Event),
    BrokenDevice(Box<dyn Pollable>),
}

pub trait Pollable : AsRawFd {
    /// Should return a list of events that were polled by this device.
    ///
    /// If this function returns Err, then the device is considered broken and shall be
    /// removed from this epoll and reduced. If it returns Err(Some), then said error shall
    /// also be printed. Err(None) is considered a request "nothing is wrong, just reduce me"
    /// and will cause the device to be silently reduced.
    fn poll(&mut self) -> Result<Vec<Message>, Option<RuntimeError>>;
}

impl Epoll {
    pub fn new() -> Result<Epoll, SystemError> {
        let epoll_fd = unsafe {
            libc::epoll_create1(libc::EPOLL_CLOEXEC)
        };
        if epoll_fd < 0 {
            return Err(SystemError::os_with_context("While trying to create an epoll instance:"));
        }

        Ok(Epoll {
            fd: epoll_fd,
            files: HashMap::new(),
            counter: 0,
            signal_block: signal::block(),
        })
    }

    fn get_unique_index(&mut self) -> FileIndex {
        self.counter += 1;
        FileIndex(self.counter)
    }

    /// # Safety
    /// The file must return a valid raw file descriptor.
    pub unsafe fn add_file(&mut self, file: Box<dyn Pollable>) -> Result<(), SystemError> {
        let index = self.get_unique_index();
        let file_fd = file.as_raw_fd();

        // Sanity check: make sure we don't add a file that already belongs to this epoll.
        if self.files.values().any(|opened_file| opened_file.as_raw_fd() == file_fd) {
            return Err(SystemError::new("Cannot add a file to an epoll that already belongs to said epoll."));
        }
        self.files.insert(index, file);

        // We set the data to the index of said file, so we know which file is ready for reading.
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: index.0,
        };

        let result = libc::epoll_ctl(
            self.fd,
            libc::EPOLL_CTL_ADD,
            file_fd,
            &mut event,
        );

        if result < 0 {
            Err(SystemError::os_with_context("While adding a device to an epoll instance:"))
        } else {
            Ok(())
        }
    }

    fn remove_file_by_index(&mut self, index: FileIndex) -> Box<dyn Pollable> {
        let file = match self.files.remove(&index) {
            Some(file) => file,
            None => panic!("Internal error: attempted to remove a device from an epoll that's not registered with it."),
        };

        let result = unsafe { libc::epoll_ctl(
            self.fd,
            libc::EPOLL_CTL_DEL,
            file.as_raw_fd(),
            std::ptr::null_mut(),
        )};

        if result < 0 {
            match std::io::Error::last_os_error().raw_os_error()
                    .expect("An unknown error occurred while removing a file from an epoll.") {
                // This file was not registered by this epoll.
                libc::ENOENT => eprintln!("Internal error: attempted to remove a device from an epoll that's not registered with it."),
                // There was not enough memory to carry out this operation.
                libc::ENOMEM => panic!("Out of kernel memory."),
                // The other error codes should never happen or indicate fundamentally broken invariants.
                _ => panic!("Failed to remove a file from an epoll: {}", std::io::Error::last_os_error()),
            }
        }

        file
    }

    fn poll_raw(&mut self) -> Result<Vec<libc::epoll_event>, std::io::Error> {
        // The number 8 was chosen arbitrarily.
        let max_events: i32 = std::cmp::min(self.files.len(), 8) as i32;
        let mut events: Vec<libc::epoll_event> = (0 .. max_events).map(|_| libc::epoll_event {
            // The following values don't matter since the kernel will overwrite them anyway.
            // We're just initialzing them to make the compiler happy.
            events: 0, u64: 0
        }).collect();

        let result = unsafe {
            libc::epoll_pwait(
                self.fd,
                events.as_mut_ptr(),
                max_events,
                -1, // timeout, -1 means it will wait indefinitely
                self.signal_block.orig_sigmask(), // Accept signals only while polling.
            )
        };

        if result < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            let num_fds = result as usize;
            Ok(events[0..num_fds].to_owned())
        }
    }

    /// Tries to read all events from all ready devices. Returns a vector containing all events read.
    /// If a device reports an error, said device is removed from self and also returned.
    pub fn poll(&mut self) -> Result<Vec<Message>, InterruptError> {
        let events = loop {
            match self.poll_raw() {
                Ok(events) => break events,
                Err(error) => match error.kind() {
                    std::io::ErrorKind::Interrupted => {
                        if sysexit::should_exit() {
                            return Err(InterruptError::new())
                        } else {
                            continue;
                        }
                    },
                    _ => {
                        if self.is_empty() {
                            eprintln!("No input devices to poll events from; evsieve will exit now.");
                        } else {
                            eprintln!("Fatal error while polling for events: {}", error);
                        }
                        return Err(InterruptError::new());
                    }
                }
            }
        };

        // Create a list of which devices are ready and which are broken.
        let mut ready_file_indices: Vec<FileIndex> = Vec::new();
        let mut broken_file_indices: Vec<FileIndex> = Vec::new();

        for event in events {
            let file_index = event.u64;
            if event.events & libc::EPOLLIN as u32 != 0 {
                ready_file_indices.push(FileIndex(file_index));
            }
            if event.events & libc::EPOLLERR as u32 != 0 || event.events & libc::EPOLLHUP as u32 != 0 {
                broken_file_indices.push(FileIndex(file_index));
            }
        }

        // Retrieve all results from ready devices.
        let mut polled_results: Vec<Message> = Vec::new();
        for index in ready_file_indices {
            if let Some(file) = self.files.get_mut(&index) {
                match file.poll() {
                    Ok(results) => polled_results.extend(results),
                    Err(error_opt) => {
                        if let Some(error) = error_opt {
                            error.print_err();
                        }
                        if ! broken_file_indices.contains(&index) {
                            broken_file_indices.push(index);
                        }
                    },
                }
            }
        }

        // Remove the broken devices from self.
        for index in broken_file_indices {
            let broken_file = self.remove_file_by_index(index);
            polled_results.push(Message::BrokenDevice(broken_file));
        }

        Ok(polled_results)
    }

    /// Returns whether currently any files are opened under this epoll.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl Drop for Epoll {
    fn drop(&mut self) {
        let res = unsafe {
            libc::close(self.fd)
        };
        if res < 0 {
            SystemError::os_with_context("While closing an epoll file descriptor:").print_err();
        }
    }
}

// SPDX-License-Identifier: GPL-2.0-or-later

use crate::io::input::InputDevice;
use crate::predevice::PreInputDevice;
use crate::capability::Capabilities;
use crate::error::SystemError;
use std::collections::HashMap;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;


pub struct InputDeviceBlueprint {
    pub pre_device: PreInputDevice,
    pub capabilities: Capabilities,
}

impl InputDeviceBlueprint {
    /// Tries to reopen the device from which this blueprint was generated.
    /// On success, returns the device. On failure, returns Err(self).
    pub fn try_open(self) -> Result<InputDevice, InputDeviceBlueprint> {
        if ! self.pre_device.path.exists() {
            return Err(self);
        }
        let input_device = match InputDevice::open(self.pre_device.clone()) {
            Ok(device) => device,
            Err(_) => return Err(self),
        };
        if *input_device.capabilities() != self.capabilities {
            // TODO: do not retry if this happens.
            eprintln!("Error: cannot reopen input device {}: this device's capabilities are different from the original device that disconnected.", self.pre_device.path.display());
            return Err(self);
        }
        Ok(input_device)
    }
}

pub struct Inotify {
    fd: RawFd,
    watches: HashMap<i32, PathBuf>,
}

impl Inotify {
    pub fn new() -> Result<Inotify, SystemError> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK) };
        if fd < 0 {
            return Err(SystemError::new("Failed to initialize an inotify instance."));
        }
        Ok(Inotify { fd, watches: HashMap::new() })
    }

    pub fn add_watch(&mut self, path: PathBuf) -> Result<(), SystemError> {
        let watch = unsafe {
            libc::inotify_add_watch(
                self.fd,
                path.as_os_str().as_bytes().as_ptr() as *const i8,
                libc::IN_CREATE | libc::IN_MOVED_TO
            )
        };
        if watch < 0 {
            return Err(SystemError::new(format!("Failed to add \"{}\" to an inotify instance.", path.display())));
        }
        self.watches.insert(watch, path);
        Ok(())
    }

    /// Does nothing besides clearing out the queued events.
    pub fn poll(&mut self) {
        // TODO: get this value from somewhere.
        const NAME_MAX: usize = 255;
        const BUFFER_SIZE: usize = std::mem::size_of::<libc::inotify_event>() + NAME_MAX + 1;
        let mut buffer: [u8; BUFFER_SIZE] = [0; BUFFER_SIZE];
        let res = unsafe {
            libc::read(self.fd, buffer.as_mut_ptr() as *mut libc::c_void, BUFFER_SIZE)
        };
        if res < 0 {
            eprintln!("Failed to read from an Inotify instance.");
        }
    }
}

impl AsRawFd for Inotify {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for Inotify {
    fn drop(&mut self) {
        // Ignore any errors because we can't do anything about them.
        unsafe { libc::close(self.fd); }
    }
}
// SPDX-License-Identifier: GPL-2.0-or-later

use std::sync::Mutex;
use std::ffi::CStr;
use libc::c_char;

pub mod shelllex;

pub fn split_once<'a>(value: &'a str, deliminator: &str) -> (&'a str, Option<&'a str>) {
    let mut splitter = value.splitn(2, deliminator);
    (splitter.next().unwrap(), splitter.next())
}

/// Tries to turn a raw pointer into a String.
/// Returns None if the pointer is a null-pointer, or the string is not valid UTF8.
///
/// # Safety
/// The pointer must be either the null-pointer, or have a valid trailing 0-byte.
pub unsafe fn parse_cstr(raw_ptr: *const c_char) -> Option<String> {
    if raw_ptr.is_null() {
        return None;
    }
    let raw_cstr: &CStr = CStr::from_ptr(raw_ptr);
    raw_cstr.to_str().ok().map(str::to_string)
}

lazy_static!{
    static ref PRINTED_WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
}

/// Prints a warning to the user using stderr, but only prints each unique message once.
pub fn warn_once(message: impl Into<String>) {
    let message: String = message.into();
    if let Ok(mut printed_warnings) = PRINTED_WARNINGS.lock() {
        if ! printed_warnings.contains(&message) {
            eprintln!("{}", message);
            printed_warnings.push(message);
        }
    } else {
        eprintln!("Warning: internal lock poisoned.");
        eprintln!("{}", message);
    }
}

/// Like str::parse, but a bit more stringent, in that it also rejects floats containing e+ or NaN.
/// Also interprets "" as 1.0 and "-" as -1.0.
pub fn parse_number(string: &str) -> Option<f64> {
    const ALLOWED_CHARS: &str = "0123456789-.";
    match string {
        "-" => Some(-1.0),
        "" => Some(1.0),
        _ => if string.chars().all(|c| ALLOWED_CHARS.contains(c)) {
            string.parse().ok()
        } else {
            None
        }
    }
}

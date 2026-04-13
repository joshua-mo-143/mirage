//! Re-exported Mirage server runtime used by other workspace binaries.
#![warn(missing_docs)]

#[allow(dead_code)]
#[path = "main.rs"]
mod main_impl;

pub use main_impl::run;

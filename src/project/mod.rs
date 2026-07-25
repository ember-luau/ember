//! The files a project owns: its manifest, its lockfile, and how an installed
//! package's own files are read back.

pub mod hooks;
pub mod lockfile;
pub mod manifest;
pub mod package;

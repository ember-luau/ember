//! The files a project owns: its manifest, its lockfile, how an installed
//! package's own files are read back, and the workspace a project belongs to.

pub mod hooks;
pub mod lockfile;
pub mod manifest;
pub mod package;
pub mod workspace;

/*! ember as a library. `embr` is the command line over it, and [`api`] is
the part other tools call. The Alloy CLI runs `alloy add`, `alloy install`,
and `alloy publish` through it, so both tools share one registry, one
lockfile, and one package store.

The other modules are embr's internals. They are public for the binary
only and carry no stability promise. */

pub mod api;

#[doc(hidden)]
pub mod art;
#[doc(hidden)]
pub mod commands;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod net;
#[doc(hidden)]
pub mod project;
#[doc(hidden)]
pub mod registry;
#[doc(hidden)]
pub mod sys;
#[doc(hidden)]
pub mod tools;
#[doc(hidden)]
pub mod ui;

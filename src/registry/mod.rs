//! Where packages come from: index lookup, dependency resolution, and the
//! archive a publish uploads.
//!
//! TODO(api): this is the layer the lpm API replaces. `index` and `resolver`
//! speak to git indices (wally and pesde format); `pack` builds the tarball
//! that publishing sends.

pub mod index;
pub mod pack;
pub mod resolver;

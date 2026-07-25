//! Talking to remote services: the HTTP wrapper, GitHub's REST API, and the
//! OAuth device flow layered on top of them.

pub mod auth;
pub mod github;
pub mod http;

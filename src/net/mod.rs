//! Talking to remote services: the HTTP wrapper, GitHub's REST API, the
//! OAuth device flow layered on them, and lpm's own registry API.

pub mod auth;
pub mod github;
pub mod http;
pub mod registry;

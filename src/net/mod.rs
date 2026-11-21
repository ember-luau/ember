/*!
Remote services. The HTTP wrapper, GitHub's REST API, embr's own registry
API, and the browser login on top of it.
*/

pub mod auth;
pub mod github;
pub mod github_template;
pub mod http;
pub mod offline;
pub mod provenance;
pub mod registry;

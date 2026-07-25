// Nothing dispatches these yet, and CI runs clippy with -D warnings, so the
// stub needs this until something does.
#[allow(dead_code)]
pub enum Hooks {
    PreInstall,
    PostInstall,
    PreBuild,
    Build,
    PostBuild,
}

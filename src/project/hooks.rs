// nothing dispatches these yet. clippy runs with -D warnings, so the allow stays until something does.
#[allow(dead_code)]
pub enum Hooks {
    PreInstall,
    PostInstall,
    PreBuild,
    Build,
    PostBuild,
}

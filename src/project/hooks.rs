/*! Lifecycle hooks: the `pre<name>` and `post<name>` entries under [scripts].

npm's model, kept deliberately. A hook is not a new kind of thing, it is an
ordinary [scripts] entry that lpm looks up by name, so there is no second
table to learn and no fixed list of hooks to be allowed into -- whatever a
user writes under [scripts] can hook whatever they run.

Two sources of names. Commands hook their own verb, so `lpm install` runs
`preinstall` and `postinstall` (see the consts below), and every script
hooks its own name, so `lpm run build` runs `prebuild`, `build`, `postbuild`.
`prebuild` therefore costs nothing to support: it falls out of the same rule.

Hooks never nest. Running `prebuild` does not look for `preprebuild`, so a
hook is free to call `lpm run` without lpm walking into itself. */

use crate::error::Error;
use crate::project::manifest::Manifest;
use crate::sys::process;
use crate::ui;

/// hooked by `lpm install`, as `preinstall` / `postinstall`.
pub const INSTALL: &str = "install";
/// hooked by `lpm add`, as `preadd` / `postadd`.
pub const ADD: &str = "add";
/// hooked by `lpm publish`, as `prepublish` / `postpublish`.
pub const PUBLISH: &str = "publish";

/** The two hooks one event has in one manifest, resolved up front.

Resolving early buys two things. The command may consume its `Manifest`
afterwards (publish does), and both lookups happen before either script
runs, so a run can never get half way through a pair because the second
name was misspelled -- it just isn't there, and absent means skipped. */
pub struct Lifecycle {
    /// "scope/name@version" for the banner each hook prints, absent in a project with no [package].
    package: Option<String>,
    event: String,
    pre: Option<String>,
    post: Option<String>,
}

impl Lifecycle {
    /// the `pre<event>`/`post<event>` scripts `manifest` defines, if any.
    pub fn of(manifest: &Manifest, event: &str) -> Self {
        Lifecycle {
            package: manifest.id(),
            event: event.to_string(),
            pre: manifest.scripts.get(&format!("pre{event}")).cloned(),
            post: manifest.scripts.get(&format!("post{event}")).cloned(),
        }
    }

    /// runs `pre<event>`, before the command does any work.
    pub fn before(&self) -> Result<(), Error> {
        self.hook(&format!("pre{}", self.event), self.pre.as_deref())
    }

    /** runs `post<event>`, after the command succeeded. call sites reach
    this through `?` on the work itself, so a failed command never gets a
    post hook -- "postpublish" should mean the publish happened. */
    pub fn after(&self) -> Result<(), Error> {
        self.hook(&format!("post{}", self.event), self.post.as_deref())
    }

    /** Runs one hook, or nothing when the manifest never defined it.

    Announced with the same banner a named script gets, since a hook is
    exactly that -- and nobody typed this one, so attributing its output
    matters more here than anywhere. A non-zero exit becomes an error
    naming the hook rather than the silent exit an explicitly typed script
    gets; the child's code still survives, see `Error::exit_code`. */
    fn hook(&self, name: &str, script: Option<&str>) -> Result<(), Error> {
        let Some(script) = script else {
            return Ok(());
        };

        ui::print_script_notice(self.package.as_deref(), name, script);
        match process::wait(process::shell(script))? {
            0 => Ok(()),
            code => Err(Error::HookFailed {
                hook: name.to_string(),
                code,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(scripts: &str) -> Manifest {
        toml::from_str(&format!(
            "[package]\nname = \"scope/name\"\nversion = \"0.1.0\"\n\n[scripts]\n{scripts}"
        ))
        .unwrap()
    }

    #[test]
    fn picks_up_both_hooks_of_an_event() {
        let manifest = manifest(
            "prebuild = \"echo before\"\nbuild = \"echo main\"\npostbuild = \"echo after\"\n",
        );
        let lifecycle = Lifecycle::of(&manifest, "build");
        assert_eq!(lifecycle.pre.as_deref(), Some("echo before"));
        assert_eq!(lifecycle.post.as_deref(), Some("echo after"));
    }

    #[test]
    fn command_events_hook_their_verb() {
        let manifest = manifest("preinstall = \"a\"\npostpublish = \"b\"\n");

        let install = Lifecycle::of(&manifest, INSTALL);
        assert_eq!(install.pre.as_deref(), Some("a"));
        assert!(install.post.is_none());

        let publish = Lifecycle::of(&manifest, PUBLISH);
        assert!(publish.pre.is_none());
        assert_eq!(publish.post.as_deref(), Some("b"));

        // an event nobody hooked is two absent scripts, not an error
        let add = Lifecycle::of(&manifest, ADD);
        assert!(add.pre.is_none() && add.post.is_none());
    }

    #[test]
    fn hooks_do_not_nest() {
        /* `preprebuild` is a script like any other, but nothing looks for
        it: running `prebuild` as a hook goes straight to the shell. */
        let manifest = manifest("preprebuild = \"never\"\nprebuild = \"echo before\"\n");
        let lifecycle = Lifecycle::of(&manifest, "prebuild");
        // only reachable by typing `lpm run prebuild` yourself
        assert_eq!(lifecycle.pre.as_deref(), Some("never"));

        let build = Lifecycle::of(&manifest, "build");
        assert_eq!(build.pre.as_deref(), Some("echo before"));
    }

    #[test]
    fn absent_hooks_run_nothing() {
        let lifecycle = Lifecycle::of(&manifest("build = \"echo main\"\n"), "build");
        assert!(lifecycle.before().is_ok());
        assert!(lifecycle.after().is_ok());
    }

    #[test]
    fn a_failing_hook_reports_its_name_and_code() {
        // `exit 3` is spelled the same for sh and cmd
        let manifest = manifest("prebuild = \"exit 3\"\n");
        assert!(matches!(
            Lifecycle::of(&manifest, "build").before(),
            Err(Error::HookFailed { hook, code }) if hook == "prebuild" && code == 3
        ));
    }
}

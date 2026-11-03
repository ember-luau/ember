/*! `embr audit`: the security notices and the yanked versions of the
installed packages. It is `embr notices` with fewer kinds, and it fails on
a security notice. See `commands::notices`. */

use crate::commands::notices::{self, Deny};
use crate::error::Error;
use crate::registry::notices::Kind;
use clap::Args;

#[derive(Args, Debug)]
pub struct AuditArgs {
    /// Also exit with an error when an item of this kind applies, such as `--deny yanked`
    #[arg(long, value_enum, value_delimiter = ',', value_name = "KIND")]
    pub deny: Vec<Deny>,
}

/** lists the security notices and the yanks that apply to ember.lock,
direct and transitive. it exits 1 when a security notice applies, or an
item of a kind that `--deny` names. a kind that `--deny` names also
shows, so the list says why the check fails. */
pub fn run(args: AuditArgs) -> Result<(), Error> {
    let deny = denied(&args.deny);
    notices::list(
        |kind| matches!(kind, Kind::Security | Kind::Yanked) || deny.contains(&kind),
        &deny,
        "No security notice or yanked version applies to the installed packages",
    )
}

/// the kinds that fail the audit: a security notice always, and each kind of `deny`.
fn denied(deny: &[Deny]) -> Vec<Kind> {
    let mut kinds = vec![Kind::Security];
    kinds.extend(deny.iter().map(|deny| deny.kind()));
    kinds.sort();
    kinds.dedup();
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_security_notice_always_fails_the_audit() {
        assert_eq!(denied(&[]), [Kind::Security]);
        assert_eq!(
            denied(&[Deny::Warnings, Deny::Security, Deny::Yanked]),
            [Kind::Security, Kind::Warning, Kind::Yanked]
        );
    }
}

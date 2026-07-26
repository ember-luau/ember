use crate::{error::Error, project::manifest::Manifest, sys::process};
use clap::Args;

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Name of the script under [scripts] in lpm.toml
    pub name: String,
}

pub fn run(args: RunArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;
    let script = manifest.script(&args.name)?;

    /* wait rather than exec: a failing script should exit with its own code
    (CI reads it), and a successful one still gets lpm's "Done in" line */
    let code = process::wait(process::shell(script))?;
    if code != 0 {
        std::process::exit(code);
    }

    Ok(())
}

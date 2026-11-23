/*! `embr pack`: the archive that `embr publish` would upload, as a file.
it runs the publish steps themselves, hooks and builds included, and takes
the archive at the point where the upload would start. a second copy of
those steps would drift, and the file would then differ from the upload. */

use crate::commands::publish::{self, PublishArgs};
use crate::error::Error;
use crate::project::manifest::Manifest;
use crate::registry::pack;
use crate::sys::hash::sha256_hex;
use crate::ui;
use clap::Args;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

#[derive(Args, Debug)]
pub struct PackArgs {
    /// File to write; the default is <name>-<version>-<target>.tar.gz in the package folder
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Pack only this target, such as `roblox` or `roblox-alloy`; repeat it for more
    #[arg(long = "target", value_name = "ID")]
    pub targets: Vec<String>,
    /// Print the result as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

/// where the archives of this run go, and the ones that are written.
struct Sink {
    output: Option<PathBuf>,
    quiet: bool,
    written: Vec<Value>,
}

/** set while `embr pack` runs. one switch for the process, as
`net::offline` is: the publish steps ask it at the upload, so they take no
argument that only this command sets. */
static SINK: Mutex<Option<Sink>> = Mutex::new(None);

pub fn run(args: PackArgs) -> Result<(), Error> {
    // absolute, because a workspace publish moves into each member folder
    let output = args.output.map(std::path::absolute).transpose()?;
    *SINK.lock().unwrap_or_else(PoisonError::into_inner) = Some(Sink {
        output,
        quiet: args.json,
        written: Vec::new(),
    });
    let result = publish::run(PublishArgs {
        dry_run: true,
        targets: args.targets,
        local: false,
        json: false,
        channel: None,
        notes: None,
    });
    let sink = SINK.lock().unwrap_or_else(PoisonError::into_inner).take();
    result?;
    let written = sink.map(|sink| sink.written).unwrap_or_default();
    if args.json {
        return ui::print_json(&json!({ "version": ui::JSON_VERSION, "archives": written }));
    }
    if written.is_empty() {
        println!("Nothing to pack");
    }
    Ok(())
}

/** takes `archive`, the packed target `id` of `manifest`, when `embr pack`
runs. None in any other run, and the publish goes on. */
pub(crate) fn intercept(
    manifest: &Manifest,
    id: &str,
    archive: &[u8],
) -> Option<Result<(), Error>> {
    let mut sink = SINK.lock().unwrap_or_else(PoisonError::into_inner);
    let sink = sink.as_mut()?;
    Some(write(sink, manifest, id, archive))
}

fn write(sink: &mut Sink, manifest: &Manifest, id: &str, archive: &[u8]) -> Result<(), Error> {
    let package = manifest.package.as_ref().ok_or(Error::PackageMissing)?;
    let file = match &sink.output {
        Some(_) if !sink.written.is_empty() => {
            return Err(std::io::Error::other(
                "--output names one file, and this run packs more than one archive; pass `--target <id>` in the folder of one package, or leave --output out",
            )
            .into());
        }
        Some(output) => output.clone(),
        None => PathBuf::from(pack::archive_name(&package.name, &package.version, id)),
    };
    std::fs::write(&file, archive)?;
    let shown = std::path::absolute(&file)?;
    /* the default name is never packed, see `pack::archive_name`. a file
    that --output puts in the package folder under another name is */
    let inside = std::env::current_dir()
        .ok()
        .and_then(|here| shown.strip_prefix(here).ok().map(Path::to_path_buf))
        .is_some_and(|relative| {
            pack::packed_files(Path::new("."), manifest)
                .is_ok_and(|files| files.contains(&relative))
        });
    if inside {
        eprintln!(
            "warning: {} is in the package, so the next `embr pack` or `embr publish` packs it too; delete it, or add it to `excludes`",
            file.display()
        );
    }
    if !sink.quiet {
        ui::print_success(&format!(
            "Packed {}@{} ({id}) to {}, {} bytes",
            package.name,
            package.version,
            shown.display(),
            archive.len()
        ));
    }
    sink.written.push(json!({
        "name": package.name,
        "version": package.version,
        "target": id,
        "file": shown,
        "bytes": archive.len(),
        "sha256": sha256_hex(archive),
    }));
    Ok(())
}

/*! `embr channel`: the channels of a package. a channel is a name, such as
`beta`, for one published version. `embr publish --channel` sets one with
the publish, and this command moves, removes and lists them. versions and
version requirements stay the main way to name what a project installs. */

use crate::commands::yank::as_owner;
use crate::error::Error;
use crate::net::registry;
use crate::project::manifest::{check_channel, split_package_name};
use crate::ui;
use clap::Subcommand;
use std::collections::BTreeMap;

#[derive(Subcommand, Debug)]
pub enum ChannelCommand {
    /// Move a channel to a published version, or make the channel
    Set {
        /// The package, as scope/name
        package: String,
        /// The channel, such as `beta`
        channel: String,
        /// The published version that the channel names
        version: String,
        /// Print the result as one JSON document on stdout
        #[arg(long)]
        json: bool,
    },
    /// Remove a channel. Its versions stay published
    #[command(visible_alias = "rm")]
    Remove {
        /// The package, as scope/name
        package: String,
        /// The channel, such as `beta`
        channel: String,
        /// Print the result as one JSON document on stdout
        #[arg(long)]
        json: bool,
    },
    /// List the channels of a package and the version that each one names
    #[command(visible_alias = "ls")]
    List {
        /// The package, as scope/name
        package: String,
        /// Print the channels as one JSON document on stdout
        #[arg(long)]
        json: bool,
    },
}

impl ChannelCommand {
    pub fn is_json(&self) -> bool {
        match self {
            ChannelCommand::Set { json, .. }
            | ChannelCommand::Remove { json, .. }
            | ChannelCommand::List { json, .. } => *json,
        }
    }
}

pub fn run(command: ChannelCommand) -> Result<(), Error> {
    ui::json_or_text(command.is_json(), || match command {
        ChannelCommand::Set {
            package,
            channel,
            version,
            ..
        } => change(&package, &channel, Some(&version)),
        ChannelCommand::Remove {
            package, channel, ..
        } => change(&package, &channel, None),
        ChannelCommand::List { package, .. } => list(&package),
    })
}

/// sets `channel` of `package` to `version`, or removes it when there is none.
fn change(package: &str, channel: &str, version: Option<&str>) -> Result<serde_json::Value, Error> {
    let name = package.trim().to_lowercase();
    split_package_name(&name)?;
    check_channel(channel)?;
    let version = version.map(semver::Version::parse).transpose()?;

    as_owner(|token| {
        ui::with_spinner("Contacting the registry", || {
            registry::channel(token, &name, channel, version.as_ref())
        })
    })?;

    match &version {
        Some(version) => {
            ui::print_success(&format!(
                "The channel {channel} of {name} now names {version}"
            ));
            println!("A project that has it in ember.lock keeps its version until `embr update`.");
        }
        None => {
            ui::print_success(&format!("Removed the channel {channel} of {name}"));
            println!(
                "Its versions stay published. A dependency on the channel no longer resolves."
            );
        }
    }
    Ok(serde_json::json!({
        "version": ui::JSON_VERSION,
        "name": name,
        "channel": channel,
        "channel_version": version.map(|version| version.to_string()),
    }))
}

/** the channels of `package`, from the registry's package answer and not
from the index copy on this computer: that copy can be minutes old, and a
`set` must show at once. */
fn list(package: &str) -> Result<serde_json::Value, Error> {
    let name = package.trim().to_lowercase();
    split_package_name(&name)?;
    let answer = registry::package_answer(&name)?;
    let channels = channels_of(&answer);
    if channels.is_empty() {
        println!("{name} has no channels");
    }
    let width = channels.keys().map(String::len).max().unwrap_or(0);
    for (channel, version) in &channels {
        println!(
            "{:<width$}  {}",
            ui::strip_controls(channel),
            ui::strip_controls(version)
        );
    }
    Ok(serde_json::json!({
        "version": ui::JSON_VERSION,
        "name": name,
        "channels": channels,
    }))
}

/// the `channels` of a package answer. the key is absent when the package has none.
pub(crate) fn channels_of(answer: &serde_json::Value) -> BTreeMap<String, String> {
    answer["channels"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(channel, version)| Some((channel.clone(), version.as_str()?.to_string())))
        .collect()
}

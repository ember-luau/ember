/*! ~/.ember/config.toml, the settings that hold for every project on this
machine. `embr self default-index` writes it. a project's own ember.toml
always wins over it. */

use crate::error::Error;
use crate::sys::paths;
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::OnceLock;

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct Settings {
    /** the index a dependency without `index` resolves against, when the
    project's [indices] has no `default`. None is ember's own index. */
    #[serde(
        rename = "default-index",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_index: Option<String>,
}

/** the settings on disk. a missing or unreadable file reads as the
defaults, the same rule credentials.toml follows: a bad file must not
break every command. */
pub fn load() -> Settings {
    paths::config_file()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(settings: &Settings) -> Result<(), Error> {
    let path = paths::config_file()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, toml::to_string(settings)?)?;
    Ok(())
}

/** the machine's default index, read once per process. tests never read
the developer's own file, so they see ember's index. */
pub fn default_index() -> Option<&'static str> {
    if cfg!(test) {
        return None;
    }
    static DEFAULT: OnceLock<Option<String>> = OnceLock::new();
    DEFAULT.get_or_init(|| load().default_index).as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_round_trips_and_stays_empty_by_default() {
        assert_eq!(toml::to_string(&Settings::default()).unwrap(), "");
        let set = Settings {
            default_index: Some("https://github.com/pesde-pkg/index".to_string()),
        };
        let text = toml::to_string(&set).unwrap();
        assert!(text.contains("default-index = "), "{text}");
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), set);
    }
}

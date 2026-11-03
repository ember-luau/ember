/*! opt-in features, Cargo-style. a package's `[features]` table maps each
feature name to a list. `default` lists the features that are on unless a
dependent says `default-features = false`. an item of any other list is a
dependency alias of the package, another feature of the package, or
`alias/feature`, which turns on a feature of that dependency.

a dependency that a feature lists by its alias is gated: it installs only
when an enabled feature lists it, by its alias or as `alias/feature`.

the same rules read an ember.toml and an index entry. `is_alias` says which
names are dependency aliases, since an alias can be a scoped `scope/name`
link key and so can hold a `/`. */

use std::collections::{BTreeMap, BTreeSet};

/// a `[features]` table: feature name -> what it turns on.
pub type Features = BTreeMap<String, Vec<String>>;

/// the key that lists the features that are on unless a dependent opts out.
pub const DEFAULT: &str = "default";

/// what one item of a feature's list turns on.
#[derive(Debug, PartialEq, Eq)]
pub enum Item<'a> {
    /// another feature of the package.
    Feature(&'a str),
    /// a dependency of the package, by its alias. this gates it.
    Dependency(&'a str),
    /// a feature of a dependency: its alias and the feature.
    DependencyFeature(&'a str, &'a str),
    /// no feature, no alias, and no `alias/feature`.
    Unknown(&'a str),
}

/** what `item` names. a feature name wins over an alias, which the manifest
check keeps apart. `alias/feature` splits at the last `/`, so a scoped
alias keeps its own `/`. */
pub fn item<'a>(features: &Features, is_alias: &dyn Fn(&str) -> bool, item: &'a str) -> Item<'a> {
    if item != DEFAULT && features.contains_key(item) {
        return Item::Feature(item);
    }
    if is_alias(item) {
        return Item::Dependency(item);
    }
    match item.rsplit_once('/') {
        Some((alias, feature)) if is_alias(alias) && !feature.is_empty() => {
            Item::DependencyFeature(alias, feature)
        }
        _ => Item::Unknown(item),
    }
}

/** whether `name` can name a feature: 1 to 64 ASCII letters, digits, `_`
and `-`, starting with a letter or a digit. */
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/** the checks on a package's own `[features]`: the names, `default`, each
item, and cycles. `aliases` are the keys of its `[dependencies]`. the error
says what to fix. */
pub fn check(features: &Features, aliases: &BTreeSet<&str>) -> Result<(), String> {
    let is_alias = |name: &str| aliases.contains(name);
    for (name, items) in features {
        if !valid_name(name) {
            return Err(format!(
                "'{name}' is not a feature name; use ASCII letters, digits, '_' and '-', 64 at most"
            ));
        }
        if is_alias(name) {
            return Err(format!(
                "feature '{name}' has the name of a dependency alias; rename one of the two"
            ));
        }
        for listed in items {
            match item(features, &is_alias, listed) {
                Item::Feature(_) | Item::Dependency(_) if name != DEFAULT => {}
                Item::Feature(_) => {}
                Item::DependencyFeature(_, feature) if name != DEFAULT => {
                    if !valid_name(feature) {
                        return Err(format!(
                            "feature '{name}' lists '{listed}', and '{feature}' is not a feature name"
                        ));
                    }
                }
                _ if name == DEFAULT => {
                    return Err(format!(
                        "default lists '{listed}', which is not a feature of this package"
                    ));
                }
                _ => {
                    return Err(format!(
                        "feature '{name}' lists '{listed}', which is not a feature of this package, an alias in [dependencies], or 'alias/feature'"
                    ));
                }
            }
        }
    }
    if let Some(cycle) = cycle(features, &is_alias) {
        return Err(format!(
            "feature '{}' turns itself on: {}",
            cycle[0],
            cycle.join(" > ")
        ));
    }
    Ok(())
}

/// the first chain of features that turns its own start back on, if any.
fn cycle(features: &Features, is_alias: &dyn Fn(&str) -> bool) -> Option<Vec<String>> {
    fn walk<'a>(
        features: &'a Features,
        is_alias: &dyn Fn(&str) -> bool,
        name: &'a str,
        path: &mut Vec<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Option<Vec<String>> {
        if let Some(start) = path.iter().position(|seen| *seen == name) {
            let mut cycle: Vec<String> = path[start..].iter().map(|s| s.to_string()).collect();
            cycle.push(name.to_string());
            return Some(cycle);
        }
        if !done.insert(name) {
            return None;
        }
        path.push(name);
        for listed in features.get(name).into_iter().flatten() {
            if let Item::Feature(next) = item(features, is_alias, listed)
                && let Some(cycle) = walk(features, is_alias, next, path, done)
            {
                return Some(cycle);
            }
        }
        path.pop();
        None
    }
    let mut done = BTreeSet::new();
    features
        .keys()
        .find_map(|name| walk(features, is_alias, name, &mut Vec::new(), &mut done))
}

/** the features that `asked` turns on, and with `defaults` the ones that
`default` lists, each with the features it lists in turn. asking for
`default` is the same as `defaults`. Err names an asked feature that the
table does not have. */
pub fn enable<'a>(
    features: &Features,
    asked: impl IntoIterator<Item = &'a str>,
    defaults: bool,
) -> Result<BTreeSet<String>, String> {
    let mut stack: Vec<&str> = Vec::new();
    let mut defaults = defaults;
    for name in asked {
        if name == DEFAULT {
            defaults = true;
        } else if features.contains_key(name) {
            stack.push(name);
        } else {
            return Err(name.to_string());
        }
    }
    if defaults {
        stack.extend(
            features
                .get(DEFAULT)
                .into_iter()
                .flatten()
                .map(String::as_str),
        );
    }
    let mut enabled = BTreeSet::new();
    while let Some(name) = stack.pop() {
        // the manifest check refuses a default item that is no feature
        if name == DEFAULT || !features.contains_key(name) || !enabled.insert(name.to_string()) {
            continue;
        }
        stack.extend(features[name].iter().map(String::as_str));
    }
    Ok(enabled)
}

/// the aliases of the gated dependencies: those that a feature lists by alias.
pub fn gated(features: &Features, is_alias: &dyn Fn(&str) -> bool) -> BTreeSet<String> {
    features
        .iter()
        .filter(|(name, _)| *name != DEFAULT)
        .flat_map(|(_, items)| items)
        .filter_map(|listed| match item(features, is_alias, listed) {
            Item::Dependency(alias) => Some(alias.to_string()),
            _ => None,
        })
        .collect()
}

/** what the `enabled` features turn on: the aliases of the dependencies
they list, and the features that they ask of each dependency. an
`alias/feature` item turns that dependency on too, as in Cargo. */
pub fn turned_on(
    features: &Features,
    enabled: &BTreeSet<String>,
    is_alias: &dyn Fn(&str) -> bool,
) -> (BTreeSet<String>, BTreeMap<String, BTreeSet<String>>) {
    let mut on = BTreeSet::new();
    let mut asked: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in enabled {
        for listed in features.get(name).into_iter().flatten() {
            match item(features, is_alias, listed) {
                Item::Dependency(alias) => {
                    on.insert(alias.to_string());
                }
                Item::DependencyFeature(alias, feature) => {
                    on.insert(alias.to_string());
                    asked
                        .entry(alias.to_string())
                        .or_default()
                        .insert(feature.to_string());
                }
                Item::Feature(_) | Item::Unknown(_) => {}
            }
        }
    }
    (on, asked)
}

/** the gated aliases that `enabled` leaves off. install neither installs
nor links these, so a package finds them with `pcall(require, ...)` only
when a feature is on. */
pub fn off(
    features: &Features,
    enabled: &BTreeSet<String>,
    is_alias: &dyn Fn(&str) -> bool,
) -> BTreeSet<String> {
    let (on, _) = turned_on(features, enabled, is_alias);
    gated(features, is_alias)
        .into_iter()
        .filter(|alias| !on.contains(alias))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Features {
        toml::from_str(text).unwrap()
    }

    const ALIASES: [&str; 3] = ["icons", "svg", "acme/theme"];

    fn is_alias(name: &str) -> bool {
        ALIASES.contains(&name)
    }

    fn checked(text: &str) -> Result<(), String> {
        check(&table(text), &ALIASES.into_iter().collect())
    }

    #[test]
    fn items_name_a_feature_an_alias_or_a_feature_of_a_dependency() {
        let features = table("default = [\"pretty\"]\npretty = [\"icons\", \"svg/fast\"]\n");
        assert_eq!(
            item(&features, &is_alias, "pretty"),
            Item::Feature("pretty")
        );
        assert_eq!(
            item(&features, &is_alias, "icons"),
            Item::Dependency("icons")
        );
        assert_eq!(
            item(&features, &is_alias, "svg/fast"),
            Item::DependencyFeature("svg", "fast")
        );
        // a scoped alias keeps its own `/`
        assert_eq!(
            item(&features, &is_alias, "acme/theme"),
            Item::Dependency("acme/theme")
        );
        assert_eq!(
            item(&features, &is_alias, "acme/theme/dark"),
            Item::DependencyFeature("acme/theme", "dark")
        );
        assert_eq!(
            item(&features, &is_alias, "nope/x"),
            Item::Unknown("nope/x")
        );
        assert_eq!(
            item(&features, &is_alias, "default"),
            Item::Unknown("default")
        );
    }

    #[test]
    fn a_good_table_passes() {
        assert_eq!(
            checked(
                "default = [\"pretty\"]\npretty = [\"icons\", \"fast\", \"acme/theme/dark\"]\n\
                 fast = [\"svg/fast\"]\nplain = []\n"
            ),
            Ok(())
        );
    }

    #[test]
    fn names_unknown_items_and_cycles_are_refused() {
        for (text, error) in [
            (
                "\"has space\" = []\n",
                "'has space' is not a feature name; use ASCII letters, digits, '_' and '-', 64 at most",
            ),
            (
                "icons = []\n",
                "feature 'icons' has the name of a dependency alias; rename one of the two",
            ),
            (
                "default = [\"icons\"]\n",
                "default lists 'icons', which is not a feature of this package",
            ),
            (
                "default = [\"svg/fast\"]\n",
                "default lists 'svg/fast', which is not a feature of this package",
            ),
            (
                "pretty = [\"ghost\"]\n",
                "feature 'pretty' lists 'ghost', which is not a feature of this package, an alias in [dependencies], or 'alias/feature'",
            ),
            (
                "pretty = [\"ghost/dark\"]\n",
                "feature 'pretty' lists 'ghost/dark', which is not a feature of this package, an alias in [dependencies], or 'alias/feature'",
            ),
            (
                "pretty = [\"svg/no way\"]\n",
                "feature 'pretty' lists 'svg/no way', and 'no way' is not a feature name",
            ),
            (
                "a = [\"b\"]\nb = [\"c\"]\nc = [\"a\"]\n",
                "feature 'a' turns itself on: a > b > c > a",
            ),
            (
                "solo = [\"solo\"]\n",
                "feature 'solo' turns itself on: solo > solo",
            ),
        ] {
            assert_eq!(checked(text), Err(error.to_string()), "{text}");
        }
    }

    #[test]
    fn enable_follows_features_and_the_defaults() {
        let features = table(
            "default = [\"pretty\"]\npretty = [\"icons\", \"fast\"]\nfast = [\"svg/fast\"]\nplain = []\n",
        );
        let names = |set: BTreeSet<String>| set.into_iter().collect::<Vec<_>>();
        assert_eq!(
            names(enable(&features, [], true).unwrap()),
            ["fast", "pretty"]
        );
        assert!(enable(&features, [], false).unwrap().is_empty());
        assert_eq!(
            names(enable(&features, ["plain"], false).unwrap()),
            ["plain"]
        );
        assert_eq!(
            names(enable(&features, ["default"], false).unwrap()),
            ["fast", "pretty"]
        );
        assert_eq!(enable(&features, ["icns"], true), Err("icns".to_string()));
        // a package with no table has no features, and the defaults are none
        assert!(enable(&Features::new(), [], true).unwrap().is_empty());
    }

    #[test]
    fn gated_dependencies_turn_on_with_a_feature() {
        let features = table(
            "default = [\"pretty\"]\npretty = [\"icons\"]\nfast = [\"svg/fast\"]\ndark = [\"acme/theme/dark\"]\n",
        );
        assert_eq!(
            gated(&features, &is_alias).into_iter().collect::<Vec<_>>(),
            ["icons"]
        );
        let enabled = enable(&features, ["fast", "dark"], false).unwrap();
        let (on, asked) = turned_on(&features, &enabled, &is_alias);
        assert_eq!(on.into_iter().collect::<Vec<_>>(), ["acme/theme", "svg"]);
        assert_eq!(asked["svg"].iter().collect::<Vec<_>>(), ["fast"]);
        assert_eq!(asked["acme/theme"].iter().collect::<Vec<_>>(), ["dark"]);
        assert_eq!(
            off(&features, &enabled, &is_alias)
                .into_iter()
                .collect::<Vec<_>>(),
            ["icons"]
        );
        let defaults = enable(&features, [], true).unwrap();
        assert!(off(&features, &defaults, &is_alias).is_empty());
    }
}

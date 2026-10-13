//! Configuration: every key of `config.toml`, read from the environment (ADR 0020;
//! CLAUDE.md, invariant 5). Zero configuration is the primary supported configuration:
//! [`Config::default`] is what a driver runs with when nothing is set, and
//! [`Config::from_env`] applies every key whose environment variable is set, refusing a
//! value the key does not take before anything starts. The `cavalry` binary applies its
//! flags on top. [`generated`] holds the keys, written from `config.toml` by
//! `scripts/gen-config.py`; `just lint-config` fails while it is stale or while a key
//! lacks its `doctor` check.
//!
//! ```
//! use cavalry_control::config::{Config, generated::CONDUCTOR_IDLE_STRATEGY};
//! use cavalry_driver::IdleStrategy;
//!
//! // Nothing set: the defaults.
//! let config = Config::from_vars(|_| None).unwrap();
//! assert_eq!(config.conductor_idle_strategy, IdleStrategy::Backoff);
//!
//! // A key set in the environment, as `CAVALRY_CONDUCTOR_IDLE_STRATEGY=yield` would be.
//! let vars = |env: &str| (env == CONDUCTOR_IDLE_STRATEGY.env).then(|| "yield".to_string());
//! assert_eq!(Config::from_vars(vars).unwrap().conductor_idle_strategy, IdleStrategy::Yield);
//!
//! // A value the key does not take is refused, and the message says what to do.
//! let error = Config::from_vars(|_| Some("fast".to_string())).unwrap_err();
//! assert!(error.to_string().contains("backoff, yield, busy-spin"));
//! assert!(error.to_string().ends_with("docs/reference/config.md#dir") || error.to_string().contains("docs/reference/config.md#"));
//! ```

pub mod generated;

use std::fmt;
use std::path::PathBuf;

use cavalry_driver::{Driver, IdleStrategy};

/// What kind of value a key takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A filesystem path.
    Path,
    /// One of [`KeyDef::values`].
    Enum,
}

/// One key of `config.toml`: its names, what it takes, and the text of its row on the
/// reference page.
#[derive(Debug, PartialEq, Eq)]
pub struct KeyDef {
    /// The key's name, dotted: `conductor.idle.strategy`.
    pub name: &'static str,
    /// The environment variable that sets it: `CAVALRY_CONDUCTOR_IDLE_STRATEGY`.
    pub env: &'static str,
    /// The flag on the `cavalry` binary that sets it, when it has one.
    pub flag: Option<&'static str>,
    /// What kind of value it takes.
    pub kind: Kind,
    /// The values an [`Kind::Enum`] key takes, in the order that numbers them; empty
    /// otherwise.
    pub values: &'static [&'static str],
    /// The default: the value, or for a path the rule in words.
    pub default: &'static str,
    /// The glossary row that defines what the key sets.
    pub term: &'static str,
    /// What the key sets, one sentence.
    pub doc: &'static str,
    /// Why the default cannot always be right (CLAUDE.md, invariant 5).
    pub why_not_default: &'static str,
    /// The `doctor` check that fires when the key is set wrong.
    pub doctor: &'static str,
    /// The docs anchor that explains the key: its section of the reference page.
    pub anchor: &'static str,
}

/// Every key, resolved: what a driver and `doctor` run with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// `dir`: the driver directory.
    pub dir: PathBuf,
    /// `conductor.idle.strategy`: what the conductor does on an idle duty cycle.
    pub conductor_idle_strategy: IdleStrategy,
}

impl Default for Config {
    /// Every key at its default: the directory of [`Driver::default_dir`] and
    /// [`IdleStrategy::Backoff`].
    fn default() -> Self {
        Self {
            dir: Driver::default_dir(),
            conductor_idle_strategy: IdleStrategy::Backoff,
        }
    }
}

impl Config {
    /// The defaults with every key whose environment variable is set applied, or the
    /// first value a key does not take. Reads the process environment;
    /// [`Config::from_vars`] is the same over a table.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_vars(|env| {
            std::env::var_os(env).map(|value| value.to_string_lossy().into_owned())
        })
    }

    /// The defaults with every key `lookup` has a value for applied, in the order of
    /// `config.toml`; `lookup` maps an environment variable's name to its value. An
    /// empty value is an unset one. The first value a key does not take is the error,
    /// so that a wrong key stops a command before anything starts.
    pub fn from_vars(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        let set = |key: &'static KeyDef| lookup(key.env).filter(|value| !value.is_empty());
        if let Some(value) = set(&generated::DIR) {
            config.dir = PathBuf::from(value);
        }
        if let Some(value) = set(&generated::CONDUCTOR_IDLE_STRATEGY) {
            config.conductor_idle_strategy =
                IdleStrategy::parse(&value).ok_or(ConfigError::Invalid {
                    key: &generated::CONDUCTOR_IDLE_STRATEGY,
                    value,
                })?;
        }
        Ok(config)
    }
}

/// Why the configuration could not be read. The message says what the operator does
/// next and names the key's section of the reference page.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// A key was set to a value it does not take.
    Invalid {
        /// The key.
        key: &'static KeyDef,
        /// The value found.
        value: String,
    },
}

impl ConfigError {
    /// The reason's name: `invalid`.
    pub const fn reason(&self) -> &'static str {
        match self {
            ConfigError::Invalid { .. } => "invalid",
        }
    }

    /// The key the error is about.
    pub const fn key(&self) -> &'static KeyDef {
        match self {
            ConfigError::Invalid { key, .. } => key,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Invalid { key, value } => {
                write!(
                    f,
                    "configuration refused (invalid): {} is `{value}`, which {} does not take; ",
                    key.env, key.name
                )?;
                if key.kind == Kind::Enum {
                    write!(f, "set it to one of {}, ", key.values.join(", "))?;
                } else {
                    write!(
                        f,
                        "set it to a {}, ",
                        match key.kind {
                            Kind::Path => "path",
                            Kind::Enum => "value",
                        }
                    )?;
                }
                write!(
                    f,
                    "or unset it for the default ({}); see {}",
                    key.default, key.anchor
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn table(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |env: &str| map.get(env).cloned()
    }

    #[test]
    fn nothing_set_is_the_defaults() {
        let config = Config::from_vars(|_| None).expect("defaults");
        assert_eq!(config, Config::default());
        assert_eq!(config.dir, Driver::default_dir());
        assert_eq!(config.conductor_idle_strategy, IdleStrategy::Backoff);
    }

    #[test]
    fn every_key_is_read_from_its_environment_variable() {
        let config = Config::from_vars(table(&[
            ("CAVALRY_DIR", "/dev/shm/cavalry-two"),
            ("CAVALRY_CONDUCTOR_IDLE_STRATEGY", "busy-spin"),
        ]))
        .expect("valid");
        assert_eq!(config.dir, PathBuf::from("/dev/shm/cavalry-two"));
        assert_eq!(config.conductor_idle_strategy, IdleStrategy::BusySpin);
    }

    #[test]
    fn an_empty_value_is_an_unset_one() {
        let config = Config::from_vars(table(&[
            ("CAVALRY_DIR", ""),
            ("CAVALRY_CONDUCTOR_IDLE_STRATEGY", ""),
        ]))
        .expect("valid");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn every_registered_enum_value_parses_and_the_default_is_the_code_default() {
        let key = &generated::CONDUCTOR_IDLE_STRATEGY;
        assert_eq!(key.kind, Kind::Enum);
        for (index, name) in key.values.iter().enumerate() {
            let strategy = IdleStrategy::parse(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(strategy.counter_value(), index as i64, "{name}");
        }
        assert_eq!(key.values.len(), IdleStrategy::ALL.len());
        assert_eq!(
            IdleStrategy::parse(key.default),
            Some(IdleStrategy::default())
        );
        assert_eq!(generated::DIR.kind, Kind::Path);
        assert!(generated::DIR.values.is_empty());
    }

    #[test]
    fn a_value_a_key_does_not_take_is_refused_with_what_to_do() {
        let error = Config::from_vars(table(&[("CAVALRY_CONDUCTOR_IDLE_STRATEGY", "spin")]))
            .expect_err("refused");
        assert_eq!(error.reason(), "invalid");
        assert_eq!(error.key().name, "conductor.idle.strategy");
        let text = error.to_string();
        assert!(
            text.contains("CAVALRY_CONDUCTOR_IDLE_STRATEGY is `spin`"),
            "{text}"
        );
        assert!(text.contains("one of backoff, yield, busy-spin"), "{text}");
        assert!(text.contains("default (backoff)"), "{text}");
        assert!(
            text.ends_with("docs/reference/config.md#conductoridlestrategy"),
            "{text}"
        );
    }

    #[test]
    fn the_registry_names_every_key_once_and_derives_its_environment_variable() {
        let mut envs = Vec::new();
        for key in generated::KEYS {
            let derived = format!("CAVALRY_{}", key.name.to_uppercase().replace('.', "_"));
            assert_eq!(key.env, derived, "{}", key.name);
            assert!(!key.why_not_default.is_empty(), "{}", key.name);
            assert!(
                key.anchor.starts_with("docs/reference/config.md#"),
                "{}",
                key.name
            );
            assert!(!envs.contains(&key.env), "{} twice", key.env);
            envs.push(key.env);
        }
        assert_eq!(generated::KEYS.len(), 2);
    }
}

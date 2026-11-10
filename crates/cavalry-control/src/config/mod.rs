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
    /// Any text.
    String,
    /// A whole number.
    Integer,
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
    /// `log.dir`: the log directory, where the recorder writes the durable log; `None`
    /// when the key is unset and the host has neither a home nor root, which `cavalry
    /// run` refuses to start on, naming the key ([`Driver::default_log_dir`]; ADR 0025).
    pub log_dir: Option<PathBuf>,
    /// `conductor.idle.strategy`: what the conductor does on an idle duty cycle.
    pub conductor_idle_strategy: IdleStrategy,
    /// `cluster.members`: the members string of the cluster this driver is a member
    /// of, as it was set; `None` for no cluster. `doctor` checks it, and `cavalry run`
    /// refuses a string the cluster protocol refuses (`spec/cluster.md`, "Members and
    /// streams"; ADR 0060).
    pub cluster_members: Option<String>,
    /// `cluster.member.id`: this driver's member id in the members string; `None` for
    /// no member, the key unset or set to −1, its default and Aeron's null value
    /// (`spec/cluster.md`, "Values"; ADR 0060).
    pub cluster_member_id: Option<i32>,
}

impl Default for Config {
    /// Every key at its default: the directory of [`Driver::default_dir`], the log
    /// directory of [`Driver::default_log_dir`], [`IdleStrategy::Backoff`] and no
    /// cluster.
    fn default() -> Self {
        Self {
            dir: Driver::default_dir(),
            log_dir: Driver::default_log_dir(),
            conductor_idle_strategy: IdleStrategy::Backoff,
            cluster_members: None,
            cluster_member_id: None,
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
        if let Some(value) = set(&generated::LOG_DIR) {
            config.log_dir = Some(PathBuf::from(value));
        }
        if let Some(value) = set(&generated::CONDUCTOR_IDLE_STRATEGY) {
            config.conductor_idle_strategy =
                IdleStrategy::parse(&value).ok_or(ConfigError::Invalid {
                    key: &generated::CONDUCTOR_IDLE_STRATEGY,
                    value,
                })?;
        }
        if let Some(value) = set(&generated::CLUSTER_MEMBERS) {
            config.cluster_members = Some(value);
        }
        if let Some(value) = set(&generated::CLUSTER_MEMBER_ID) {
            // A member id is 0 to 2147483647, and −1 is none (`spec/cluster.md`,
            // "Values"): −1, the key's default (ADR 0060), reads as the key unset, so
            // that a unit file may write the default out. Any other value is refused
            // here, before anything starts.
            config.cluster_member_id = match value.parse::<i32>() {
                Ok(-1) => None,
                Ok(id) if id >= 0 => Some(id),
                _ => {
                    return Err(ConfigError::Invalid {
                        key: &generated::CLUSTER_MEMBER_ID,
                        value,
                    });
                }
            };
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
                match key.kind {
                    Kind::Enum => write!(f, "set it to one of {}, ", key.values.join(", "))?,
                    Kind::Path => f.write_str("set it to a path, ")?,
                    Kind::String => f.write_str("set it to a string, ")?,
                    Kind::Integer => write!(f, "set it to {}, ", integer_takes(key))?,
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

/// What an integer key takes, in the words of its refusal. The range is the key's own,
/// as [`Config::from_vars`] reads it, and not its kind's: a later integer key names its
/// own range here.
fn integer_takes(key: &KeyDef) -> &'static str {
    if key.name == generated::CLUSTER_MEMBER_ID.name {
        "this host's member id, a whole number from 0 to 2147483647, or -1 for no member"
    } else {
        "a whole number"
    }
}

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
    fn the_log_directory_comes_from_its_variable_or_the_drivers_rule() {
        let config = Config::from_vars(table(&[])).unwrap();
        assert_eq!(config.log_dir, Driver::default_log_dir());
        let config =
            Config::from_vars(table(&[("CAVALRY_LOG_DIR", "/var/tmp/cavalry-log")])).unwrap();
        assert_eq!(config.log_dir, Some(PathBuf::from("/var/tmp/cavalry-log")));
        assert_eq!(generated::LOG_DIR.flag, Some("--log-dir"));
        assert_eq!(generated::LOG_DIR.doctor, "log-directory-backing");
        assert_eq!(generated::LOG_DIR.env, "CAVALRY_LOG_DIR");
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
        assert_eq!(generated::KEYS.len(), 5);
    }

    #[test]
    fn the_cluster_keys_are_read_and_a_member_id_that_is_no_id_is_refused() {
        let config = Config::from_vars(table(&[])).expect("defaults");
        assert_eq!(
            (config.cluster_members, config.cluster_member_id),
            (None, None)
        );
        let members = "0,a:1,a:2,a:3,a:4,a:5|1,b:1,b:2,b:3,b:4,b:5";
        let config = Config::from_vars(table(&[
            ("CAVALRY_CLUSTER_MEMBERS", members),
            ("CAVALRY_CLUSTER_MEMBER_ID", "1"),
        ]))
        .expect("valid");
        assert_eq!(config.cluster_members.as_deref(), Some(members));
        assert_eq!(config.cluster_member_id, Some(1));
        assert_eq!(generated::CLUSTER_MEMBERS.kind, Kind::String);
        assert_eq!(generated::CLUSTER_MEMBER_ID.kind, Kind::Integer);
        assert_eq!(generated::CLUSTER_MEMBERS.doctor, "cluster-members");
        assert_eq!(generated::CLUSTER_MEMBER_ID.doctor, "cluster-member-id");

        // −1, the key's documented default (config.toml; ADR 0060), is no member, as the
        // key unset is: a unit file may write the default out.
        assert!(generated::CLUSTER_MEMBER_ID.default.starts_with("-1,"));
        let config =
            Config::from_vars(table(&[("CAVALRY_CLUSTER_MEMBER_ID", "-1")])).expect("the default");
        assert_eq!(config.cluster_member_id, None);
        assert_eq!(config, Config::default());
        let config = Config::from_vars(table(&[
            ("CAVALRY_CLUSTER_MEMBERS", members),
            ("CAVALRY_CLUSTER_MEMBER_ID", "-1"),
        ]))
        .expect("valid");
        assert_eq!(config.cluster_member_id, None);
        let config =
            Config::from_vars(table(&[("CAVALRY_CLUSTER_MEMBER_ID", "0")])).expect("valid");
        assert_eq!(config.cluster_member_id, Some(0));

        for value in ["one", "-2", "-2147483648", "2147483648", "1.5"] {
            let error = Config::from_vars(table(&[("CAVALRY_CLUSTER_MEMBER_ID", value)]))
                .expect_err("refused");
            assert_eq!(error.key().name, "cluster.member.id");
            let text = error.to_string();
            assert!(
                text.contains(&format!("CAVALRY_CLUSTER_MEMBER_ID is `{value}`")),
                "{text}"
            );
            assert!(
                text.contains(
                    "set it to this host's member id, a whole number from 0 to 2147483647, \
                     or -1 for no member,"
                ),
                "{text}"
            );
            assert!(text.contains("default (-1, which is no member)"), "{text}");
            assert!(
                text.ends_with("docs/reference/config.md#clustermemberid"),
                "{text}"
            );
        }
    }
}

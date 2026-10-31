//! The channel URI: `cavalry:ipc`, and `cavalry:udp?endpoint=<host>:<port>` with its one
//! option, `interface` (`spec/udp.md`, "The channel URI"; `spec/client-protocol.md`,
//! version 4; ADR 0040).
//!
//! [`Channel::parse`] reads a channel string, borrowing from it, and runs the spec's
//! checks in the spec's order, each over the whole string before the next: `grammar`,
//! `key`, `endpoint`, `multicast`; a channel wrong in two ways reports the earlier
//! check whichever parameter carries each fault, as a frame does. Resolving a host name
//! is the driver's, on its control plane, and its failure is error code 9; every
//! refusal here is error code 1.

use core::fmt;
use core::net::{Ipv4Addr, Ipv6Addr};

/// The IPC channel.
pub const IPC: &str = "cavalry:ipc";

/// What every UDP channel begins with.
pub const UDP_PREFIX: &str = "cavalry:udp?";

/// The docs anchor every error message names.
const DOC: &str = "docs/reference/udp.md#the-channel-uri";

/// Why a channel was refused. [`ChannelError::reason`] is the name the spec and the
/// vectors use; every variant is error code 1 in the client protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelError {
    /// The string is not `cavalry:ipc` or `cavalry:udp?` followed by `key=value`
    /// parameters separated by `|`, each key `[a-z0-9-]+` and each value non-empty, no
    /// key twice.
    Grammar {
        /// What was wrong, in words.
        what: &'static str,
    },
    /// A key this version does not name: not `endpoint` or `interface`.
    Key {
        /// The byte offset of the key within the channel string.
        at: usize,
        /// The key's length in bytes.
        len: usize,
    },
    /// The endpoint is missing or malformed: no `<host>:<port>`, a port of 0 or above
    /// 65535, an empty host, a host with characters outside names and addresses, or an
    /// IPv6 address outside brackets.
    Endpoint {
        /// What was wrong, in words.
        what: &'static str,
    },
    /// The endpoint is a multicast address, which this version refuses.
    Multicast,
}

impl ChannelError {
    /// The reason's name in `spec/udp.md`, "What a reader rejects": `grammar`, `key`,
    /// `endpoint` or `multicast`.
    pub const fn reason(self) -> &'static str {
        match self {
            ChannelError::Grammar { .. } => "grammar",
            ChannelError::Key { .. } => "key",
            ChannelError::Endpoint { .. } => "endpoint",
            ChannelError::Multicast => "multicast",
        }
    }

    /// The client protocol's error code for this refusal: 1, invalid channel.
    pub const fn code(self) -> i32 {
        1
    }

    /// The unknown key, for [`ChannelError::Key`], sliced from the channel string it was
    /// parsed from; empty for any other variant.
    pub fn key<'a>(&self, channel: &'a [u8]) -> &'a [u8] {
        match *self {
            ChannelError::Key { at, len } => channel.get(at..at + len).unwrap_or(&[]),
            _ => &[],
        }
    }
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ChannelError::Grammar { what } => write!(
                f,
                "channel refused (grammar): {what}; a channel is cavalry:ipc or \
                 cavalry:udp?endpoint=<host>:<port> with |interface=<address> as its one \
                 option; see {DOC}"
            ),
            ChannelError::Key { at, len } => write!(
                f,
                "channel refused (key): the {len}-byte parameter at byte {at} is not one this \
                 version names, endpoint or interface; remove it, or upgrade the driver to a \
                 version that names it; see {DOC}"
            ),
            ChannelError::Endpoint { what } => write!(
                f,
                "channel refused (endpoint): {what}; the endpoint is <host>:<port> with a \
                 port from 1 to 65535 and an IPv6 address in brackets; see {DOC}"
            ),
            ChannelError::Multicast => write!(
                f,
                "channel refused (multicast): the endpoint is a multicast address, which \
                 this version does not send to or receive on; name a unicast address; see \
                 {DOC}"
            ),
        }
    }
}

impl core::error::Error for ChannelError {}

/// An address in a channel: a host and, for an endpoint always and an interface
/// sometimes, a port.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Address<'a> {
    host: &'a str,
    port: Option<u16>,
}

impl<'a> Address<'a> {
    /// The host: a name, an IPv4 address, or an IPv6 address without its brackets.
    pub const fn host(&self) -> &'a str {
        self.host
    }

    /// The port, when one was named.
    pub const fn port(&self) -> Option<u16> {
        self.port
    }

    /// The host as an IPv4 address, when it is one.
    pub fn ipv4(&self) -> Option<Ipv4Addr> {
        self.host.parse().ok()
    }

    /// The host as an IPv6 address, when it is one.
    pub fn ipv6(&self) -> Option<Ipv6Addr> {
        self.host.parse().ok()
    }

    /// Whether the host is a multicast address.
    pub fn is_multicast(&self) -> bool {
        self.ipv4().is_some_and(|ip| ip.is_multicast())
            || self.ipv6().is_some_and(|ip| ip.is_multicast())
    }
}

/// A `cavalry:udp` channel, parsed: its endpoint and its interface, borrowed from the
/// string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UdpChannel<'a> {
    endpoint: Address<'a>,
    interface: Option<Address<'a>>,
}

impl<'a> UdpChannel<'a> {
    /// The endpoint: the address a sender sends to and a receiver binds.
    pub const fn endpoint(&self) -> Address<'a> {
        self.endpoint
    }

    /// The endpoint's port.
    pub fn endpoint_port(&self) -> u16 {
        self.endpoint.port.unwrap_or(0)
    }

    /// The interface, when one was named: the local address a sender sends from or a
    /// receiver binds beside the endpoint.
    pub const fn interface(&self) -> Option<Address<'a>> {
        self.interface
    }
}

/// A channel, parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel<'a> {
    /// `cavalry:ipc`.
    Ipc,
    /// A `cavalry:udp` channel.
    Udp(UdpChannel<'a>),
}

impl<'a> Channel<'a> {
    /// Parses a channel string, the bytes of a client protocol string field, running the
    /// checks of `spec/udp.md`, "What a reader rejects", in order, each over the whole
    /// string before the next: `grammar`, then `key`, then `endpoint`, then `multicast`.
    /// A channel wrong in two ways reports the earlier check whichever parameter carries
    /// each fault, so that two readers of the page refuse one string for one reason.
    pub fn parse(channel: &'a [u8]) -> Result<Self, ChannelError> {
        let text = core::str::from_utf8(channel).map_err(|_| ChannelError::Grammar {
            what: "the channel is not UTF-8",
        })?;
        if text == IPC {
            return Ok(Channel::Ipc);
        }
        let Some(params) = text.strip_prefix(UDP_PREFIX) else {
            return Err(ChannelError::Grammar {
                what: if text.starts_with("cavalry:udp") {
                    "cavalry:udp has no parameters; it needs ?endpoint=<host>:<port>"
                } else if text.starts_with("cavalry:ipc") {
                    "cavalry:ipc takes no parameters"
                } else {
                    "the scheme is not cavalry:ipc or cavalry:udp"
                },
            });
        };
        // `grammar`, over every parameter: the key=value form, the key's characters, a
        // non-empty value, and no key twice. The repeated-key check reads the earlier
        // parameters again rather than remember them, so that it allocates nothing; a
        // channel string is bounded by the client protocol's string length, and this
        // runs on the control plane.
        for (index, param) in params.split('|').enumerate() {
            let Some((key, value)) = param.split_once('=') else {
                return Err(ChannelError::Grammar {
                    what: "a parameter is not key=value",
                });
            };
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return Err(ChannelError::Grammar {
                    what: "a key is empty or has a character outside a-z, 0-9 and -",
                });
            }
            if value.is_empty() {
                return Err(ChannelError::Grammar {
                    what: "a value is empty",
                });
            }
            if params
                .split('|')
                .take(index)
                .any(|earlier| key_of(earlier) == key)
            {
                return Err(ChannelError::Grammar {
                    what: match key {
                        "endpoint" => "the key endpoint appears twice",
                        "interface" => "the key interface appears twice",
                        _ => "a key appears twice",
                    },
                });
            }
        }
        // `key`, over every parameter: one this version names.
        let mut at = UDP_PREFIX.len();
        for param in params.split('|') {
            let key = key_of(param);
            if key != "endpoint" && key != "interface" {
                return Err(ChannelError::Key { at, len: key.len() });
            }
            at += param.len() + 1;
        }
        // `endpoint`: each address left to right, then that there is an endpoint.
        let mut endpoint = None;
        let mut interface = None;
        for param in params.split('|') {
            let (key, value) = param.split_once('=').unwrap_or((param, ""));
            match key {
                "endpoint" => endpoint = Some(parse_address(value, true)?),
                "interface" => interface = Some(parse_address(value, false)?),
                _ => {}
            }
        }
        let Some(endpoint) = endpoint else {
            return Err(ChannelError::Endpoint {
                what: "no endpoint parameter",
            });
        };
        // `multicast`.
        if endpoint.is_multicast() {
            return Err(ChannelError::Multicast);
        }
        Ok(Channel::Udp(UdpChannel {
            endpoint,
            interface,
        }))
    }
}

/// The key of a parameter: what is before its `=`, or the whole parameter when it has
/// none, which the grammar check refuses before any later check reads it.
fn key_of(param: &str) -> &str {
    param.split_once('=').map_or(param, |(key, _)| key)
}

/// `<host>:<port>`, or `[<ipv6>]:<port>`, with the port required for an endpoint and
/// optional for an interface.
fn parse_address(value: &str, port_required: bool) -> Result<Address<'_>, ChannelError> {
    let bracketed = value.starts_with('[');
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let Some((inside, after)) = rest.split_once(']') else {
            return Err(ChannelError::Endpoint {
                what: "an IPv6 address needs its closing bracket",
            });
        };
        if inside.parse::<Ipv6Addr>().is_err() {
            return Err(ChannelError::Endpoint {
                what: "the bracketed host is not an IPv6 address",
            });
        }
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port),
            None if after.is_empty() => None,
            None => {
                return Err(ChannelError::Endpoint {
                    what: "the bracketed address is followed by something other than :<port>",
                });
            }
        };
        (inside, port)
    } else {
        match value.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (value, None),
        }
    };
    if host.is_empty() {
        return Err(ChannelError::Endpoint {
            what: "the host is empty",
        });
    }
    if !bracketed {
        if host.contains(':') {
            return Err(ChannelError::Endpoint {
                what: "an IPv6 address goes in brackets",
            });
        }
        if !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err(ChannelError::Endpoint {
                what: "the host has a character outside a name or an address",
            });
        }
    }
    let port = match port {
        Some(port) => match port.parse::<u16>() {
            Ok(0) => {
                return Err(ChannelError::Endpoint {
                    what: "the port is 0",
                });
            }
            Ok(port) => Some(port),
            Err(_) => {
                return Err(ChannelError::Endpoint {
                    what: "the port is not a number from 1 to 65535",
                });
            }
        },
        None if port_required => {
            return Err(ChannelError::Endpoint {
                what: "the endpoint has no port",
            });
        }
        None => None,
    };
    Ok(Address { host, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A channel wrong in two ways reports the earlier check whichever parameter
    /// carries each fault (`spec/udp.md`, "What a reader rejects"): the grammar of the
    /// whole string before any key, every key before any address, every address before
    /// the multicast check. The vectors of `spec/vectors/udp.py` hold single-fault
    /// strings; these pin the order between the checks.
    #[test]
    fn a_channel_wrong_in_two_ways_reports_the_earlier_check() {
        let cases = [
            ("cavalry:udp?mtu=1|endpoint", "grammar"),
            (
                "cavalry:udp?endpoint=10.0.0.2:1|mtu=1|endpoint=10.0.0.2:2",
                "grammar",
            ),
            ("cavalry:udp?endpoint=10.0.0.2:0|mtu=1408", "key"),
            ("cavalry:udp?interface=10.0.0.1|mtu=1408", "key"),
            ("cavalry:udp?endpoint=224.0.1.1:0", "endpoint"),
            (
                "cavalry:udp?endpoint=224.0.1.1:40123|interface=10.0.0.1:0",
                "endpoint",
            ),
        ];
        for (string, reason) in cases {
            match Channel::parse(string.as_bytes()) {
                Err(error) => assert_eq!(error.reason(), reason, "{string}: {error}"),
                Ok(channel) => panic!("{string}: accepted as {channel:?}, expected {reason}"),
            }
        }
    }

    /// The `key` error names the unknown parameter wherever it stands, the key check
    /// having run after the grammar of every parameter before it.
    #[test]
    fn the_key_error_slices_the_unknown_parameter() {
        for (string, key) in [
            ("cavalry:udp?mtu=1408|endpoint=10.0.0.2:1", "mtu"),
            (
                "cavalry:udp?endpoint=10.0.0.2:0|term-length=65536",
                "term-length",
            ),
        ] {
            match Channel::parse(string.as_bytes()) {
                Err(error) => assert_eq!(error.key(string.as_bytes()), key.as_bytes(), "{string}"),
                Ok(channel) => panic!("{string}: accepted as {channel:?}"),
            }
        }
    }
}

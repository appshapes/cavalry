//! The channel URI: `cavalry:ipc`; `cavalry:udp?endpoint=<host>:<port>` with its option
//! `interface`; and, from version 1 of `spec/udp.md`, the control channel
//! `cavalry:udp?control=<host>:<port>|control-mode=manual` of multi-destination cast,
//! and `fc=max|min`, the flow control strategy, on a publication's channel, and, from
//! version 2, the position keys `init-term-id`, `term-id` and `term-offset`, which
//! create a publication at a position (`spec/udp.md`, "The channel URI";
//! `spec/client-protocol.md`, version 6; ADR 0040, ADR 0053, ADR 0059).
//!
//! [`Channel::parse`] reads a channel string, borrowing from it, and runs the spec's
//! checks in the spec's order: `grammar`, `key`, `control`, `endpoint`, `fc`,
//! `multicast`. Resolving a host name is the driver's, on its control plane, and its
//! failure is error code 9; every refusal here is error code 1.

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
    /// A key this version does not name: not `endpoint`, `interface`, `control`,
    /// `control-mode`, `fc`, `init-term-id`, `term-id` or `term-offset`.
    Key {
        /// The byte offset of the key within the channel string.
        at: usize,
        /// The key's length in bytes.
        len: usize,
    },
    /// A control address without `control-mode=manual` or `control-mode` without a
    /// control address, a control mode other than `manual`, a control address beside
    /// an endpoint or an interface, or a control address with no port, a port of 0 or
    /// above 65535, an empty host, a host with characters outside names and addresses,
    /// or an IPv6 address outside brackets.
    Control {
        /// What was wrong, in words.
        what: &'static str,
    },
    /// Neither an endpoint nor a control address; or an endpoint or interface with no
    /// `<host>:<port>`, a port of 0 or above 65535, an empty host, a host with
    /// characters outside names and addresses, or an IPv6 address outside brackets.
    Endpoint {
        /// What was wrong, in words.
        what: &'static str,
    },
    /// An `fc` value other than `max` or `min`.
    FlowControl {
        /// What was wrong, in words.
        what: &'static str,
    },
    /// One or two of the position keys `init-term-id`, `term-id` and `term-offset`
    /// without the rest; a value that is not a 32-bit integer in decimal; a `term-id`
    /// below `init-term-id` by the 32-bit two's complement count; or a `term-offset`
    /// that is negative, not a multiple of 32 or not below the UDP term length
    /// (version 2, ADR 0059).
    Position {
        /// What was wrong, in words.
        what: &'static str,
    },
    /// The endpoint or the control address is a multicast address, which this version
    /// refuses.
    Multicast,
}

impl ChannelError {
    /// The reason's name in `spec/udp.md`, "What a reader rejects": `grammar`, `key`,
    /// `control`, `endpoint`, `fc` or `multicast`.
    pub const fn reason(self) -> &'static str {
        match self {
            ChannelError::Grammar { .. } => "grammar",
            ChannelError::Key { .. } => "key",
            ChannelError::Control { .. } => "control",
            ChannelError::Endpoint { .. } => "endpoint",
            ChannelError::FlowControl { .. } => "fc",
            ChannelError::Position { .. } => "position",
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
                "channel refused (grammar): {what}; a channel is cavalry:ipc, \
                 cavalry:udp?endpoint=<host>:<port> with |interface=<address> and |fc=max|min \
                 as its options, or cavalry:udp?control=<host>:<port>|control-mode=manual with \
                 |fc=max|min; see {DOC}"
            ),
            ChannelError::Key { at, len } => write!(
                f,
                "channel refused (key): the {len}-byte parameter at byte {at} is not one this \
                 version names, endpoint, interface, control, control-mode, fc, init-term-id, \
                 term-id or term-offset; remove it, or upgrade the driver to a version that \
                 names it; see {DOC}"
            ),
            ChannelError::Control { what } => write!(
                f,
                "channel refused (control): {what}; a control channel is \
                 cavalry:udp?control=<host>:<port>|control-mode=manual, with no endpoint and no \
                 interface, the port from 1 to 65535 and an IPv6 address in brackets; see {DOC}"
            ),
            ChannelError::Endpoint { what } => write!(
                f,
                "channel refused (endpoint): {what}; the endpoint is <host>:<port> with a \
                 port from 1 to 65535 and an IPv6 address in brackets, or the channel names a \
                 control address instead; see {DOC}"
            ),
            ChannelError::FlowControl { what } => write!(
                f,
                "channel refused (fc): {what}; the flow control strategy is fc=max or fc=min, \
                 max when the channel names none; see {DOC}"
            ),
            ChannelError::Position { what } => write!(
                f,
                "channel refused (position): {what}; a publication's channel names \
                 init-term-id=<i32>|term-id=<i32>|term-offset=<multiple of 32 below the term \
                 length>, all three or none, to be created at that position; see {DOC}"
            ),
            ChannelError::Multicast => write!(
                f,
                "channel refused (multicast): the endpoint or control address is a multicast \
                 address, which this version does not send to or receive on; name a unicast \
                 address; see {DOC}"
            ),
        }
    }
}

impl core::error::Error for ChannelError {}

/// An address in a channel: a host and, for an endpoint and a control address always
/// and an interface sometimes, a port.
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

/// The control mode of a control channel: `manual`, the one this version names, where
/// a client adds and removes the destinations (`spec/udp.md`, "The channel URI").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControlMode {
    /// `control-mode=manual`.
    Manual,
}

impl ControlMode {
    /// The mode's name in the channel string.
    pub const fn name(self) -> &'static str {
        match self {
            ControlMode::Manual => "manual",
        }
    }
}

/// The flow control strategy of a publication: how its sender limit moves on the
/// status messages of its receivers (`spec/udp.md`, "The sender").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlowControl {
    /// `fc=max`, the default: the fastest receiver sets the pace.
    Max,
    /// `fc=min`: the slowest receiver sets the pace, one that goes quiet for the flow
    /// control receiver timeout dropped from the accounting.
    Min,
}

impl FlowControl {
    /// The strategy's name in the channel string.
    pub const fn name(self) -> &'static str {
        match self {
            FlowControl::Max => "max",
            FlowControl::Min => "min",
        }
    }
}

/// A `cavalry:udp` channel, parsed: its endpoint and interface, or its control address
/// and mode, and its flow control strategy, borrowed from the string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UdpChannel<'a> {
    endpoint: Option<Address<'a>>,
    interface: Option<Address<'a>>,
    control: Option<Address<'a>>,
    control_mode: Option<ControlMode>,
    flow_control: Option<FlowControl>,
    position: Option<PublicationPosition>,
}

/// The position keys of a publication's channel, `init-term-id`, `term-id` and
/// `term-offset`, which create its log at a position (`spec/udp.md`, version 2, "The
/// channel URI"; ADR 0059).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PublicationPosition {
    /// `init-term-id`: the log's initial term id.
    pub initial_term_id: i32,
    /// `term-id`: the term id of the position, `initial_term_id` plus the term count.
    pub term_id: i32,
    /// `term-offset`: a multiple of 32 below the UDP term length.
    pub term_offset: i32,
}

impl PublicationPosition {
    /// The term count: `term_id` − `initial_term_id` in 32-bit two's complement, 0 or
    /// more once the channel has parsed.
    pub const fn term_count(self) -> i32 {
        self.term_id.wrapping_sub(self.initial_term_id)
    }

    /// The position the keys name under the UDP term length of `spec/udp.md`, "Values":
    /// the term count × the term length + the term offset.
    pub const fn position(self) -> i64 {
        self.term_count() as i64 * crate::udp::UDP_TERM_LENGTH as i64 + self.term_offset as i64
    }
}

impl<'a> UdpChannel<'a> {
    /// The endpoint: the address a sender sends to and a receiver binds. None for a
    /// control channel, which names a control address instead.
    pub const fn endpoint(&self) -> Option<Address<'a>> {
        self.endpoint
    }

    /// The interface, when one was named: the local address a sender sends from or a
    /// receiver binds beside the endpoint.
    pub const fn interface(&self) -> Option<Address<'a>> {
        self.interface
    }

    /// The control address, when the channel is a control channel: the local address
    /// the sender binds, where its destinations' status messages and NAKs arrive.
    pub const fn control(&self) -> Option<Address<'a>> {
        self.control
    }

    /// The control mode, when the channel is a control channel.
    pub const fn control_mode(&self) -> Option<ControlMode> {
        self.control_mode
    }

    /// Whether the channel names a control address: a publication's channel whose
    /// sender sends to destinations.
    pub const fn is_control(&self) -> bool {
        self.control.is_some()
    }

    /// The flow control strategy: what `fc` names, or [`FlowControl::Max`] when the
    /// channel names none.
    pub const fn flow_control(&self) -> FlowControl {
        match self.flow_control {
            Some(strategy) => strategy,
            None => FlowControl::Max,
        }
    }

    /// Whether the channel names `fc` at all, which an `add subscription` refuses.
    pub const fn names_flow_control(&self) -> bool {
        self.flow_control.is_some()
    }

    /// The position keys, when the channel names them: a publication's log is created
    /// at [`PublicationPosition::position`] with their initial term id (version 2).
    pub const fn position_keys(&self) -> Option<PublicationPosition> {
        self.position
    }

    /// Whether the channel names the position keys, which an `add subscription` and a
    /// destination channel refuse.
    pub const fn names_position(&self) -> bool {
        self.position.is_some()
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
    /// checks of `spec/udp.md`, "What a reader rejects", in order.
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
                    "cavalry:udp has no parameters; it needs ?endpoint=<host>:<port> or \
                     ?control=<host>:<port>|control-mode=manual"
                } else if text.starts_with("cavalry:ipc") {
                    "cavalry:ipc takes no parameters"
                } else {
                    "the scheme is not cavalry:ipc or cavalry:udp"
                },
            });
        };
        // `grammar` over every parameter first, then `key`, `control`, `endpoint`, `fc`
        // and `multicast`: the page's order, so that a string wrong in two ways reports
        // the earlier check whatever the order of its parameters.
        //
        // `grammar`: the key=value form, the key's characters, a non-empty value, and no
        // key twice, whether or not this version names the key. The repeated-key check
        // reads the earlier parameters again rather than remember them, so that it
        // allocates nothing; a channel string is bounded by the client protocol's string
        // length, and this runs on the control plane.
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
                    what: "a key appears twice",
                });
            }
        }
        // `key`, over every parameter: one this version names.
        let mut at = UDP_PREFIX.len();
        for param in params.split('|') {
            let key = key_of(param);
            if !matches!(
                key,
                "endpoint"
                    | "interface"
                    | "control"
                    | "control-mode"
                    | "fc"
                    | "init-term-id"
                    | "term-id"
                    | "term-offset"
            ) {
                return Err(ChannelError::Key { at, len: key.len() });
            }
            at += param.len() + 1;
        }
        // `control`: a control address and its mode come together, the mode is manual,
        // nothing stands beside the control address that it already is, and the
        // address has the shape of an endpoint's.
        let mut control_value = None;
        let mut mode_value = None;
        let mut endpoint_value = None;
        let mut interface_value = None;
        let mut fc_value = None;
        let mut initial_term_id_value = None;
        let mut term_id_value = None;
        let mut term_offset_value = None;
        for param in params.split('|') {
            let (key, value) = param.split_once('=').unwrap_or((param, ""));
            match key {
                "control" => control_value = Some(value),
                "control-mode" => mode_value = Some(value),
                "endpoint" => endpoint_value = Some(value),
                "interface" => interface_value = Some(value),
                "fc" => fc_value = Some(value),
                "init-term-id" => initial_term_id_value = Some(value),
                "term-id" => term_id_value = Some(value),
                "term-offset" => term_offset_value = Some(value),
                _ => {}
            }
        }
        let (control, control_mode) = match (control_value, mode_value) {
            (None, None) => (None, None),
            (Some(_), None) => {
                return Err(ChannelError::Control {
                    what: "a control address needs control-mode=manual beside it",
                });
            }
            (None, Some(_)) => {
                return Err(ChannelError::Control {
                    what: "control-mode needs a control address beside it",
                });
            }
            (Some(value), Some(mode)) => {
                if mode != "manual" {
                    return Err(ChannelError::Control {
                        what: "the control mode is not manual, the one this version names",
                    });
                }
                if endpoint_value.is_some() {
                    return Err(ChannelError::Control {
                        what: "a channel names a control address or an endpoint, not both",
                    });
                }
                if interface_value.is_some() {
                    return Err(ChannelError::Control {
                        what: "an interface has no meaning beside a control address, which \
                               is the local address itself",
                    });
                }
                let address =
                    parse_address(value, true).map_err(|what| ChannelError::Control { what })?;
                (Some(address), Some(ControlMode::Manual))
            }
        };
        // `endpoint`: the endpoint and the interface, each left to right, then that the
        // channel names an endpoint or a control address.
        let endpoint = match endpoint_value {
            Some(value) => {
                Some(parse_address(value, true).map_err(|what| ChannelError::Endpoint { what })?)
            }
            None => None,
        };
        let interface = match interface_value {
            Some(value) => {
                Some(parse_address(value, false).map_err(|what| ChannelError::Endpoint { what })?)
            }
            None => None,
        };
        if endpoint.is_none() && control.is_none() {
            return Err(ChannelError::Endpoint {
                what: "no endpoint parameter and no control address",
            });
        }
        // `fc`: max or min, bare.
        let flow_control = match fc_value {
            None => None,
            Some("max") => Some(FlowControl::Max),
            Some("min") => Some(FlowControl::Min),
            Some(_) => {
                return Err(ChannelError::FlowControl {
                    what: "the fc value is not max or min",
                });
            }
        };
        // `position` (version 2): the three keys together, each a 32-bit integer, the
        // term id at or past the initial one and the offset within the UDP term.
        let position = match (initial_term_id_value, term_id_value, term_offset_value) {
            (None, None, None) => None,
            (Some(initial), Some(term), Some(offset)) => {
                let parse = |value: &str| value.parse::<i32>().ok();
                let (Some(initial_term_id), Some(term_id), Some(term_offset)) =
                    (parse(initial), parse(term), parse(offset))
                else {
                    return Err(ChannelError::Position {
                        what: "init-term-id, term-id and term-offset are each a 32-bit integer in \
                               decimal",
                    });
                };
                let keys = PublicationPosition {
                    initial_term_id,
                    term_id,
                    term_offset,
                };
                if keys.term_count() < 0 {
                    return Err(ChannelError::Position {
                        what: "term-id is below init-term-id by the 32-bit two's complement count",
                    });
                }
                if term_offset < 0
                    || term_offset % 32 != 0
                    || term_offset >= crate::udp::UDP_TERM_LENGTH as i32
                {
                    return Err(ChannelError::Position {
                        what: "term-offset is not a multiple of 32 from 0 to below the UDP term \
                               length",
                    });
                }
                Some(keys)
            }
            _ => {
                return Err(ChannelError::Position {
                    what: "init-term-id, term-id and term-offset come together, all three or none",
                });
            }
        };
        // `multicast`.
        if endpoint.is_some_and(|address| address.is_multicast())
            || control.is_some_and(|address| address.is_multicast())
        {
            return Err(ChannelError::Multicast);
        }
        Ok(Channel::Udp(UdpChannel {
            endpoint,
            interface,
            control,
            control_mode,
            flow_control,
            position,
        }))
    }
}

/// The key of a `key=value` parameter: the whole parameter when it has no `=`, which
/// `grammar` has refused before any later check asks.
fn key_of(param: &str) -> &str {
    param.split_once('=').map_or(param, |(key, _)| key)
}

/// `<host>:<port>`, or `[<ipv6>]:<port>`, with the port required for an endpoint or a
/// control address and optional for an interface; what was wrong, in words, when it
/// is refused, for the caller to name the check.
fn parse_address(value: &str, port_required: bool) -> Result<Address<'_>, &'static str> {
    let bracketed = value.starts_with('[');
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let Some((inside, after)) = rest.split_once(']') else {
            return Err("an IPv6 address needs its closing bracket");
        };
        if inside.parse::<Ipv6Addr>().is_err() {
            return Err("the bracketed host is not an IPv6 address");
        }
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port),
            None if after.is_empty() => None,
            None => {
                return Err("the bracketed address is followed by something other than :<port>");
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
        return Err("the host is empty");
    }
    if !bracketed {
        if host.contains(':') {
            return Err("an IPv6 address goes in brackets");
        }
        if !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err("the host has a character outside a name or an address");
        }
    }
    let port = match port {
        Some(port) => match port.parse::<u16>() {
            Ok(0) => return Err("the port is 0"),
            Ok(port) => Some(port),
            Err(_) => return Err("the port is not a number from 1 to 65535"),
        },
        None if port_required => return Err("the address has no port"),
        None => None,
    };
    Ok(Address { host, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A channel wrong in two ways reports the earlier check whichever parameter
    /// carries each fault (`spec/udp.md`, "What a reader rejects"): the grammar of the
    /// whole string before any key, every key before the control checks, those before
    /// any address, every address before `fc`, and `fc` before the multicast check. A
    /// repeated key is `grammar` whether or not this version names the key, so `mtu`
    /// twice is `grammar` and not `key`. Some of these strings are vectors in
    /// `spec/vectors/udp.py` too; these pin the order between every pair of checks.
    #[test]
    fn a_channel_wrong_in_two_ways_reports_the_earlier_check() {
        let cases = [
            ("cavalry:udp?mtu=1|endpoint", "grammar"),
            (
                "cavalry:udp?endpoint=10.0.0.2:1|mtu=1|endpoint=10.0.0.2:2",
                "grammar",
            ),
            ("cavalry:udp?mtu=1|mtu=2", "grammar"),
            ("cavalry:udp?endpoint=10.0.0.2:0|mtu=1408", "key"),
            ("cavalry:udp?interface=10.0.0.1|mtu=1408", "key"),
            ("cavalry:udp?control=10.0.0.1:0|mtu=1408", "key"),
            ("cavalry:udp?control-mode=manual|fc=x", "control"),
            (
                "cavalry:udp?endpoint=10.0.0.2:0|control=10.0.0.1:1|control-mode=manual",
                "control",
            ),
            (
                "cavalry:udp?control=10.0.0.1:0|control-mode=manual|fc=x",
                "control",
            ),
            ("cavalry:udp?control=224.0.1.1:1", "control"),
            ("cavalry:udp?endpoint=224.0.1.1:0", "endpoint"),
            ("cavalry:udp?endpoint=10.0.0.2:0|fc=x", "endpoint"),
            ("cavalry:udp?fc=x", "endpoint"),
            (
                "cavalry:udp?endpoint=224.0.1.1:40123|interface=10.0.0.1:0",
                "endpoint",
            ),
            ("cavalry:udp?endpoint=224.0.1.1:40123|fc=x", "fc"),
            (
                "cavalry:udp?control=224.0.1.1:1|control-mode=manual|fc=x",
                "fc",
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
    /// having run after the grammar of every parameter.
    #[test]
    fn the_key_error_slices_the_unknown_parameter() {
        for (string, key) in [
            ("cavalry:udp?mtu=1408|endpoint=10.0.0.2:1", "mtu"),
            (
                "cavalry:udp?endpoint=10.0.0.2:0|term-length=65536",
                "term-length",
            ),
            (
                "cavalry:udp?control=10.0.0.1:1|control-mode=manual|group=1",
                "group",
            ),
        ] {
            match Channel::parse(string.as_bytes()) {
                Err(error) => assert_eq!(error.key(string.as_bytes()), key.as_bytes(), "{string}"),
                Ok(channel) => panic!("{string}: accepted as {channel:?}"),
            }
        }
    }

    /// A control channel parses to its control address and mode with no endpoint, and
    /// `fc` is `max` unless named, with whether it was named kept for the driver's
    /// `add subscription` check.
    #[test]
    fn a_control_channel_and_fc_parse_to_their_fields() {
        let Ok(Channel::Udp(control)) =
            Channel::parse(b"cavalry:udp?control=10.0.0.1:40456|control-mode=manual")
        else {
            panic!("a control channel parses");
        };
        assert!(control.is_control());
        assert!(control.endpoint().is_none());
        assert_eq!(control.control().map(|a| a.host()), Some("10.0.0.1"));
        assert_eq!(control.control().and_then(|a| a.port()), Some(40456));
        assert_eq!(control.control_mode(), Some(ControlMode::Manual));
        assert_eq!(control.flow_control(), FlowControl::Max);
        assert!(!control.names_flow_control());
        let Ok(Channel::Udp(min)) = Channel::parse(b"cavalry:udp?endpoint=10.0.0.2:40123|fc=min")
        else {
            panic!("fc=min parses");
        };
        assert!(!min.is_control());
        assert_eq!(min.endpoint().map(|a| a.host()), Some("10.0.0.2"));
        assert_eq!(min.flow_control(), FlowControl::Min);
        assert!(min.names_flow_control());
        assert_eq!(FlowControl::Min.name(), "min");
        assert_eq!(ControlMode::Manual.name(), "manual");
    }
}

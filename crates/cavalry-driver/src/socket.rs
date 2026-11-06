//! A send endpoint's socket (`spec/udp.md`, "The sender" and "The receiver"; ADR
//! 0041): a `std::net::UdpSocket` opened, bound to the interface when one is named,
//! connected to the endpoint and set non-blocking on the control plane as the command
//! is handled, or, for a control channel, bound to the control address and connected to
//! nothing, every send carrying a destination's address (ADR 0053); and `nix` on its
//! descriptor for what `std` lacks: one `sendmmsg` per burst and destination on Linux
//! and one `sendmsg` per datagram elsewhere, `recvmmsg` or `recvmsg` for the control
//! frames the endpoint receives, with their source addresses, and the buffer lengths
//! read back for `channel.socket_rcvbuf` and `channel.socket_sndbuf`. The headers the batched calls
//! fill are [`Headers`], preallocated once by the sender as it is built on its own
//! thread and lent to every endpoint's calls: `nix`'s `MultiHeaders` holds raw
//! pointers and cannot be sent between threads, so a socket could not carry its own
//! through the queue from the conductor to the sender, and the sender uses one socket
//! at a time (ADR 0041, acceptance). Nothing here allocates after [`SendSocket::open`]
//! and [`Headers::preallocate`], and no call passes a control message, so none
//! allocates per call (ADR 0041, Evidence). A send or a receive the operating system
//! refuses is returned to the sender, which counts it and tries again on its next
//! cycle; no failure on a live socket errors the endpoint in this version.

use std::io::{self, IoSlice, IoSliceMut};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;

use cavalry_proto::udp::{DATAGRAMS_PER_BURST, DATAGRAMS_PER_POLL, SOCKET_RCVBUF_LENGTH};
#[cfg(not(target_os = "linux"))]
use nix::sys::socket::recvmsg;
use nix::sys::socket::{MsgFlags, SockaddrStorage, getsockopt, sendmsg, setsockopt, sockopt};
#[cfg(target_os = "linux")]
use nix::sys::socket::{MultiHeaders, recvmmsg, sendmmsg};

/// The longest datagram a receive endpoint reads whole: the largest MTU a setup frame
/// may name, 65504 bytes (`spec/udp.md`, "Setup frame"), rounded to the page, so that a
/// sender of any MTU this version accepts is never truncated.
pub(crate) const MAX_DATAGRAM_LENGTH: usize = 65536;

/// The longest datagram a send endpoint reads: a control frame is at most 44 bytes, and
/// a longer datagram is of another kind, read this far and then dropped by its type.
pub(crate) const CONTROL_DATAGRAM_LENGTH: usize = 64;

/// The datagrams the socket took, out of the burst it was given, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sent {
    /// How many datagrams the socket took whole, from the first.
    pub whole: usize,
    /// Whether the socket took only part of the datagram after those.
    pub short: bool,
    /// Whether a datagram after those was refused, with the error; the first refused
    /// datagram of a burst is [`Err`] from the call instead, with nothing sent.
    pub failed: bool,
}

/// The headers `sendmmsg` and `recvmmsg` fill, one set for a burst and one for a poll,
/// preallocated once on the sender's thread and lent to every endpoint's calls: the
/// sender uses one socket at a time, so one set serves them all. `nix`'s
/// `MultiHeaders` holds raw pointers into itself and is not `Send`, which is why the
/// sender makes it as it is built on its thread, and not the conductor in
/// [`SendSocket::open`]. Nothing on another platform, where every call is per datagram
/// with its header on the stack.
pub(crate) struct Headers {
    #[cfg(target_os = "linux")]
    send: MultiHeaders<SockaddrStorage>,
    #[cfg(target_os = "linux")]
    receive: MultiHeaders<SockaddrStorage>,
}

impl Headers {
    /// Allocates the headers for [`DATAGRAMS_PER_BURST`] sends and [`DATAGRAMS_PER_POLL`]
    /// receives, once, with no control message buffer; the one allocation of the
    /// sender's life after its queues.
    pub(crate) fn preallocate() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            send: MultiHeaders::preallocate(DATAGRAMS_PER_BURST, None),
            #[cfg(target_os = "linux")]
            receive: MultiHeaders::preallocate(DATAGRAMS_PER_POLL, None),
        }
    }
}

/// One send endpoint's socket, with the buffers its receives fill; the headers its
/// batched calls fill are the sender's [`Headers`], lent to each call.
pub(crate) struct SendSocket {
    socket: UdpSocket,
    /// What the last poll received, boxed once at open so that the socket stays small
    /// in the request that carries it to the sender.
    polled: Box<Polled>,
}

/// The control datagrams a send endpoint's last poll received.
struct Polled {
    /// One buffer per datagram.
    buffers: [[u8; CONTROL_DATAGRAM_LENGTH]; DATAGRAMS_PER_POLL],
    /// How many bytes each datagram holds.
    received: [usize; DATAGRAMS_PER_POLL],
    /// Where each datagram came from: on a control channel's socket, where an RTT
    /// measurement's answer goes (`spec/udp.md`, "The sender", step 5; ADR 0053).
    sources: [Option<SockaddrStorage>; DATAGRAMS_PER_POLL],
}

impl SendSocket {
    /// Opens a socket bound to `interface`, or to the unspecified address of the
    /// endpoint's family on any port, connected to `endpoint` and set non-blocking, with
    /// the receive buffer asked for at the 131072 bytes of `spec/udp.md`, "Values" (the
    /// host may give less, which [`SendSocket::buffer_lengths`] reads back) and the send
    /// buffer the operating system's; the operating system's reason is the error, which
    /// the conductor answers with error code 4 (`spec/client-protocol.md`, "Error codes").
    pub(crate) fn open(endpoint: SocketAddr, interface: Option<SocketAddr>) -> io::Result<Self> {
        let bind = interface.unwrap_or(match endpoint {
            SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        });
        let socket = UdpSocket::bind(bind)?;
        // A host that caps the length answers with less, read back, not with an error.
        let _ = setsockopt(&socket, sockopt::RcvBuf, &(SOCKET_RCVBUF_LENGTH as usize));
        socket.connect(endpoint)?;
        socket.set_nonblocking(true)?;
        Ok(Self::over(socket))
    }

    /// Opens a control channel's socket: bound to `control`, connected to nothing, set
    /// non-blocking, with the receive buffer asked for as [`SendSocket::open`] asks it;
    /// every send names a destination (`spec/udp.md`, "The sender"; ADR 0053). The
    /// operating system's reason is the error, code 4 to the client.
    pub(crate) fn open_control(control: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(control)?;
        let _ = setsockopt(&socket, sockopt::RcvBuf, &(SOCKET_RCVBUF_LENGTH as usize));
        socket.set_nonblocking(true)?;
        Ok(Self::over(socket))
    }

    fn over(socket: UdpSocket) -> Self {
        Self {
            socket,
            polled: Box::new(Polled {
                buffers: [[0u8; CONTROL_DATAGRAM_LENGTH]; DATAGRAMS_PER_POLL],
                received: [0; DATAGRAMS_PER_POLL],
                sources: [None; DATAGRAMS_PER_POLL],
            }),
        }
    }

    /// The receive and send buffer lengths the socket holds, read back from it, for
    /// `channel.socket_rcvbuf` and `channel.socket_sndbuf`; 0 for one the operating
    /// system would not say.
    pub(crate) fn buffer_lengths(&self) -> (i64, i64) {
        let read =
            |value: nix::Result<usize>| value.map_or(0, |n| i64::try_from(n).unwrap_or(i64::MAX));
        (
            read(getsockopt(&self.socket, sockopt::RcvBuf)),
            read(getsockopt(&self.socket, sockopt::SndBuf)),
        )
    }

    /// Sends up to [`DATAGRAMS_PER_BURST`] datagrams, each one slice, in one call on Linux
    /// through `headers` and one call each elsewhere, to the connected endpoint, or to
    /// `to` on a control channel's socket, and says how many the socket took whole.
    /// `Err` is the operating system's refusal of the first datagram, with nothing
    /// sent.
    pub(crate) fn send_burst(
        &self,
        headers: &mut Headers,
        datagrams: &[[IoSlice<'_>; 1]],
        to: Option<&SockaddrStorage>,
    ) -> io::Result<Sent> {
        let count = datagrams.len().min(DATAGRAMS_PER_BURST);
        let datagrams = &datagrams[..count];
        if count == 0 {
            return Ok(Sent::default());
        }
        #[cfg(target_os = "linux")]
        {
            // One address per datagram, the same one: a copy of 128 bytes each on the
            // stack, no allocation.
            let addresses: [Option<SockaddrStorage>; DATAGRAMS_PER_BURST] =
                std::array::from_fn(|_| to.copied());
            let results = sendmmsg(
                self.socket.as_raw_fd(),
                &mut headers.send,
                datagrams,
                &addresses[..count],
                [],
                MsgFlags::MSG_DONTWAIT,
            )
            .map_err(io::Error::from)?;
            let mut sent = Sent::default();
            for (result, datagram) in results.zip(datagrams) {
                if result.bytes < datagram[0].len() {
                    sent.short = true;
                    break;
                }
                sent.whole += 1;
            }
            Ok(sent)
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Per datagram, each call's header on the stack: the preallocated ones are
            // Linux's.
            let _ = headers;
            let mut sent = Sent::default();
            for datagram in datagrams {
                match sendmsg::<SockaddrStorage>(
                    self.socket.as_raw_fd(),
                    datagram,
                    &[],
                    MsgFlags::MSG_DONTWAIT,
                    to,
                ) {
                    Ok(bytes) if bytes >= datagram[0].len() => sent.whole += 1,
                    Ok(_) => {
                        sent.short = true;
                        break;
                    }
                    Err(errno) if sent.whole == 0 => return Err(io::Error::from(errno)),
                    Err(_) => {
                        sent.failed = true;
                        break;
                    }
                }
            }
            Ok(sent)
        }
    }

    /// Sends one datagram, a control frame, to the connected endpoint or to `to`;
    /// `Ok(false)` when the socket took only part of it, `Err` when it refused it.
    pub(crate) fn send_one(
        &self,
        headers: &mut Headers,
        bytes: &[u8],
        to: Option<&SockaddrStorage>,
    ) -> io::Result<bool> {
        let datagram = [[IoSlice::new(bytes)]];
        let sent = self.send_burst(headers, &datagram, to)?;
        Ok(sent.whole == 1)
    }

    /// Receives up to [`DATAGRAMS_PER_POLL`] datagrams into the endpoint's buffers, one
    /// call on Linux through `headers` and one per datagram elsewhere, and returns how
    /// many arrived, 0 when none waited; [`SendSocket::datagram`] reads each. `Err` is a
    /// refusal other than "nothing to read", which the sender counts in
    /// `receiver.receive_failed`.
    pub(crate) fn receive(&mut self, headers: &mut Headers) -> io::Result<usize> {
        let polled = &mut *self.polled;
        polled.received = [0; DATAGRAMS_PER_POLL];
        polled.sources = [None; DATAGRAMS_PER_POLL];
        #[cfg(target_os = "linux")]
        {
            let buffers: &mut [[u8; CONTROL_DATAGRAM_LENGTH]] = &mut polled.buffers[..];
            let mut rest = buffers;
            let mut slices: [[IoSliceMut<'_>; 1]; DATAGRAMS_PER_POLL] =
                std::array::from_fn(|_| match std::mem::take(&mut rest).split_first_mut() {
                    Some((head, tail)) => {
                        rest = tail;
                        [IoSliceMut::new(head)]
                    }
                    None => [IoSliceMut::new(&mut [])],
                });
            // `recvmmsg` takes each datagram's slices as `&mut`, one item per header.
            let results = match recvmmsg(
                self.socket.as_raw_fd(),
                &mut headers.receive,
                slices.iter_mut(),
                MsgFlags::MSG_DONTWAIT,
                None,
            ) {
                Ok(results) => results,
                Err(nix::errno::Errno::EAGAIN) => return Ok(0),
                Err(errno) => return Err(io::Error::from(errno)),
            };
            let mut count = 0;
            for (result, (received, source)) in
                results.zip(polled.received.iter_mut().zip(polled.sources.iter_mut()))
            {
                *received = result.bytes.min(CONTROL_DATAGRAM_LENGTH);
                *source = result.address;
                count += 1;
            }
            Ok(count)
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Per datagram, each call's header on the stack: the preallocated ones are
            // Linux's.
            let _ = headers;
            let mut count = 0;
            while count < DATAGRAMS_PER_POLL {
                let Some(buffer) = polled.buffers.get_mut(count) else {
                    break;
                };
                let mut slice = [IoSliceMut::new(buffer)];
                match recvmsg::<SockaddrStorage>(
                    self.socket.as_raw_fd(),
                    &mut slice,
                    None,
                    MsgFlags::MSG_DONTWAIT,
                ) {
                    Ok(message) => {
                        polled.received[count] = message.bytes.min(CONTROL_DATAGRAM_LENGTH);
                        polled.sources[count] = message.address;
                        count += 1;
                    }
                    Err(nix::errno::Errno::EAGAIN) => break,
                    Err(errno) if count == 0 => return Err(io::Error::from(errno)),
                    Err(_) => break,
                }
            }
            Ok(count)
        }
    }

    /// The bytes of the `index`th datagram the last [`SendSocket::receive`] took, empty
    /// past what it took.
    pub(crate) fn datagram(&self, index: usize) -> &[u8] {
        match (
            self.polled.buffers.get(index),
            self.polled.received.get(index),
        ) {
            (Some(buffer), Some(&length)) => &buffer[..length.min(CONTROL_DATAGRAM_LENGTH)],
            _ => &[],
        }
    }

    /// Where the `index`th datagram the last [`SendSocket::receive`] took came from,
    /// `None` past what it took or when the operating system gave no address.
    pub(crate) fn source(&self, index: usize) -> Option<&SockaddrStorage> {
        self.polled
            .sources
            .get(index)
            .and_then(|source| source.as_ref())
    }
}

/// The buffers a receive endpoint's poll fills, one set for the receiver thread and lent
/// to every endpoint's poll, as the sender lends its [`Headers`]: [`DATAGRAMS_PER_POLL`]
/// datagrams of [`MAX_DATAGRAM_LENGTH`] bytes with each one's length and source address,
/// and on Linux the headers `recvmmsg` fills, which hold raw pointers and are not `Send`,
/// so the receiver preallocates them as it is built on its own thread (ADR 0042).
pub(crate) struct ReceiveBuffers {
    data: Box<[[u8; MAX_DATAGRAM_LENGTH]; DATAGRAMS_PER_POLL]>,
    lengths: [usize; DATAGRAMS_PER_POLL],
    sources: [Option<SockaddrStorage>; DATAGRAMS_PER_POLL],
    #[cfg(target_os = "linux")]
    headers: MultiHeaders<SockaddrStorage>,
}

impl ReceiveBuffers {
    /// Allocates the buffers and the headers once, with no control message buffer: the
    /// one allocation of the receiver's life after its queues and tables.
    pub(crate) fn preallocate() -> Self {
        Self {
            data: Box::new([[0u8; MAX_DATAGRAM_LENGTH]; DATAGRAMS_PER_POLL]),
            lengths: [0; DATAGRAMS_PER_POLL],
            sources: [None; DATAGRAMS_PER_POLL],
            #[cfg(target_os = "linux")]
            headers: MultiHeaders::preallocate(DATAGRAMS_PER_POLL, None),
        }
    }

    /// The bytes and the source address of the `index`th datagram the last
    /// [`ReceiveSocket::receive`] took; empty and `None` past what it took.
    pub(crate) fn datagram(&self, index: usize) -> (&[u8], Option<&SockaddrStorage>) {
        match (
            self.data.get(index),
            self.lengths.get(index),
            self.sources.get(index),
        ) {
            (Some(buffer), Some(&length), Some(source)) => {
                (&buffer[..length.min(MAX_DATAGRAM_LENGTH)], source.as_ref())
            }
            _ => (&[], None),
        }
    }
}

/// One receive endpoint's socket: a `std::net::UdpSocket` bound to the endpoint and set
/// non-blocking on the control plane as the `add subscription` is handled, with the
/// receive buffer asked for; `recvmmsg` on Linux and `recvmsg` elsewhere take the
/// datagrams with their source addresses, and `sendmsg` with an address sends the
/// receiver's status messages, NAKs and RTT answers to the source of the setup frame
/// that created the image (`spec/udp.md`, "The receiver"; ADR 0042). An unconnected
/// socket, since one endpoint receives from any sender.
pub(crate) struct ReceiveSocket {
    socket: UdpSocket,
}

impl ReceiveSocket {
    /// Opens a socket bound to `endpoint` and set non-blocking, with the receive buffer
    /// asked for at the 131072 bytes of `spec/udp.md`, "Values" (the host may give less,
    /// which [`ReceiveSocket::buffer_lengths`] reads back); the operating system's reason
    /// is the error, which the conductor answers with error code 4.
    pub(crate) fn open(endpoint: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(endpoint)?;
        let _ = setsockopt(&socket, sockopt::RcvBuf, &(SOCKET_RCVBUF_LENGTH as usize));
        socket.set_nonblocking(true)?;
        Ok(Self { socket })
    }

    /// The receive and send buffer lengths the socket holds, as
    /// [`SendSocket::buffer_lengths`] reads them.
    pub(crate) fn buffer_lengths(&self) -> (i64, i64) {
        let read =
            |value: nix::Result<usize>| value.map_or(0, |n| i64::try_from(n).unwrap_or(i64::MAX));
        (
            read(getsockopt(&self.socket, sockopt::RcvBuf)),
            read(getsockopt(&self.socket, sockopt::SndBuf)),
        )
    }

    /// Receives up to [`DATAGRAMS_PER_POLL`] datagrams into `buffers`, one call on Linux
    /// and one per datagram elsewhere, each with its source address, and returns how
    /// many arrived, 0 when none waited; [`ReceiveBuffers::datagram`] reads each. `Err`
    /// is a refusal other than "nothing to read", counted in `receiver.receive_failed`.
    pub(crate) fn receive(&self, buffers: &mut ReceiveBuffers) -> io::Result<usize> {
        buffers.lengths = [0; DATAGRAMS_PER_POLL];
        buffers.sources = [None; DATAGRAMS_PER_POLL];
        #[cfg(target_os = "linux")]
        {
            let data: &mut [[u8; MAX_DATAGRAM_LENGTH]] = &mut buffers.data[..];
            let mut rest = data;
            let mut slices: [[IoSliceMut<'_>; 1]; DATAGRAMS_PER_POLL] =
                std::array::from_fn(|_| match std::mem::take(&mut rest).split_first_mut() {
                    Some((head, tail)) => {
                        rest = tail;
                        [IoSliceMut::new(head)]
                    }
                    None => [IoSliceMut::new(&mut [])],
                });
            let results = match recvmmsg(
                self.socket.as_raw_fd(),
                &mut buffers.headers,
                slices.iter_mut(),
                MsgFlags::MSG_DONTWAIT,
                None,
            ) {
                Ok(results) => results,
                Err(nix::errno::Errno::EAGAIN) => return Ok(0),
                Err(errno) => return Err(io::Error::from(errno)),
            };
            let mut count = 0;
            for (result, (length, source)) in
                results.zip(buffers.lengths.iter_mut().zip(buffers.sources.iter_mut()))
            {
                *length = result.bytes.min(MAX_DATAGRAM_LENGTH);
                *source = result.address;
                count += 1;
            }
            Ok(count)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut count = 0;
            while count < DATAGRAMS_PER_POLL {
                let Some(buffer) = buffers.data.get_mut(count) else {
                    break;
                };
                let mut slice = [IoSliceMut::new(buffer)];
                match recvmsg::<SockaddrStorage>(
                    self.socket.as_raw_fd(),
                    &mut slice,
                    None,
                    MsgFlags::MSG_DONTWAIT,
                ) {
                    Ok(message) => {
                        buffers.lengths[count] = message.bytes.min(MAX_DATAGRAM_LENGTH);
                        buffers.sources[count] = message.address;
                        count += 1;
                    }
                    Err(nix::errno::Errno::EAGAIN) => break,
                    Err(errno) if count == 0 => return Err(io::Error::from(errno)),
                    Err(_) => break,
                }
            }
            Ok(count)
        }
    }

    /// Sends one control frame to `to` in one `sendmsg` with no control message, which
    /// allocates nothing (ADR 0041, Evidence); `Ok(false)` when the socket took only
    /// part of it, `Err` when it refused it.
    pub(crate) fn send_to(&self, bytes: &[u8], to: &SockaddrStorage) -> io::Result<bool> {
        let iov = [IoSlice::new(bytes)];
        match sendmsg(
            self.socket.as_raw_fd(),
            &iov,
            &[],
            MsgFlags::MSG_DONTWAIT,
            Some(to),
        ) {
            Ok(sent) => Ok(sent >= bytes.len()),
            Err(errno) => Err(io::Error::from(errno)),
        }
    }
}

impl std::fmt::Debug for ReceiveSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiveSocket")
            .field("local", &self.socket.local_addr().ok())
            .finish()
    }
}

impl std::fmt::Debug for SendSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendSocket")
            .field("local", &self.socket.local_addr().ok())
            .field("peer", &self.socket.peer_addr().ok())
            .finish()
    }
}

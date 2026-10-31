//! A send endpoint's socket (`spec/udp.md`, "The sender" and "The receiver"; ADR
//! 0041): a `std::net::UdpSocket` opened, bound to the interface when one is named,
//! connected to the endpoint and set non-blocking on the control plane as the command
//! is handled, and `nix` on its descriptor for what `std` lacks: one `sendmmsg` per burst
//! on Linux and one `sendmsg` per datagram elsewhere, `recvmmsg` or `recvmsg` for the
//! control frames the endpoint receives, and the buffer lengths read back for
//! `channel.socket_rcvbuf` and `channel.socket_sndbuf`. Nothing here allocates after
//! [`SendSocket::open`]: the headers the batched calls fill are preallocated once, and
//! no call passes a control message, so none allocates per call (ADR 0041, Evidence).
//! A send or a receive the operating system refuses is returned to the sender, which
//! counts it and tries again on its next cycle; no failure on a live socket errors the
//! endpoint in this version.

use std::io::{self, IoSlice, IoSliceMut};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;

use cavalry_proto::udp::{DATAGRAMS_PER_BURST, DATAGRAMS_PER_POLL, SOCKET_RCVBUF_LENGTH};
use nix::sys::socket::{MsgFlags, getsockopt, setsockopt, sockopt};
#[cfg(target_os = "linux")]
use nix::sys::socket::{MultiHeaders, recvmmsg, sendmmsg};
#[cfg(not(target_os = "linux"))]
use nix::sys::socket::{recvmsg, sendmsg};

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

/// One send endpoint's socket, with the preallocated headers and buffers its calls use.
pub(crate) struct SendSocket {
    socket: UdpSocket,
    #[cfg(target_os = "linux")]
    send_headers: MultiHeaders<()>,
    #[cfg(target_os = "linux")]
    receive_headers: MultiHeaders<()>,
    /// The control datagrams the last poll received, one buffer each.
    receive_buffers: Box<[[u8; CONTROL_DATAGRAM_LENGTH]; DATAGRAMS_PER_POLL]>,
    /// How many bytes each of the last poll's datagrams holds.
    received: [usize; DATAGRAMS_PER_POLL],
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
        Ok(Self {
            socket,
            #[cfg(target_os = "linux")]
            send_headers: MultiHeaders::preallocate(DATAGRAMS_PER_BURST, None),
            #[cfg(target_os = "linux")]
            receive_headers: MultiHeaders::preallocate(DATAGRAMS_PER_POLL, None),
            receive_buffers: Box::new([[0u8; CONTROL_DATAGRAM_LENGTH]; DATAGRAMS_PER_POLL]),
            received: [0; DATAGRAMS_PER_POLL],
        })
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
    /// and one call each elsewhere, and says how many the socket took whole. `Err` is the
    /// operating system's refusal of the first datagram, with nothing sent.
    pub(crate) fn send_burst(&mut self, datagrams: &[[IoSlice<'_>; 1]]) -> io::Result<Sent> {
        let count = datagrams.len().min(DATAGRAMS_PER_BURST);
        let datagrams = &datagrams[..count];
        if count == 0 {
            return Ok(Sent::default());
        }
        #[cfg(target_os = "linux")]
        {
            const NO_ADDRESS: [Option<()>; DATAGRAMS_PER_BURST] = [None; DATAGRAMS_PER_BURST];
            let results = sendmmsg(
                self.socket.as_raw_fd(),
                &mut self.send_headers,
                datagrams,
                &NO_ADDRESS[..count],
                &[],
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
            let mut sent = Sent::default();
            for datagram in datagrams {
                match sendmsg::<()>(
                    self.socket.as_raw_fd(),
                    datagram,
                    &[],
                    MsgFlags::MSG_DONTWAIT,
                    None,
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

    /// Sends one datagram, a control frame; `Ok(false)` when the socket took only part
    /// of it, `Err` when it refused it.
    pub(crate) fn send_one(&mut self, bytes: &[u8]) -> io::Result<bool> {
        let datagram = [[IoSlice::new(bytes)]];
        let sent = self.send_burst(&datagram)?;
        Ok(sent.whole == 1)
    }

    /// Receives up to [`DATAGRAMS_PER_POLL`] datagrams into the endpoint's buffers, one
    /// call on Linux and one per datagram elsewhere, and returns how many arrived, 0 when
    /// none waited; [`SendSocket::datagram`] reads each. `Err` is a refusal other than
    /// "nothing to read", which the sender counts in `receiver.receive_failed`.
    pub(crate) fn receive(&mut self) -> io::Result<usize> {
        self.received = [0; DATAGRAMS_PER_POLL];
        #[cfg(target_os = "linux")]
        {
            let buffers: &mut [[u8; CONTROL_DATAGRAM_LENGTH]] = &mut self.receive_buffers[..];
            let mut rest = buffers;
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
                &mut self.receive_headers,
                slices.iter_mut().map(|slice| &*slice),
                MsgFlags::MSG_DONTWAIT,
                None,
            ) {
                Ok(results) => results,
                Err(nix::errno::Errno::EAGAIN) => return Ok(0),
                Err(errno) => return Err(io::Error::from(errno)),
            };
            let mut count = 0;
            for (result, received) in results.zip(self.received.iter_mut()) {
                *received = result.bytes.min(CONTROL_DATAGRAM_LENGTH);
                count += 1;
            }
            Ok(count)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut count = 0;
            while count < DATAGRAMS_PER_POLL {
                let Some(buffer) = self.receive_buffers.get_mut(count) else {
                    break;
                };
                let mut slice = [IoSliceMut::new(buffer)];
                match recvmsg::<()>(
                    self.socket.as_raw_fd(),
                    &mut slice,
                    None,
                    MsgFlags::MSG_DONTWAIT,
                ) {
                    Ok(message) => {
                        self.received[count] = message.bytes.min(CONTROL_DATAGRAM_LENGTH);
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
        match (self.receive_buffers.get(index), self.received.get(index)) {
            (Some(buffer), Some(&length)) => &buffer[..length.min(CONTROL_DATAGRAM_LENGTH)],
            _ => &[],
        }
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

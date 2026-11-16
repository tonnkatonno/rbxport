//! Binding the sockets the three RPC programs answer on.
//!
//! One blocking thread per socket, using `std::net`. There is no async runtime
//! here on purpose: a datagram in, a reply out, with no I/O in between except
//! one bounded file read, so a thread costs a stack and nothing else, while a
//! runtime would cost a dependency and a scheduler on the read path.

use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::Server;

/// The largest datagram we will read. A request is small — a `READ` asks for
/// 32 KB but carries none — so this is headroom for a long `LOOKUP` name and
/// a generous `READDIR` budget, not for data.
const DATAGRAM: usize = 65_535;

/// [STATIC] libFilSiNE `_tkfTransportOpen` requests 0x10000 bytes for both
/// `SO_SNDBUF` and `SO_RCVBUF`. The OS may clamp or account for these differently.
const SOCKET_BUFFER: usize = 0x10000;

/// A call that takes this long to answer is logged by name (fork diagnostics).
const SLOW_CALL: Duration = Duration::from_millis(20);

/// Replies larger than this (more than one Ethernet frame) go twice to a lossy peer.
const DUPLICATE_ABOVE: usize = 1400;
/// How long a peer counts as lossy after it last repeated a call.
const LOSSY_FOR: Duration = Duration::from_secs(120);
/// How many recent call xids are remembered per peer to spot a repeat.
const RECENT_XIDS: usize = 64;

/// Peers that repeated a call recently (fork): the player resends a call only when no reply reached it.
#[derive(Default)]
struct LossyPeers {
    recent: std::collections::HashMap<std::net::Ipv4Addr, std::collections::VecDeque<u32>>,
    until: std::collections::HashMap<std::net::Ipv4Addr, std::time::Instant>,
}

impl LossyPeers {
    /// Records a call; true while `peer` counts as lossy (this call being a repeat makes it so).
    fn note(&mut self, peer: std::net::Ipv4Addr, datagram: &[u8], now: std::time::Instant) -> bool {
        // Only calls (message type 0) carry a fresh xid worth remembering.
        if let (Some(x), Some([0, 0, 0, 0])) = (datagram.get(..4), datagram.get(4..8)) {
            let xid = u32::from_be_bytes([x[0], x[1], x[2], x[3]]);
            let recent = self.recent.entry(peer).or_default();
            if recent.contains(&xid) {
                let was = self.until.get(&peer).is_some_and(|t| *t > now);
                self.until.insert(peer, now + LOSSY_FOR);
                if !was {
                    tracing::info!(%peer, xid, "a player repeated a call; its large replies go twice for a while");
                }
            } else {
                if recent.len() == RECENT_XIDS {
                    recent.pop_front();
                }
                recent.push_back(xid);
            }
        }
        self.until.get(&peer).is_some_and(|t| *t > now)
    }
}

/// `nfs READ xid=…` for a log line, from the call's RPC header.
fn rpc_label(datagram: &[u8]) -> String {
    let word = |at: usize| datagram.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    match (word(0), word(12), word(20)) {
        (Some(xid), Some(program), Some(procedure)) => format!("{} xid={xid}", crate::call_name(program, procedure)),
        _ => "unreadable".to_owned(),
    }
}

/// How long a socket blocks before checking whether it has been asked to stop.
const POLL: Duration = Duration::from_millis(100);

/// Serves RPC on one bound socket until `stop` is set.
///
/// Returns only on an error that is not worth continuing through; a malformed
/// datagram, or one from a peer that has gone away, is counted and skipped.
pub fn serve(server: &Arc<Server>, socket: &UdpSocket, stop: &Arc<AtomicBool>) -> io::Result<()> {
    let socket_id = crate::Receiver::default();
    socket.set_read_timeout(Some(POLL))?;
    // Match `_tkfTransportOpen`: broadcast, then receive and send buffers.
    socket.set_broadcast(true)?;
    let socket_ref = socket2::SockRef::from(socket);
    socket_ref.set_recv_buffer_size(SOCKET_BUFFER)?;
    socket_ref.set_send_buffer_size(SOCKET_BUFFER)?;
    let mut buffer = vec![0_u8; DATAGRAM];
    let duplicate = std::env::var("RBXPORT_NFS_DUPLICATE").map_or(true, |v| v.trim() != "0");
    let mut lossy_peers = LossyPeers::default();
    while !stop.load(Ordering::Relaxed) {
        let (len, from) = match socket.recv_from(&mut buffer) {
            Ok(received) => received,
            Err(error) if is_timeout(&error) => continue,
            // A datagram whose peer has vanished surfaces here on some
            // platforms; it says nothing about the socket's health.
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {
                tracing::trace!(%error, "a peer went away; ignored");
                continue;
            }
            Err(error) => return Err(error),
        };
        tracing::trace!(%from, len, "RPC datagram received");
        let peer = match from {
            SocketAddr::V4(v4) => *v4.ip(),
            SocketAddr::V6(v6) => v6.ip().to_ipv4_mapped().unwrap_or(std::net::Ipv4Addr::UNSPECIFIED),
        };
        let received = std::time::Instant::now();
        let lossy = duplicate && lossy_peers.note(peer, buffer.get(..len).unwrap_or(&[]), received);
        let Some(reply) = server.handle_on(buffer.get(..len).unwrap_or(&[]), peer, from.port(), socket_id) else {
            continue;
        };
        let handled = received.elapsed();
        let started = std::time::Instant::now();
        let call = rpc_label(buffer.get(..len).unwrap_or(&[]));
        match socket.send_to(&reply, from) {
            Ok(sent) => {
                let sending = started.elapsed();
                // Fork diagnostics: one thread serves every player on this socket, so a single slow call holds all
                // of them up; a CDJ-2000NXS2 then stops playing or reboots [OBS 2026-11-15]. Name each slow one.
                if handled + sending >= SLOW_CALL {
                    tracing::warn!(%from, call = %call, sent, handle_ms = handled.as_millis(), send_ms = sending.as_millis(), "slow RPC call");
                }
                tracing::trace!(%from, sent, elapsed_us = sending.as_micros(), "RPC reply sent");
                // Fork: a peer that has had to repeat a call lately is on a line that drops datagrams; a large reply
                // goes out as a dozen IP fragments and is lost with any one of them, and the player then waits a
                // whole second before asking again. A second copy makes a loss need both copies to go [OBS
                // 2026-11-16, CDJ-2000NXS2 on a bad cable: 1.5-19% of replies repeated, load never finished]. The
                // player drops the copy it does not need by its xid, as it drops a late reply to a repeated call.
                if lossy && reply.len() > DUPLICATE_ABOVE {
                    if let Err(error) = socket.send_to(&reply, from) {
                        tracing::debug!(%from, %error, "second copy of a reply not sent");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%from, %error, len = reply.len(), "could not send an RPC reply");
            }
        }
    }
    Ok(())
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// The three sockets a player needs, bound and serving.
///
/// Ports are taken as arguments rather than hard-coded so a test can bind
/// ephemeral loopback ports: the real ones are held by rekordbox whenever it
/// is running, and a test that fought it for them would be a test that only
/// passes when the app under test is closed.
#[derive(Debug)]
pub struct Bound {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    portmap: SocketAddr,
    mount: SocketAddr,
    nfs: SocketAddr,
    server: Arc<Server>,
}

impl Bound {
    /// Advertises portmap, mount and NFS on `address`, and starts serving.
    /// [STATIC] libFilSiNE binds its sockets to `INADDR_ANY`.
    /// [LOCAL] On macOS, pin those sockets to the advertised interface so
    /// the default Wi-Fi route cannot override the user's adapter selection.
    /// Other platforms bind the advertised address.
    ///
    /// A port of 0 asks the OS for a free one, which is what the tests use.
    pub fn start(
        exports: crate::Exports,
        address: std::net::IpAddr,
        portmap_port: u16,
        mount_port: u16,
        nfs_port: u16,
        export_host: Option<String>,
        up: Option<Arc<std::sync::atomic::AtomicU8>>,
    ) -> io::Result<Self> {
        let portmap_socket = bind_rpc(address, portmap_port)?;
        let mount_socket = bind_rpc(address, mount_port)?;
        let nfs_socket = bind_rpc(address, nfs_port)?;

        let (portmap, mount, nfs) = (
            SocketAddr::new(address, portmap_socket.local_addr()?.port()),
            SocketAddr::new(address, mount_socket.local_addr()?.port()),
            SocketAddr::new(address, nfs_socket.local_addr()?.port()),
        );
        tracing::debug!(%portmap, %mount, %nfs, "file server bound");

        // Portmap must report the ports actually bound, which with ephemeral
        // ports are not known until now.
        let mut server = Server::new(exports, nfs.port(), mount.port());
        if let Some(host) = export_host {
            server = server.with_export_host(host);
        }
        if let Some(up) = up {
            server = server.with_gate(up);
        }
        let server = Arc::new(server);
        let stop = Arc::new(AtomicBool::new(false));

        let mut threads = Vec::with_capacity(3);
        for (name, socket) in [
            ("portmap", portmap_socket),
            ("mount", mount_socket),
            ("nfs", nfs_socket),
        ] {
            let server = Arc::clone(&server);
            let stop = Arc::clone(&stop);
            threads.push(std::thread::spawn(move || {
                if let Err(error) = serve(&server, &socket, &stop) {
                    tracing::error!(program = name, %error, "RPC socket stopped; players cannot read files");
                }
            }));
        }

        Ok(Self { stop, threads, portmap, mount, nfs, server })
    }

    /// Whether `host` has an export mounted.
    pub fn is_mounted(&self, host: std::net::Ipv4Addr) -> bool {
        self.server.is_mounted(host)
    }

    /// The hosts with an export mounted.
    pub fn mounted_hosts(&self) -> Vec<std::net::Ipv4Addr> {
        self.server.mounted_hosts()
    }

    pub const fn portmap_address(&self) -> SocketAddr {
        self.portmap
    }

    pub const fn mount_address(&self) -> SocketAddr {
        self.mount
    }

    pub const fn nfs_address(&self) -> SocketAddr {
        self.nfs
    }

    /// Stops the threads and waits for them.
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            // A thread that panicked has already logged; there is nothing to
            // recover here, and the caller is shutting down either way.
            drop(thread.join());
        }
        tracing::debug!("file server stopped");
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Keep RPC replies on the adapter whose address LINK advertises.
fn bind_rpc(address: IpAddr, port: u16) -> io::Result<UdpSocket> {
    #[cfg(target_vendor = "apple")]
    {
        let bind_address = match address {
            IpAddr::V4(_) => IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
        };
        let socket = UdpSocket::bind(SocketAddr::new(bind_address, port))?;
        if !address.is_unspecified() {
            let index = if_addrs::get_if_addrs()?
                .into_iter()
                .find(|interface| interface.ip() == address)
                .and_then(|interface| interface.index)
                .and_then(std::num::NonZeroU32::new)
                .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable,
                    format!("no network interface has address {address}")))?;
            let socket_ref = socket2::SockRef::from(&socket);
            match address {
                IpAddr::V4(_) => socket_ref.bind_device_by_index_v4(Some(index))?,
                IpAddr::V6(_) => socket_ref.bind_device_by_index_v6(Some(index))?,
            }
        }
        Ok(socket)
    }
    #[cfg(not(target_vendor = "apple"))]
    UdpSocket::bind(SocketAddr::new(address, port))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn call(xid: u32) -> Vec<u8> {
        let mut d = xid.to_be_bytes().to_vec();
        d.extend([0_u8; 20]);
        d
    }

    #[test]
    fn a_peer_turns_lossy_when_it_repeats_a_call_and_recovers_after_a_while() {
        let mut peers = LossyPeers::default();
        let a = std::net::Ipv4Addr::new(192, 168, 8, 150);
        let b = std::net::Ipv4Addr::new(192, 168, 8, 244);
        let t = std::time::Instant::now();
        assert!(!peers.note(a, &call(1), t));
        assert!(!peers.note(a, &call(2), t));
        assert!(peers.note(a, &call(2), t), "the repeat of xid 2 marks the peer lossy");
        assert!(peers.note(a, &call(3), t + Duration::from_secs(60)));
        assert!(!peers.note(a, &call(4), t + LOSSY_FOR + Duration::from_secs(1)), "and it wears off");
        assert!(!peers.note(b, &call(2), t), "another peer's xid 2 is its own");
        let mut reply = call(9);
        reply[7] = 1;                    // a reply (message type 1) is never a repeated call
        assert!(!peers.note(b, &reply, t) && !peers.note(b, &reply, t));
    }
}

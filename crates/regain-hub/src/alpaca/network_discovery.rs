//! User-requested UDP candidate search. Replies are untrusted addresses, never
//! identities: no HTTP catalog, credential lookup, source lease or adoption.
use crate::source::{ErrorKind, SourceError};
use futures_util::{StreamExt, stream::FuturesUnordered};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    net::{Ipv6Addr, SocketAddr, SocketAddrV6},
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    time::{Instant, timeout_at},
};
use uuid::Uuid;

pub const TIMEOUT: Duration = Duration::from_secs(3);
pub const PORT: u16 = 32227;
pub const MAX_INTERFACES: usize = 64;
pub const MAX_SERVERS: usize = 256;
pub const MAX_DATAGRAMS: usize = 4096;
pub const MAX_REPLY_BYTES: usize = 1024;
const REQUEST: &[u8] = b"alpacadiscovery1";
const GROUP: Ipv6Addr = Ipv6Addr::new(0xff12, 0, 0, 0, 0, 0, 0xa1, 0x9aca);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Server {
    #[schemars(length(min = 2, max = 45))]
    pub address: String,
    #[schemars(range(min = 0, max = 4294967295u64))]
    pub scope_id: u32,
    #[schemars(range(min = 1, max = 65535))]
    pub port: u16,
    #[schemars(length(min = 1, max = 128))]
    pub base_url: String,
}
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Search {
    pub configuration_revision: Uuid,
    #[schemars(length(max = 256))]
    pub servers: Vec<Server>,
    #[schemars(range(max = 64))]
    pub interfaces_tried: u32,
    #[schemars(range(max = 64))]
    pub interfaces_failed: u32,
    #[schemars(range(max = 4096))]
    pub ignored_datagrams: u32,
    pub incomplete: bool,
}
pub fn description() -> Value {
    json!({"operation":"searchAlpaca", "opensSource":false, "writesEquipment":false,
        "persistsConfiguration":false, "readsCatalog":false, "usesCredentials":false,
        "timeoutSeconds":TIMEOUT.as_secs(), "port":PORT, "maximumInterfaces":MAX_INTERFACES,
        "maximumServers":MAX_SERVERS, "maximumDatagrams":MAX_DATAGRAMS, "maximumReplyBytes":MAX_REPLY_BYTES,
        "responseSchema":schemars::schema_for!(Search),
        "label":"Find Alpaca servers on the host's local networks",
        "description":"Search from the shared host using IPv4 broadcast and IPv6 multicast. Select a candidate, then explicitly read its device catalog. UDP replies do not prove server or device identity. Reverse-proxy prefixes and HTTPS must be entered manually. IPv6 link-local addresses keep their host interface scope separately."})
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Target {
    bind: SocketAddr,
    destination: SocketAddr,
}
fn targets(interfaces: Vec<if_addrs::Interface>) -> Vec<Target> {
    let mut selected = BTreeSet::new();
    let mut ipv6 = BTreeMap::<u32, Ipv6Addr>::new();
    for interface in interfaces {
        if !interface.is_oper_up()
            || interface.ip().is_unspecified()
            || interface.ip().is_multicast()
        {
            continue;
        }
        match interface.addr {
            if_addrs::IfAddr::V4(address) => {
                let destination = if address.ip.is_loopback() {
                    Some(address.ip)
                } else if !interface.is_p2p {
                    address.broadcast
                } else {
                    None
                };
                if let Some(destination) =
                    destination.filter(|ip| !ip.is_unspecified() && !ip.is_multicast())
                {
                    selected.insert(Target {
                        bind: (address.ip, 0).into(),
                        destination: (destination, PORT).into(),
                    });
                }
            }
            if_addrs::IfAddr::V6(address) => {
                if address.ip.is_loopback() {
                    selected.insert(Target {
                        bind: (address.ip, 0).into(),
                        destination: (address.ip, PORT).into(),
                    });
                } else if let Some(index) = interface.index.filter(|i| *i != 0)
                    && !address.ip.is_unspecified()
                    && !address.ip.is_multicast()
                {
                    // One multicast query per IPv6 interface. Prefer its link-local
                    // source; do not erase the interface index or merge zones.
                    ipv6.entry(index)
                        .and_modify(|ip| {
                            if address.ip.is_unicast_link_local() && !ip.is_unicast_link_local()
                                || address.ip.is_unicast_link_local() == ip.is_unicast_link_local()
                                    && address.ip < *ip
                            {
                                *ip = address.ip;
                            }
                        })
                        .or_insert(address.ip);
                }
            }
        }
    }
    for (index, address) in ipv6 {
        selected.insert(Target {
            bind: SocketAddrV6::new(address, 0, 0, index).into(),
            destination: SocketAddrV6::new(GROUP, PORT, 0, index).into(),
        });
    }
    selected.into_iter().collect()
}
fn socket(target: Target) -> io::Result<UdpSocket> {
    let socket = Socket::new(
        if target.bind.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        },
        Type::DGRAM,
        Some(Protocol::UDP),
    )?;
    socket.set_nonblocking(true)?;
    if let SocketAddr::V6(destination) = target.destination {
        socket.set_only_v6(true)?;
        if destination.ip().is_multicast() {
            socket.set_multicast_if_v6(destination.scope_id())?;
            socket.set_multicast_hops_v6(1)?;
        }
    } else {
        socket.set_broadcast(true)?;
    }
    socket.bind(&target.bind.into())?;
    UdpSocket::from_std(socket.into())
}

#[derive(Deserialize)]
struct Reply {
    #[serde(rename = "AlpacaPort")]
    port: u16,
}
fn candidate(bytes: &[u8], sender: SocketAddr, interface: Target) -> Option<(SocketAddr, Server)> {
    if bytes.len() > MAX_REPLY_BYTES || sender.is_ipv4() != interface.bind.is_ipv4() {
        return None;
    }
    // Serde structs also accept positional JSON arrays; the wire protocol
    // requires an object even when its only known field is AlpacaPort.
    if bytes
        .iter()
        .find(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        != Some(&b'{')
    {
        return None;
    }
    // Typed decoding rejects duplicate keys, fractions, strings and bad ports;
    // unknown extension fields remain allowed by the Alpaca discovery protocol.
    let reply: Reply = serde_json::from_slice(bytes).ok()?;
    if reply.port == 0 || sender.ip().is_unspecified() || sender.ip().is_multicast() {
        return None;
    }
    let endpoint = match sender {
        SocketAddr::V4(mut address) => {
            if address.ip().is_broadcast() || address.ip().octets()[0] == 0 {
                return None;
            }
            address.set_port(reply.port);
            SocketAddr::V4(address)
        }
        SocketAddr::V6(mut address) => {
            // IPv4-mapped addresses and contradictory scopes are not discovery
            // endpoints. The receiving interface supplies an omitted local zone.
            if address.ip().to_ipv4_mapped().is_some() {
                return None;
            }
            address.set_port(reply.port);
            address.set_flowinfo(0);
            let index = match interface.bind {
                SocketAddr::V6(bind) => bind.scope_id(),
                _ => return None,
            };
            if address.ip().is_unicast_link_local() {
                if index == 0 || address.scope_id() != 0 && address.scope_id() != index {
                    return None;
                }
                address.set_scope_id(index);
            } else {
                address.set_scope_id(0);
            }
            SocketAddr::V6(address)
        }
    };
    Some((
        endpoint,
        Server {
            address: endpoint.ip().to_string(),
            scope_id: match endpoint {
                SocketAddr::V6(a) => a.scope_id(),
                _ => 0,
            },
            port: reply.port,
            base_url: match endpoint {
                SocketAddr::V6(address) => format!("http://[{}]:{}", address.ip(), address.port()),
                SocketAddr::V4(address) => format!("http://{address}"),
            },
        },
    ))
}
pub(crate) async fn search(
    revision: Uuid,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<Search, SourceError> {
    if revision.is_nil() {
        return Err(SourceError::new(
            ErrorKind::InvalidValue,
            "Select the current configuration revision",
        ));
    }
    let deadline = Instant::now() + TIMEOUT;
    // Retain admission even if a cancelled/timed-out waiter abandons the OS
    // enumeration. A stuck OS call cannot create unlimited blocking workers.
    let (interfaces, _permit) = timeout_at(
        deadline,
        tokio::task::spawn_blocking(move || (if_addrs::get_if_addrs(), permit)),
    )
    .await
    .map_err(|_| {
        SourceError::new(
            ErrorKind::Transient,
            "Network interface discovery timed out",
        )
    })?
    .map_err(|_| SourceError::new(ErrorKind::Unavailable, "Network interfaces are unavailable"))?;
    let interfaces = interfaces.map_err(|_| {
        SourceError::new(ErrorKind::Unavailable, "Network interfaces are unavailable")
    })?;
    Ok(collect(targets(interfaces), revision, deadline).await)
}
async fn receive(
    socket: UdpSocket,
    target: Target,
) -> (UdpSocket, Target, io::Result<(Vec<u8>, SocketAddr)>) {
    // Receive the whole possible UDP datagram, then enforce the smaller protocol
    // limit. Windows otherwise returns WSAEMSGSIZE and a single oversized reply
    // could terminate this interface's search. Memory is bounded by 64 sockets.
    let mut bytes = vec![0; 65536];
    let result = socket.recv_from(&mut bytes).await.map(|(n, sender)| {
        bytes.truncate(n);
        (bytes, sender)
    });
    (socket, target, result)
}
async fn collect(targets: Vec<Target>, revision: Uuid, deadline: Instant) -> Search {
    let mut result = Search {
        configuration_revision: revision,
        servers: vec![],
        interfaces_tried: 0,
        interfaces_failed: 0,
        ignored_datagrams: 0,
        incomplete: targets.len() > MAX_INTERFACES,
    };
    let mut pending = FuturesUnordered::new();
    for target in targets.into_iter().take(MAX_INTERFACES) {
        result.interfaces_tried += 1;
        let opened = socket(target);
        let sent = match opened {
            Ok(socket) => {
                match timeout_at(deadline, socket.send_to(REQUEST, target.destination)).await {
                    Ok(Ok(n)) if n == REQUEST.len() => Some(socket),
                    _ => None,
                }
            }
            Err(_) => None,
        };
        if let Some(socket) = sent {
            pending.push(receive(socket, target));
        } else {
            result.interfaces_failed += 1;
            result.incomplete = true;
        }
    }
    let mut servers = BTreeMap::new();
    let mut received = 0;
    while !pending.is_empty() && received < MAX_DATAGRAMS {
        let Ok(Some((socket, target, packet))) = timeout_at(deadline, pending.next()).await else {
            break;
        };
        match packet {
            Ok((bytes, sender)) => {
                received += 1;
                if let Some((key, server)) = candidate(&bytes, sender, target) {
                    if servers.contains_key(&key) || servers.len() < MAX_SERVERS {
                        servers.insert(key, server);
                    } else {
                        result.incomplete = true;
                    }
                } else {
                    result.ignored_datagrams += 1;
                }
                pending.push(receive(socket, target));
            }
            Err(_) => {
                result.interfaces_failed += 1;
                result.incomplete = true;
            }
        }
    }
    if received == MAX_DATAGRAMS {
        result.incomplete = true;
    }
    // Dropping the futures closes every query socket, including on cancellation.
    result.servers = servers.into_values().collect();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(destination: SocketAddr) -> Target {
        Target {
            bind: if destination.is_ipv4() {
                "127.0.0.1:0".parse().unwrap()
            } else {
                "[::1]:0".parse().unwrap()
            },
            destination,
        }
    }
    fn decode(body: &[u8]) -> Option<Server> {
        candidate(
            body,
            "192.0.2.3:1234".parse().unwrap(),
            target("127.0.0.1:9".parse().unwrap()),
        )
        .map(|(_, server)| server)
    }
    fn interface(ip: &str, index: u32) -> if_addrs::Interface {
        let ip: std::net::IpAddr = ip.parse().unwrap();
        if_addrs::Interface {
            name: "[SIMULATION] interface".into(),
            index: Some(index),
            oper_status: if_addrs::IfOperStatus::Up,
            is_p2p: false,
            #[cfg(windows)]
            adapter_name: "[SIMULATION] adapter".into(),
            addr: match ip {
                std::net::IpAddr::V4(ip) => if_addrs::IfAddr::V4(if_addrs::Ifv4Addr {
                    ip,
                    netmask: "255.255.255.0".parse().unwrap(),
                    prefixlen: 24,
                    broadcast: Some("192.0.2.255".parse().unwrap()),
                }),
                std::net::IpAddr::V6(ip) => if_addrs::IfAddr::V6(if_addrs::Ifv6Addr {
                    ip,
                    netmask: "ffff:ffff:ffff:ffff::".parse().unwrap(),
                    prefixlen: 64,
                    broadcast: None,
                }),
            },
        }
    }
    #[test]
    fn plans_up_interfaces_with_broadcast_multicast_and_loopback_without_zone_aliases() {
        let mut down = interface("198.51.100.1", 3);
        down.oper_status = if_addrs::IfOperStatus::Down;
        let mut tunnel = interface("203.0.113.1", 4);
        tunnel.is_p2p = true;
        let inputs = vec![
            interface("192.0.2.1", 1),
            interface("192.0.2.1", 1),
            interface("127.0.0.1", 2),
            interface("2001:db8::1", 7),
            interface("fe80::3", 7),
            interface("fe80::2", 7),
            interface("fe80::2", 8),
            interface("fe80::4", 0),
            interface("0.0.0.0", 9),
            interface("239.1.2.3", 10),
            interface("::", 11),
            interface("ff02::1", 12),
            interface("::1", 2),
            down,
            tunnel,
        ];
        let expected = targets(inputs.clone());
        let mut reversed = inputs;
        reversed.reverse();
        assert_eq!(targets(reversed), expected);
        assert_eq!(expected.len(), 5);
        assert!(expected.contains(&Target {
            bind: "192.0.2.1:0".parse().unwrap(),
            destination: "192.0.2.255:32227".parse().unwrap()
        }));
        for scope in [7, 8] {
            assert!(expected.contains(&Target {
                bind: format!("[fe80::2%{scope}]:0").parse().unwrap(),
                destination: format!("[ff12::a1:9aca%{scope}]:32227").parse().unwrap()
            }));
        }
    }
    #[test]
    fn strict_port_decoder_allows_extensions_but_not_ambiguous_or_invalid_fields() {
        let server = decode(br#"{"AlpacaPort":11111,"FutureExtension":{"value":true}}"#).unwrap();
        assert_eq!(server.base_url, "http://192.0.2.3:11111");
        assert_eq!(server.scope_id, 0);
        for body in [
            r#"{"AlpacaPort":0}"#,
            r#"{"AlpacaPort":65536}"#,
            r#"{"AlpacaPort":1.0}"#,
            r#"{"AlpacaPort":-1}"#,
            r#"{"AlpacaPort":"11111"}"#,
            r#"{"AlpacaPort":null}"#,
            r#"{"alpacaport":11111}"#,
            r#"{"AlpacaPort":1,"AlpacaPort":2}"#,
            r#"[11111]"#,
            r#"{"AlpacaPort":1} garbage"#,
            r#"{}"#,
        ] {
            assert!(decode(body.as_bytes()).is_none(), "{body}");
        }
        let oversized = format!(
            "{{\"AlpacaPort\":1,\"extra\":\"{}\"}}",
            "x".repeat(MAX_REPLY_BYTES)
        );
        assert!(decode(oversized.as_bytes()).is_none());
        for sender in [
            "0.0.0.0:1",
            "0.1.2.3:1",
            "255.255.255.255:1",
            "239.1.2.3:1",
            "[::]:1",
            "[ff02::1]:1",
            "[::ffff:192.0.2.3]:1",
        ] {
            assert!(
                candidate(
                    br#"{"AlpacaPort":1}"#,
                    sender.parse().unwrap(),
                    target("[::1]:9".parse().unwrap())
                )
                .is_none()
            );
        }
    }
    #[test]
    fn ipv6_zones_are_retained_separately_from_the_http_url() {
        let interface = Target {
            bind: "[fe80::2%7]:0".parse().unwrap(),
            destination: "[ff12::a1:9aca%7]:32227".parse().unwrap(),
        };
        let (key, server) = candidate(
            br#"{"AlpacaPort":11111}"#,
            "[fe80::42]:1234".parse().unwrap(),
            interface,
        )
        .unwrap();
        assert_eq!(key, "[fe80::42%7]:11111".parse().unwrap());
        assert_eq!(server.scope_id, 7);
        assert_eq!(server.base_url, "http://[fe80::42]:11111");
        assert!(
            candidate(
                br#"{"AlpacaPort":1}"#,
                "[fe80::42%8]:1234".parse().unwrap(),
                interface
            )
            .is_none()
        );
        let (_, global) = candidate(
            br#"{"AlpacaPort":1}"#,
            "[2001:db8::42%7]:1234".parse().unwrap(),
            interface,
        )
        .unwrap();
        assert_eq!(global.scope_id, 0);
        assert_eq!(global.base_url, "http://[2001:db8::42]:1");
    }
    #[test]
    fn ipc_search_accepts_only_revision_and_never_arbitrary_targets_or_credentials() {
        let command = json!({"op":"searchAlpaca","expectedRevision":Uuid::new_v4()});
        assert!(serde_json::from_value::<crate::ipc::Command>(command.clone()).is_ok());
        for field in [
            "baseUrl",
            "credentialReference",
            "authorization",
            "port",
            "destination",
        ] {
            let mut invalid = command.clone();
            invalid[field] = json!("must-not-be-used");
            assert!(serde_json::from_value::<crate::ipc::Command>(invalid).is_err());
        }
        assert!(
            serde_json::from_value::<crate::ipc::Command>(json!({"op":"searchAlpaca"})).is_err()
        );
    }
    #[tokio::test]
    async fn real_udp_search_uses_sender_ip_advertised_port_and_continues_after_oversize() {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let destination = peer.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut request = [0; 32];
            let (n, client) = peer.recv_from(&mut request).await.unwrap();
            assert_eq!(&request[..n], REQUEST);
            for bytes in [
                vec![b'x'; 8192],
                b"{\"AlpacaPort\":0}".to_vec(),
                b"{\"AlpacaPort\":11111}".to_vec(),
                b"{\"AlpacaPort\":11111}".to_vec(),
                b"{\"AlpacaPort\":22222}".to_vec(),
            ] {
                peer.send_to(&bytes, client).await.unwrap();
            }
        });
        let revision = Uuid::new_v4();
        let result = collect(
            vec![target(destination)],
            revision,
            Instant::now() + Duration::from_millis(500),
        )
        .await;
        responder.await.unwrap();
        assert_eq!(result.configuration_revision, revision);
        assert_eq!(
            result.servers.iter().map(|s| s.port).collect::<Vec<_>>(),
            vec![11111, 22222]
        );
        assert_eq!(result.servers[0].base_url, "http://127.0.0.1:11111");
        assert_eq!(result.ignored_datagrams, 2);
        assert_eq!(result.interfaces_failed, 0);
        assert!(!result.incomplete);
    }
    #[tokio::test]
    async fn real_ipv6_loopback_search_and_cancellation_close_query_sockets() {
        let peer = UdpSocket::bind("[::1]:0").await.unwrap();
        let destination = peer.local_addr().unwrap();
        let task = tokio::spawn(collect(
            vec![target(destination)],
            Uuid::new_v4(),
            Instant::now() + Duration::from_secs(30),
        ));
        let mut request = [0; 32];
        let (_, client) = peer.recv_from(&mut request).await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let rebound = UdpSocket::bind(client).await.unwrap();
        drop(rebound);
        let responder = tokio::spawn(async move {
            let (_, client) = peer.recv_from(&mut request).await.unwrap();
            peer.send_to(br#"{"AlpacaPort":12345}"#, client)
                .await
                .unwrap();
        });
        let result = collect(
            vec![target(destination)],
            Uuid::new_v4(),
            Instant::now() + Duration::from_millis(500),
        )
        .await;
        responder.await.unwrap();
        assert_eq!(result.servers[0].base_url, "http://[::1]:12345");
    }
    #[tokio::test]
    async fn deadline_and_interface_limits_report_partial_results_without_orphan_tasks() {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let destination = peer.local_addr().unwrap();
        let started = Instant::now();
        let result = collect(
            vec![target(destination); MAX_INTERFACES + 1],
            Uuid::new_v4(),
            started + Duration::from_millis(50),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(result.interfaces_tried, MAX_INTERFACES as u32);
        assert!(result.incomplete);
        assert!(result.servers.is_empty());
    }
    #[tokio::test]
    async fn noisy_peer_cannot_extend_deadline_or_exceed_global_packet_budget() {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let destination = peer.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut request = [0; 32];
            let (_, client) = peer.recv_from(&mut request).await.unwrap();
            for _ in 0..MAX_DATAGRAMS * 3 {
                let _ = peer.send_to(b"not a discovery reply", client).await;
                tokio::task::yield_now().await;
            }
        });
        let result = collect(
            vec![target(destination)],
            Uuid::new_v4(),
            Instant::now() + Duration::from_secs(3),
        )
        .await;
        responder.abort();
        let _ = responder.await;
        assert_eq!(result.ignored_datagrams, MAX_DATAGRAMS as u32);
        assert!(result.incomplete);
        assert!(result.servers.is_empty());
    }
    #[tokio::test]
    async fn candidate_limit_preserves_first_results_and_reports_incomplete() {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let destination = peer.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut request = [0; 32];
            let (_, client) = peer.recv_from(&mut request).await.unwrap();
            for port in 1..=MAX_SERVERS + 8 {
                peer.send_to(format!("{{\"AlpacaPort\":{port}}}").as_bytes(), client)
                    .await
                    .unwrap();
                tokio::task::yield_now().await;
            }
        });
        let result = collect(
            vec![target(destination)],
            Uuid::new_v4(),
            Instant::now() + Duration::from_secs(2),
        )
        .await;
        responder.await.unwrap();
        assert_eq!(result.servers.len(), MAX_SERVERS);
        assert!(result.incomplete);
        assert_eq!(result.servers.first().unwrap().port, 1);
        assert_eq!(result.servers.last().unwrap().port, MAX_SERVERS as u16);
    }
}

//! Private loopback wire tests. The advertised link-local identity and scope
//! stay intact; only the injected dial future targets ::1. These tests do not
//! assert that the host has a working LAN interface or link-local route.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const WAIT: Duration = Duration::from_secs(3);
const SHORT: Duration = Duration::from_millis(200);

struct Peer<T> {
    address: std::net::SocketAddr,
    task: Option<JoinHandle<T>>,
}
impl<T> Peer<T> {
    async fn finish(mut self) -> T {
        tokio::time::timeout(WAIT, self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap()
    }
}
impl<T> Drop for Peer<T> {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
async fn peer<F, Fut, T>(handler: F) -> Peer<T>
where
    F: FnOnce(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        handler(socket).await
    });
    Peer {
        address,
        task: Some(task),
    }
}
async fn request_text<S: AsyncRead + Unpin>(socket: &mut S) -> String {
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 16 * 1024);
        socket.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
    }
    let headers = String::from_utf8(bytes.clone()).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(length < 16 * 1024);
    let offset = bytes.len();
    bytes.resize(offset + length, 0);
    socket.read_exact(&mut bytes[offset..]).await.unwrap();
    String::from_utf8(bytes).unwrap()
}
async fn closed<S: AsyncRead + Unpin>(socket: &mut S) {
    let mut byte = [0];
    let result = tokio::time::timeout(WAIT, socket.read(&mut byte))
        .await
        .unwrap();
    assert!(
        matches!(result, Ok(0) | Err(_)),
        "client socket remained open"
    );
}
fn request(method: Method) -> reqwest::Request {
    reqwest::Client::new()
        .request(
            method,
            "http://[fe80::42]:11111/prefix/api/v1/focuser/7/move",
        )
        .query(&[("ClientID", 5), ("ClientTransactionID", 9)])
        .header("Authorization", "Bearer private-test-token")
        .form(&[("Position", "12"), ("name", "a b")])
        .build()
        .unwrap()
}

#[test]
fn scope_is_routing_context_and_part_of_endpoint_identity() {
    let root = Url::parse("http://[fe80::42]:11111/prefix").unwrap();
    let seven = scoped_address(&root, Some(7)).unwrap().unwrap();
    let eight = scoped_address(&root, Some(8)).unwrap().unwrap();
    assert_eq!(seven.to_string(), "[fe80::42%7]:11111");
    assert_ne!(seven, eight);
    assert_eq!(
        scoped_address(&Url::parse("https://[fe80::42]").unwrap(), Some(7))
            .unwrap()
            .unwrap()
            .port(),
        443
    );
    assert!(scoped_address(&root, None).is_err());
    assert!(scoped_address(&root, Some(0)).is_err());
    for url in [
        "http://localhost",
        "http://127.0.0.1",
        "http://[::1]",
        "http://[2001:db8::42]",
    ] {
        let url = Url::parse(url).unwrap();
        assert_eq!(scoped_address(&url, None).unwrap(), None);
        assert!(scoped_address(&url, Some(7)).is_err());
    }
}

#[tokio::test]
async fn scoped_http_preserves_authority_form_headers_and_response_length() {
    let peer = peer(|mut socket| async move {
        let wire = request_text(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody")
            .await
            .unwrap();
        wire
    })
    .await;
    let response = scoped_request(
        request(Method::PUT),
        TcpStream::connect(peer.address),
        None,
        WAIT,
        Some(WAIT),
    )
    .await
    .unwrap();
    assert_eq!(response.content_length(), Some(4));
    assert_eq!(response.url().host_str(), Some("[fe80::42]"));
    assert_eq!(response.bytes().await.unwrap(), "body");
    let wire = peer.finish().await;
    assert!(wire.starts_with(
        "PUT /prefix/api/v1/focuser/7/move?ClientID=5&ClientTransactionID=9 HTTP/1.1\r\n"
    ));
    assert!(
        wire.to_ascii_lowercase()
            .contains("host: [fe80::42]:11111\r\n")
    );
    assert!(wire.contains("Bearer private-test-token"));
    assert!(wire.to_ascii_lowercase().contains("connection: close\r\n"));
    assert!(wire.ends_with("Position=12&name=a+b"));
    assert!(!wire.contains('%'));
}

#[tokio::test]
async fn redirects_are_returned_without_replay() {
    let peer = peer(|mut socket| async move {
        request_text(&mut socket).await;
        socket.write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\nLocation: http://127.0.0.1:1/steal\r\n\r\n").await.unwrap();
        closed(&mut socket).await;
    }).await;
    let response = scoped_request(
        request(Method::PUT),
        TcpStream::connect(peer.address),
        None,
        WAIT,
        Some(WAIT),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 302);
    drop(response);
    peer.finish().await;
}

#[tokio::test]
async fn scoped_client_refuses_authority_changes_before_connecting() {
    let root = Url::parse("http://[fe80::42]:11111").unwrap();
    let client = HttpClient::new(&root, Some(7), HeaderMap::new(), SHORT, true).unwrap();
    for url in [
        "http://[fe80::43]:11111",
        "https://[fe80::42]:11111",
        "http://[fe80::42]:11112",
    ] {
        assert!(
            client
                .send(client.get(Url::parse(url).unwrap()))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn header_timeout_closes_the_socket() {
    let peer = peer(|mut socket| async move {
        request_text(&mut socket).await;
        closed(&mut socket).await;
    })
    .await;
    assert!(
        scoped_request(
            request(Method::GET),
            TcpStream::connect(peer.address),
            None,
            WAIT,
            Some(SHORT)
        )
        .await
        .is_err()
    );
    peer.finish().await;
}

#[tokio::test]
async fn body_timeout_keeps_the_original_absolute_deadline() {
    let peer = peer(|mut socket| async move {
        request_text(&mut socket).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nx")
            .await
            .unwrap();
        closed(&mut socket).await;
    })
    .await;
    let started = Instant::now();
    let response = scoped_request(
        request(Method::GET),
        TcpStream::connect(peer.address),
        None,
        WAIT,
        Some(SHORT),
    )
    .await
    .unwrap();
    assert!(response.bytes().await.is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    peer.finish().await;
}

#[tokio::test]
async fn dropping_an_image_response_closes_its_driver() {
    let peer = peer(|mut socket| async move {
        request_text(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nx")
            .await
            .unwrap();
        closed(&mut socket).await;
    })
    .await;
    let response = scoped_request(
        request(Method::GET),
        TcpStream::connect(peer.address),
        None,
        WAIT,
        None,
    )
    .await
    .unwrap();
    drop(response);
    peer.finish().await;
}

#[tokio::test]
async fn cancelling_before_headers_closes_its_driver() {
    let (sent, received) = tokio::sync::oneshot::channel();
    let peer = peer(|mut socket| async move {
        request_text(&mut socket).await;
        sent.send(()).unwrap();
        closed(&mut socket).await;
    })
    .await;
    let address = peer.address;
    let task = tokio::spawn(async move {
        scoped_request(
            request(Method::GET),
            TcpStream::connect(address),
            None,
            WAIT,
            None,
        )
        .await
    });
    tokio::time::timeout(WAIT, received).await.unwrap().unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    peer.finish().await;
}

#[tokio::test]
async fn oversized_response_headers_fail_without_replay() {
    let peer = peer(|mut socket| async move {
        request_text(&mut socket).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nX-Huge: {}\r\nContent-Length: 0\r\n\r\n",
            "a".repeat(40 * 1024)
        );
        let _ = socket.write_all(response.as_bytes()).await;
        closed(&mut socket).await;
    })
    .await;
    assert!(
        scoped_request(
            request(Method::GET),
            TcpStream::connect(peer.address),
            None,
            WAIT,
            Some(WAIT)
        )
        .await
        .is_err()
    );
    peer.finish().await;
}

#[tokio::test]
async fn excessive_header_count_and_reason_phrase_are_rejected() {
    for response in [
        format!(
            "HTTP/1.1 200 OK\r\n{}Content-Length: 0\r\n\r\n",
            "X-Extra: a\r\n".repeat(101)
        ),
        format!(
            "HTTP/1.1 200 {}\r\nContent-Length: 0\r\n\r\n",
            "a".repeat(40 * 1024)
        ),
    ] {
        let peer = peer(|mut socket| async move {
            request_text(&mut socket).await;
            let _ = socket.write_all(response.as_bytes()).await;
            closed(&mut socket).await;
        })
        .await;
        assert!(
            scoped_request(
                request(Method::GET),
                TcpStream::connect(peer.address),
                None,
                WAIT,
                Some(WAIT)
            )
            .await
            .is_err()
        );
        peer.finish().await;
    }
}

#[tokio::test]
async fn imagebytes_reuses_the_ordinary_codec_and_retained_image_budget() {
    use crate::camera::image::{ImageBudget, read_imagebytes};
    use futures_util::TryStreamExt;
    let mut encoded = Vec::new();
    for field in [1u32, 0, 7, 9, 44, 8, 8, 2, 2, 2, 0] {
        encoded.extend_from_slice(&field.to_le_bytes());
    }
    let pixels = [0u16, 1, 4096, 65535]
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    encoded.extend_from_slice(&pixels);
    let peer = peer(|mut socket| async move {
        request_text(&mut socket).await;
        let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: application/imagebytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", encoded.len());
        socket.write_all(headers.as_bytes()).await.unwrap(); socket.write_all(&encoded).await.unwrap();
    }).await;
    let response = scoped_request(
        request(Method::GET),
        TcpStream::connect(peer.address),
        None,
        WAIT,
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.content_length(), Some(52));
    let stream = response
        .bytes_stream()
        .map_err(|_| io::Error::other("fixture body failed"));
    let mut reader = tokio_util::io::StreamReader::new(stream);
    let budget = ImageBudget::new(8).unwrap();
    let image = read_imagebytes(&mut reader, &budget, 7).await.unwrap();
    assert_eq!(image.image.bytes(), pixels);
    assert_eq!(budget.used_bytes(), 8);
    drop(image);
    assert_eq!(budget.used_bytes(), 0);
    peer.finish().await;
}

fn certificate(ip: &str) -> (Arc<rustls::ServerConfig>, Arc<rustls::ClientConfig>) {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec![ip.to_owned()]).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let server = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
    )
    .unwrap();
    (Arc::new(server), Arc::new(client))
}

#[tokio::test]
async fn tls_checks_the_advertised_ip_san_and_preserves_host() {
    let (server, client) = certificate("fe80::42");
    let peer = peer(|socket| async move {
        let mut socket = TlsAcceptor::from(server).accept(socket).await.unwrap();
        let wire = request_text(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .unwrap();
        wire
    })
    .await;
    let mut request = request(Method::GET);
    request.url_mut().set_scheme("https").unwrap();
    let response = scoped_request(
        request,
        TcpStream::connect(peer.address),
        Some(client),
        WAIT,
        Some(WAIT),
    )
    .await
    .unwrap();
    assert_eq!(response.bytes().await.unwrap(), "ok");
    assert!(
        peer.finish()
            .await
            .to_ascii_lowercase()
            .contains("host: [fe80::42]:11111\r\n")
    );
}

#[tokio::test]
async fn wrong_ip_and_untrusted_certificates_fail_before_http() {
    for wrong_identity in [false, true] {
        let (server, trusted) = certificate(if wrong_identity {
            "fe80::43"
        } else {
            "fe80::42"
        });
        let client = if wrong_identity {
            trusted
        } else {
            tls_config().unwrap()
        };
        let peer = peer(|socket| async move {
            assert!(TlsAcceptor::from(server).accept(socket).await.is_err());
        })
        .await;
        let mut request = request(Method::GET);
        request.url_mut().set_scheme("https").unwrap();
        assert!(
            scoped_request(
                request,
                TcpStream::connect(peer.address),
                Some(client),
                WAIT,
                Some(WAIT)
            )
            .await
            .is_err()
        );
        peer.finish().await;
    }
}

#[tokio::test]
async fn tls_handshake_is_bounded_by_the_connect_budget() {
    let peer = peer(|mut socket| async move {
        let mut hello = [0; 4096];
        assert!(socket.read(&mut hello).await.unwrap() > 0);
        closed(&mut socket).await;
    })
    .await;
    let mut request = request(Method::GET);
    request.url_mut().set_scheme("https").unwrap();
    assert!(
        scoped_request(
            request,
            TcpStream::connect(peer.address),
            Some(tls_config().unwrap()),
            SHORT,
            None
        )
        .await
        .is_err()
    );
    peer.finish().await;
}

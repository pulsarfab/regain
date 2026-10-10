//! HTTP transport shared by catalog, scalar and image requests. Ordinary URLs
//! use reqwest; literal IPv6 link-local URLs carry a separate socket scope.
//! Scoped requests retain normal HTTP authority/TLS identity and response codecs,
//! without DNS aliases, proxies, redirected credentials or command replay.
use super::invalid;
use crate::source::SourceError;
use bytes::Bytes;
use hyper::{
    body::{Body, Frame, Incoming, SizeHint},
    client::conn::http1,
};
use hyper_util::rt::TokioIo;
use reqwest::{
    Method, RequestBuilder, Response, ResponseBuilderExt,
    header::{CONNECTION, HOST, HeaderMap},
};
use std::{
    future::Future,
    io,
    net::SocketAddrV6,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    task::JoinHandle,
    time::{Instant, Sleep, sleep_until, timeout_at},
};
use tokio_rustls::{
    TlsConnector,
    rustls::{self, pki_types::ServerName},
};
use url::{Host, Position, Url};

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;

pub(crate) fn scoped_address(
    root: &Url,
    scope: Option<u32>,
) -> Result<Option<SocketAddrV6>, &'static str> {
    match (root.host(), scope) {
        (Some(Host::Ipv6(address)), Some(scope))
            if address.is_unicast_link_local() && scope != 0 =>
        {
            Ok(Some(SocketAddrV6::new(
                address,
                root.port_or_known_default().ok_or("Invalid HTTP port")?,
                0,
                scope,
            )))
        }
        (Some(Host::Ipv6(address)), _) if address.is_unicast_link_local() => {
            Err("A link-local IPv6 server requires a positive interface scope ID")
        }
        (_, Some(_)) => {
            Err("An interface scope ID is only valid for a literal link-local IPv6 server")
        }
        (_, None) => Ok(None),
    }
}
pub(super) struct HttpClient {
    client: reqwest::Client,
    headers: HeaderMap,
    route: Option<Route>,
    connect_timeout: Duration,
    scalar_timeout: Option<Duration>,
}
struct Route {
    address: SocketAddrV6,
    scheme: String,
    tls: Option<Arc<rustls::ClientConfig>>,
}
fn tls_config() -> Result<Arc<rustls::ClientConfig>, SourceError> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| invalid("Could not initialize Alpaca TLS"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(config))
}
impl HttpClient {
    pub(super) fn new(
        root: &Url,
        scope: Option<u32>,
        headers: HeaderMap,
        deadline: Duration,
        scalar: bool,
    ) -> Result<Self, SourceError> {
        let address = scoped_address(root, scope).map_err(invalid)?;
        let route = address
            .map(|address| {
                Ok(Route {
                    address,
                    scheme: root.scheme().to_owned(),
                    tls: if root.scheme() == "https" {
                        Some(tls_config()?)
                    } else {
                        None
                    },
                })
            })
            .transpose()?;
        let builder = reqwest::Client::builder()
            .default_headers(headers.clone())
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(deadline)
            .pool_max_idle_per_host(1);
        let builder = if scalar {
            builder.timeout(deadline)
        } else {
            builder
        };
        let client = builder
            .build()
            .map_err(|_| invalid("Could not initialize Alpaca HTTP client"))?;
        Ok(Self {
            client,
            headers,
            route,
            connect_timeout: deadline,
            scalar_timeout: scalar.then_some(deadline),
        })
    }
    pub(super) fn request(&self, method: Method, url: Url) -> RequestBuilder {
        self.client
            .request(method, url)
            .headers(self.headers.clone())
    }
    pub(super) fn get(&self, url: Url) -> RequestBuilder {
        self.request(Method::GET, url)
    }
    pub(super) async fn send(&self, builder: RequestBuilder) -> Result<Response, ()> {
        let Some(route) = &self.route else {
            return builder.send().await.map_err(|_| ());
        };
        let request = builder.build().map_err(|_| ())?;
        // A pinned catalog and its device requests share this exact route. URL
        // path/query changes are allowed; host, port and scheme changes are not.
        if request.url().host() != Some(Host::Ipv6(*route.address.ip()))
            || request.url().port_or_known_default() != Some(route.address.port())
            || request.url().scheme() != route.scheme
        {
            return Err(());
        }
        scoped_request(
            request,
            TcpStream::connect(route.address),
            route.tls.clone(),
            self.connect_timeout,
            self.scalar_timeout,
        )
        .await
    }
}
trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
struct Driver(JoinHandle<()>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct ResponseBody {
    incoming: Incoming,
    driver: Driver,
    deadline: Option<Pin<Box<Sleep>>>,
    failed: bool,
}
impl Body for ResponseBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        if this.failed {
            return Poll::Ready(None);
        }
        if this
            .deadline
            .as_mut()
            .is_some_and(|timer| timer.as_mut().poll(cx).is_ready())
        {
            this.failed = true;
            this.driver.0.abort();
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Alpaca request timed out",
            ))));
        }
        match Pin::new(&mut this.incoming).poll_frame(cx) {
            Poll::Ready(Some(Err(_))) => {
                this.failed = true;
                this.driver.0.abort();
                Poll::Ready(Some(Err(io::Error::other("Alpaca HTTP body failed"))))
            }
            Poll::Ready(None) => {
                this.driver.0.abort();
                Poll::Ready(None)
            }
            other => other.map(|frame| {
                frame.map(|frame| frame.map_err(|_| io::Error::other("Alpaca HTTP body failed")))
            }),
        }
    }
    fn size_hint(&self) -> SizeHint {
        self.incoming.size_hint()
    }
    fn is_end_stream(&self) -> bool {
        self.failed || self.incoming.is_end_stream()
    }
}
async fn scoped_request(
    mut request: reqwest::Request,
    dial: impl Future<Output = io::Result<TcpStream>>,
    tls: Option<Arc<rustls::ClientConfig>>,
    connect_timeout: Duration,
    scalar_timeout: Option<Duration>,
) -> Result<Response, ()> {
    let started = Instant::now();
    let deadline = scalar_timeout.map(|duration| started + duration);
    let connect_deadline = deadline.map_or(started + connect_timeout, |total| {
        total.min(started + connect_timeout)
    });
    let url = request.url().clone();
    let connected = async {
        let tcp = dial.await.map_err(|_| ())?;
        tcp.set_nodelay(true).map_err(|_| ())?;
        let stream: Box<dyn Stream> = if let Some(config) = tls {
            let Some(Host::Ipv6(address)) = url.host() else {
                return Err(());
            };
            // The zone is local routing context, not part of the TLS server
            // identity or HTTP Host header. Keep ordinary IP SAN verification.
            let name = ServerName::IpAddress(address.into());
            Box::new(
                TlsConnector::from(config)
                    .connect(name, tcp)
                    .await
                    .map_err(|_| ())?,
            )
        } else {
            Box::new(tcp)
        };
        Ok::<_, ()>(stream)
    };
    let stream = timeout_at(connect_deadline, connected)
        .await
        .map_err(|_| ())??;
    let exchange = async {
        let (mut sender, connection) = http1::Builder::new()
            .max_buf_size(32 * 1024)
            .max_headers(100)
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|_| ())?;
        let driver = Driver(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let bytes = match request.body_mut().take() {
            Some(body) => Bytes::copy_from_slice(body.as_bytes().ok_or(())?),
            None => Bytes::new(),
        };
        let mut outgoing = hyper::Request::builder()
            .method(request.method().clone())
            .uri(&url[Position::BeforePath..]);
        *outgoing.headers_mut().ok_or(())? = request.headers().clone();
        outgoing.headers_mut().ok_or(())?.insert(
            HOST,
            url[Position::BeforeHost..Position::AfterPort]
                .parse()
                .map_err(|_| ())?,
        );
        outgoing
            .headers_mut()
            .ok_or(())?
            .insert(CONNECTION, "close".parse().map_err(|_| ())?);
        let outgoing = outgoing
            .body(http_body_util::Full::new(bytes))
            .map_err(|_| ())?;
        let response = sender.send_request(outgoing).await.map_err(|_| ())?;
        // Hyper's buffer limit bounds an incomplete parse, but a complete
        // header block can arrive above it in one read. Bound accepted headers
        // explicitly as well; keep status-line/framing overhead in the budget.
        let reason_bytes = response
            .extensions()
            .get::<hyper::ext::ReasonPhrase>()
            .map_or(0, |reason| reason.as_bytes().len());
        let header_bytes = response
            .headers()
            .iter()
            .try_fold(128usize + reason_bytes, |total, (name, value)| {
                total
                    .checked_add(name.as_str().len())?
                    .checked_add(value.as_bytes().len())?
                    .checked_add(4)
            })
            .ok_or(())?;
        if header_bytes > 32 * 1024 {
            return Err(());
        }
        let (parts, incoming) = response.into_parts();
        let body = reqwest::Body::wrap(ResponseBody {
            incoming,
            driver,
            deadline: deadline.map(|time| Box::pin(sleep_until(time))),
            failed: false,
        });
        let mut response = hyper::Response::builder()
            .status(parts.status)
            .version(parts.version)
            .url(url.clone());
        *response.headers_mut().ok_or(())? = parts.headers;
        Ok::<_, ()>(Response::from(response.body(body).map_err(|_| ())?))
    };
    match deadline {
        Some(deadline) => timeout_at(deadline, exchange).await.map_err(|_| ())?,
        None => exchange.await,
    }
}

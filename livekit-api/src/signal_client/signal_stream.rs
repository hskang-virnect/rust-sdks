// Copyright 2023 LiveKit, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use livekit_protocol as proto;
use livekit_runtime::{JoinHandle, TcpStream};
use prost::Message as ProtoMessage;
use std::{env, io};

use tokio::sync::{mpsc, oneshot};

#[cfg(feature = "signal-client-tokio")]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

#[cfg(feature = "signal-client-tokio")]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream as TokioTcpStream,
};

#[cfg(feature = "signal-client-tokio")]
use tokio_tungstenite::{
    tungstenite::error::ProtocolError,
    tungstenite::{Error as WsError, Message},
    MaybeTlsStream, WebSocketStream,
};

#[cfg(feature = "__signal-client-async-compatible")]
use async_tungstenite::{
    async_std::connect_async,
    async_std::ClientStream as MaybeTlsStream,
    tungstenite::error::ProtocolError,
    tungstenite::{Error as WsError, Message},
    WebSocketStream,
};

use super::{SignalError, SignalResult};

type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Options for SignalStream connection
#[derive(Debug, Clone)]
pub struct SignalStreamOptions {
    /// Skip TLS certificate verification
    /// Useful for corporate networks with self-signed certificates
    /// WARNING: This should only be used in trusted networks
    pub skip_cert_verify: bool,
}

impl Default for SignalStreamOptions {
    fn default() -> Self {
        Self {
            skip_cert_verify: true,
        }
    }
}

impl SignalStreamOptions {
    /// Create new options with certificate verification enabled
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether to skip certificate verification
    pub fn with_skip_cert_verify(mut self, skip: bool) -> Self {
        self.skip_cert_verify = skip;
        self
    }
}

#[derive(Debug)]
enum InternalMessage {
    Signal {
        signal: proto::signal_request::Message,
        response_chn: oneshot::Sender<SignalResult<()>>,
    },
    Pong {
        ping_data: Vec<u8>,
    },
    Close,
}

/// SignalStream hold the WebSocket connection
///
/// It is replaced by [SignalClient] at each reconnection.
#[derive(Debug)]
pub(super) struct SignalStream {
    internal_tx: mpsc::Sender<InternalMessage>,
    read_handle: JoinHandle<()>,
    write_handle: JoinHandle<()>,
}

impl SignalStream {
    /// Connect to livekit websocket with default options.
    /// Return SignalError if the connections failed
    ///
    /// SignalStream will never try to reconnect if the connection has been
    /// closed.
    pub async fn connect(
        url: url::Url,
    ) -> SignalResult<(Self, mpsc::UnboundedReceiver<Box<proto::signal_response::Message>>)> {
        Self::connect_with_options(url, SignalStreamOptions::default()).await
    }

    /// Connect to livekit websocket with custom options.
    /// Return SignalError if the connections failed
    ///
    /// SignalStream will never try to reconnect if the connection has been
    /// closed.
    pub async fn connect_with_options(
        url: url::Url,
        options: SignalStreamOptions,
    ) -> SignalResult<(Self, mpsc::UnboundedReceiver<Box<proto::signal_response::Message>>)> {
        {
            // Don't log sensitive info
            let mut url = url.clone();
            let filtered_pairs: Vec<_> = url
                .query_pairs()
                .filter(|(key, _)| key != "access_token")
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();

            {
                let mut query_pairs = url.query_pairs_mut();
                query_pairs.clear();
                for (key, value) in filtered_pairs {
                    query_pairs.append_pair(&key, &value);
                }

                query_pairs.append_pair("access_token", "...");
            }

            log::info!("connecting to {}", url);
        }

        #[cfg(feature = "signal-client-tokio")]
        let ws_stream = {
            // Check if certificate verification should be disabled from options or env var
            let skip_cert_verify = options.skip_cert_verify || 
                env::var("LIVEKIT_SKIP_CERT_VERIFY")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);

            if skip_cert_verify {
                log::warn!("Certificate verification is disabled. This should only be used in trusted networks (corporate networks with self-signed certificates).");
            }

            // Check for HTTP_PROXY or HTTPS_PROXY environment variables
            let proxy_env = if url.scheme() == "wss" {
                env::var("HTTPS_PROXY").or_else(|_| env::var("https_proxy"))
            } else {
                env::var("HTTP_PROXY").or_else(|_| env::var("http_proxy"))
            };

            // Connect directly or through proxy
            let ws_stream = if let Ok(proxy_url) = proxy_env {
                if !proxy_url.is_empty() {
                    log::info!("Using proxy: {}", proxy_url);
                    let proxy_url = url::Url::parse(&proxy_url).map_err(|e| {
                        WsError::Io(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("Invalid proxy URL: {}", e),
                        ))
                    })?;

                    let host = url.host_str().ok_or_else(|| {
                        WsError::Io(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "Target URL has no host",
                        ))
                    })?;

                    let port = url.port_or_known_default().ok_or_else(|| {
                        WsError::Io(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "Target URL has no port and no default for scheme",
                        ))
                    })?;

                    let proxy_host = proxy_url.host_str().ok_or_else(|| {
                        WsError::Io(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "Proxy URL has no host",
                        ))
                    })?;

                    let proxy_port = proxy_url.port_or_known_default().unwrap_or(80);
                    let proxy_addr = format!("{}:{}", proxy_host, proxy_port);

                    // Use tokio's lookup_host for proxy connection (fixes iOS/macOS DNS issues)
                    let proxy_socket_addrs: Vec<_> = tokio::net::lookup_host(&proxy_addr)
                        .await
                        .map_err(WsError::Io)?
                        .collect();
                    
                    if proxy_socket_addrs.is_empty() {
                        return Err(WsError::Io(io::Error::new(
                            io::ErrorKind::Other,
                            format!("Failed to resolve proxy host: {}", proxy_host),
                        )));
                    }
                    
                    log::debug!("Resolved proxy {} to {:?}", proxy_addr, proxy_socket_addrs);
                    let mut proxy_stream =
                        TokioTcpStream::connect(&proxy_socket_addrs[..]).await.map_err(WsError::Io)?;

                    let mut proxy_auth_header = None;
                    if let Some(password) = proxy_url.password() {
                        let auth = format!("{}:{}", proxy_url.username(), password);
                        let auth = format!("Basic {}", BASE64.encode(auth));
                        proxy_auth_header = Some(auth);
                    }

                    // Send CONNECT request
                    let target = format!("{}:{}", host, port);
                    let mut connect_req =
                        format!("CONNECT {} HTTP/1.1\r\nHost: {}\r\n", target, target);

                    // Add proxy authorization if needed
                    if let Some(auth) = proxy_auth_header {
                        connect_req.push_str(&format!("Proxy-Authorization: {}\r\n", auth));
                    }

                    // Finalize request
                    connect_req.push_str("\r\n");

                    log::debug!("Sending CONNECT request to proxy");
                    proxy_stream.write_all(connect_req.as_bytes()).await.map_err(WsError::Io)?;

                    // Read and parse response
                    let mut response = Vec::new();
                    let mut buf = [0u8; 4096];
                    let mut headers_complete = false;

                    while !headers_complete {
                        let n = proxy_stream.read(&mut buf).await.map_err(WsError::Io)?;
                        if n == 0 {
                            return Err(WsError::Io(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "Proxy connection closed while reading response",
                            ))
                            .into());
                        }

                        response.extend_from_slice(&buf[..n]);

                        // Check if we've received the end of headers (double CRLF)
                        if response.windows(4).any(|w| w == b"\r\n\r\n") {
                            headers_complete = true;
                        }
                    }

                    // Parse status line
                    let response_str = String::from_utf8_lossy(&response);
                    let status_line = response_str.lines().next().ok_or_else(|| {
                        WsError::Io(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid proxy response",
                        ))
                    })?;

                    // Check status code
                    if !status_line.contains("200") {
                        return Err(WsError::Io(io::Error::new(
                            io::ErrorKind::ConnectionRefused,
                            format!("Proxy connection failed: {}", status_line),
                        ))
                        .into());
                    }

                    log::debug!("Proxy connection established to {}", target);

                    // Create MaybeTlsStream based on original URL scheme
                    let stream = if url.scheme() == "wss" {
                        #[cfg(feature = "rustls-tls-native-roots")]
                        {
                            Self::create_rustls_stream(proxy_stream, host, skip_cert_verify).await?
                        }

                        #[cfg(not(feature = "rustls-tls-native-roots"))]
                        {
                            return Err(WsError::Io(io::Error::new(
                                io::ErrorKind::Other,
                                "WSS over proxy requires rustls-tls-native-roots feature",
                            ))
                            .into());
                        }
                    } else {
                        // For plain WS, just use the proxy stream directly
                        MaybeTlsStream::Plain(proxy_stream)
                    };

                    // Now perform WebSocket handshake over the established connection
                    let (ws_stream, response) =
                        tokio_tungstenite::client_async_with_config(url, stream, None).await?;
                    log::info!(
                        "websocket handshake response (proxy): status={}, headers={:?}",
                        response.status(),
                        response.headers()
                    );
                    ws_stream
                } else {
                    // Empty proxy URL, connect directly
                    Self::connect_direct(url, skip_cert_verify).await?
                }
            } else {
                // No proxy - connect directly
                Self::connect_direct(url, skip_cert_verify).await?
            };

            ws_stream
        };

        #[cfg(not(feature = "signal-client-tokio"))]
        let (ws_stream, response) = connect_async(url).await?;
        #[cfg(not(feature = "signal-client-tokio"))]
        log::info!(
            "websocket handshake response (async): status={}, headers={:?}",
            response.status(),
            response.headers()
        );
        let (ws_writer, ws_reader) = ws_stream.split();

        let (emitter, events) = mpsc::unbounded_channel();
        let (internal_tx, internal_rx) = mpsc::channel::<InternalMessage>(8);
        let write_handle = livekit_runtime::spawn(Self::write_task(internal_rx, ws_writer));
        let read_handle =
            livekit_runtime::spawn(Self::read_task(internal_tx.clone(), ws_reader, emitter));

        Ok((Self { internal_tx, read_handle, write_handle }, events))
    }

    /// Close the websocket
    /// It sends a CloseFrame to the server before closing
    pub async fn close(self, notify_close: bool) {
        if notify_close {
            let _ = self.internal_tx.send(InternalMessage::Close).await;
        }
        let _ = self.write_handle.await;
        let _ = self.read_handle.await;
    }

    /// Send a SignalRequest to the websocket
    /// It also waits for the message to be sent
    pub async fn send(&self, signal: proto::signal_request::Message) -> SignalResult<()> {
        let (send, recv) = oneshot::channel();
        let msg = InternalMessage::Signal { signal, response_chn: send };
        let _ = self.internal_tx.send(msg).await;
        recv.await.map_err(|_| SignalError::SendError)?
    }

    /// This task is used to send messages to the websocket
    /// It is also responsible for closing the connection
    async fn write_task(
        mut internal_rx: mpsc::Receiver<InternalMessage>,
        mut ws_writer: SplitSink<WebSocket, Message>,
    ) {
        while let Some(msg) = internal_rx.recv().await {
            match msg {
                InternalMessage::Signal { signal, response_chn } => {
                    let data = proto::SignalRequest { message: Some(signal) }.encode_to_vec();

                    if let Err(err) = ws_writer.send(Message::Binary(data)).await {
                        let _ = response_chn.send(Err(err.into()));
                        break;
                    }

                    let _ = response_chn.send(Ok(()));
                }
                InternalMessage::Pong { ping_data } => {
                    if let Err(err) = ws_writer.send(Message::Pong(ping_data)).await {
                        log::error!("failed to send pong message: {:?}", err);
                    }
                }
                InternalMessage::Close => break,
            }
        }

        let _ = ws_writer.close().await;
    }

    /// This task is used to read incoming messages from the websocket
    /// and dispatch them through the EventEmitter.
    ///
    /// It can also send messages to [handle_write] task ( Used e.g. answer to pings )
    async fn read_task(
        internal_tx: mpsc::Sender<InternalMessage>,
        mut ws_reader: SplitStream<WebSocket>,
        emitter: mpsc::UnboundedSender<Box<proto::signal_response::Message>>,
    ) {
        while let Some(msg) = ws_reader.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    let res = proto::SignalResponse::decode(data.as_slice())
                        .expect("failed to decode SignalResponse");

                    if let Some(msg) = res.message {
                        let _ = emitter.send(Box::new(msg));
                    }
                }
                Ok(Message::Ping(data)) => {
                    let _ = internal_tx.send(InternalMessage::Pong { ping_data: data }).await;
                    continue;
                }
                Ok(Message::Close(close)) => {
                    log::debug!("server closed the connection: {:?}", close);
                    break;
                }
                Ok(Message::Frame(_)) => {}
                Err(WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake)) => {
                    break; // Ignore
                }
                _ => {
                    log::error!("unhandled websocket message {:?}", msg);
                    break;
                }
            }
        }

        let _ = internal_tx.send(InternalMessage::Close).await;
    }

    #[cfg(feature = "signal-client-tokio")]
    async fn connect_direct(url: url::Url, skip_cert_verify: bool) -> Result<WebSocket, WsError> {
        // For non-TLS (ws://) connections, manually resolve and connect
        // to avoid iOS/macOS DNS lookup issues
        if url.scheme() == "ws" {
            let host = url.host_str().ok_or_else(|| {
                WsError::Io(io::Error::new(io::ErrorKind::InvalidInput, "No host in URL"))
            })?;
            
            let port = url.port_or_known_default().ok_or_else(|| {
                WsError::Io(io::Error::new(io::ErrorKind::InvalidInput, "No port in URL"))
            })?;
            
            let addr = format!("{}:{}", host, port);
            let socket_addrs: Vec<_> = tokio::net::lookup_host(&addr)
                .await
                .map_err(WsError::Io)?
                .collect();
            
            if socket_addrs.is_empty() {
                return Err(WsError::Io(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to resolve host: {}", host),
                )));
            }
            
            log::debug!("Resolved {} to {:?}", addr, socket_addrs);
            let tcp_stream = TokioTcpStream::connect(&socket_addrs[..]).await.map_err(WsError::Io)?;
            let stream = MaybeTlsStream::Plain(tcp_stream);
            let (ws_stream, response) = tokio_tungstenite::client_async(url.as_str(), stream).await?;
            log::info!(
                "websocket handshake response (ws): status={}, headers={:?}",
                response.status(),
                response.headers()
            );
            return Ok(ws_stream);
        }
        
        // For TLS (wss://) connections
        #[cfg(feature = "rustls-tls-native-roots")]
        {
            use std::sync::Arc;
            use tokio_rustls::rustls;
            use tokio_rustls::TlsConnector;
            
            let tls_config = if skip_cert_verify {
                rustls::ClientConfig::builder()
                    .with_safe_defaults()
                    .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
                    .with_no_client_auth()
            } else {
                // Load native root certificates for proper verification
                let mut root_store = rustls::RootCertStore::empty();
                match rustls_native_certs::load_native_certs() {
                    Ok(certs) => {
                        let roots: Vec<rustls::Certificate> = certs
                            .into_iter()
                            .map(|cert| rustls::Certificate(cert.0))
                            .collect();

                        for root in roots {
                            root_store.add(&root).map_err(|e| {
                                WsError::Io(io::Error::new(
                                    io::ErrorKind::Other,
                                    format!("Failed to parse root certificate: {:?}", e),
                                ))
                            })?;
                        }
                    }
                    Err(e) => {
                        log::warn!("Could not load native root certificates: {}", e);
                    }
                }

                rustls::ClientConfig::builder()
                    .with_safe_defaults()
                    .with_root_certificates(root_store)
                    .with_no_client_auth()
            };
            
            let host = url.host_str().ok_or_else(|| {
                WsError::Io(io::Error::new(io::ErrorKind::InvalidInput, "No host in URL"))
            })?;
            
            let port = url.port_or_known_default().ok_or_else(|| {
                WsError::Io(io::Error::new(io::ErrorKind::InvalidInput, "No port in URL"))
            })?;
            
            // Use tokio's lookup_host to explicitly resolve DNS before connecting
            // This fixes iOS/macOS DNS resolution issues where direct string-based
            // connect() fails with "nodename nor servname provided, or not known"
            let addr = format!("{}:{}", host, port);
            let socket_addrs: Vec<_> = tokio::net::lookup_host(&addr)
                .await
                .map_err(WsError::Io)?
                .collect();
            
            if socket_addrs.is_empty() {
                return Err(WsError::Io(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to resolve host: {}", host),
                )));
            }
            
            log::debug!("Resolved {} to {:?}", addr, socket_addrs);
            let tcp_stream = TokioTcpStream::connect(&socket_addrs[..]).await.map_err(WsError::Io)?;
            
            let connector = TlsConnector::from(Arc::new(tls_config));
            let server_name = rustls::ServerName::try_from(host).map_err(|_| {
                WsError::Io(io::Error::new(io::ErrorKind::InvalidInput, "Invalid DNS name"))
            })?;
            
            let tls_stream = connector.connect(server_name, tcp_stream).await.map_err(|e| {
                WsError::Io(io::Error::new(io::ErrorKind::Other, format!("TLS error: {}", e)))
            })?;
            
            // Wrap in MaybeTlsStream
            let stream = MaybeTlsStream::Rustls(tls_stream);
            let (ws_stream, response) =
                tokio_tungstenite::client_async(url.as_str(), stream).await?;
            log::info!(
                "websocket handshake response (wss): status={}, headers={:?}",
                response.status(),
                response.headers()
            );
            Ok(ws_stream)
        }
        
        #[cfg(not(feature = "rustls-tls-native-roots"))]
        {
            if skip_cert_verify {
                log::warn!("Certificate verification skip is only supported with rustls-tls-native-roots feature");
            }
            let (ws_stream, response) = tokio_tungstenite::connect_async(url).await?;
            log::info!(
                "websocket handshake response (wss-fallback): status={}, headers={:?}",
                response.status(),
                response.headers()
            );
            Ok(ws_stream)
        }
    }

    /// This function is used to create a Rustls stream over an existing TcpStream
    #[cfg(all(feature = "signal-client-tokio", feature = "rustls-tls-native-roots"))]
    async fn create_rustls_stream(
        stream: TokioTcpStream,
        host: &str,
        skip_cert_verify: bool,
    ) -> Result<MaybeTlsStream<TokioTcpStream>, WsError> {
        use std::sync::Arc;
        use tokio_rustls::{rustls, TlsConnector};

        let tls_config = if skip_cert_verify {
            rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
                .with_no_client_auth()
        } else {
            // Load native root certificates for proper verification
            let mut root_store = rustls::RootCertStore::empty();
            match rustls_native_certs::load_native_certs() {
                Ok(certs) => {
                    let roots: Vec<rustls::Certificate> = certs
                        .into_iter()
                        .map(|cert| rustls::Certificate(cert.0))
                        .collect();

                    for root in roots {
                        root_store.add(&root).map_err(|e| {
                            WsError::Io(io::Error::new(
                                io::ErrorKind::Other,
                                format!("Failed to parse root certificate: {:?}", e),
                            ))
                        })?;
                    }
                }
                Err(e) => {
                    log::warn!("Could not load native root certificates: {}", e);
                }
            }

            rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_root_certificates(root_store)
                .with_no_client_auth()
        };

        let server_name = rustls::ServerName::try_from(host).map_err(|_| {
            WsError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid DNS name: {}", host),
            ))
        })?;

        let connector = TlsConnector::from(Arc::new(tls_config));
        let tls_stream = connector.connect(server_name, stream).await.map_err(|e| {
            WsError::Io(io::Error::new(io::ErrorKind::Other, format!("TLS connection error: {}", e)))
        })?;

        Ok(MaybeTlsStream::Rustls(tls_stream))
    }
}

#[cfg(all(feature = "signal-client-tokio", feature = "rustls-tls-native-roots"))]
struct NoCertificateVerification;

#[cfg(all(feature = "signal-client-tokio", feature = "rustls-tls-native-roots"))]
impl tokio_rustls::rustls::client::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &tokio_rustls::rustls::Certificate,
        _intermediates: &[tokio_rustls::rustls::Certificate],
        _server_name: &tokio_rustls::rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<tokio_rustls::rustls::client::ServerCertVerified, tokio_rustls::rustls::Error> {
        // Skip all certificate verification - accept any certificate
        // WARNING: This is insecure and should only be used in trusted networks
        Ok(tokio_rustls::rustls::client::ServerCertVerified::assertion())
    }
}
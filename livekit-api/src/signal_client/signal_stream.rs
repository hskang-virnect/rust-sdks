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
    SinkExt, StreamExt, Sink, Stream,
};
use livekit_protocol as proto;
use livekit_runtime::{JoinHandle, TcpStream};
use prost::Message as ProtoMessage;
use std::{env, io};
use std::fmt::Debug;

use tokio::sync::{mpsc, oneshot};

#[cfg(feature = "signal-client-tokio")]
use base64;

#[cfg(feature = "signal-client-tokio")]
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream as TokioTcpStream,
};

#[cfg(feature = "signal-client-tokio")]
use tokio_tungstenite::{
    connect_async,
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

#[cfg(feature = "signal-client-tokio")]
use tokio_rustls::{rustls, TlsConnector};
#[cfg(feature = "signal-client-tokio")]
use rustls_pemfile;
#[cfg(feature = "signal-client-tokio")]
use rustls_native_certs;

use super::{SignalError, SignalResult};

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
    /// Connect to livekit websocket.
    /// Return SignalError if the connections failed
    ///
    /// SignalStream will never try to reconnect if the connection has been
    /// closed.
    pub async fn connect(
        url: url::Url,
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
        {
            if url.scheme() == "wss" {
                let certs = &[
                    crate::signal_client::signal_stream::FIRST_CERT_PEM,
                    crate::signal_client::signal_stream::SECOND_CERT_PEM,
                    crate::signal_client::signal_stream::INTERMEDIATE_CERT_PEM,
                ];
                return Self::connect_with_certs(url, certs).await;
            } else {
                let (ws_stream, _) = connect_async(url).await?;
                let (ws_writer, ws_reader) = ws_stream.split();
                let (emitter, events) = mpsc::unbounded_channel();
                let (internal_tx, internal_rx) = mpsc::channel::<InternalMessage>(8);
                let write_handle = livekit_runtime::spawn(Self::write_task(internal_rx, ws_writer));
                let read_handle = livekit_runtime::spawn(Self::read_task(internal_tx.clone(), ws_reader, emitter));
                return Ok((Self { internal_tx, read_handle, write_handle }, events));
            }
        }

        #[cfg(not(feature = "signal-client-tokio"))]
        {
            let (ws_stream, _) = connect_async(url).await?;
            let (ws_writer, ws_reader) = ws_stream.split();
            let (emitter, events) = mpsc::unbounded_channel();
            let (internal_tx, internal_rx) = mpsc::channel::<InternalMessage>(8);
            let write_handle = livekit_runtime::spawn(Self::write_task(internal_rx, ws_writer));
            let read_handle = livekit_runtime::spawn(Self::read_task(internal_tx.clone(), ws_reader, emitter));
            Ok((Self { internal_tx, read_handle, write_handle }, events))
        }
    }
    
    /// Connect to livekit websocket with multiple custom root certificates (PEM strings).
    /// Only works for wss scheme.
    #[cfg(feature = "signal-client-tokio")]
    pub async fn connect_with_certs(
        url: url::Url,
        cert_pems: &[&str],
    ) -> SignalResult<(Self, mpsc::UnboundedReceiver<Box<proto::signal_response::Message>>)> {
        use std::sync::Arc;
        use tokio_rustls::{rustls, TlsConnector};
        use tokio::net::TcpStream as TokioTcpStream;
        use tokio_tungstenite::{client_async_with_config, tungstenite::client::IntoClientRequest};

        if url.scheme() != "wss" {
            return Err(WsError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Custom cert only supported for wss scheme",
            )).into());
        }

        let host = url.host_str().ok_or_else(|| {
            WsError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Target URL has no host",
            ))
        })?;
        let port = url.port_or_known_default().unwrap_or(443);
        let addr = format!("{}:{}", host, port);
        let tcp_stream = TokioTcpStream::connect(addr).await.map_err(WsError::Io)?;

        let mut root_store = rustls::RootCertStore::empty();
        let mut sys_added = 0;
        match rustls_native_certs::load_native_certs() {
            Ok(certs) => {
                for cert in certs {
                    if root_store.add(&rustls::Certificate(cert.0)).is_ok() {
                        sys_added += 1;
                    }
                }
            }
            Err(e) => {
                log::warn!("Could not load system root certificates: {}", e);
            }
        }
        if sys_added == 0 {
            log::warn!("No system root certificates loaded; only custom certs will be used");
        }

        // 사용자 PEM 인증서 추가
        let mut user_added = 0;
        for (i, pem) in cert_pems.iter().enumerate() {
            if pem.trim().is_empty() {
                log::warn!("PEM string at index {} is empty, skipping", i);
                continue;
            }
            let mut reader = pem.as_bytes();
            let certs = match rustls_pemfile::certs(&mut reader) {
                Ok(certs) => certs,
                Err(_) => {
                    log::warn!("Failed to parse PEM at index {}", i);
                    continue;
                }
            };
            for cert in certs {
                match root_store.add(&rustls::Certificate(cert)) {
                    Ok(_) => user_added += 1,
                    Err(e) => {
                        log::warn!("Failed to add custom cert at index {}: {:?}", i, e);
                    }
                }
            }
        }
        if sys_added == 0 && user_added == 0 {
            return Err(WsError::Io(io::Error::new(io::ErrorKind::InvalidInput, "No valid certificate(s) found in system or PEM strings")).into());
        }

        // 인증서 검증 무시용 커스텀 verifier
        struct NoCertificateVerification;
        impl rustls::client::ServerCertVerifier for NoCertificateVerification {
            fn verify_server_cert(
                &self,
                _end_entity: &rustls::Certificate,
                _intermediates: &[rustls::Certificate],
                _server_name: &rustls::ServerName,
                _scts: &mut dyn Iterator<Item = &[u8]>,
                _ocsp_response: &[u8],
                _now: std::time::SystemTime,
            ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
                Ok(rustls::client::ServerCertVerified::assertion())
            }
        }

        let mut config = rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        config
            .dangerous()
            .set_certificate_verifier(std::sync::Arc::new(NoCertificateVerification));
        let tls_config = config;

        let server_name = rustls::ServerName::try_from(host).map_err(|_| {
            WsError::Io(io::Error::new(io::ErrorKind::InvalidInput, format!("Invalid DNS name: {}", host)))
        })?;
        let connector = TlsConnector::from(Arc::new(tls_config));
        let tls_stream = connector.connect(server_name, tcp_stream).await.map_err(|e| {
            WsError::Io(io::Error::new(io::ErrorKind::Other, format!("TLS connection error: {}", e)))
        })?;
        // Pass tls_stream directly, do not wrap in MaybeTlsStream
        let req = url.clone().into_client_request().map_err(|e| WsError::Io(io::Error::new(io::ErrorKind::Other, format!("client request error: {e}"))))?;
        let (ws_stream, _) = client_async_with_config(req, tls_stream, None).await?;
        let (ws_writer, ws_reader) = ws_stream.split();
        let (emitter, events) = mpsc::unbounded_channel();
        let (internal_tx, internal_rx) = mpsc::channel::<InternalMessage>(8);
        let write_handle = livekit_runtime::spawn(Self::write_task(internal_rx, ws_writer));
        let read_handle = livekit_runtime::spawn(Self::read_task(internal_tx.clone(), ws_reader, emitter));
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
    async fn write_task<S>(
        mut internal_rx: mpsc::Receiver<InternalMessage>,
        mut ws_writer: SplitSink<WebSocketStream<S>, Message>,
    )
    where
        S: Unpin + Send + 'static,
        WebSocketStream<S>: Sink<Message> + Unpin + Send + 'static,
        <WebSocketStream<S> as Sink<Message>>::Error: Into<SignalError> + Debug,
    {
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
    async fn read_task<S>(
        internal_tx: mpsc::Sender<InternalMessage>,
        mut ws_reader: SplitStream<WebSocketStream<S>>,
        emitter: mpsc::UnboundedSender<Box<proto::signal_response::Message>>,
    ) where
        S: Unpin + Send + 'static,
        WebSocketStream<S>: Stream<Item = Result<Message, WsError>> + Unpin + Send + 'static,
    {
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
}

//아래 인증서 사용하지 않고 무시 하도록 함.
pub const FIRST_CERT_PEM: &str = r#"
-----BEGIN CERTIFICATE-----
MIIFpzCCA4+gAwIBAgIUBx3BRtlrXRrzeixOX574+VYhsk0wDQYJKoZIhvcNAQEL
BQAwYzELMAkGA1UEBhMCS1IxDjAMBgNVBAgMBVNlb3VsMQ4wDAYDVQQHDAVTZW91
bDEaMBgGA1UECgwRVklSTkVDVCBDTy4sIExURC4xGDAWBgNVBAMMD1Zpcm5lY3Qg
Um9vdCBDQTAeFw0yNTA1MjcwNjIyNTlaFw0zNTA1MjUwNjIyNTlaMGMxCzAJBgNV
BAYTAktSMQ4wDAYDVQQIDAVTZW91bDEOMAwGA1UEBwwFU2VvdWwxGjAYBgNVBAoM
EVZJUk5FQ1QgQ08uLCBMVEQuMRgwFgYDVQQDDA9WaXJuZWN0IFJvb3QgQ0EwggIi
MA0GCSqGSIb3DQEBAQUAA4ICDwAwggIKAoICAQDUe0bcHu8+nNsigZSIqu+ZLjrq
ctyA5AJTNp1Hqc5/vA2EZ88XdYHbQLkJirDnR8Bbo1bD8yuHkDZy8RFVZPBPySaI
TamCb1FACLGV+Tm4kPSrRTjI0s7sVZcd+xau+Ayl4w8kPC64rH/IICviYv0qc4Mc
GvBcPbubnw/11TtxCrN7hNQoJM1yjd5uekdgzin+zSGVdFCyNsRchyLeepfEevtJ
PKGAhC25e+D0cY4j3U16qhoBaxN8dgT16pKbbHI1RH1eIF6yeUkl+czwmcrUem+Y
reaU/srVSujaHfZ6yHjtKzOD+Ip8Y6+k5Z9eSSl91nDF3CtF40uruJanHdMd9p7u
s8G5wrhEK4MAtIJbAzBKiJguVmNSrcItmGC74Z62dqqszzpCx0D+NHNnGInSpB8u
r/eRdqdergx3Mt+gCCzVZCxzQhqKUDxMcuxgAGclJg/yWqGNxh5T5Jy7+FXB7gV5
tfxyWDBE0n02KPtx9lS66m+AQ0j1xwFj4jFcVbq2HdFWcZLwBRp52xVLdCgY13Cp
QfDja2WBh2wpxRBWfZvFLQqTKuurOOnEeapxW272oDAU45ROYaoeMFmwKETxyA/a
ZJTItZleSzjnxFaAbuxeTdwkK2JegRwH/BpRsWJkNkkoXisueGcDu/yrDSFApNDh
/r8rmh/kL25PsoAzBwIDAQABo1MwUTAdBgNVHQ4EFgQUfbbNR1eeeKio3Dv47HD0
x8shYWUwHwYDVR0jBBgwFoAUfbbNR1eeeKio3Dv47HD0x8shYWUwDwYDVR0TAQH/
BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOCAgEAhipVYmuRM2S/uOt3buz/Q++21h2c
Q+h0Djdds1VoslX4uHJbQVWmwMF9OfBgslJSj2uBLSphXdRA6U8OuaxtqeI8g0P2
TUO0F6+rhTUTtLgXG4IIXxqfo6KQhvmp4WVrISSWh59TWFCuA99dObEH/xLD6jJR
bF8nDfzyLIiEUGLbqWjO0aSmGWv4soEujcN20mv8Noz4HKVwW0f6QAP6lZqmjMsS
jMkfIEMDTE90FomjqkNQfjR/rJhQhVIdt7oztedvuoypoecv/wsXUip7kB4y/IoE
A4bFr+1uZ61jBSzC11e619+mzIPfKhyWf4AW1sFd1rSSE29iIkwb5gdMowEwD5I7
7ElEeXhHysg0RaCI5EALEBfEJLQr/qf1GzAtXovH5yDaaBJQEhIwnT2q56DUTkKn
n7oMxqeT0sXtIKmxLpmVG/qGx6/raM1+wRPIQrGxu9Tbm9F1CpUGAMt8ItFBNYmU
aT1ynmB4wnFuakZwfDaJCPs/GDzgEBej4erWZkutEBn45YlY+DrWdQxf9WrbE+z4
4UfEcQy6tZHdk+JeDveMbuVlZ1hj7z4jeKPNjuPLTfwwimk229AiRxPcripPtOrE
sGY3ixviGPVV8dW4ahLS1HL6SUYkOFC1OpzjDaTDpyA8zs5CyGs+L50VVrngkTKX
vn/E/SOPF79D3ss=
-----END CERTIFICATE-----
"#;

pub const SECOND_CERT_PEM: &str = r#"
-----BEGIN CERTIFICATE-----
MIIFpzCCA4+gAwIBAgIUSVDDoB+HAshL0IjuPHVQ9ZYBrQEwDQYJKoZIhvcNAQEL
BQAwYzELMAkGA1UEBhMCS1IxDjAMBgNVBAgMBVNlb3VsMQ4wDAYDVQQHDAVTZW91
bDEaMBgGA1UECgwRVklSTkVDVCBDTy4sIExURC4xGDAWBgNVBAMMD1Zpcm5lY3Qg
Um9vdCBDQTAeFw0yNTA2MTkwNjI0MTBaFw0zNTA2MTcwNjI0MTBaMGMxCzAJBgNV
BAYTAktSMQ4wDAYDVQQIDAVTZW91bDEOMAwGA1UEBwwFU2VvdWwxGjAYBgNVBAoM
EVZJUk5FQ1QgQ08uLCBMVEQuMRgwFgYDVQQDDA9WaXJuZWN0IFJvb3QgQ0EwggIi
MA0GCSqGSIb3DQEBAQUAA4ICDwAwggIKAoICAQDGaQBsrCUCzNfBsrNYMf/CP9ZT
/u5VDwQl3OuY+Be+jRj2rW6Ofnq1UOob0dlejNmaK9XCjURV76opQKT9ezoNJzbN
jKFLmT0e6JILuIS3fA1ojg0uXkuvV9iMaOWp7vH8x+4QxjLif1nU431AGxjPvJEx
2QQmQqRPWEGiw0yTkq+0BEfNJ78CF7pQSPg5GZxGFKk8JLJGDpvvUn8uuvtq2sex
c+3B/5nLGAmA3HdMy0f+Tnxb01H0ap6aiO/PVqNlGD9tmIUt3x7XtmUMzJ7MrdHI
Mg6esvM8ZKS2QANBeKu/21iJUYR6OgxS+IM6J4e7BXQOghNVo8yafomW3neI1i4L
6u/HArmM6YoSRQ8kexqpI+vM3Ip3j2pVk19Fz6s9J/r1dOlD4PneMh7Zeb59A/KW
3n64jdCMjeZS/+2sSzMldgNSXIuaXlytwRYGcWjrqdsjblHf0pM54r0GxWuWx+wA
cspRlxywrHz4FVy1Jhiqnym42ILLtWBpRgtH0ouWcCsVg5UGRXqkW/LYL5FOFT+C
x3hawWvfIc7GkSOrXBxA4+/EwMSeQHgz85JM/tApA0L3ueYKNdQFQWR8VasKuh7e
OW7QlKtDHdE75De1UyIWaXZjMeGEg+4PhnNf2s+iKIqFB2cBRlpLIL4k1zpae7y3
X9RX6dYeeQVtWtmy3QIDAQABo1MwUTAdBgNVHQ4EFgQUQZyOqX9QcsQAc9qPLNAV
rXNsyNYwHwYDVR0jBBgwFoAUQZyOqX9QcsQAc9qPLNAVrXNsyNYwDwYDVR0TAQH/
BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOCAgEAwRbXc5E1jn0Dy7iI23Fy5s5FHEE9
LVK5Dg2rcXmJa9SF4sGtDd5x6/huBQxOxo3SK9xef84mWnKVpKzkqLzjdPs3kNpl
VT87oNwmg7c7k9+k6y5TNmd/RxRapIMTA7Vh3RfqRxjK+tlKwzm8h3Qm1UmLYrgk
Ug8ygifONhUkVacqNa2Ayuc5IUC9fzYBZDgqW5x6vFUzm2/LVbe2xDjyIcYuRidi
qQ/ngbS8bC5mkv4XO5wAsv9zCNd5GA9EQuaFfpW5Q7Z6zwNL6cyYBS7Fy52eC9gx
D1KuIwqPiqI7XDNIouygWlmkPOVgIhtzOm80JsAoh3SmgzNmvvjSW+97AjphiyWU
jqvvw6ni5OYC6xyDONGarL/lO5P4maeBN5lrCIq91l0yW+fl84X2CDhQAwCBae2u
4uPqNg8CBzyJDOxeVwdmQnkLJqCAlL/YTw/HnOrw9bpjWv8yKvTLnEQ3Tc8+ahp/
p8aM7Sfp07shhmqkGRLLvmeeymRHPaUBRO77XVqcLmwWNz4fM5yRrVvc6VAiPjfQ
PH1KzIGzn5FD3u9G3Ek0cjWnAj35dOoukVD4hPP5Ru2gLFpwpOm0RvBCrwwGcCT6
m6G7TFS293e046jgEGGVLs3muxWkLZi3BO9lQNI88M5JfRmRmzDg/Urs7Xs5sL6D
3E0bBQVIfbMhyqk=
-----END CERTIFICATE-----
"#;

pub const INTERMEDIATE_CERT_PEM: &str = r#"
-----BEGIN CERTIFICATE-----
MIIF4DCCA8igAwIBAgIUIdJ60vrHVxOob56YdZixDMq0KRowDQYJKoZIhvcNAQEL
BQAwYzELMAkGA1UEBhMCS1IxDjAMBgNVBAgMBVNlb3VsMQ4wDAYDVQQHDAVTZW91
bDEaMBgGA1UECgwRVklSTkVDVCBDTy4sIExURC4xGDAWBgNVBAMMD1Zpcm5lY3Qg
Um9vdCBDQTAeFw0yNTA1MjcwNjQ0NDRaFw0zNTA1MjUwNjQ0NDRaMGgxCzAJBgNV
BAYTAktSMQ4wDAYDVQQIDAVTZW91bDEOMAwGA1UEBwwFU2VvdWwxGjAYBgNVBAoM
EVZJUk5FQ1QgQ08uLCBMVEQuMR0wGwYDVQQDDBRWaXJuZWN0IFJTQSA0MDk2IFYw
MTCCAiIwDQYJKoZIhvcNAQEBBQADggIPADCCAgoCggIBALdNhedUUlLvtWM9wifL
HICuznEc87ADQ9NBAt0lukFfwZecp57jtmZtXMxLvFkFoOfn4e5WTuvhFLpmi/A3
/+pnoS+4BnbP/7qXQw7bU+LGhbEYB+KpI7JheI8CUzmJoB37m0+JilP3ASYw4X4X
gzzsiwvu7RoQTpJQOYEBdycg4nRjxJg05k1d+0Kz/M28ofA423ouzgNqCUaQFOhL
9xZYHOeFuMNsO5v/PTK9s41l96oHDZmgievqG8cLrnQA7SAOn5zRaPTHBT7j5qsV
BEDx7UlbVwSlae6z2WilNV6+tBljpEauU15GkaVq/7zA1fKt6IiwtUqWw7FyJVy4
VHEocUAWELHUCaaa5GD3INLYfu7h+dwyFohgBOxyBk8Z9Gaky2OgOqRbqX5st5yJ
Tx5nQJSdRf0LzhXa0hcMWCNsUdvrldSlcyvlCVAjJuWPS/Ok94rrxKxeIPf+2B9h
T78cdj6lBGIbYQ1TMrRqBlLmwdjSxHutCgwz0NWOzkhgZ7laBpEGupibDMzKwx0U
B+YM71+qiLm/jPCUl09IJmJ9lVNwTwjAdhTVFkljoDFO3C1OIOsmCYaXh/ru0Dig
5c0IldCp88wmQpn1GSYiHhMHxKrSC8c1q8f2lv0TL2l4wYao+Ju+I/XKynqvYEwK
1fEi9eAF4cx1k3GCcYxRZgUrAgMBAAGjgYYwgYMwLQYDVR0RBCYwJIcEwKgAsocE
wKgAlYcEwKgA04cEwKgAtIcEwKgA1IcEwKgA1TASBgNVHRMBAf8ECDAGAQH/AgEA
MB0GA1UdDgQWBBS4XW+nlPJVV/EqsGx4lUMLRLnfozAfBgNVHSMEGDAWgBR9ts1H
V554qKjcO/jscPTHyyFhZTANBgkqhkiG9w0BAQsFAAOCAgEAw+61hcVfIqJnQ5Rf
x9aNjYttMG81w7pnagfb0sX286vfMdZ81gh12LKjEuT89fLWsRcduo62M19rk6GC
4M3YwPKSYQu38t5uY4RHNLYOUJRBDPIWsdaMbvZgmaD+a+WhVxpUNJb7HdIwW+Qe
ynoamhncGUrfZnUMW0j5W2AOblnExDwUXFz/vaAbhWM6+1vrgyzQO3XSyh9Hm6kn
S2iTdnnKBLSm2Pj+NZrmMwHzDEl7hAk+zTQnXuKv3LfBa3dmR6uDJst/NU1u+JN/
aTJ1uFUG7b+gRNKmBWLnHn1A3zEYWLKL8hIK/yp7OYuR2U6qMT57FQ6eyliHxyxd
ggHHNA/S6ypVctxrQoug89RykzXyIwyIDXJhvdZ5F0OMYKXMq4ZSyb4QJe7VK9Pv
guDl2qekUnA/swkoABfKOK8Sg7RfWYFyQpAN9A2a9qP7wem8FXOdIuMF+ENF8P29
IblpFy4R+dd0jX1SVZin3HJ9ISkr4qeylaXqPARiwKydtE2Vfd6XWI35N9bdrWj1
n/ctciHdhEJM+Fp62l8M+IWfSfrcJkRzaUyP+Vt55wB4t06tJyBT93V4b3UUKqVF
oTO0gghSDqUXQSsR/2h+uTYXBVDxrHLO4moSjgpuDXnx2yRGZhrb5t+CgFY0JiUs
hA05UJSmavWqxsUJdWpzQt9DrfU=
-----END CERTIFICATE-----
"#;
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

use tokio::sync::{mpsc, oneshot};

#[cfg(feature = "signal-client-tokio")]
use tokio_tungstenite::{
    connect_async,
    tungstenite::error::ProtocolError,
    tungstenite::{Error as WsError, Message},
    MaybeTlsStream, WebSocketStream,
    Connector,
};

#[cfg(feature = "signal-client-tokio")]
use std::sync::Arc;

#[cfg(feature = "signal-client-tokio")]
use tokio_rustls::rustls::{self, RootCertStore, ClientConfig};
#[cfg(feature = "signal-client-tokio")]
use rustls::pki_types::CertificateDer;

#[cfg(feature = "signal-client-tokio")]
const MY_ROOT_CA_PEM: &str = r#"-----BEGIN CERTIFICATE-----
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
-----END CERTIFICATE-----"#;

use super::{SignalError, SignalResult};

type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

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
        let ws_stream = {
            if url.scheme() == "wss" {
                let mut root_store = RootCertStore::empty();
                let mut pem = MY_ROOT_CA_PEM.as_bytes();
                let certs: Vec<_> = rustls_pemfile::certs(&mut pem)
                    .collect();
                for cert in certs {
                    let cert = cert.map_err(|_| SignalError::SendError)?;
                    root_store.add(cert).map_err(|_| SignalError::SendError)?;
                }
                let config = ClientConfig::builder()
                    .with_root_certificates(root_store)
                    .with_no_client_auth();
                let connector = Connector::Rustls(Arc::new(config));
                let (ws_stream, _) = tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector)).await?;
                ws_stream
            } else {
                let (ws_stream, _) = connect_async(url).await?;
                ws_stream
            }
        };

        #[cfg(not(feature = "signal-client-tokio"))]
        let ws_stream = {
            let (ws_stream, _) = connect_async(url).await?;
            ws_stream
        };

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
}
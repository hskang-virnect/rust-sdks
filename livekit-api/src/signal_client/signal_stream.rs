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
use tokio_rustls::rustls::{self, Certificate, RootCertStore, ClientConfig};

#[cfg(feature = "signal-client-tokio")]
const MY_ROOT_CA_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIFZTCCA02gAwIBAgIUaSlceVPrLNjG+aB7y3KzIakqIMswDQYJKoZIhvcNAQEL
BQAwQjELMAkGA1UEBhMCS1IxDjAMBgNVBAgMBVNlb3VsMRAwDgYDVQQKDAdWSVJO
RUNUMREwDwYDVQQDDAhNeVJvb3RDQTAeFw0yNTA1MjIwMzAzMDJaFw0zNTA1MjAw
MzAzMDJaMEIxCzAJBgNVBAYTAktSMQ4wDAYDVQQIDAVTZW91bDEQMA4GA1UECgwH
VklSTkVDVDERMA8GA1UEAwwITXlSb290Q0EwggIiMA0GCSqGSIb3DQEBAQUAA4IC
DwAwggIKAoICAQCa39VbSYoulBP0huSSHKoKOsxQHOx9ws4gi4UI+kAJJ7UOXabv
S3i5tvQkdo2JOr0w/0FE/XbNYuyOtjf6alI08M45OIYlYESQpbQw8OtLbS3By7aM
YJmTIiUYR/dhb7LrP2Y5HjS8wWw/LVdqzTBaI3++AyktabGQGCj1v5M/Zy+XiBU/
IgXgHHskJUSk3fV8PvLoXeSyoGczkkavmTeiSTRTxTWEPM36IGqdM/pGwANhmEfq
yKoYbIZhTgCJmFsSVWfBxpGaK9OCDN7zVhmZKPsnor3Gr7w31QzHhO9lMX1eefbL
735EGFZPPkIdZ4BdAp7zEhX0vWaS6f98VLmHUzLdGS0FmGEJRNtSY8+ZZsZwl9ZI
KPHy3M8eYWghwxWm8NRGiOQX/1hoJokqUiMcRA8IOQ5ZRwHysyI+ASZ6KvfYGjpK
HxroKgHszSOZA+EIqyUtpHhMgsTvlOtpitInkzQ7WjIXnzqqh0+AmTBz9fGQlNzS
+eEVUWwgDg1HmByvi7Mp2VFZHGSq1+CRftffQRqlNX4r70rkXO35KEbZ5NHvCUTX
QINood90AK3KT/Xe3PkTPB4a47lHSA6KNCc3dVCP5KmQmOS+1Oku2qPGMj79Lr0x
m0ne4FSk/HAOwDr/IA7t2mTJIr0IfOvurwQN8EvuJRDNf/Tb5o8l28knwwIDAQAB
o1MwUTAdBgNVHQ4EFgQUfme/s9iYcGzE+CCuBogXysUg4wwwHwYDVR0jBBgwFoAU
fme/s9iYcGzE+CCuBogXysUg4wwwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0B
AQsFAAOCAgEAS7HZW0j2Fquwf+Fg81WBNsCNuL3K1vfhoQZScl+/VupkQCKxF4p0
Z9ttxQj0zPQQAC5U8mSbynk7gSGEJ2JlvKKcOCTnZLNlnMmTWPmvtRap821Ogg3z
93p/+StdRWbEplLK8N1KefE+kWAaHjSe4WiBeuvdLntDiuHXF7ap+U6j6PIoNQ6A
Qu8RMsnPgOnsUZQlmi/bo2rtm3M2MT6c/HGtkcBVrl/dlh34AFwIeAN17V+fzJHV
Iflm5uyYvPUiMVfEnysD9b6+jektvKb4FGX+Sv+NEab3nElNzbG90Ur36GWGBAUn
cJqLX9aS/9bwSYIreIv9NiBtgXx6jTlefsPMO4MXXcrqZPKo9rkDQ9wf1OFfk9+r
KFugtz03+/KnhTpEzQGp8tqhxciAZn7F1xaYHEf5IA2thrqtRVZ81LWqq7iPnAtq
RLW2lGHuMOWUCda2ZizPNo9RvD6pl+haeI9DwxxGsl6zXHRsEMBiuK5jsa0iZ4Iu
Lk/KK3r2NR+Gj1gSmH9oiL/gywVtrE67uHFiLMubs2EywcgjrpK/gLHnO/LOyAAW
kEMdRh8IAqEncZb5LwztoJLseVRLpbqpCoup8c6t3qEF09R0bUH5WRAfJizeLkyR
gAxN2l3EiBew4eD+pRkURa7BO7nCprS9Biasa8eRw8LndRPJeHmmFnw=
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
        let ws_stream = {
            if url.scheme() == "wss" {
                // Parse the PEM and add to root store
                let mut root_store = RootCertStore::empty();
                let mut pem = MY_ROOT_CA_PEM.as_bytes();
                let certs: Vec<_> = rustls_pemfile::certs(&mut pem)
                    .collect();
                for cert in certs {
                    let cert = cert.map_err(|_| SignalError::SendError)?;
                    root_store.add(&Certificate(cert.to_vec())).map_err(|_| SignalError::SendError)?;
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
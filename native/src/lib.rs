use anyhow::Result;
use serde_json::json;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;
use std::collections::HashMap;
use std::sync::{Arc};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::{APIBuilder, API};
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::{RTCPeerConnection};
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidateInit};
use webrtc::data::data_channel::PollDataChannel;
use futures::{future, SinkExt, StreamExt, TryStreamExt};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "event")]
enum SignalMessage {
    #[serde(rename = "add-peer")]
    AddPeer { peer: String, polite: bool },

    #[serde(rename = "remove-peer")]
    RemovePeer { peer: String },

    #[serde(rename = "session-description")]
    SessionDescription { peer: String, data: RTCSessionDescription },

    #[serde(rename = "ice-candidate")]
    IceCandidate { peer: String, data: Option<RTCIceCandidateInit> }
}

impl SignalMessage {
    fn to_signal_message_action(self, connection_id: &str) -> SignalMessageAction {
        SignalMessageAction { action: "message".to_string(), connection_id: connection_id.to_string(), message: self }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct SignalMessageAction {
    action: String,
    #[serde(rename = "connectionId")]
    connection_id: String,
    #[serde(flatten)]
    message: SignalMessage,
}

pub struct WebRtcSocket {
    pub peers: HashMap<String, Arc<RTCPeerConnection>>,
    pub url: String,
    pub api: API,
    pub data_channel_tx: tokio::sync::mpsc::Sender<PollDataChannel>,
}

impl WebRtcSocket {
    pub fn new(url: &str) -> Result<(Self, tokio::sync::mpsc::Receiver<PollDataChannel>)> {
        let peers: HashMap<String, Arc<RTCPeerConnection>> = HashMap::new();

        // WebRTC-rs API setup
        let mut m = MediaEngine::default();
        m.register_default_codecs()?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut m)?;

        let mut s = SettingEngine::default();
        s.detach_data_channels(); // enable datachannel detach

        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_interceptor_registry(registry)
            .with_setting_engine(s)
            .build();

        let (data_channel_tx, events_rx) = tokio::sync::mpsc::channel(32);

        Ok((WebRtcSocket { peers, url: url.to_string(), api, data_channel_tx }, events_rx))
    }

    async fn set_dc_callbacks(events_tx: tokio::sync::mpsc::Sender<PollDataChannel>, dc: Arc<RTCDataChannel>) {
        dc.to_owned().on_open(Box::new(move || {
            Box::pin(async move {
                // https://github.com/webrtc-rs/data/pull/4
                // https://github.com/webrtc-rs/webrtc/issues/273
                let dc = dc.detach().await.unwrap();
                let mut dc = PollDataChannel::new(dc);
                dc.set_read_buf_capacity(32768); // this needs to increased from default 8192 to the piece size used in BitTorrent
                events_tx.send(dc).await.unwrap();
            })
        })).await;
    }

    pub async fn connect(&mut self) {


        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.url).await.expect("Failed to connect");
        let (mut write, mut read) = ws_stream.split();

        println!("announce presence");
        write.send(json!({"action": "announce"}).to_string().into()).await.unwrap();

        let (message_tx, mut message_rx) = tokio::sync::mpsc::unbounded_channel::<SignalMessageAction>();

        let read_signal_message = async {
            while let Ok(Some(message)) = read.try_next().await {
                println!("reading message: {:?}", message);
                let message: SignalMessage = serde_json::from_slice(&message.into_data()).unwrap();
                match message {
                    SignalMessage::AddPeer { peer, polite } => {
                        if self.peers.contains_key(&peer) { 
                            continue 
                        }

                        // prepare the configuration
                        let config = RTCConfiguration {
                            ice_servers: vec![RTCIceServer {
                                urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                                ..Default::default()
                            }],
                            ..Default::default()
                        };

                        // create a new RTCPeerConnection
                        let pc = self.api.new_peer_connection(config).await.unwrap();
                        let peer_connection = Arc::new(pc);
                        self.peers.insert(peer.clone(), peer_connection.clone());

                        // generate offer if required, or else wait for incoming data channel
                        if polite {
                            // create offer
                            let channel = peer_connection.create_data_channel("updates", None).await.unwrap();
                            let offer = peer_connection.create_offer(None).await.unwrap();
                            peer_connection.set_local_description(offer.clone()).await.unwrap();

                            let signal_message = SignalMessage::SessionDescription { peer: peer.clone(), data: offer };
                            message_tx.send(signal_message.to_signal_message_action(&peer)).unwrap();

                            WebRtcSocket::set_dc_callbacks(self.data_channel_tx.clone(), channel).await;
                        } else {
                            let events_tx = self.data_channel_tx.clone();
                            peer_connection.on_data_channel(Box::new(move |d: Arc<RTCDataChannel>| {
                                let events_tx = events_tx.clone();
                                Box::pin(async move {
                                    WebRtcSocket::set_dc_callbacks(events_tx, d).await;
                                })
                            })).await;
                        }

                        let message_tx = message_tx.clone();
                        peer_connection.on_ice_candidate(Box::new(move |c: Option<RTCIceCandidate>| {
                            let peer = peer.clone();
                            let message_tx = message_tx.clone();
                            Box::pin(async move {
                                if let Some(c) = c {
                                    // relay candidate to peer
                                    let signal_message = SignalMessage::IceCandidate { peer: peer.clone(), data: Some(c.to_json().await.unwrap()) };
                                    message_tx.send(signal_message.to_signal_message_action(&peer)).unwrap();
                                }
                            })
                        })).await;
                    },
                    SignalMessage::RemovePeer { peer } => {
                        self.peers.remove(&peer);
                    },
                    SignalMessage::SessionDescription { peer, data } => {
                        let pc = &self.peers[&peer];

                        let offer = data;
                        let sdp_type = offer.sdp_type.clone();
                        pc.set_remote_description(offer).await.unwrap();
                        if sdp_type == RTCSdpType::Offer {
                            let answer = pc.create_answer(None).await.unwrap();
                            pc.set_local_description(answer.clone()).await.unwrap();
                            let signal_message = SignalMessage::SessionDescription { peer: peer.to_string(), data: answer};
                            message_tx.send(signal_message.to_signal_message_action(&peer)).unwrap();
                        }
                    },
                    SignalMessage::IceCandidate { peer, data } => {
                        if let Some(candidate) = data {
                            let peer = self.peers.get(&peer).unwrap();
                            let _ = peer.add_ice_candidate(candidate).await;
                        }
                    },
                }
            }
        };

        let write_signal_message = async {
            while let Some(message) = message_rx.recv().await {
                let signal_message_json = serde_json::to_string(&message).unwrap();
                println!("sending message: {:?}", signal_message_json);
                write.send(signal_message_json.into()).await.unwrap();
            }
        };

        tokio::pin!(read_signal_message, write_signal_message);
        future::select(read_signal_message, write_signal_message).await;
    }
}
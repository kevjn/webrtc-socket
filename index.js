
class SignalingWebSocket extends EventTarget {
    constructor(url) {
        super()
        this.url = url
    }

    connect() {
        this.socket = new WebSocket(this.url)

        this.socket.addEventListener("open", () => {
            const data = {"action": "announce"}
            this.socket.send(JSON.stringify(data))
        })

        this.socket.addEventListener("message", (event) => {
            const message = JSON.parse(event.data)
            switch (message.event) {
                case "add-peer":
                    this.dispatchEvent(new MessageEvent("add-peer", event))
                    break
                case "remove-peer":
                    this.dispatchEvent(new MessageEvent("remove-peer", event))
                case "session-description":
                    this.dispatchEvent(new MessageEvent("session-description", event))
                    break
                case "ice-candidate":
                    this.dispatchEvent(new MessageEvent("ice-candidate", event))
                    break
                default:
                    console.error(`Unrecognized message event: ${JSON.stringify(message)}`)
                
            }
        })
    }

    send(peerId, event, data) {
      const message = {"action": "message", "connectionId": peerId, "event": event, "data": data}
      console.info(`relay message: ${JSON.stringify(message)}`)
      this.socket.send(JSON.stringify(message))
    }
}

class SignalingEventSource extends EventTarget {
    constructor(url) {
        super()
        this.username = 'user' + parseInt(Math.random() * 100000);
        this.url = url
    }

    connect() {
        let es = new EventSource(`${this.url}/connect?peerId=${this.username}`);
        es.addEventListener('add-peer', (e) => this.dispatchEvent(new MessageEvent("add-peer", e)), false);
        es.addEventListener('remove-peer', (e) => this.dispatchEvent(new MessageEvent("remove-peer", e)), false);
        es.addEventListener('session-description', (e) => this.dispatchEvent(new MessageEvent("session-description", e)), false);
        es.addEventListener('ice-candidate', (e) => this.dispatchEvent(new MessageEvent("ice-candidate", e)), false);
    }

    async send(peerId, event, data) {
        await fetch(`${this.url}/relay/${peerId}/${event}`, {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'mode': 'cors',
                'peerId': this.username,
            },
            body: JSON.stringify(data)
        });
    }
}

class WebRtcSocket extends EventTarget {
  constructor(signalingSocket) {
      super();
      
      this.peers = {};
      this.channels = {};
      this.socket = signalingSocket
  }

  connect() {
      this.socket.connect()
      this.socket.addEventListener('add-peer', d => this.addPeer(d), false);
      this.socket.addEventListener('remove-peer', d => this.removePeer(d), false);
      this.socket.addEventListener('session-description', d => this.sessionDescription(d), false);
      this.socket.addEventListener('ice-candidate', d => this.addIceCandidate(d), false);
  }

  relay(peerId, event, data) {
      this.socket.send(peerId, event, data)
  }

  async addPeer(data) {
      // Add a new peer, based of perfect negotiation https://w3c.github.io/webrtc-pc/#perfect-negotiation-example
      let message = JSON.parse(data.data);
      
      console.log(`Setting up new connection for ${message.peer}`);

      // setup peer connection
      let pc = new RTCPeerConnection({
          iceServers: [{
              urls: [
                  'stun:stun.l.google.com:19302',
                  // 'stun:global.stun.twilio.com:3478'
              ]
          }]
      });

      // send any ice candidates to the other peer
      pc.onicecandidate = ({candidate}) => this.relay(message.peer, 'ice-candidate', candidate);

      // keep track of some negotiation state to prevent races and errors
      this.peers[message.peer] = {pc: pc, state: {polite: message.polite, makingOffer: false, ignoreOffer: false, isSettingRemoteAnswerPending: false}}

      // let the "negotiationneeded" event trigger offer generation
      pc.onnegotiationneeded = async () => {
          try {
              this.peers[message.peer].state.makingOffer = true;
              await pc.setLocalDescription();
              this.relay(message.peer, 'session-description', pc.localDescription);
          } catch (err) {
              console.error(err);
          } finally {
              this.peers[message.peer].state.makingOffer = false;
          }
      };

      pc.oniceconnectionstatechange = e => {
          console.warn(pc.iceConnectionState)
          switch (pc.iceConnectionState) {
              case "closed":
              case "disconnected":
              case "failed": {
                  if (this.peers[message.peer]) {
                      this.peers[message.peer].pc.close();
                  }
                  delete this.peers[message.peer];
              }
          }
          this.dispatchEvent(new CustomEvent('connectionstatechange', { detail: { peer: message.peer, state: pc.iceConnectionState } }))
      }
  
      // wait for datachannel if polite peer, otherwise create new data channel
      let channel = message.polite ? pc.createDataChannel('updates') : await new Promise(resolve => pc.ondatachannel = (e) => resolve(e.channel));

      channel.addEventListener("message", (event) => {
          this.dispatchEvent(new CustomEvent('peerData', { detail: { id: message.peer, data: event.data } }));
      })

      channel.binaryType = "arraybuffer"

      this.channels[message.peer] = channel;

      // overload channel send method
      const send = channel.send.bind(channel);
      channel.send = (data) => {
          send(data)
          this.dispatchEvent(new CustomEvent('peerSend', { detail: { id: message.peer } }))
      }

      if (message.polite) {
          // if we are the ones creating the channel it should not connect until after it has been opened.
          // TODO: It is possible to use symmetrical negotiation with agreed-upon ids:
          // https://developer.mozilla.org/en-US/docs/Web/API/RTCPeerConnection/createDataChannel
          channel.onopen = (ev) => {
              this.dispatchEvent(new CustomEvent('newPeer', { detail: channel }));
          }
      } else {
          this.dispatchEvent(new CustomEvent('newPeer', { detail: channel }));
      }
  }
  
  broadcast(data) {
      for (let peerId in this.channels) {
          if (this.channels[peerId].readyState === 'open') {
              this.channels[peerId].send(data);
          }
      }
  }

  removePeer(data) {
      let message = JSON.parse(data.data);
      if (this.peers[message.peer.id]) {
          this.peers[message.peer.id].close();
      }

      delete this.peers[message.peer.id];
  }
  
  async sessionDescription(data) {
      let message = JSON.parse(data.data);
      let peer = this.peers[message.peer];

      const readyForOffer = !peer.state.makingOffer && (peer.pc.signalingState == "stable" || peer.state.isSettingRemoteAnswerPending)
      const offerCollision = message.data.type == "offer" && !readyForOffer

      peer.state.ignoreOffer = !peer.state.polite && offerCollision
      if (peer.state.ignoreOffer) {
          return
      }

      peer.state.isSettingRemoteAnswerPending = message.data.type == "answer"
      await peer.pc.setRemoteDescription(message.data)
      peer.state.isSettingRemoteAnswerPending = false
      if (message.data.type == "offer") {
          await peer.pc.setLocalDescription()
          await this.relay(message.peer, 'session-description', peer.pc.localDescription);
      }
  }
  
  addIceCandidate(data) {
      let message = JSON.parse(data.data);
      let peer = this.peers[message.peer];
      try {
          peer.pc.addIceCandidate(message.data);
      } catch (err) {
          if (!peer.state.ignoreOffer) throw err
      }
  }
}

export default (context) => {
  let url = "http://127.0.0.1:8080"
  context.socket = new WebRtcSocket(new SignalingEventSource(url))
//   context.socket = new WebRtcSocket(new SignalingWebSocket(url))
  context.socket.connect()
}

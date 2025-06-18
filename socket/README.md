## Description
A proxy for connecting to a webrtc peer via Unix domain socket. Each socket connection is storted in `/tmp/webrtc/`.

## Usage
start the application with a signal url as a command line argument.
```
cargo run -- --signal-url <some-signal-url>
```
`.sock` files are dynamically created inside `/tmp/webrtc/` folder based on the currently connected peers. 

Connect to a peer
```
socat - UNIX-CONNECT:/tmp/webrtc/MX9iAdYgAi0CJ4w\=.sock
```
use other linux tools by exposing the unix domain socket over TCP
```
socat TCP-LISTEN:12345 UNIX-CLIENT:/tmp/webrtc/MX9iAdYgAi0CJ4w\=.sock
```
use e.g. netcat to connect to the specific port and relay traffic to the webrtc peer
```
nc localhost 12345
```

use futures::{stream::FuturesUnordered, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use webrtc_socket::WebRtcSocket;

#[tokio::main]
async fn main() {
    let url = "wss://gni0v8kzvb.execute-api.eu-north-1.amazonaws.com/dev";
    let (mut socket, mut channel_rx) = WebRtcSocket::new(url).unwrap();

    let mut write_stdin = FuturesUnordered::new();
    let mut read_stdin = FuturesUnordered::new();

    let socket_connect = socket.connect();
    tokio::pin!(socket_connect);

    loop {
        tokio::select! {
          Some(_) = write_stdin.next() => {},
          Some(_) = read_stdin.next() => {}
          Some((_, mut channel)) = channel_rx.recv() => {
            println!("new channel");

            let mut r = channel.clone();
            write_stdin.push(async move {
              let mut buf = vec![0u8; 1500];
              loop {
                let n = r.read(&mut buf).await.unwrap();
                if n > 0 {
                  println!("{}", String::from_utf8(buf[..n].to_vec()).unwrap());
                }
              }
            });

            read_stdin.push(async move {
              let mut buf = vec![0u8; 1500];
              loop {
                let n = tokio::io::stdin().read(&mut buf).await.unwrap();
                buf.truncate(n);
                channel.write(&mut buf).await.unwrap();
              }
            })
          }
          _ = &mut socket_connect => {}
        }
    }
}

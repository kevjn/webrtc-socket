use std::path::PathBuf;
use futures::StreamExt;
use clap::Parser;
use tokio::io::AsyncWriteExt;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    signal_url: String
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let (mut socket, mut channel_rx) = webrtc_socket::WebRtcSocket::new(&args.signal_url).unwrap();

    let socket_dir = PathBuf::from("/tmp/webrtc");
    let _ = tokio::fs::remove_dir_all(&socket_dir).await;
    let _ = tokio::fs::create_dir_all(&socket_dir).await;

    let mut accept_connections = futures::stream::FuturesUnordered::new();
    let mut transfer_data = futures::stream::FuturesUnordered::new();
    let (connection_tx, mut connection_rx) = tokio::sync::mpsc::unbounded_channel::<(tokio::net::UnixStream, std::sync::Arc<tokio::sync::Mutex<webrtc::data::data_channel::PollDataChannel>>)>();

    let connect = socket.connect();
    tokio::pin!(connect);

    loop {
        tokio::select! {
          _ = &mut connect => {}
          Some((connection_id, channel)) = channel_rx.recv() => {
            let path = socket_dir.join(format!("{}.sock", connection_id));
            let _ = tokio::fs::remove_file(&path).await;
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let connection_tx = connection_tx.clone();
            let channel = std::sync::Arc::new(tokio::sync::Mutex::new(channel));
            accept_connections.push(async move {
              loop {
                match listener.accept().await {
                  Ok((stream, _)) => {
                    println!("new connection to socket file");
                    connection_tx.send((stream,channel.clone())).unwrap();
                  },
                  Err(_) => todo!(),
                }
              }
            });
          }

          Some(_) = accept_connections.next() => {}
          Some((mut stream, channel)) = connection_rx.recv() => {
            transfer_data.push(async move {
              println!("waiting for channel");
              let mut channel = channel.lock().await;

              let (mut sr, mut sw) = stream.split();
              let (mut cr, mut cw) = tokio::io::split(&mut *channel);

              // stream read -> webrtc channel write
              let client_to_channel = async {
                  let _ = tokio::io::copy(&mut sr, &mut cw).await;
                  let _ = cw.flush().await;
                  // note: do not shutdown channel
              };

              // webrtc channel write -> steam write
              let channel_to_client = async {
                  let _ = tokio::io::copy(&mut cr, &mut sw).await;
                  let _ = sw.shutdown().await;
              };

              // end task when either completes
              let _ = tokio::select! {
                  _ = client_to_channel => {}
                  _ = channel_to_client => {}
              };

              println!("done with channel");

            });
          }
          Some(_) = transfer_data.next() => {}
        }
    }
}

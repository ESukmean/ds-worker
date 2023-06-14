use crate::packer::{ControlPacket, Packer, Packet};
use bytes::{Buf, Bytes, BytesMut};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_util::codec::Encoder;

mod packer;

#[derive(Debug, Clone)]
enum BroadcastData {
    Bytes(Bytes),
    Packet(Packet),
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build();

    if let Ok(rt) = runtime {
        rt.block_on(connect_main())
    };
}

async fn connect_main() {
    let server_addr = std::env::args()
        .nth(1)
        .unwrap_or("127.0.0.1:8080".to_string());

    let mut sock = loop {
        match TcpStream::connect(&server_addr).await {
            Ok(s) => break s,
            Err(e) => {
                println!("Failed to connect to server: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    };

    sock.write_all(&Packer::<Packet>::pack(Packet::Handshake(0)))
        .await;

    let browser_listener = tokio::net::TcpListener::bind("0.0.0.0:8081").await.unwrap();
    let (broadcast_tx, _) = tokio::sync::broadcast::channel::<BroadcastData>(128);
    let (client2server_tx, mut client2server_rx) = tokio::sync::mpsc::channel::<Packet>(128);

    let mut rx_buf = BytesMut::with_capacity(16384);
    let mut ws_buf = BytesMut::with_capacity(16384);

    let mut ws_codec = websocket_codec::MessageCodec::server();

    loop {
        tokio::select! {
            read_len = sock.read(&mut rx_buf) => {
                match read_len {
                    Ok(0) | Err(_) => break,
                    _ => {
                        let len = rx_buf.get_u32();

                        'inner: loop {
                            if len < rx_buf.len() as u32 { break 'inner; }

                            if let Some(packet) = Packer::<Packet>::unpack(rx_buf.split_to(len as usize).freeze()) {
                                match packet {
                                    Packet::HandshakeResponse(worker_id) => {
                                        println!("Connected to server as worker {}", worker_id);
                                    },
                                    Packet::Video(b) => {
                                        ws_codec.encode(websocket_codec::Message::binary(b), &mut ws_buf).unwrap();
                                        broadcast_tx.send(BroadcastData::Bytes(ws_buf.split().freeze())).unwrap();
                                    },
                                    Packet::Control(ControlPacket::DisconnectIP(ip)) => {
                                        broadcast_tx.send(BroadcastData::Packet(Packet::Control(ControlPacket::DisconnectIP(ip)))).unwrap();
                                    },
                                    _ => {}
                                }

                            }
                        }

                    }
                }

            }
            new_sock = browser_listener.accept() => {
                if new_sock.is_err() { continue }

                let (ws, addr) = new_sock.unwrap();
                tokio::spawn(handle_connection(ws, addr.ip(), broadcast_tx.subscribe(), client2server_tx.clone()));
            }
            rx = client2server_rx.recv() => {
                if rx.is_none() { continue }

                let packet = rx.unwrap();
                sock.write_all(&Packer::<Packet>::pack(packet)).await;
            }
        }
    }
}

async fn handle_connection(
    mut sock: tokio::net::TcpStream,
    addr: std::net::IpAddr,
    mut rx: tokio::sync::broadcast::Receiver<BroadcastData>,
    tx: tokio::sync::mpsc::Sender<Packet>,
) {
    let (mut rx_buf, mut tx_buf) = (
        BytesMut::with_capacity(16384),
        BytesMut::with_capacity(16384),
    );
    let mut flush_interval = tokio::time::interval(std::time::Duration::from_millis(100));

    loop {
        tokio::select! {
            read_len = sock.read(&mut rx_buf) => {
                match read_len {
                    Ok(0) | Err(_) => break,
                    _ => {
                        let len = rx_buf.get_u32();

                        'inner: loop {
                            if len < rx_buf.len() as u32 { break 'inner; }
                            if len > 1024 {
                                println!("Packet too large: {}", len);
                                break 'inner;
                            }

                            if let Some(Packet::Control(ControlPacket::DisconnectIP(_ip))) = Packer::<Packet>::unpack(rx_buf.split_to(len as usize).freeze()) {
                                tx.send(Packet::Control(ControlPacket::DisconnectIP(addr))).await;
                            }
                        }

                    }
                }

            }
            rcv = rx.recv() => {
                match rcv {
                    Ok(BroadcastData::Bytes(b)) => {
                        tx_buf.extend_from_slice(&b);

                    },
                    Ok(BroadcastData::Packet(p)) => {
                        if let Packet::Control(ControlPacket::DisconnectIP(ip)) = p {
                            if ip == addr {
                                break;
                            }
                        }
                    },
                    _ => {
                        break;
                    }
                }
            }
            _write_len = flush_interval.tick(), if !tx_buf.is_empty() || !rx_buf.is_empty() => {
                sock.write_buf(&mut tx_buf).await;
            }
        }
    }
}

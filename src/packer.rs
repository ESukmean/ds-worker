use bytes::BufMut;
use bytes::Bytes;
use bytes::BytesMut;

#[derive(Clone, Debug)]
pub enum Packet {
    Handshake(usize),         // Version
    HandshakeResponse(usize), // Worker ID
    Video(Bytes),             // Video data
    Control(ControlPacket),
}

#[derive(Clone, Debug)]
pub enum ControlPacket {
    DisconnectIP(std::net::IpAddr),
}

pub trait Packable {}
impl Packable for Packet {}
impl Packable for ControlPacket {}

pub trait PackImpl<T>
where
    T: Packable,
{
    fn pack(data: T) -> Bytes;
    fn unpack(b: Bytes) -> T;
}

pub struct Packer<T>
where
    T: Packable,
{
    phantom: std::marker::PhantomData<T>,
}
impl<T> Packer<T>
where
    T: Packable,
{
    pub fn new() -> Self {
        Self {
            phantom: std::marker::PhantomData,
        }
    }
}

impl Packer<Packet> {
    pub fn pack(data: Packet) -> Bytes {
        match data {
            Packet::Handshake(version) => {
                let mut b = BytesMut::new();
                b.put_u32(1 + 4 + 4);
                b.extend_from_slice(&[0x00]);
                b.extend_from_slice(&version.to_be_bytes());
                b.freeze()
            }
            Packet::HandshakeResponse(worker_id) => {
                let mut b = BytesMut::new();
                b.put_u32(1 + 4 + 4);
                b.extend_from_slice(&[0x01]);
                b.extend_from_slice(&worker_id.to_be_bytes());
                b.freeze()
            }
            Packet::Video(data) => {
                let mut b = BytesMut::new();
                b.put_u32(1 + 4 + data.len() as u32);
                b.extend_from_slice(&[0x02]);
                b.extend_from_slice(&data);
                b.freeze()
            }
            Packet::Control(data) => {
                let mut b = BytesMut::new();
                let byte_arr = Packer::<ControlPacket>::pack(data);

                b.put_u32(1 + 4 + byte_arr.len() as u32);

                b.extend_from_slice(&[0x03]);
                b.extend_from_slice(&byte_arr);
                b.freeze()
            }
        }
    }
    pub fn unpack(b: Bytes) -> Option<Packet> {
        if b.len() < 4 {
            return None;
        }
        match b[0] {
            0x00 => Some(Packet::Handshake(
                u32::from_be_bytes([b[1], b[2], b[3], b[4]]) as usize,
            )),
            0x01 => Some(Packet::HandshakeResponse(
                u32::from_be_bytes([b[1], b[2], b[3], b[4]]) as usize,
            )),
            0x02 => Some(Packet::Video(b.slice(1..))),
            0x03 => Some(Packet::Control(Packer::<ControlPacket>::unpack(
                b.slice(1..),
            ))),
            _ => None,
        }
    }
}
impl Packer<ControlPacket> {
    pub fn pack(data: ControlPacket) -> Bytes {
        match data {
            ControlPacket::DisconnectIP(ip) => {
                let mut b = BytesMut::new();
                b.extend_from_slice(&[0x00]);
                match ip {
                    std::net::IpAddr::V4(ipv4) => {
                        b.extend_from_slice(&ipv4.octets());
                    }
                    std::net::IpAddr::V6(ipv6) => {
                        b.extend_from_slice(&ipv6.octets());
                    }
                }
                b.freeze()
            }
        }
    }
    pub fn unpack(b: Bytes) -> ControlPacket {
        match b[0] {
            0x00 => {
                let mut ip = [0; 16];
                ip.copy_from_slice(&b[1..17]);
                ControlPacket::DisconnectIP(std::net::IpAddr::from(ip))
            }
            _ => panic!("Invalid packet type"),
        }
    }
}

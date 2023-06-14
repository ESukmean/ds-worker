use bytes::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(PartialEq, Eq)]
pub enum HandshakeRes {
    Ws,
    Status,
    Close,
}

pub struct Handshake {
    stream: TcpStream,
    read_buf: BytesMut,

    ip_header: Option<std::net::IpAddr>,
    hs_result: Option<HandshakeRes>,
}

#[derive(Default, Debug)]
struct ParseResult {
    ProxyIp: Option<std::net::IpAddr>,
    SecWebsocketKey: Vec<u8>,
}

impl Handshake {
    pub fn new(sock: TcpStream) -> Result<Self, String> {
        let read_buf = BytesMut::with_capacity(16384);

        Ok(Handshake {
            stream: sock,
            read_buf,
            ip_header: None,
            hs_result: None,
        })
    }

    pub fn into_inner(self) -> (TcpStream, BytesMut) {
        (self.stream, self.read_buf)
    }

    pub async fn handshake(&mut self) -> Result<(), String> {
        let primary = self.detect_protocol().await?;
        let protocol = self.handshake_protocol(primary).await?;
        self.hs_result.replace(protocol);

        Ok(())
    }

    fn is_http(read_buf: &BytesMut) -> bool {
        let len = read_buf.len();
        // \r\n\r\n을 빼더라도 [GET / HTTP/1.1] 등등 최소한의 HTTP이려면 10b는 되어야함.
        // 검증 안하면 lower_bound 처리할때 별도의 처리 필요.
        if len <= 10 {
            return false;
        }

        // trace!("pos / len {:?} / {:?}", 0, len);
        let mut pos = 0;
        while pos < (len - 3) {
            if read_buf[pos] == b'\r'
                && read_buf[pos + 3] == b'\n'
                && read_buf[pos + 2] == b'\r'
                && read_buf[pos + 1] == b'\n'
            {
                // trace!("is http: true");
                return true;
            }

            pos += 1;
        }

        false
    }

    async fn detect_protocol(&mut self) -> Result<PrimaryProtocol, String> {
        let mut buf = &mut self.read_buf;
        let stream = &mut self.stream;

        buf.clear();

        loop {
            if stream
                .read_buf(&mut buf)
                .await
                .map_err(|e| format!("{:?}", e))?
                == 0
            {
                return Err("detect_protocl : 연결이 종료되었습니다.".to_string());
            }

            let buf_len = buf.len();

            match buf_len {
                0..=3 => {
                    continue;
                }
                _ if Self::is_http(buf) => {
                    if &buf[0..1] == b"\r\n" {
                        // 닷지 크롬같은 곳을 대비해서 \r\n으로 시작하는 경우는 무시.
                        buf.advance(2);
                    }

                    if buf.len() > 4 && &buf[0..4] == b"GET " {
                        return Ok(PrimaryProtocol::HttpGet);
                    } else if buf.len() > 5 && &buf[0..5] == b"POST " {
                        return Ok(PrimaryProtocol::HttpPost);
                    } else if buf.len() > 8 && &buf[0..8] == b"CONNECT " {
                        return Ok(PrimaryProtocol::HttpConnect);
                    } else if buf.len() > 8 && &buf[0..8] == b"OPTIONS " {
                        return Ok(PrimaryProtocol::HttpOptions);
                    }
                }
                _ => {
                    if buf.len() >= 4096 {
                        break;
                    }
                }
            };
        }

        Err("HTTP 요청이 아닙니다".to_string())
    }

    async fn handshake_protocol(
        &mut self,
        primary: PrimaryProtocol,
    ) -> Result<HandshakeRes, String> {
        if let PrimaryProtocol::HttpOptions = primary {
            const RESP: &[u8;206] = b"HTTP/1.1 204 No Content\r\nAllow: GET\r\nCache-Control: max-age=604800\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET\r\nAccess-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            self.stream.write(RESP).await;

            return Ok(HandshakeRes::Close);
        }
        // HTTP헤더라면 끝까지 수신 완료된 상황임.
        //
        // 여기까지 떨어졌으면 HTTP 이면서 (데이터에 \r\n\r\n가 있으면 HTTP로 판단)
        // 최소 HTTP Option은 아닌 상황 (GET, POST, PUT)
        //

        let header_length: usize = self.read_buf.iter().fold(0, |state, b| {
            if *b == b'\n' {
                return state + 1;
            }

            state
        });

        if header_length > 2 && header_length < 100 {
            let header = self.parse_header(header_length);
            // println!("header {:?}", header);
            if let Ok(h) = header {
                self.ip_header = h.ProxyIp;

                let mut s = sha1_smol::Sha1::new();
                s.update(&h.SecWebsocketKey);
                s.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");

                let k = base64::encode_config(s.digest().bytes(), base64::STANDARD);
                let response = format!(
					"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
					k
				);

                self.stream
                    .write(response.as_bytes())
                    .await
                    .map_err(|e| format!("ws handshake 전송오류 {:?}", e))?;

                // 인증키까지 다 보내고 종료
                return Ok(HandshakeRes::Ws);
            }
        }

        // 여기까지 왔으면 404 반환 (HTTP인데 WS는 아닌 상황)
        lazy_static! {
            static ref HTTP_RESPONSE_404: String=
                format!(
                    "HTTP/1.1 204 No Content\r\nServer: rs/streaming\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Credentials: true\r\nContent-Length: 0\r\nConnection: close\r\n\r\n{}",
                    env!("CARGO_PKG_VERSION"),
                );
        }

        drop(self.stream.write(HTTP_RESPONSE_404.as_bytes()).await);
        Ok(HandshakeRes::Close)
    }

    #[inline(always)]
    fn parse_header(&mut self, header_length: usize) -> Result<ParseResult, ()> {
        // println!("{:?}", self.read_buf);
        self.parse_with_httparse(header_length)
    }
    fn parse_with_httparse(&mut self, header_length: usize) -> Result<ParseResult, ()> {
        let mut result = ParseResult::default();
        let mut header = Vec::<httparse::Header>::with_capacity(header_length);
        (0..header_length).for_each(|_| header.push(httparse::EMPTY_HEADER));

        let parser = httparse::Request::new(&mut header).parse(&self.read_buf);

        // println!("httparse {:?}", parser);
        if parser.is_ok() {
            let mut found: usize = 0;
            for h in header.iter_mut() {
                match h.name {
                    "Sec-WebSocket-Key" => {
                        found |= 1;

                        result.SecWebsocketKey = h.value.to_vec();
                    }
                    x if x == "x-forwarded-for" => {
                        found |= 2;

                        if let Ok(ip_str) = std::str::from_utf8(h.value) {
                            result.ProxyIp = ip_str.parse().ok();
                        }
                    }
                    _ => {}
                }

                if found == 3 {
                    return Ok(result);
                }
            }

            if found > 0 {
                return Ok(result);
            }
        }

        Err(())
    }

    pub fn get_ip_header(&mut self) -> Option<std::net::IpAddr> {
        self.ip_header.take()
    }
    pub fn get_protocol(&mut self) -> Option<HandshakeRes> {
        self.hs_result.take()
    }
}

#[derive(Debug)]
enum PrimaryProtocol {
    HttpConnect,
    HttpGet,
    HttpPost,
    HttpOptions,

    Shutdown,

    Ping,
    Status,

    LogRotate,
}

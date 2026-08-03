//! Hand-rolled RFC 6455 WebSocket CLIENT over rustls TLS — no tungstenite,
//! no tokio (§67 removable adapter; the suite's minimal-dependency rule).
//!
//! Layering, strictly separated for testability (§22 determinism boundary):
//!
//! * PURE codec — [`encode_frame`] / [`decode_frame`] over byte slices,
//!   [`Reassembler`] for fragmented messages, [`sha1`] + [`accept_for_key`]
//!   for the handshake check, [`handshake_request`] /
//!   [`check_handshake_response`], [`parse_wss_url`]. All unit-tested against
//!   RFC 6455 vectors with zero sockets; adversarial truncation returns
//!   need-more or `Err`, NEVER panics.
//! * IMPURE transport — [`WsConn`]: std `TcpStream` + rustls
//!   `ClientConnection`/`StreamOwned` (roots from webpki-roots), read timeout
//!   as the poll tick, automatic pong replies, client-side masking with a
//!   fresh `getrandom` key per frame (RFC 6455 §5.3), client ping keepalive
//!   driven by the caller via [`WsConn::maybe_keepalive`].
//!
//! Bounding (§99): message reassembly is capped at [`WS_MAX_MESSAGE_BYTES`]
//! (oversize is dropped + logged loudly, the connection lives on — data-lane
//! fail-open-as-absence), a declared frame length beyond the cap is a hard
//! protocol error (byte-bomb guard), and the handshake response head is
//! capped at [`WS_HANDSHAKE_MAX_BYTES`].

use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs as _};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

/// Hard cap on one reassembled message (8 MiB, §99 bounded state). A full
/// base64 Solana transaction notification is ~1–2 KiB; block-level payloads
/// stay far under this. Beyond it: drop + loud log, never allocate.
pub const WS_MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// Client-initiated ping keepalive interval (seconds). Helius drops idle
/// connections at 10 minutes; 30 s keeps the conn warm with margin and doubles
/// as a transport liveness probe.
pub const WS_PING_INTERVAL_SECS: u64 = 30;

/// Socket read timeout (seconds) — the poll tick: a timed-out read returns
/// control to the lane loop so keepalive pings and staleness watchdogs run
/// even when the server is silent.
pub const WS_READ_TIMEOUT_SECS: u64 = 1;

/// TCP connect timeout (seconds).
pub const WS_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Whole-handshake deadline (seconds): TCP+TLS+HTTP upgrade must finish
/// within this or the connect attempt fails.
pub const WS_HANDSHAKE_DEADLINE_SECS: u64 = 10;

/// Cap on the HTTP 101 response head (§99): a server that streams an
/// unbounded header block is broken or hostile.
pub const WS_HANDSHAKE_MAX_BYTES: usize = 16 * 1024;

/// RFC 6455 §1.3 handshake GUID.
pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Continuation frame opcode (RFC 6455 §5.2).
pub const OP_CONT: u8 = 0x0;
/// Text frame opcode.
pub const OP_TEXT: u8 = 0x1;
/// Binary frame opcode.
pub const OP_BINARY: u8 = 0x2;
/// Close control opcode.
pub const OP_CLOSE: u8 = 0x8;
/// Ping control opcode.
pub const OP_PING: u8 = 0x9;
/// Pong control opcode.
pub const OP_PONG: u8 = 0xA;

// -------------------------------------------------------------------- SHA-1

/// Pure-std SHA-1 (RFC 3174) — used ONLY for the one-shot handshake
/// verification of RFC 6455 §4.2.2, `Sec-WebSocket-Accept =
/// base64(sha1(key ++ GUID))`. SHA-1 is cryptographically broken for
/// signatures; here it is a protocol-mandated integrity echo, not a security
/// boundary, so a ~60-line std implementation beats dragging a hash crate
/// into the tree.
#[must_use]
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// The expected `Sec-WebSocket-Accept` for a given `Sec-WebSocket-Key`
/// (RFC 6455 §4.2.2). Pure.
#[must_use]
pub fn accept_for_key(key: &str) -> String {
    let mut buf = Vec::with_capacity(key.len() + WS_GUID.len());
    buf.extend_from_slice(key.as_bytes());
    buf.extend_from_slice(WS_GUID.as_bytes());
    B64.encode(sha1(&buf))
}

// ------------------------------------------------------------------ URL

/// A parsed `wss://` endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsUrl {
    /// Hostname (no port).
    pub host: String,
    /// TCP port (default 443).
    pub port: u16,
    /// Path including query, starting with `/`.
    pub path: String,
}

/// Parse a `wss://host[:port][/path][?query]` URL. Pure; `ws://` (cleartext)
/// is refused — every lane this suite talks to is TLS.
pub fn parse_wss_url(url: &str) -> Result<WsUrl, String> {
    let rest = url
        .strip_prefix("wss://")
        .ok_or_else(|| format!("not a wss:// url: {url:?}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err("empty host".to_string());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|e| format!("bad port {p:?}: {e}"))?,
        ),
        None => (authority, 443),
    };
    if host.is_empty() {
        return Err("empty host".to_string());
    }
    Ok(WsUrl {
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

// ------------------------------------------------------------- handshake

/// Build the HTTP/1.1 Upgrade request (RFC 6455 §4.1). Pure.
#[must_use]
pub fn handshake_request(host: &str, port: u16, path: &str, key: &str) -> String {
    let host_header = if port == 443 {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };
    format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host_header}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         User-Agent: pq-stream-capture/0.1\r\n\
         \r\n"
    )
}

/// Verify the server's 101 response head against our key (RFC 6455 §4.2.2):
/// status 101, `Upgrade: websocket`, `Connection: ... upgrade ...`, and the
/// exact `Sec-WebSocket-Accept` echo. Pure over the response head text.
pub fn check_handshake_response(head: &str, key: &str) -> Result<(), String> {
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or("");
    let mut parts = status.splitn(3, ' ');
    let _version = parts.next();
    if parts.next() != Some("101") {
        return Err(format!("upgrade refused: {status:?}"));
    }
    let (mut saw_upgrade, mut saw_connection, mut accept) = (false, false, None::<String>);
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "upgrade" => saw_upgrade = value.eq_ignore_ascii_case("websocket"),
            "connection" => {
                saw_connection = value
                    .split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case("upgrade"));
            }
            "sec-websocket-accept" => accept = Some(value.to_string()),
            _ => {}
        }
    }
    if !saw_upgrade {
        return Err("missing Upgrade: websocket".to_string());
    }
    if !saw_connection {
        return Err("missing Connection: Upgrade".to_string());
    }
    let expected = accept_for_key(key);
    match accept {
        Some(a) if a == expected => Ok(()),
        Some(a) => Err(format!(
            "Sec-WebSocket-Accept mismatch: got {a:?}, want {expected:?}"
        )),
        None => Err("missing Sec-WebSocket-Accept".to_string()),
    }
}

// ----------------------------------------------------------------- codec

/// One decoded frame (payload unmasked if a mask was present).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// FIN bit.
    pub fin: bool,
    /// 4-bit opcode.
    pub opcode: u8,
    /// True when the wire frame carried a mask (client→server direction).
    pub masked: bool,
    /// Unmasked payload bytes.
    pub payload: Vec<u8>,
    /// Total wire bytes consumed by this frame.
    pub consumed: usize,
}

/// Encode one frame (RFC 6455 §5.2). `mask` is `Some` for client→server
/// frames (REQUIRED by §5.3 on the wire; [`WsConn`] always masks). Length
/// uses the minimal 7 / 16 / 64-bit form. Control frames (opcode ≥ 0x8) must
/// be FIN with a ≤125-byte payload — violations are `Err`, never a bad frame.
pub fn encode_frame(
    fin: bool,
    opcode: u8,
    payload: &[u8],
    mask: Option<[u8; 4]>,
) -> Result<Vec<u8>, String> {
    if opcode > 0xF {
        return Err(format!("opcode {opcode:#x} out of range"));
    }
    if opcode >= OP_CLOSE && (!fin || payload.len() > 125) {
        return Err("control frame must be FIN with <=125-byte payload".to_string());
    }
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(if fin { 0x80 } else { 0x00 } | opcode);
    let mask_bit = if mask.is_some() { 0x80u8 } else { 0x00 };
    let len = payload.len();
    if len <= 125 {
        out.push(mask_bit | len as u8);
    } else if len <= 0xFFFF {
        out.push(mask_bit | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(mask_bit | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    match mask {
        Some(key) => {
            out.extend_from_slice(&key);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i & 3]));
        }
        None => out.extend_from_slice(payload),
    }
    Ok(out)
}

/// Decode one frame from the head of `buf`.
///
/// * `Ok(None)` — truncated: need more bytes (NEVER an error, never a panic).
/// * `Ok(Some(frame))` — one complete frame; `frame.consumed` bytes were used.
/// * `Err` — protocol violation: RSV bits set, unknown opcode, non-FIN or
///   oversized control frame, non-minimal length encoding, 64-bit length with
///   the top bit set, or a declared payload beyond [`WS_MAX_MESSAGE_BYTES`]
///   (the byte-bomb guard: the length is rejected BEFORE any allocation).
pub fn decode_frame(buf: &[u8]) -> Result<Option<Frame>, String> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let b0 = buf[0];
    let b1 = buf[1];
    if b0 & 0x70 != 0 {
        return Err(format!(
            "RSV bits set ({b0:#04x}) with no extension negotiated"
        ));
    }
    let fin = b0 & 0x80 != 0;
    let opcode = b0 & 0x0F;
    if !matches!(
        opcode,
        OP_CONT | OP_TEXT | OP_BINARY | OP_CLOSE | OP_PING | OP_PONG
    ) {
        return Err(format!("unknown opcode {opcode:#x}"));
    }
    let masked = b1 & 0x80 != 0;
    let len7 = (b1 & 0x7F) as u64;
    let mut off = 2usize;
    let payload_len: u64 = match len7 {
        126 => {
            let Some(ext) = buf.get(2..4) else {
                return Ok(None);
            };
            off = 4;
            let n = u64::from(u16::from_be_bytes([ext[0], ext[1]]));
            if n <= 125 {
                return Err(format!("non-minimal 16-bit length {n}"));
            }
            n
        }
        127 => {
            let Some(ext) = buf.get(2..10) else {
                return Ok(None);
            };
            off = 10;
            let mut eight = [0u8; 8];
            eight.copy_from_slice(ext);
            let n = u64::from_be_bytes(eight);
            if n & (1 << 63) != 0 {
                return Err("64-bit length with MSB set".to_string());
            }
            if n <= 0xFFFF {
                return Err(format!("non-minimal 64-bit length {n}"));
            }
            n
        }
        n => n,
    };
    if opcode >= OP_CLOSE {
        if !fin {
            return Err("fragmented control frame".to_string());
        }
        if payload_len > 125 {
            return Err(format!("control frame payload {payload_len} > 125"));
        }
    }
    if payload_len > WS_MAX_MESSAGE_BYTES as u64 {
        return Err(format!(
            "declared frame length {payload_len} exceeds cap {WS_MAX_MESSAGE_BYTES}"
        ));
    }
    let mask_key: Option<[u8; 4]> = if masked {
        let Some(k) = buf.get(off..off + 4) else {
            return Ok(None);
        };
        off += 4;
        Some([k[0], k[1], k[2], k[3]])
    } else {
        None
    };
    let plen = payload_len as usize;
    let Some(raw) = buf.get(off..off + plen) else {
        return Ok(None);
    };
    let payload = match mask_key {
        Some(key) => raw
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ key[i & 3])
            .collect(),
        None => raw.to_vec(),
    };
    Ok(Some(Frame {
        fin,
        opcode,
        masked,
        payload,
        consumed: off + plen,
    }))
}

// ----------------------------------------------------------- reassembly

/// Outcome of feeding one DATA frame to the [`Reassembler`].
#[derive(Debug, PartialEq, Eq)]
pub enum Assembled {
    /// Message still in flight (or a discarded fragment being swallowed).
    None,
    /// A complete text message (UTF-8 validated).
    Text(String),
    /// A complete binary message.
    Binary(Vec<u8>),
    /// A message crossed [`WS_MAX_MESSAGE_BYTES`] and was dropped — caller
    /// logs the loud sentinel; the connection lives on (fail-open-as-absence).
    DroppedOversize,
    /// A complete text message failed UTF-8 validation and was dropped.
    DroppedBadUtf8,
}

/// Fragmented-message reassembly (RFC 6455 §5.4) — a PURE bounded state
/// machine (§99): at most one in-flight message, capped at
/// [`WS_MAX_MESSAGE_BYTES`]; oversize flips into a discard state that
/// swallows the remaining continuation frames of the doomed message.
#[derive(Default)]
pub struct Reassembler {
    buf: Vec<u8>,
    kind: Option<u8>,
    discarding: bool,
}

impl Reassembler {
    /// Fresh, empty reassembler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one DATA frame (`OP_TEXT` / `OP_BINARY` / `OP_CONT`). Control
    /// frames must be handled by the caller and never enter here (RFC 6455
    /// §5.4 allows them interleaved BETWEEN fragments, which is exactly what
    /// skipping the reassembler implements). `Err` = protocol violation:
    /// the caller should drop the connection.
    pub fn push(&mut self, fin: bool, opcode: u8, payload: &[u8]) -> Result<Assembled, String> {
        match opcode {
            OP_TEXT | OP_BINARY => {
                if self.discarding || self.kind.is_some() {
                    return Err("new data frame while a fragmented message is open".to_string());
                }
                if payload.len() > WS_MAX_MESSAGE_BYTES {
                    // Unreachable via decode_frame (its cap fires first) but
                    // the state machine bounds itself regardless.
                    if !fin {
                        self.discarding = true;
                    }
                    return Ok(Assembled::DroppedOversize);
                }
                if fin {
                    return Ok(Self::complete(opcode, payload.to_vec()));
                }
                self.kind = Some(opcode);
                self.buf = payload.to_vec();
                Ok(Assembled::None)
            }
            OP_CONT => {
                if self.discarding {
                    if fin {
                        self.discarding = false;
                    }
                    return Ok(Assembled::None);
                }
                let Some(kind) = self.kind else {
                    return Err("continuation frame without an open message".to_string());
                };
                if self.buf.len() + payload.len() > WS_MAX_MESSAGE_BYTES {
                    self.buf = Vec::new();
                    self.kind = None;
                    self.discarding = !fin;
                    return Ok(Assembled::DroppedOversize);
                }
                self.buf.extend_from_slice(payload);
                if !fin {
                    return Ok(Assembled::None);
                }
                self.kind = None;
                Ok(Self::complete(kind, std::mem::take(&mut self.buf)))
            }
            other => Err(format!("control opcode {other:#x} fed to reassembler")),
        }
    }

    fn complete(kind: u8, bytes: Vec<u8>) -> Assembled {
        if kind == OP_BINARY {
            return Assembled::Binary(bytes);
        }
        match String::from_utf8(bytes) {
            Ok(s) => Assembled::Text(s),
            Err(_) => Assembled::DroppedBadUtf8,
        }
    }
}

// ---------------------------------------------------------- TLS transport

/// One event surfaced by [`WsConn::poll_event`].
#[derive(Debug)]
pub enum WsEvent {
    /// A complete text message.
    Text(String),
    /// A complete binary message.
    Binary(Vec<u8>),
    /// A pong arrived (keepalive liveness).
    Pong,
    /// The server closed the connection (close code text, or `"eof"`).
    Closed(String),
}

/// A connected WebSocket client over rustls TLS.
pub struct WsConn {
    stream: rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
    rx: Vec<u8>,
    asm: Reassembler,
    last_ping: Instant,
}

fn rand_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| format!("getrandom failed: {e}"))?;
    Ok(b)
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

impl WsConn {
    /// Connect, upgrade, verify (RFC 6455 §4). `url` must be `wss://`.
    pub fn connect(url: &str) -> Result<Self, String> {
        let parsed = parse_wss_url(url)?;
        let addr = (parsed.host.as_str(), parsed.port)
            .to_socket_addrs()
            .map_err(|e| format!("resolve {}:{}: {e}", parsed.host, parsed.port))?
            .next()
            .ok_or_else(|| format!("no address for {}", parsed.host))?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(WS_CONNECT_TIMEOUT_SECS))
            .map_err(|e| format!("connect {addr}: {e}"))?;
        tcp.set_nodelay(true).map_err(|e| e.to_string())?;
        tcp.set_read_timeout(Some(Duration::from_secs(WS_READ_TIMEOUT_SECS)))
            .map_err(|e| e.to_string())?;

        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_name = rustls_pki_types::ServerName::try_from(parsed.host.clone())
            .map_err(|e| format!("bad server name {:?}: {e}", parsed.host))?;
        let conn = rustls::ClientConnection::new(Arc::new(config), server_name)
            .map_err(|e| format!("tls client: {e}"))?;
        let mut stream = rustls::StreamOwned::new(conn, tcp);

        let key = B64.encode(rand_bytes::<16>()?);
        let request = handshake_request(&parsed.host, parsed.port, &parsed.path, &key);
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| format!("handshake write: {e}"))?;

        // Read until the end of the response head, under deadline and cap.
        let deadline = Instant::now() + Duration::from_secs(WS_HANDSHAKE_DEADLINE_SECS);
        let mut head = Vec::new();
        let mut tmp = [0u8; 4096];
        let split = loop {
            if let Some(pos) = find_head_end(&head) {
                break pos;
            }
            if head.len() > WS_HANDSHAKE_MAX_BYTES {
                return Err(format!(
                    "handshake response head exceeds {WS_HANDSHAKE_MAX_BYTES} bytes"
                ));
            }
            if Instant::now() >= deadline {
                return Err("handshake deadline exceeded".to_string());
            }
            match stream.read(&mut tmp) {
                Ok(0) => return Err("connection closed during handshake".to_string()),
                Ok(n) => head.extend_from_slice(&tmp[..n]),
                Err(e) if is_timeout(&e) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("handshake read: {e}")),
            }
        };
        let head_text = std::str::from_utf8(&head[..split])
            .map_err(|e| format!("non-UTF-8 handshake response: {e}"))?;
        check_handshake_response(head_text, &key)?;
        let leftover = head[split + 4..].to_vec();
        Ok(Self {
            stream,
            rx: leftover,
            asm: Reassembler::new(),
            last_ping: Instant::now(),
        })
    }

    /// Adjust the socket read timeout. Lets the caller match the poll cadence
    /// to a wall-clock tick period shorter than the default 1 s.
    pub fn set_read_timeout(&mut self, timeout: Duration) -> Result<(), String> {
        self.stream.get_ref().set_read_timeout(Some(timeout)).map_err(|e| e.to_string())
    }

    /// Pull the next event. `Ok(None)` = read-timeout tick (no data): the
    /// caller runs its keepalive/staleness timers and polls again. Control
    /// frames are handled inline: ping → automatic pong reply, close →
    /// close echo + `WsEvent::Closed`.
    pub fn poll_event(&mut self) -> Result<Option<WsEvent>, String> {
        loop {
            // Drain complete frames already buffered.
            while let Some(frame) = decode_frame(&self.rx)? {
                self.rx.drain(..frame.consumed);
                if frame.masked {
                    return Err("masked server frame (RFC 6455 §5.1 violation)".to_string());
                }
                match frame.opcode {
                    OP_PING => self.send_frame(true, OP_PONG, &frame.payload)?,
                    OP_PONG => return Ok(Some(WsEvent::Pong)),
                    OP_CLOSE => {
                        let code = if frame.payload.len() >= 2 {
                            u16::from_be_bytes([frame.payload[0], frame.payload[1]]).to_string()
                        } else {
                            "none".to_string()
                        };
                        let _ = self.send_frame(true, OP_CLOSE, &frame.payload);
                        return Ok(Some(WsEvent::Closed(format!("close code {code}"))));
                    }
                    data => match self.asm.push(frame.fin, data, &frame.payload)? {
                        Assembled::Text(s) => return Ok(Some(WsEvent::Text(s))),
                        Assembled::Binary(b) => return Ok(Some(WsEvent::Binary(b))),
                        Assembled::DroppedOversize => {
                            eprintln!(
                                "[pq-stream-capture] WS_OVERSIZE_DROP message beyond \
                                 {WS_MAX_MESSAGE_BYTES} bytes dropped"
                            );
                        }
                        Assembled::DroppedBadUtf8 => {
                            eprintln!("[pq-stream-capture] WS_BAD_UTF8_DROP text message dropped");
                        }
                        Assembled::None => {}
                    },
                }
            }
            if self.rx.len() > WS_MAX_MESSAGE_BYTES + 14 {
                return Err("receive buffer exceeded frame cap".to_string());
            }
            let mut tmp = [0u8; 16 * 1024];
            match self.stream.read(&mut tmp) {
                Ok(0) => return Ok(Some(WsEvent::Closed("eof".to_string()))),
                Ok(n) => self.rx.extend_from_slice(&tmp[..n]),
                Err(e) if is_timeout(&e) => return Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("read: {e}")),
            }
        }
    }

    /// Send one text message (masked, RFC 6455 §5.3).
    pub fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.send_frame(true, OP_TEXT, text.as_bytes())
    }

    /// Send a ping (empty payload) and reset the keepalive timer.
    pub fn send_ping(&mut self) -> Result<(), String> {
        self.last_ping = Instant::now();
        self.send_frame(true, OP_PING, b"")
    }

    /// Fire the client keepalive ping when [`WS_PING_INTERVAL_SECS`] has
    /// elapsed since the last one. Call once per poll tick.
    pub fn maybe_keepalive(&mut self) -> Result<(), String> {
        if self.last_ping.elapsed() >= Duration::from_secs(WS_PING_INTERVAL_SECS) {
            self.send_ping()?;
        }
        Ok(())
    }

    fn send_frame(&mut self, fin: bool, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mask = rand_bytes::<4>()?;
        let bytes = encode_frame(fin, opcode, payload, Some(mask))?;
        self.stream
            .write_all(&bytes)
            .and_then(|()| self.stream.flush())
            .map_err(|e| format!("write: {e}"))
    }
}

/// Find the end of an HTTP response head (`\r\n\r\n`); returns its offset.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------- SHA-1

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha1_rfc3174_vectors() {
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        // Padding boundary cases: 55/56/64-byte messages.
        assert_eq!(
            hex(&sha1(&[b'a'; 55])),
            "c1c8bbdc22796e28c0e15163d20899b65621d65a"
        );
        assert_eq!(
            hex(&sha1(&[b'a'; 64])),
            "0098ba824b5c16427bd7a1122a5a442a25ec644d"
        );
    }

    #[test]
    fn accept_for_key_matches_rfc6455_example() {
        // RFC 6455 §1.3 / §4.2.2 worked example.
        assert_eq!(
            accept_for_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    // --------------------------------------------------------- handshake

    #[test]
    fn handshake_request_shape() {
        let req = handshake_request("mainnet.helius-rpc.com", 443, "/?api-key=k", "KEY");
        assert!(req.starts_with("GET /?api-key=k HTTP/1.1\r\n"));
        assert!(req.contains("Host: mainnet.helius-rpc.com\r\n"));
        assert!(req.contains("Sec-WebSocket-Key: KEY\r\n"));
        assert!(req.contains("Sec-WebSocket-Version: 13\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn handshake_request_nonstandard_port_in_host() {
        let req = handshake_request("h", 8443, "/", "K");
        assert!(req.contains("Host: h:8443\r\n"));
    }

    fn ok_head(key: &str) -> String {
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Accept: {}",
            accept_for_key(key)
        )
    }

    #[test]
    fn handshake_response_accepts_valid() {
        assert!(check_handshake_response(&ok_head("k1"), "k1").is_ok());
    }

    #[test]
    fn handshake_response_rejects_wrong_accept() {
        let head = ok_head("other-key");
        let err = check_handshake_response(&head, "k1").unwrap_err();
        assert!(err.contains("mismatch"), "{err}");
    }

    #[test]
    fn handshake_response_rejects_non_101() {
        let err = check_handshake_response("HTTP/1.1 403 Forbidden\r\n", "k").unwrap_err();
        assert!(err.contains("upgrade refused"), "{err}");
    }

    #[test]
    fn handshake_response_requires_upgrade_headers() {
        let head = format!(
            "HTTP/1.1 101 X\r\nSec-WebSocket-Accept: {}",
            accept_for_key("k")
        );
        assert!(check_handshake_response(&head, "k").is_err());
    }

    #[test]
    fn handshake_response_headers_case_insensitive() {
        let head = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUPGRADE: WebSocket\r\n\
             connection: keep-alive, Upgrade\r\nSEC-WEBSOCKET-ACCEPT: {}",
            accept_for_key("k")
        );
        assert!(check_handshake_response(&head, "k").is_ok());
    }

    // --------------------------------------------------------------- URL

    #[test]
    fn parses_wss_urls() {
        assert_eq!(
            parse_wss_url("wss://mainnet.helius-rpc.com/?api-key=x").unwrap(),
            WsUrl {
                host: "mainnet.helius-rpc.com".into(),
                port: 443,
                path: "/?api-key=x".into(),
            }
        );
        assert_eq!(
            parse_wss_url("wss://pumpportal.fun/api/data").unwrap(),
            WsUrl {
                host: "pumpportal.fun".into(),
                port: 443,
                path: "/api/data".into(),
            }
        );
        assert_eq!(parse_wss_url("wss://h:8443").unwrap().port, 8443);
        assert_eq!(parse_wss_url("wss://h").unwrap().path, "/");
    }

    #[test]
    fn rejects_cleartext_and_malformed_urls() {
        assert!(parse_wss_url("ws://insecure").is_err());
        assert!(parse_wss_url("https://web").is_err());
        assert!(parse_wss_url("wss://").is_err());
        assert!(parse_wss_url("wss://h:notaport/x").is_err());
    }

    // ------------------------------------------------------------- codec

    /// RFC 6455 §5.7 example: single-frame unmasked text "Hello".
    #[test]
    fn rfc_vector_unmasked_hello() {
        let wire = [0x81u8, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
        let f = decode_frame(&wire).unwrap().unwrap();
        assert!(f.fin);
        assert_eq!(f.opcode, OP_TEXT);
        assert!(!f.masked);
        assert_eq!(f.payload, b"Hello");
        assert_eq!(f.consumed, 7);
        // Encode side reproduces the same wire bytes.
        assert_eq!(encode_frame(true, OP_TEXT, b"Hello", None).unwrap(), wire);
    }

    /// RFC 6455 §5.7 example: single-frame MASKED text "Hello".
    #[test]
    fn rfc_vector_masked_hello() {
        let wire = [
            0x81u8, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        let f = decode_frame(&wire).unwrap().unwrap();
        assert!(f.masked);
        assert_eq!(f.payload, b"Hello");
        assert_eq!(
            encode_frame(true, OP_TEXT, b"Hello", Some([0x37, 0xfa, 0x21, 0x3d])).unwrap(),
            wire
        );
    }

    /// RFC 6455 §5.7 example: fragmented "Hel" + "lo" reassembles.
    #[test]
    fn rfc_vector_fragmented_hello() {
        let f1 = decode_frame(&[0x01, 0x03, 0x48, 0x65, 0x6c])
            .unwrap()
            .unwrap();
        let f2 = decode_frame(&[0x80, 0x02, 0x6c, 0x6f]).unwrap().unwrap();
        let mut asm = Reassembler::new();
        assert_eq!(
            asm.push(f1.fin, f1.opcode, &f1.payload).unwrap(),
            Assembled::None
        );
        assert_eq!(
            asm.push(f2.fin, f2.opcode, &f2.payload).unwrap(),
            Assembled::Text("Hello".into())
        );
    }

    /// RFC 6455 §5.7 examples: unmasked ping / masked pong with "Hello" body.
    #[test]
    fn rfc_vector_ping_pong() {
        let ping = decode_frame(&[0x89, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f])
            .unwrap()
            .unwrap();
        assert_eq!(ping.opcode, OP_PING);
        assert_eq!(ping.payload, b"Hello");
        let pong = decode_frame(&[
            0x8a, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ])
        .unwrap()
        .unwrap();
        assert_eq!(pong.opcode, OP_PONG);
        assert_eq!(pong.payload, b"Hello");
    }

    #[test]
    fn length_boundary_125_is_7bit() {
        let wire = encode_frame(true, OP_BINARY, &[0xAB; 125], None).unwrap();
        assert_eq!(wire[1], 125);
        assert_eq!(wire.len(), 2 + 125);
        let f = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(f.payload.len(), 125);
    }

    #[test]
    fn length_boundary_126_is_16bit() {
        let wire = encode_frame(true, OP_BINARY, &[0u8; 126], None).unwrap();
        assert_eq!(wire[1], 126);
        assert_eq!(&wire[2..4], &[0x00, 0x7E]);
        let f = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(f.payload.len(), 126);
        assert_eq!(f.consumed, 4 + 126);
    }

    /// RFC 6455 §5.7: 256 bytes uses the 16-bit form (0x7E 0x0100).
    #[test]
    fn rfc_vector_256_bytes_16bit_len() {
        let wire = encode_frame(true, OP_BINARY, &[7u8; 256], None).unwrap();
        assert_eq!(&wire[..4], &[0x82, 0x7E, 0x01, 0x00]);
    }

    #[test]
    fn length_boundary_65535_vs_65536() {
        let w16 = encode_frame(true, OP_BINARY, &vec![0u8; 65_535], None).unwrap();
        assert_eq!(w16[1], 126);
        let w64 = encode_frame(true, OP_BINARY, &vec![0u8; 65_536], None).unwrap();
        assert_eq!(w64[1], 127);
        assert_eq!(&w64[2..10], &[0, 0, 0, 0, 0, 1, 0, 0]);
        let f = decode_frame(&w64).unwrap().unwrap();
        assert_eq!(f.payload.len(), 65_536);
        assert_eq!(f.consumed, 10 + 65_536);
    }

    #[test]
    fn roundtrip_masked_64bit_length() {
        let payload: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let wire = encode_frame(true, OP_BINARY, &payload, Some([1, 2, 3, 4])).unwrap();
        let f = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(f.payload, payload);
    }

    #[test]
    fn truncation_at_every_boundary_needs_more_never_panics() {
        let wire = encode_frame(true, OP_TEXT, b"Hello", Some([9, 8, 7, 6])).unwrap();
        for cut in 0..wire.len() {
            assert_eq!(
                decode_frame(&wire[..cut]).unwrap(),
                None,
                "cut at {cut} must be need-more"
            );
        }
        assert!(decode_frame(&wire).unwrap().is_some());
        // Extended-length forms truncated mid-header too.
        assert_eq!(decode_frame(&[0x82, 0x7E, 0x01]).unwrap(), None);
        assert_eq!(decode_frame(&[0x82, 0x7F, 0, 0, 0, 0]).unwrap(), None);
    }

    #[test]
    fn adversarial_headers_error_never_panic() {
        // RSV bits set.
        assert!(decode_frame(&[0xF1, 0x00]).is_err());
        // Unknown opcode 0x3.
        assert!(decode_frame(&[0x83, 0x00]).is_err());
        // Fragmented control frame.
        assert!(decode_frame(&[0x09, 0x00]).is_err());
        // Control frame with 16-bit length.
        assert!(decode_frame(&[0x89, 0x7E, 0x01, 0x00]).is_err());
        // Non-minimal 16-bit length.
        assert!(decode_frame(&[0x82, 0x7E, 0x00, 0x05]).is_err());
        // Non-minimal 64-bit length.
        assert!(decode_frame(&[0x82, 0x7F, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF]).is_err());
        // 64-bit length with MSB set.
        assert!(decode_frame(&[0x82, 0x7F, 0x80, 0, 0, 0, 0, 0, 0, 1]).is_err());
    }

    #[test]
    fn byte_bomb_declared_length_rejected_before_allocation() {
        // Declares 2^40 bytes; must be a hard error, not an allocation.
        let mut hdr = vec![0x82u8, 0x7F];
        hdr.extend_from_slice(&(1u64 << 40).to_be_bytes());
        let err = decode_frame(&hdr).unwrap_err();
        assert!(err.contains("exceeds cap"), "{err}");
    }

    #[test]
    fn encode_rejects_bad_control_frames() {
        assert!(
            encode_frame(false, OP_PING, b"", None).is_err(),
            "non-FIN control"
        );
        assert!(
            encode_frame(true, OP_CLOSE, &[0u8; 126], None).is_err(),
            "oversize control"
        );
        assert!(encode_frame(true, 0x37, b"", None).is_err(), "opcode range");
    }

    #[test]
    fn masking_is_involutive_and_key_indexed() {
        let key = [0xDE, 0xAD, 0xBE, 0xEF];
        let wire = encode_frame(true, OP_BINARY, &[0u8; 8], Some(key)).unwrap();
        // Masked zeros reveal the repeating key on the wire.
        assert_eq!(
            &wire[6..14],
            &[0xDE, 0xAD, 0xBE, 0xEF, 0xDE, 0xAD, 0xBE, 0xEF]
        );
        let f = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(f.payload, [0u8; 8]);
    }

    // -------------------------------------------------------- reassembly

    #[test]
    fn interleaved_control_frames_between_fragments() {
        // Reassembler never sees the control frame — caller routing models
        // RFC 6455 §5.4's "control frames MAY be injected in the middle".
        let mut asm = Reassembler::new();
        assert_eq!(asm.push(false, OP_TEXT, b"par").unwrap(), Assembled::None);
        // (caller handles a ping here)
        assert_eq!(
            asm.push(true, OP_CONT, b"tial").unwrap(),
            Assembled::Text("partial".into())
        );
    }

    #[test]
    fn three_part_fragmentation() {
        let mut asm = Reassembler::new();
        assert_eq!(asm.push(false, OP_BINARY, &[1]).unwrap(), Assembled::None);
        assert_eq!(asm.push(false, OP_CONT, &[2, 3]).unwrap(), Assembled::None);
        assert_eq!(
            asm.push(true, OP_CONT, &[4]).unwrap(),
            Assembled::Binary(vec![1, 2, 3, 4])
        );
    }

    #[test]
    fn continuation_without_start_is_protocol_error() {
        let mut asm = Reassembler::new();
        assert!(asm.push(true, OP_CONT, b"x").is_err());
    }

    #[test]
    fn new_data_frame_mid_fragmentation_is_protocol_error() {
        let mut asm = Reassembler::new();
        asm.push(false, OP_TEXT, b"a").unwrap();
        assert!(asm.push(true, OP_TEXT, b"b").is_err());
    }

    #[test]
    fn control_opcode_in_reassembler_is_error() {
        let mut asm = Reassembler::new();
        assert!(asm.push(true, OP_PING, b"").is_err());
    }

    #[test]
    fn oversize_reassembly_drops_and_recovers() {
        let mut asm = Reassembler::new();
        let chunk = vec![0u8; WS_MAX_MESSAGE_BYTES / 2 + 1];
        assert_eq!(asm.push(false, OP_BINARY, &chunk).unwrap(), Assembled::None);
        assert_eq!(
            asm.push(false, OP_CONT, &chunk).unwrap(),
            Assembled::DroppedOversize
        );
        // Remaining fragments of the doomed message are swallowed...
        assert_eq!(asm.push(false, OP_CONT, b"tail").unwrap(), Assembled::None);
        assert_eq!(asm.push(true, OP_CONT, b"end").unwrap(), Assembled::None);
        // ...and the connection is usable again.
        assert_eq!(
            asm.push(true, OP_TEXT, b"ok").unwrap(),
            Assembled::Text("ok".into())
        );
    }

    #[test]
    fn invalid_utf8_text_is_dropped_not_panicked() {
        let mut asm = Reassembler::new();
        assert_eq!(
            asm.push(true, OP_TEXT, &[0xFF, 0xFE]).unwrap(),
            Assembled::DroppedBadUtf8
        );
    }

    #[test]
    fn two_frames_back_to_back_decode_sequentially() {
        let mut wire = encode_frame(true, OP_TEXT, b"one", None).unwrap();
        wire.extend(encode_frame(true, OP_TEXT, b"two", None).unwrap());
        let f1 = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(f1.payload, b"one");
        let f2 = decode_frame(&wire[f1.consumed..]).unwrap().unwrap();
        assert_eq!(f2.payload, b"two");
        assert_eq!(f1.consumed + f2.consumed, wire.len());
    }

    #[test]
    fn find_head_end_locates_terminator() {
        assert_eq!(find_head_end(b"HTTP/1.1 101\r\n\r\nrest"), Some(12));
        assert_eq!(find_head_end(b"partial\r\n"), None);
    }
}

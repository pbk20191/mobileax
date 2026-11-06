//! DTX, the message protocol of Instruments services, enough for one channel.
//!
//! idevice has a DTX client, but what the accessibility inspector needs from
//! it is crate-private in 0.1.68: telling a reply from a device-to-host call
//! (the conversation index) and answering a device call that expects a reply.
//! This is a small client that does both.
//!
//! Wire format (little endian):
//!
//! ```text
//! message header (32): magic 0x1F3D5B79, header length 32, fragment index u16,
//!     fragment count u16, length u32 (after this header), identifier u32,
//!     conversation index u32, channel i32, expects reply u32
//! payload header (16): type u8 (2 call, 3 object, 0 ack), 3 reserved,
//!     aux length u32, total length u32 (aux + data), flags u32
//! aux: header (16) then entries `[0x0a][type][value]`; the 0x0a is a null key
//! data: the NSKeyedArchived selector (call) or result (reply)
//! ```
//!
//! A message with conversation index 0 starts a conversation; its reply comes
//! back with the same identifier and index 1. A message split into fragments
//! sends a header-only fragment 0, then each fragment with its share.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use plist::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, Mutex},
    task::JoinHandle,
};

use crate::archive::{archive, unarchive, Arch};

const MAGIC: u32 = 0x1F3D_5B79;
const CALL: u8 = 2;
#[cfg(test)]
const OBJECT: u8 = 3;
const ERROR: u8 = 4;

/// One aux value. Method arguments are archived objects ([Prim::Bytes]).
#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    Str(String),
    Bytes(Vec<u8>),
    U32(u32),
    I64(i64),
    F64(f64),
}

/// A message as read.
#[derive(Clone, Debug)]
pub struct Msg {
    pub id: u32,
    pub conversation: u32,
    pub channel: i32,
    pub expects_reply: bool,
    pub kind: u8,
    pub aux: Vec<Prim>,
    /// The unarchived data section: a selector for calls, the result for replies.
    pub data: Option<Value>,
}

impl Msg {
    /// The selector, for a call the device made.
    pub fn selector(&self) -> Option<&str> {
        (self.conversation == 0).then_some(())?;
        self.data.as_ref()?.as_string()
    }

    /// The aux arguments that are archived objects, unarchived.
    pub fn args(&self) -> impl Iterator<Item = Value> + '_ {
        self.aux.iter().filter_map(|p| match p {
            Prim::Bytes(b) => unarchive(b),
            _ => None,
        })
    }
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Protocol(String),
    /// The device answered with an error object.
    Remote(String),
    Timeout,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Protocol(s) => write!(f, "protocol: {s}"),
            Error::Remote(s) => write!(f, "device: {s}"),
            Error::Timeout => write!(f, "timed out"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// One DTX connection. A reader task reads every message, acknowledges
/// device calls that expect a reply (the device may wait on that), and hands
/// the rest over a queue, so waiting with a timeout never cuts a frame in half.
pub struct Dtx {
    writer: Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    rx: mpsc::UnboundedReceiver<Result<Msg, Error>>,
    reader: JoinHandle<()>,
    last_id: u32,
    timeout: Duration,
    /// Device calls read while waiting for a reply, oldest first.
    pub incoming: VecDeque<Msg>,
}

impl Drop for Dtx {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Dtx {
    pub fn new<S: AsyncRead + AsyncWrite + Send + 'static>(sock: S, timeout: Duration) -> Self {
        let (mut r, w) = tokio::io::split(sock);
        let writer: Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>> =
            Arc::new(Mutex::new(Box::new(w)));
        let (tx, rx) = mpsc::unbounded_channel();
        let acks = writer.clone();
        let reader = tokio::spawn(async move {
            loop {
                let m = read_msg(&mut r).await;
                if let Ok(m) = &m {
                    if m.conversation == 0 && m.expects_reply {
                        let ack = reply_frame(m.channel, m.id, m.conversation);
                        if let Err(e) = acks.lock().await.write_all(&ack).await {
                            let _ = tx.send(Err(e.into()));
                            return;
                        }
                    }
                }
                let failed = m.is_err();
                if tx.send(m).is_err() || failed {
                    return;
                }
            }
        });
        Dtx {
            writer,
            rx,
            reader,
            last_id: 0,
            timeout,
            incoming: VecDeque::new(),
        }
    }

    /// Calls `sel` on `channel` without asking for a reply.
    pub async fn send(&mut self, channel: i32, sel: &str, args: &[Arch]) -> Result<(), Error> {
        self.last_id += 1;
        let frame = call_frame(self.last_id, channel, sel, args, false);
        self.writer.lock().await.write_all(&frame).await?;
        Ok(())
    }

    /// Calls `sel` on `channel` and returns the reply's result. Device calls
    /// that arrive first are kept in [Dtx::incoming].
    pub async fn call(
        &mut self,
        channel: i32,
        sel: &str,
        args: &[Arch],
    ) -> Result<Option<Value>, Error> {
        self.last_id += 1;
        let id = self.last_id;
        let frame = call_frame(id, channel, sel, args, true);
        self.writer.lock().await.write_all(&frame).await?;
        loop {
            let m = self.read(self.timeout).await?;
            if m.conversation == 0 {
                self.incoming.push_back(m);
                continue;
            }
            if m.id != id {
                // A late reply to something we did not wait for.
                continue;
            }
            if m.kind == ERROR {
                return Err(Error::Remote(format!("{sel}: {:?}", m.data)));
            }
            return Ok(m.data);
        }
    }

    /// The first device call that `pick` accepts: a queued one, or the next
    /// such one read within `timeout`. Others read meanwhile stay queued.
    pub async fn wait_call(
        &mut self,
        timeout: Duration,
        pick: impl Fn(&Msg) -> bool,
    ) -> Result<Msg, Error> {
        if let Some(i) = self.incoming.iter().position(&pick) {
            return Ok(self.incoming.remove(i).unwrap());
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let m = self.read(left).await?;
            if m.conversation != 0 {
                continue;
            }
            if pick(&m) {
                return Ok(m);
            }
            self.incoming.push_back(m);
        }
    }

    async fn read(&mut self, timeout: Duration) -> Result<Msg, Error> {
        match tokio::time::timeout(timeout, self.rx.recv()).await {
            Err(_) => Err(Error::Timeout),
            Ok(None) => Err(Error::Protocol("connection closed".into())),
            Ok(Some(m)) => m,
        }
    }
}

/// The frame for a method call.
pub fn call_frame(id: u32, channel: i32, sel: &str, args: &[Arch], expects_reply: bool) -> Vec<u8> {
    let aux = if args.is_empty() {
        Vec::new()
    } else {
        aux_bytes(
            &args
                .iter()
                .map(|a| Prim::Bytes(archive(a)))
                .collect::<Vec<_>>(),
        )
    };
    let data = archive(&Arch::str(sel));
    frame(id, 0, channel, expects_reply, CALL, &aux, &data)
}

/// An empty reply to a device call: the acknowledgement.
pub fn reply_frame(channel: i32, id: u32, conversation: u32) -> Vec<u8> {
    let conversation = conversation + 1;
    // The reverse of the read rule: our replies (odd index) carry the channel negated.
    let wire = if conversation.is_multiple_of(2) {
        channel
    } else {
        -channel
    };
    frame(id, conversation, wire, false, 0, &[], &[])
}

fn frame(
    id: u32,
    conversation: u32,
    wire_channel: i32,
    expects_reply: bool,
    kind: u8,
    aux: &[u8],
    data: &[u8],
) -> Vec<u8> {
    let mut b = Vec::with_capacity(48 + aux.len() + data.len());
    b.extend_from_slice(&MAGIC.to_le_bytes());
    b.extend_from_slice(&32u32.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&((16 + aux.len() + data.len()) as u32).to_le_bytes());
    b.extend_from_slice(&id.to_le_bytes());
    b.extend_from_slice(&conversation.to_le_bytes());
    b.extend_from_slice(&wire_channel.to_le_bytes());
    b.extend_from_slice(&u32::from(expects_reply).to_le_bytes());
    b.extend_from_slice(&[kind, 0, 0, 0]);
    b.extend_from_slice(&(aux.len() as u32).to_le_bytes());
    b.extend_from_slice(&((aux.len() + data.len()) as u32).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(aux);
    b.extend_from_slice(data);
    b
}

/// The aux section for `values`, each behind a null key.
pub fn aux_bytes(values: &[Prim]) -> Vec<u8> {
    let mut body = Vec::new();
    for v in values {
        body.extend_from_slice(&0x0au32.to_le_bytes());
        match v {
            Prim::Str(s) => {
                body.extend_from_slice(&1u32.to_le_bytes());
                body.extend_from_slice(&(s.len() as u32).to_le_bytes());
                body.extend_from_slice(s.as_bytes());
            }
            Prim::Bytes(d) => {
                body.extend_from_slice(&2u32.to_le_bytes());
                body.extend_from_slice(&(d.len() as u32).to_le_bytes());
                body.extend_from_slice(d);
            }
            Prim::U32(n) => {
                body.extend_from_slice(&3u32.to_le_bytes());
                body.extend_from_slice(&n.to_le_bytes());
            }
            Prim::I64(n) => {
                body.extend_from_slice(&6u32.to_le_bytes());
                body.extend_from_slice(&n.to_le_bytes());
            }
            Prim::F64(n) => {
                body.extend_from_slice(&9u32.to_le_bytes());
                body.extend_from_slice(&n.to_le_bytes());
            }
        }
    }
    let mut out = Vec::with_capacity(16 + body.len());
    out.extend_from_slice(&496u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Parses an aux section, dropping the null keys.
pub fn parse_aux(b: &[u8]) -> Result<Vec<Prim>, Error> {
    let bad = || Error::Protocol("short aux".into());
    let mut r = Reader { b, at: 16 };
    if b.len() < 16 {
        return Err(bad());
    }
    let mut out = Vec::new();
    while r.at + 4 <= b.len() {
        match r.u32().ok_or_else(bad)? {
            0x0a => {}
            1 => {
                let n = r.u32().ok_or_else(bad)? as usize;
                let s = r.take(n).ok_or_else(bad)?;
                out.push(Prim::Str(String::from_utf8_lossy(s).into_owned()));
            }
            2 => {
                let n = r.u32().ok_or_else(bad)? as usize;
                out.push(Prim::Bytes(r.take(n).ok_or_else(bad)?.to_vec()));
            }
            3 => out.push(Prim::U32(r.u32().ok_or_else(bad)?)),
            4 | 6 => out.push(Prim::I64(r.u64().ok_or_else(bad)? as i64)),
            9 => out.push(Prim::F64(f64::from_bits(r.u64().ok_or_else(bad)?))),
            0xf0 => {
                // A primitive dictionary block; the flat entries carry the arguments.
                r.u32().ok_or_else(bad)?;
                let n = r.u64().ok_or_else(bad)? as usize;
                r.take(n).ok_or_else(bad)?;
            }
            t => return Err(Error::Protocol(format!("aux type {t:#x}"))),
        }
    }
    Ok(out)
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|s| u64::from_le_bytes(s.try_into().unwrap()))
    }
}

/// Reads one whole message, joining fragments.
pub async fn read_msg<R: AsyncRead + Unpin>(r: &mut R) -> Result<Msg, Error> {
    let mut body = Vec::new();
    let head = loop {
        let mut h = [0u8; 32];
        r.read_exact(&mut h).await?;
        let u32_at = |i: usize| u32::from_le_bytes(h[i..i + 4].try_into().unwrap());
        if u32_at(0) != MAGIC {
            return Err(Error::Protocol(format!("bad magic {:#x}", u32_at(0))));
        }
        let index = u16::from_le_bytes([h[8], h[9]]);
        let count = u16::from_le_bytes([h[10], h[11]]);
        if count > 1 && index == 0 {
            continue;
        }
        let len = u32_at(12) as usize;
        let start = body.len();
        body.resize(start + len, 0);
        r.read_exact(&mut body[start..]).await?;
        if index + 1 >= count {
            break h;
        }
    };
    let u32_at = |i: usize| u32::from_le_bytes(head[i..i + 4].try_into().unwrap());
    let conversation = u32_at(20);
    let wire = u32_at(24) as i32;
    if body.len() < 16 {
        return Err(Error::Protocol("short payload".into()));
    }
    let aux_len = u32::from_le_bytes(body[4..8].try_into().unwrap()) as usize;
    let total = u32::from_le_bytes(body[8..12].try_into().unwrap()) as usize;
    let aux = body
        .get(16..16 + aux_len)
        .ok_or_else(|| Error::Protocol("aux past the end".into()))?;
    let data = body
        .get(16 + aux_len..16 + total)
        .ok_or_else(|| Error::Protocol("data past the end".into()))?;
    Ok(Msg {
        id: u32_at(16),
        conversation,
        channel: if conversation.is_multiple_of(2) {
            -wire
        } else {
            wire
        },
        expects_reply: u32_at(28) == 1,
        kind: body[0],
        aux: if aux.is_empty() {
            Vec::new()
        } else {
            parse_aux(aux)?
        },
        data: if data.is_empty() {
            None
        } else {
            unarchive(data)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// A frame as the device sends it: a call it makes, or a reply.
    fn device_frame(
        id: u32,
        conversation: u32,
        expects_reply: bool,
        kind: u8,
        data: &Arch,
        aux: &[Prim],
    ) -> Vec<u8> {
        let aux = if aux.is_empty() {
            Vec::new()
        } else {
            aux_bytes(aux)
        };
        frame(
            id,
            conversation,
            0,
            expects_reply,
            kind,
            &aux,
            &archive(data),
        )
    }

    #[test]
    fn a_call_reads_back() {
        rt().block_on(async {
            let f = call_frame(7, 0, "deviceSetAuditTargetPid:", &[Arch::Int(0)], true);
            let m = read_msg(&mut &f[..]).await.unwrap();
            assert_eq!(
                (m.id, m.conversation, m.expects_reply, m.kind),
                (7, 0, true, CALL)
            );
            assert_eq!(m.selector(), Some("deviceSetAuditTargetPid:"));
            assert_eq!(m.args().collect::<Vec<_>>(), vec![Value::Integer(0.into())]);
        });
    }

    #[test]
    fn fragments_are_joined() {
        rt().block_on(async {
            let whole = call_frame(
                3,
                0,
                "deviceCapabilities",
                &[Arch::str("x".repeat(100))],
                true,
            );
            let payload = &whole[32..];
            let (a, b) = payload.split_at(40);
            let mut wire = Vec::new();
            let head = |index: u16, len: usize| {
                let mut h = whole[..32].to_vec();
                h[8..10].copy_from_slice(&index.to_le_bytes());
                h[10..12].copy_from_slice(&3u16.to_le_bytes());
                h[12..16].copy_from_slice(&(len as u32).to_le_bytes());
                h
            };
            wire.extend(head(0, payload.len()));
            wire.extend(head(1, a.len()));
            wire.extend(a);
            wire.extend(head(2, b.len()));
            wire.extend(b);
            let m = read_msg(&mut &wire[..]).await.unwrap();
            assert_eq!(m.selector(), Some("deviceCapabilities"));
            assert_eq!(m.args().next(), Some(Value::String("x".repeat(100))));
        });
    }

    /// The two things idevice 0.1.68 could not do for us: a device call that
    /// arrives while we wait is acknowledged and queued, and the reply is the
    /// one with our id at conversation index 1.
    #[test]
    fn device_calls_are_acked_and_queued_around_a_reply() {
        rt().block_on(async {
            let (host, mut device) = duplex(1 << 16);
            let mut dtx = Dtx::new(host, Duration::from_secs(5));
            let fake = async {
                let call = read_msg(&mut device).await.unwrap();
                assert_eq!(call.selector(), Some("deviceApiVersion"));
                let focus = Arch::mdict([("Value", Arch::str("el"))]);
                let push = device_frame(
                    90,
                    0,
                    true,
                    CALL,
                    &Arch::str("hostInspectorCurrentElementChanged:"),
                    &[Prim::Bytes(archive(&focus))],
                );
                device.write_all(&push).await.unwrap();
                // A reply to some other id, then ours.
                device
                    .write_all(&device_frame(
                        call.id + 5,
                        1,
                        false,
                        OBJECT,
                        &Arch::Int(1),
                        &[],
                    ))
                    .await
                    .unwrap();
                device
                    .write_all(&device_frame(
                        call.id,
                        1,
                        false,
                        OBJECT,
                        &Arch::Int(27),
                        &[],
                    ))
                    .await
                    .unwrap();
                let ack = read_msg(&mut device).await.unwrap();
                assert_eq!(
                    (ack.id, ack.conversation, ack.kind, ack.expects_reply),
                    (90, 1, 0, false)
                );
                device
            };
            let (got, _device) = tokio::join!(dtx.call(0, "deviceApiVersion", &[]), fake);
            assert_eq!(got.unwrap(), Some(Value::Integer(27.into())));
            let queued = dtx
                .wait_call(Duration::from_secs(1), |m| m.selector().is_some())
                .await
                .unwrap();
            assert_eq!(
                queued.selector(),
                Some("hostInspectorCurrentElementChanged:")
            );
            assert!(queued.args().next().is_some());
        });
    }

    #[test]
    fn an_error_reply_is_an_error() {
        rt().block_on(async {
            let (host, mut device) = duplex(1 << 16);
            let mut dtx = Dtx::new(host, Duration::from_secs(5));
            let fake = async {
                let call = read_msg(&mut device).await.unwrap();
                device
                    .write_all(&device_frame(
                        call.id,
                        1,
                        false,
                        ERROR,
                        &Arch::str("nope"),
                        &[],
                    ))
                    .await
                    .unwrap();
                device
            };
            let (got, _d) = tokio::join!(dtx.call(0, "x", &[]), fake);
            assert!(matches!(got, Err(Error::Remote(_))));
        });
    }
}

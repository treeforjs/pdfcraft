//! Bounded socket admission, framing and absolute deadlines for the opt-in control server.
use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub(super) const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const MESSAGE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const LINE_LIMIT: usize = 1024 * 1024;
pub(super) const REPLY_LIMIT: usize = 32 * 1024 * 1024;
const WORKERS: usize = 4;
const QUEUE: usize = 4;

/// Accepted sockets cannot create additional threads. Queued sockets retain their acceptance
/// time, so waiting in the admission queue never extends the authentication deadline.
pub(super) struct Pool(mpsc::SyncSender<(TcpStream, Instant)>);

impl Pool {
    pub(super) fn new(handler: impl Fn(TcpStream, Instant) + Send + Sync + 'static) -> io::Result<Self> {
        let (send, recv) = mpsc::sync_channel::<(TcpStream, Instant)>(QUEUE);
        let recv = Arc::new(Mutex::new(recv));
        let handler = Arc::new(handler);
        for _ in 0..WORKERS {
            let recv = recv.clone();
            let handler = handler.clone();
            std::thread::Builder::new().name("pdfcraft-control-conn".into()).spawn(move || {
                loop {
                    let next = match recv.lock() {
                        Ok(recv) => recv.recv(),
                        Err(_) => return,
                    };
                    let Ok((stream, accepted)) = next else { return };
                    // A failed connection must not permanently reduce the bounded worker pool.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(stream, accepted)));
                }
            })?;
        }
        Ok(Self(send))
    }

    pub(super) fn admit(&self, stream: TcpStream) -> bool {
        self.0.try_send((stream, Instant::now())).is_ok()
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "control deadline"))
}

/// Includes the newline in the byte cap; accepts a final unterminated line on EOF.
pub(super) fn line(read: &mut BufReader<TcpStream>, deadline: Instant) -> io::Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    loop {
        read.get_ref().set_read_timeout(Some(remaining(deadline)?))?;
        let bytes = read.fill_buf()?;
        if bytes.is_empty() {
            return Ok((!out.is_empty()).then_some(out));
        }
        let end = bytes.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let n = end.unwrap_or(bytes.len());
        if n > LINE_LIMIT.saturating_sub(out.len()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "control request exceeds 1 MiB"));
        }
        out.try_reserve(n).map_err(io::Error::other)?;
        out.extend_from_slice(&bytes[..n]);
        read.consume(n);
        if end.is_some() {
            return Ok(Some(out));
        }
    }
}

/// `serde_json::to_writer` uses this sink, bounding allocation before each append.
pub(super) struct ReplyBuffer(pub(super) Vec<u8>);

impl Write for ReplyBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > REPLY_LIMIT.saturating_sub(self.0.len()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "control reply exceeds 32 MiB"));
        }
        self.0.try_reserve(bytes.len()).map_err(io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn reply(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    for mut pending in [bytes, b"\n".as_slice()] {
        while !pending.is_empty() {
            stream.set_write_timeout(Some(remaining(deadline)?))?;
            let n = stream.write(pending)?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            pending = &pending[n..];
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        (writer, listener.accept().unwrap().0)
    }

    #[test]
    fn exact_line_limit_and_one_byte_over() {
        for n in [LINE_LIMIT, LINE_LIMIT + 1] {
            let (mut writer, read) = pair();
            let task = std::thread::spawn(move || {
                let mut bytes = vec![b'x'; n];
                *bytes.last_mut().unwrap() = b'\n';
                writer.write_all(&bytes).unwrap();
            });
            let got = line(&mut BufReader::new(read), Instant::now() + MESSAGE_TIMEOUT);
            if n == LINE_LIMIT {
                assert_eq!(got.unwrap().unwrap().len(), n);
            } else {
                assert_eq!(got.unwrap_err().kind(), io::ErrorKind::InvalidData);
            }
            task.join().unwrap();
        }
    }

    #[test]
    fn trickle_does_not_renew_authentication_deadline() {
        let (mut writer, read) = pair();
        let task = std::thread::spawn(move || {
            for _ in 0..10 {
                if writer.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let start = Instant::now();
        let got = line(&mut BufReader::new(read), start + Duration::from_millis(70));
        assert!(matches!(got.unwrap_err().kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock));
        assert!(start.elapsed() < Duration::from_secs(1));
        task.join().unwrap();
    }

    #[test]
    fn reply_refuses_excess_before_copying_it() {
        let mut sink = ReplyBuffer(vec![0; REPLY_LIMIT - 1]);
        sink.write_all(b"x").unwrap();
        assert!(sink.write_all(b"y").is_err());
        assert_eq!(sink.0.len(), REPLY_LIMIT);
        assert_eq!(sink.0.last(), Some(&b'x'));
    }

    #[test]
    fn unauthenticated_connections_use_a_fixed_pool() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (release, wait) = mpsc::channel::<()>();
        let wait = Arc::new(Mutex::new(wait));
        let pool = Pool::new({
            let active = active.clone();
            let peak = peak.clone();
            move |_, _| {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(count, Ordering::SeqCst);
                let _ = wait.lock().unwrap().recv();
                active.fetch_sub(1, Ordering::SeqCst);
            }
        })
        .unwrap();
        let mut clients = Vec::new();
        let mut rejected = 0;
        for _ in 0..40 {
            let (client, server) = pair();
            clients.push(client);
            if !pool.admit(server) {
                rejected += 1;
            }
        }
        assert!(rejected >= 40 - WORKERS - QUEUE);
        drop(release);
        drop(pool);
        assert!(peak.load(Ordering::SeqCst) <= WORKERS);
    }
}

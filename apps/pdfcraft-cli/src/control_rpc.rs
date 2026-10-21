//! One bounded control request. Missing or malformed replies never trigger an edit retry.
use std::io::{self, BufRead, BufReader, Write as _};
use std::net::TcpStream;
use std::time::{Duration, Instant};

pub(crate) fn request(
    read: &mut BufReader<TcpStream>,
    write: &mut TcpStream,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let uncertain = |error: String| {
        if method == "auth" {
            error
        } else {
            format!("{error}. The request outcome is unknown; it was not retried. Inspect the document before retrying an edit.")
        }
    };
    let deadline = Instant::now() + Duration::from_secs(40);
    writeln!(write, "{}", serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
        .map_err(|e| uncertain(e.to_string()))?;
    let line = read_reply(read, deadline).map_err(|e| uncertain(e.to_string()))?;
    let reply: serde_json::Value = serde_json::from_str(&line).map_err(|e| uncertain(e.to_string()))?;
    if reply.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
        || reply.get("id").and_then(serde_json::Value::as_u64) != Some(id)
        || (reply.get("result").is_some() == reply.get("error").is_some())
    {
        return Err(uncertain("the app returned a mismatched or malformed reply".into()));
    }
    match reply.get("error") {
        Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
        None => Ok(reply["result"].clone()),
    }
}

fn read_reply(read: &mut BufReader<TcpStream>, deadline: Instant) -> io::Result<String> {
    const LIMIT: usize = 32 * 1024 * 1024 + 1;
    let mut out = Vec::new();
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "control reply deadline"))?;
        read.get_ref().set_read_timeout(Some(left))?;
        let bytes = read.fill_buf()?;
        if bytes.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the app closed the connection before its reply"));
        }
        let newline = bytes.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let n = newline.unwrap_or(bytes.len());
        if n > LIMIT.saturating_sub(out.len()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the app reply exceeded its byte limit"));
        }
        out.try_reserve(n).map_err(io::Error::other)?;
        out.extend_from_slice(&bytes[..n]);
        read.consume(n);
        if newline.is_some() {
            return String::from_utf8(out).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::TcpListener;

    fn exchange(reply: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client.set_read_timeout(Some(std::time::Duration::from_secs(1))).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap()["method"], "ui.command");
            if let Some(reply) = reply {
                writeln!(stream, "{reply}").unwrap();
            }
            // One operation only; dropping this connection must not result in a retry.
        });
        let got = request(&mut BufReader::new(client.try_clone().unwrap()), &mut client, 2, "ui.command", json!({"id":"edit.undo"}));
        server.join().unwrap();
        got
    }

    #[test]
    fn trickled_reply_bytes_do_not_renew_the_absolute_deadline() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for _ in 0..10 {
                if stream.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let start = Instant::now();
        let error = read_reply(&mut BufReader::new(client), start + Duration::from_millis(70)).unwrap_err();
        assert!(matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock));
        assert!(start.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }

    #[test]
    fn lost_reply_has_an_unknown_outcome_and_is_not_retried() {
        let error = exchange(None).unwrap_err();
        assert!(error.contains("outcome is unknown") && error.contains("not retried"), "{error}");
    }

    #[test]
    fn mismatched_ids_cannot_be_mistaken_for_success() {
        let error = exchange(Some(json!({"jsonrpc":"2.0","id":99,"result":{"ok":true}}))).unwrap_err();
        assert!(error.contains("mismatched") && error.contains("outcome is unknown"), "{error}");
    }

    #[test]
    fn successful_results_preserve_the_existing_protocol() {
        assert_eq!(exchange(Some(json!({"jsonrpc":"2.0","id":2,"result":{"ok":true}}))).unwrap(), json!({"ok":true}));
    }
}

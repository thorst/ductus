//! `ductus sink --smtp` and `--http`: a mail server and a web server that accept everything and
//! print it, to see what alerts send (and for the demos) without a real one. Each mail or request
//! is printed as one line (`mail 3 to a@x.org: <subject>`, `POST /hook: <body>`) followed by the
//! mail's text, and with `--dir` also written to a file of its own. Not for production: no TLS,
//! any password is accepted.

use anyhow::Result;
use base64::Engine as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Messages and requests above this are refused (a sink, not a mail server).
const MAX_SIZE: usize = 10 * 1024 * 1024;

pub async fn smtp(port: u16, dir: Option<PathBuf>) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    println!("SMTP sink listening on {port}; Ctrl-C to stop");
    let count = Arc::new(AtomicU64::new(0));
    serve(listener, move |socket| {
        let (count, dir) = (count.clone(), dir.clone());
        async move {
            if let Err(e) = smtp_session(socket, &count, dir.as_deref()).await {
                eprintln!("smtp session: {e:#}");
            }
        }
    })
    .await
}

async fn smtp_session(socket: tokio::net::TcpStream, count: &AtomicU64, dir: Option<&std::path::Path>) -> Result<()> {
    let (read, mut write) = socket.into_split();
    let mut read = BufReader::new(read);
    write.write_all(b"220 ductus-sink ESMTP\r\n").await?;
    let (mut from, mut to) = (String::new(), Vec::<String>::new());
    let mut line = String::new();
    loop {
        line.clear();
        if read.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let command = line.trim_end();
        let verb = command.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
        let reply: &[u8] = match verb.as_str() {
            "EHLO" => b"250-ductus-sink\r\n250-8BITMIME\r\n250-SMTPUTF8\r\n250 AUTH PLAIN LOGIN\r\n",
            "HELO" => b"250 ductus-sink\r\n",
            "AUTH" => {
                // Any credentials do: AUTH LOGIN asks for the name and password first.
                if command.to_ascii_uppercase().starts_with("AUTH LOGIN") && command.split_whitespace().count() == 2 {
                    for prompt in [&b"334 VXNlcm5hbWU6\r\n"[..], b"334 UGFzc3dvcmQ6\r\n"] {
                        write.write_all(prompt).await?;
                        line.clear();
                        read.read_line(&mut line).await?;
                    }
                }
                b"235 2.7.0 accepted\r\n"
            }
            "MAIL" => {
                from = address(command);
                to.clear();
                b"250 2.1.0 ok\r\n"
            }
            "RCPT" => {
                to.push(address(command));
                b"250 2.1.5 ok\r\n"
            }
            "DATA" => {
                write.write_all(b"354 end with <CRLF>.<CRLF>\r\n").await?;
                let mut data = String::new();
                loop {
                    line.clear();
                    if read.read_line(&mut line).await? == 0 {
                        return Ok(());
                    }
                    if line.trim_end_matches(['\r', '\n']) == "." {
                        break;
                    }
                    // Dot-stuffing: a line starting with a dot was sent with one more.
                    data.push_str(line.strip_prefix('.').unwrap_or(&line));
                    if data.len() > MAX_SIZE {
                        write.write_all(b"552 too big\r\n").await?;
                        return Ok(());
                    }
                }
                let n = count.fetch_add(1, Ordering::Relaxed) + 1;
                let mail = Mail::parse(&data);
                println!("mail {n} from {from} to {}: {}", to.join(", "), mail.subject);
                for body_line in mail.body.lines() {
                    println!("  {body_line}");
                }
                if let Some(dir) = dir {
                    std::fs::create_dir_all(dir)?;
                    std::fs::write(dir.join(format!("mail-{n}.eml")), &data)?;
                }
                b"250 2.0.0 queued\r\n"
            }
            "RSET" => {
                (from, to) = (String::new(), Vec::new());
                b"250 ok\r\n"
            }
            "NOOP" => b"250 ok\r\n",
            "QUIT" => {
                write.write_all(b"221 bye\r\n").await?;
                return Ok(());
            }
            _ => b"502 not implemented\r\n",
        };
        write.write_all(reply).await?;
    }
}

/// `MAIL FROM:<a@x.org> SIZE=..` → `a@x.org`.
fn address(command: &str) -> String {
    match (command.find('<'), command.find('>')) {
        (Some(a), Some(b)) if a < b => command[a + 1..b].to_string(),
        _ => command.split_once(':').map(|(_, a)| a.trim().to_string()).unwrap_or_default(),
    }
}

/// What a person would read of a mail: its subject and its text.
struct Mail {
    subject: String,
    body: String,
}

impl Mail {
    fn parse(data: &str) -> Self {
        let (head, body) = data.split_once("\r\n\r\n").or_else(|| data.split_once("\n\n")).unwrap_or((data, ""));
        // Unfold continued header lines first.
        let head = head.replace("\r\n ", " ").replace("\r\n\t", " ").replace("\n ", " ");
        let header = |name: &str| {
            head.lines().find_map(|l| {
                let (n, v) = l.split_once(':')?;
                n.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
            })
        };
        let subject = header("Subject").map(|s| decode_words(&s)).unwrap_or_default();
        let body = match header("Content-Transfer-Encoding").map(|e| e.to_ascii_lowercase()).as_deref() {
            Some("base64") => {
                let compact: String = body.split_whitespace().collect();
                base64::engine::general_purpose::STANDARD
                    .decode(compact)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_else(|_| body.to_string())
            }
            Some("quoted-printable") => quoted_printable(body, false),
            _ => body.to_string(),
        };
        Self { subject, body: body.replace("\r\n", "\n") }
    }
}

/// RFC 2047 encoded words (`=?utf-8?b?...?=`, `=?utf-8?q?...?=`), as a mail program shows them.
fn decode_words(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("=?") {
        let Some(end) = rest[start + 2..].find("?=").map(|e| start + 2 + e) else { break };
        let parts: Vec<&str> = rest[start + 2..end].splitn(3, '?').collect();
        let decoded = match parts.as_slice() {
            [_, enc, data] if enc.eq_ignore_ascii_case("b") => base64::engine::general_purpose::STANDARD
                .decode(data)
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned()),
            [_, enc, data] if enc.eq_ignore_ascii_case("q") => Some(quoted_printable(data, true)),
            _ => None,
        };
        let Some(decoded) = decoded else { break };
        // Whitespace between two encoded words isn't part of the text.
        let before = &rest[..start];
        if !(out.ends_with('\u{0}') && before.trim().is_empty()) {
            out.push_str(before);
        }
        out.push_str(&decoded);
        out.push('\u{0}');
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    out.replace('\u{0}', "")
}

/// Quoted-printable (`=E2=86=92`, soft line breaks `=\n`); in a header word `_` is a space.
fn quoted_printable(text: &str, word: bool) -> String {
    let text = text.replace("=\r\n", "").replace("=\n", "");
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'=' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'='),
                }
            }
            b'_' if word => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub async fn http(port: u16, dir: Option<PathBuf>) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    println!("HTTP sink listening on {port}; Ctrl-C to stop");
    let count = Arc::new(AtomicU64::new(0));
    serve(listener, move |socket| {
        let (count, dir) = (count.clone(), dir.clone());
        async move {
            if let Err(e) = http_request(socket, &count, dir.as_deref()).await {
                eprintln!("http request: {e:#}");
            }
        }
    })
    .await
}

async fn http_request(socket: tokio::net::TcpStream, count: &AtomicU64, dir: Option<&std::path::Path>) -> Result<()> {
    let (read, mut write) = socket.into_split();
    let mut read = BufReader::new(read);
    loop {
        let mut request_line = String::new();
        if read.read_line(&mut request_line).await? == 0 {
            return Ok(());
        }
        let mut length = 0usize;
        let mut close = false;
        loop {
            let mut header = String::new();
            if read.read_line(&mut header).await? == 0 || header.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                match name.trim().to_ascii_lowercase().as_str() {
                    "content-length" => length = value.trim().parse().unwrap_or(0),
                    "connection" => close = value.trim().eq_ignore_ascii_case("close"),
                    _ => {}
                }
            }
        }
        if length > MAX_SIZE {
            write
                .write_all(b"HTTP/1.1 413 Payload Too Large\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await?;
            return Ok(());
        }
        let mut body = vec![0; length];
        read.read_exact(&mut body).await?;
        let body = String::from_utf8_lossy(&body);
        let n = count.fetch_add(1, Ordering::Relaxed) + 1;
        let mut words = request_line.split_whitespace();
        let (method, path) = (words.next().unwrap_or("?"), words.next().unwrap_or("?"));
        println!("{method} {path}: {}", body.replace('\n', " "));
        if let Some(dir) = dir {
            std::fs::create_dir_all(dir)?;
            std::fs::write(dir.join(format!("request-{n}.txt")), format!("{method} {path}\n\n{body}"))?;
        }
        write.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await?;
        if close {
            return Ok(());
        }
    }
}

/// Accept connections until Ctrl-C, each handled on its own task.
async fn serve<F, Fut>(listener: TcpListener, handle: F) -> Result<()>
where
    F: Fn(tokio::net::TcpStream) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                tokio::spawn(handle(socket));
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_lettre_writes() {
        let data = "From: d@example.org\r\nSubject: =?utf-8?b?W0R1Y3R1cyBwcm9kXSBDUklUSUNBTCBhZHQg4oaSIGxhYg==?=\r\n =?utf-8?q?_is_stalled?=\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nadt =E2=86=92 lab is stalled since 12:00.=\r\n More.\r\n";
        let mail = Mail::parse(data);
        assert_eq!(mail.subject, "[Ductus prod] CRITICAL adt → lab is stalled");
        assert_eq!(mail.body, "adt → lab is stalled since 12:00. More.\n");
        let b64 = "Subject: plain\r\nContent-Transfer-Encoding: base64\r\n\r\nYWR0IOKGkiBsYWI=\r\n";
        assert_eq!(Mail::parse(b64).body, "adt → lab");
        assert_eq!(address("MAIL FROM:<a@x.org> SIZE=10"), "a@x.org");
        assert_eq!(quoted_printable("a=3Db=", false), "a=b=");
    }
}

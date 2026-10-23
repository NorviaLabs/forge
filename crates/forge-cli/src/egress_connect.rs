//! `forge __egress-connect <proxy-host:port> <target-host> <target-port>`.
//!
//! A hidden helper that carries one TCP stream through the egress gateway, so
//! SSH can leave through it without `nc` or `socat` on the machine. Git uses
//! it as
//!
//! ```text
//! ssh -o ProxyCommand="<forge exe> __egress-connect 127.0.0.1:PORT %h %p"
//! ```
//!
//! It sends `CONNECT target:port`, requires a `200`, then copies stdin to the
//! socket and the socket to stdout until either side closes. A refusal exits
//! non-zero with the gateway's reason on stderr, which is what SSH (and so
//! git) reports. Bytes past the handshake are never interpreted: the gateway
//! stays a pass-through, and SSH's own encryption runs end to end.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};

/// The subcommand name, matched before argument parsing so it never appears
/// in `--help`.
pub const SUBCOMMAND: &str = "__egress-connect";

const USAGE: &str = "usage: forge __egress-connect <proxy-host:port> <target-host> <target-port>";

/// Run the helper and return the process exit code.
pub fn run(args: &[String]) -> i32 {
    let [proxy, host, port] = args else {
        eprintln!("{USAGE}");
        return 2;
    };
    let Ok(port) = port.parse::<u16>() else {
        eprintln!("forge __egress-connect: invalid target port {port:?}");
        return 2;
    };
    match connect(proxy, host, port) {
        Ok(()) => 0,
        Err(Failure::Refused(message)) => {
            eprintln!("forge __egress-connect: {message}");
            1
        }
        Err(Failure::Io(error)) => {
            eprintln!("forge __egress-connect: {error}");
            1
        }
    }
}

enum Failure {
    /// The gateway answered, and the answer was not a tunnel.
    Refused(String),
    Io(io::Error),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn connect(proxy: &str, host: &str, port: u16) -> Result<(), Failure> {
    // A host that would break the request line is not a host.
    if host.is_empty()
        || host
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(Failure::Refused(format!("invalid target host {host:?}")));
    }
    let stream = TcpStream::connect(proxy)?;
    let target = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    (&stream)
        .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())?;

    // Buffered for the handshake only; anything read past the blank line is
    // already tunnel data and is drained from this same reader below.
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut status = String::new();
    reader.read_line(&mut status)?;
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let code = status.split_whitespace().nth(1).unwrap_or_default();
    if code != "200" {
        let mut body = vec![0u8; content_length.min(4096)];
        let read = reader.read(&mut body).unwrap_or(0);
        let body = String::from_utf8_lossy(&body[..read]);
        let status = status.trim();
        let status = if status.is_empty() {
            "the gateway closed the connection without answering"
        } else {
            status
        };
        let message = match body.trim() {
            "" => status.to_string(),
            body => format!("{status}: {body}"),
        };
        return Err(Failure::Refused(message));
    }

    // stdin → socket on a thread; socket → stdout here. The session is over
    // when the far side closes, whether or not stdin has.
    let mut upstream = stream.try_clone()?;
    std::thread::spawn(move || {
        let _ = io::copy(&mut io::stdin().lock(), &mut upstream);
        let _ = upstream.shutdown(Shutdown::Write);
    });
    let mut stdout = io::stdout().lock();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let read = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        stdout.write_all(&buf[..read])?;
        stdout.flush()?;
    }
    Ok(())
}

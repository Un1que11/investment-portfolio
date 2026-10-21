//! `portfolio-web healthcheck`: asks the running server's `/health` and exits 0 only on `200 ok`.
//! The Docker image has no curl, so compose's healthcheck (BACKLOG P36) runs this instead. Plain
//! blocking HTTP/1.0 over `std::net`: one request, no extra dependencies.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// How long the probe waits to connect, and then for the answer.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// A server listening on all interfaces (`0.0.0.0` / `::`) is asked on the loopback address of
/// the same family; any other address is asked as it is.
pub fn probe_addr(listen: SocketAddr) -> SocketAddr {
    match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), listen.port())
        }
        _ => listen,
    }
}

/// Checks a raw HTTP response: status 200 and the body `ok`.
pub fn check_response(response: &str) -> Result<(), String> {
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "no complete HTTP response".to_owned())?;
    let status = head.lines().next().unwrap_or_default();
    let code = status.split_whitespace().nth(1);
    if !status.starts_with("HTTP/1.") || code != Some("200") {
        return Err(format!("unexpected status: {status}"));
    }
    if body.trim() != "ok" {
        return Err(format!("unexpected body: {:?}", body.trim()));
    }
    Ok(())
}

/// Sends `GET /health` to the server listening on `listen`.
pub fn check(listen: SocketAddr, timeout: Duration) -> Result<(), String> {
    let addr = probe_addr(listen);
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| format!("set timeout: {e}"))?;
    // HTTP/1.0 with Connection: close, so the server closes the stream after one answer and
    // reading to the end needs no Content-Length handling.
    stream
        .write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(|e| format!("send to {addr}: {e}"))?;
    let mut response = Vec::new();
    // `/health` answers two bytes; anything far larger is not this server.
    stream
        .take(64 * 1024)
        .read_to_end(&mut response)
        .map_err(|e| format!("read from {addr}: {e}"))?;
    check_response(&String::from_utf8_lossy(&response))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn unspecified_listen_address_is_probed_on_loopback() {
        assert_eq!(probe_addr(addr("0.0.0.0:3000")), addr("127.0.0.1:3000"));
        assert_eq!(probe_addr(addr("[::]:3000")), addr("[::1]:3000"));
        assert_eq!(probe_addr(addr("127.0.0.1:3001")), addr("127.0.0.1:3001"));
        assert_eq!(
            probe_addr(addr("192.168.1.5:3000")),
            addr("192.168.1.5:3000")
        );
    }

    #[test]
    fn accepts_200_ok_only() {
        let ok = "HTTP/1.0 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\n\r\nok";
        assert_eq!(check_response(ok), Ok(()));
        assert_eq!(check_response("HTTP/1.1 200 OK\r\n\r\nok\n"), Ok(()));

        assert_eq!(
            check_response("HTTP/1.0 500 Internal Server Error\r\n\r\nok"),
            Err("unexpected status: HTTP/1.0 500 Internal Server Error".to_owned())
        );
        // Another server on the port: 200, but not our answer.
        assert_eq!(
            check_response("HTTP/1.1 200 OK\r\n\r\n<html>"),
            Err("unexpected body: \"<html>\"".to_owned())
        );
        // "200" somewhere else on the status line is not a 200 status.
        assert!(check_response("HTTP/1.1 404 200\r\n\r\nok").is_err());
        assert!(check_response("SSH-2.0-OpenSSH\r\n\r\nok").is_err());
        assert_eq!(
            check_response("HTTP/1.0 200 OK\r\n"),
            Err("no complete HTTP response".to_owned())
        );
        assert!(check_response("").is_err());
    }
}

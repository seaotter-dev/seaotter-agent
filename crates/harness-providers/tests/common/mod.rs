#![allow(dead_code)]

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub struct Captured {
    pub head: String,
    pub body: Value,
}

impl Captured {
    pub fn request_line(&self) -> &str {
        self.head.lines().next().unwrap_or_default()
    }

    pub fn has_header(&self, line: &str) -> bool {
        self.head.lines().any(|l| l.eq_ignore_ascii_case(line))
    }
}

/// Serves one HTTP response on a local port and returns what it received.
pub async fn serve_once(
    status: &'static str,
    body: &'static str,
) -> (String, JoinHandle<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed before headers");
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8(buf[..header_end].to_vec()).unwrap();
        let len: usize = head
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        while buf.len() < header_end + len {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed before body");
            buf.extend_from_slice(&chunk[..n]);
        }
        let received = serde_json::from_slice(&buf[header_end..header_end + len]).unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len(),
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
        Captured {
            head,
            body: received,
        }
    });
    (base_url, handle)
}

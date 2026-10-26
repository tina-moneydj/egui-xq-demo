//! 伺服器下發版面的範例伺服器（只用標準函式庫）：每個請求都重新讀 JSON 檔回傳，
//! 所以改檔存檔，app 下一次輪詢（約 1 秒）就換版面，不用重編譯也不用重開。
//!
//! ```bash
//! cargo run --release --example layout-server -- layouts/demo.json --port 47700
//! XQ_LAYOUT_URL=http://127.0.0.1:47700/layout.json ./target/release/egui-xq-demo
//! ```
//! 正式環境換成公司的設定服務即可（只要回 JSON）；格式見 docs/server-layout.md。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

fn main() {
    let mut file = "layouts/demo.json".to_string();
    let mut port = 47700u16;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|v| v.parse().ok()).unwrap_or(port),
            _ => file = a,
        }
    }
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[layout-server] 綁不到 127.0.0.1:{port}：{e}");
            std::process::exit(1);
        }
    };
    eprintln!("[layout-server] http://127.0.0.1:{port}/layout.json ← {file}（每次請求重新讀檔）");
    for s in listener.incoming().flatten() {
        if let Err(e) = serve(s, &file) {
            eprintln!("[layout-server] {e}");
        }
    }
}

fn serve(mut s: TcpStream, file: &str) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    // 只看請求行；標頭讀到空行為止（最多 8 KB）
    let mut buf = [0u8; 8192];
    let mut n = 0;
    while n < buf.len() {
        let k = s.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let line = req.lines().next().unwrap_or("");
    let (status, body) = if !line.starts_with("GET ") {
        ("405 Method Not Allowed", b"only GET".to_vec())
    } else {
        match std::fs::read(file) {
            Ok(b) => ("200 OK", b),
            Err(e) => ("500 Internal Server Error", format!("讀不到 {file}：{e}").into_bytes()),
        }
    };
    write!(s, "HTTP/1.0 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n", body.len())?;
    s.write_all(&body)
}

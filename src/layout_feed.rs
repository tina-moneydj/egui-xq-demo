//! 伺服器下發版面：背景執行緒抓版面 JSON（`XQ_LAYOUT_URL`＝http://… 或本機檔案路徑），內容有變才解析，
//! 解析也在背景做；UI 每幀只 `take()` 一次（O(1)，鎖裡只做 take）。
//!
//! 樣品用輪詢（預設每秒，`XQ_LAYOUT_POLL_MS`）。桌面只支援 http（不加 TLS 依賴）；網頁版走瀏覽器 fetch，http／https 都可以。正式版應該由伺服器主動推
//! （例如行情連線上多一種訊息），這裡先不改 xq-feed 協議。

use std::sync::Arc;

use parking_lot::Mutex;
use xq_layout::{Platform, Screen};

/// 背景抓到的最新結果：Ok＝新版面、Err＝抓取或解析失敗（UI 沿用上一份好的版面，只顯示錯誤）
pub type Update = Result<Screen, String>;

pub struct LayoutFeed {
    slot: Arc<Mutex<Option<Update>>>,
    /// 顯示用（狀態列）
    pub source: String,
}

impl LayoutFeed {
    /// UI 每幀呼叫一次；沒有新東西回 None
    pub fn take(&self) -> Option<Update> {
        self.slot.lock().take()
    }
}

/// 上一次的原始內容／錯誤：一樣就不解析、不叫醒 UI
#[derive(Default)]
struct Dedup {
    last: Option<Vec<u8>>,
    last_err: Option<String>,
}

impl Dedup {
    fn next(&mut self, got: Result<Vec<u8>, String>, platform: Platform) -> Option<Update> {
        match got {
            Ok(body) if self.last.as_deref() == Some(body.as_slice()) => None,
            Ok(body) => {
                let u = xq_layout::parse_for(&body, platform);
                self.last = Some(body);
                self.last_err = None;
                Some(u)
            }
            Err(e) if self.last_err.as_deref() == Some(e.as_str()) => None,
            Err(e) => {
                self.last_err = Some(e.clone());
                // 伺服器恢復後就算內容沒變也要重新套用（清掉狀態列的錯誤）
                self.last = None;
                Some(Err(e))
            }
        }
    }
}

fn poll_ms() -> u64 {
    crate::plat::var("XQ_LAYOUT_POLL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1000u64).clamp(100, 60_000)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn start(source: String, ctx: egui::Context) -> LayoutFeed {
    let slot: Arc<Mutex<Option<Update>>> = Arc::new(Mutex::new(None));
    let out = slot.clone();
    let src = source.clone();
    let poll = poll_ms();
    let spawned = std::thread::Builder::new().name("xq-layout".into()).spawn(move || {
        let mut dd = Dedup::default();
        loop {
            if let Some(u) = dd.next(fetch(&src), Platform::Desktop) {
                *out.lock() = Some(u);
                ctx.request_repaint();
            }
            std::thread::sleep(std::time::Duration::from_millis(poll));
        }
    });
    if let Err(e) = spawned {
        *slot.lock() = Some(Err(format!("版面執行緒開不起來：{e}")));
    }
    LayoutFeed { slot, source }
}

/// 網頁版：瀏覽器 fetch（非同步，不卡畫面）＋setTimeout 輪詢；網址用 `?XQ_LAYOUT_URL=…`。
/// 瀏覽器只有一條執行緒，內容有變時的解析在主執行緒做（≤ 256 KB、只在改版時一次）。跨網域要伺服器回 CORS 標頭。
#[cfg(target_arch = "wasm32")]
pub fn start(source: String, ctx: egui::Context) -> LayoutFeed {
    let slot: Arc<Mutex<Option<Update>>> = Arc::new(Mutex::new(None));
    let out = slot.clone();
    let src = source.clone();
    let poll = poll_ms();
    wasm_bindgen_futures::spawn_local(async move {
        let mut dd = Dedup::default();
        loop {
            if let Some(u) = dd.next(fetch_web(&src).await, Platform::Web) {
                *out.lock() = Some(u);
                ctx.request_repaint();
            }
            sleep_web(poll).await;
        }
    });
    LayoutFeed { slot, source }
}

#[cfg(target_arch = "wasm32")]
async fn fetch_web(url: &str) -> Result<Vec<u8>, String> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    let w = web_sys::window().ok_or("沒有 window")?;
    let resp = JsFuture::from(w.fetch_with_str(url)).await.map_err(|e| format!("連不上版面伺服器：{e:?}"))?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| "版面回應型別不對".to_string())?;
    if !resp.ok() {
        return Err(format!("版面伺服器回 {}", resp.status()));
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(|e| format!("讀取回應失敗：{e:?}"))?)
        .await
        .map_err(|e| format!("讀取回應失敗：{e:?}"))?;
    let arr = js_sys::Uint8Array::new(&buf);
    if arr.length() as usize > xq_layout::MAX_BYTES {
        return Err(format!("版面 JSON 太大（上限 {} KB）", xq_layout::MAX_BYTES / 1024));
    }
    Ok(arr.to_vec())
}

#[cfg(target_arch = "wasm32")]
async fn sleep_web(ms: u64) {
    let p = js_sys::Promise::new(&mut |res, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&res, ms as i32);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch(src: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    if let Some(rest) = src.strip_prefix("http://") {
        return http_get(rest);
    }
    if src.starts_with("https://") {
        return Err("樣品只支援 http://（不加 TLS 依賴）".into());
    }
    let path = src.strip_prefix("file://").unwrap_or(src);
    let f = std::fs::File::open(path).map_err(|e| format!("讀不到 {path}：{e}"))?;
    let mut buf = Vec::new();
    f.take(xq_layout::MAX_BYTES as u64 + 1).read_to_end(&mut buf).map_err(|e| format!("讀 {path} 失敗：{e}"))?;
    Ok(buf)
}

/// 最小的 HTTP/1.0 GET（Connection: close，不處理 chunked／轉址）；`rest` = host[:port]/path
#[cfg(not(target_arch = "wasm32"))]
fn http_get(rest: &str) -> Result<Vec<u8>, String> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr_s = if hostport.contains(':') { hostport.to_string() } else { format!("{hostport}:80") };
    let addr = addr_s
        .to_socket_addrs()
        .map_err(|e| format!("解析 {hostport} 失敗：{e}"))?
        .next()
        .ok_or_else(|| format!("解析 {hostport} 沒有位址"))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| format!("連不上版面伺服器 {hostport}：{e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
    write!(s, "GET {path} HTTP/1.0\r\nHost: {hostport}\r\nConnection: close\r\n\r\n").map_err(|e| format!("送出請求失敗：{e}"))?;
    let mut buf = Vec::new();
    // 標頭最多預留 16 KB；超過 MAX_BYTES 的 body 交給 parse 回錯
    s.take(xq_layout::MAX_BYTES as u64 + 16 * 1024 + 1).read_to_end(&mut buf).map_err(|e| format!("讀取回應失敗：{e}"))?;
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or("回應沒有 HTTP 標頭")?;
    let status = buf[..split].split(|b| *b == b'\n').next().map(String::from_utf8_lossy).unwrap_or_default().trim().to_string();
    if status.split_whitespace().nth(1) != Some("200") {
        return Err(format!("版面伺服器回 {status}"));
    }
    Ok(buf.split_off(split + 4))
}

/// 選單的網址（第 1 層：低頻頁面交給網頁）：用系統瀏覽器開，背景執行緒呼叫，不卡 UI
#[cfg(not(target_arch = "wasm32"))]
pub fn open_url(url: &str) {
    let url = url.to_string();
    let _ = std::thread::Builder::new().name("xq-open-url".into()).spawn(move || {
        #[cfg(target_os = "windows")]
        let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", &url]).status();
        #[cfg(target_os = "macos")]
        let r = std::process::Command::new("open").arg(&url).status();
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let r = std::process::Command::new("xdg-open").arg(&url).status();
        if let Err(e) = r {
            eprintln!("[layout] 開網址失敗 {url}: {e}");
        }
    });
}

#[cfg(target_arch = "wasm32")]
pub fn open_url(url: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.open_with_url_and_target(url, "_blank");
    }
}

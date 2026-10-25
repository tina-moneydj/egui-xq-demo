// 一頁嵌 4 個 egui 元件：無頭 Chromium（Playwright）驗證腳本。
// 用法：node verify.js <dist/web-multi 資料夾> <輸出資料夾>
// 會自己開一個靜態檔案伺服器；輸出 results.json、console-*.log、截圖。
// 這台沒有 GPU：Chromium 用 SwiftShader（軟體）跑 WebGL2，數字只供參考。
const http = require("http");
const fs = require("fs");
const path = require("path");
const zlib = require("zlib");
const { chromium } = require("playwright");

const DIST = path.resolve(process.argv[2] || "../../dist/web-multi");
const OUT = path.resolve(process.argv[3] || "out");
fs.mkdirSync(OUT, { recursive: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const TYPES = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".wasm": "application/wasm" };
// 伺服器可選 gzip（GZIP=1 時對 Accept-Encoding: gzip 的請求壓縮），量「頁面總下載量」用
function serve(gzip) {
  return new Promise((resolve) => {
    const srv = http.createServer((req, res) => {
      const u = new URL(req.url, "http://x");
      const f = path.join(DIST, decodeURIComponent(u.pathname === "/" ? "/index.html" : u.pathname));
      if (!f.startsWith(DIST) || !fs.existsSync(f)) {
        res.writeHead(404).end();
        return;
      }
      let body = fs.readFileSync(f);
      const h = { "Content-Type": TYPES[path.extname(f)] || "application/octet-stream", "Cache-Control": "no-store" };
      if (gzip && /gzip/.test(req.headers["accept-encoding"] || "")) {
        body = zlib.gzipSync(body, { level: 9 });
        h["Content-Encoding"] = "gzip";
      }
      res.writeHead(200, h).end(body);
    });
    srv.listen(0, "127.0.0.1", () => resolve(srv));
  });
}

async function open(browser, base, query, tag) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const log = [];
  const net = [];
  page.on("console", (m) => log.push(`[${m.type()}] ${m.text()}`));
  page.on("pageerror", (e) => log.push(`[pageerror] ${e.message}`));
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Network.enable");
  await cdp.send("Performance.enable");
  const reqUrl = {};
  cdp.on("Network.requestWillBeSent", (e) => (reqUrl[e.requestId] = e.request.url));
  cdp.on("Network.loadingFinished", (e) => net.push({ url: reqUrl[e.requestId], bytes: e.encodedDataLength }));
  await page.goto(`${base}/index.html${query}`);
  await page.waitForFunction(() => window.__xq && (window.__xq.readyMs !== undefined || window.__xq.bootError), null, { timeout: 120000 });
  const boot = await page.evaluate(() => ({ readyMs: window.__xq.readyMs, bootError: window.__xq.bootError }));
  const save = () => fs.writeFileSync(path.join(OUT, `console-${tag}.log`), log.join("\n") + "\n");
  return { page, cdp, log, net, boot, save };
}

const frames = (p) => p.evaluate(() => window.__xq.frames());
const panicked = (p) => p.evaluate(() => window.__xq.panicked());
async function alive(p, ms = 3000) {
  const a = await frames(p);
  await sleep(ms);
  const b = await frames(p);
  const r = {};
  for (const k of Object.keys(a)) r[k] = { before: a[k], after: b[k], moving: typeof a[k] === "number" && b[k] > a[k] };
  return r;
}
const bad = (log) => log.filter((l) => /panic|PANIC|\[error\]|\[pageerror\]|LOADFAIL/i.test(l));

async function memory(o) {
  await o.cdp.send("HeapProfiler.collectGarbage");
  await sleep(500);
  const m = await o.cdp.send("Performance.getMetrics");
  const g = (n) => (m.metrics.find((x) => x.name === n) || {}).value || 0;
  const wasm = await o.page.evaluate(() => window.__xq.wasmBytes());
  return { jsHeapUsedMB: +(g("JSHeapUsedSize") / 1048576).toFixed(1), jsHeapTotalMB: +(g("JSHeapTotalSize") / 1048576).toFixed(1), wasmMemoryMB: +(wasm / 1048576).toFixed(1) };
}

(async () => {
  const browser = await chromium.launch({ args: ["--ignore-gpu-blocklist", "--enable-unsafe-swiftshader", "--use-angle=swiftshader"] });
  const srv = await serve(false);
  const gsrv = await serve(true);
  const base = `http://127.0.0.1:${srv.address().port}`;
  const gbase = `http://127.0.0.1:${gsrv.address().port}`;
  const R = { browser: browser.version(), when: new Date().toISOString() };

  // 1) 共用一個 instance＋panic 隔離 hook（預設）：四格都畫、都在動、主控台乾淨、移除一格、讓一格 panic
  {
    const o = await open(browser, base, "", "shared");
    const s = (R.shared = { boot: o.boot });
    await sleep(3000);
    s.webgl = await o.page.evaluate(() => {
      const c = document.createElement("canvas").getContext("webgl2");
      const d = c && c.getExtension("WEBGL_debug_renderer_info");
      return d ? c.getParameter(d.UNMASKED_RENDERER_WEBGL) : String(!!c);
    });
    await o.page.screenshot({ path: path.join(OUT, "1-four-widgets.png") });
    s.alive = await alive(o.page);
    s.consoleBad = bad(o.log);
    // 滑鼠只影響自己那格：點報價表第 3 列（2454），再把滑鼠移到 K 線上（十字線只出現在 K 線）
    const tb = await o.page.locator("#cv-table").boundingBox();
    const cb = await o.page.locator("#cv-chart").boundingBox();
    await o.page.mouse.click(tb.x + 60, tb.y + 102);
    await o.page.mouse.move(cb.x + 300, cb.y + 150);
    await sleep(600);
    await o.page.screenshot({ path: path.join(OUT, "1b-click-table-hover-chart.png") });
    await o.page.evaluate(() => window.__xq.remove("c2", true));
    s.afterRemoveC2 = await alive(o.page);
    await o.page.evaluate(() => window.__xq.widgets.c1.panic_next_frame());
    await sleep(500);
    s.afterPanicC1 = await alive(o.page);
    s.panickedAfter = await panicked(o.page);
    s.consoleAfterPanic = bad(o.log);
    await o.page.screenshot({ path: path.join(OUT, "2-shared-removed-c2-panicked-c1.png") });
    // 第二次 panic（另一格）：std 的 panic 計數在第一次 trap 後沒有歸零，確認第二次 panic 也只停那一格
    await o.page.evaluate(() => window.__xq.widgets.table.panic_next_frame());
    await sleep(500);
    s.afterSecondPanicTable = await alive(o.page);
    s.consoleAfterSecondPanic = bad(o.log);
    o.save();
    await o.page.close();
  }

  // 2) eframe 原本的行為（不換 hook）：一格 panic 會不會拖垮其他格
  {
    const o = await open(browser, base, "?isolate=0", "shared-noisolate");
    const s = (R.sharedNoIsolate = { boot: o.boot });
    await sleep(2000);
    s.before = await alive(o.page);
    await o.page.evaluate(() => window.__xq.widgets.c1.panic_next_frame());
    await sleep(500);
    s.afterPanicC1 = await alive(o.page);
    s.panickedAfter = await panicked(o.page);
    s.consoleAfterPanic = bad(o.log);
    await o.page.screenshot({ path: path.join(OUT, "3-noisolate-panic-c1.png") });
    o.save();
    await o.page.close();
  }

  // 3) 只把 canvas 從 DOM 拿掉、不呼叫 destroy
  {
    const o = await open(browser, base, "", "shared-remove-nodestroy");
    const s = (R.removeNoDestroy = { boot: o.boot });
    await sleep(2000);
    await o.page.evaluate(() => window.__xq.remove("chart", false));
    s.after = await alive(o.page);
    s.consoleBad = bad(o.log);
    o.save();
    await o.page.close();
  }

  // 4) 每個元件各自一個 instance（同一份 wasm 檔）：panic 一格
  {
    const o = await open(browser, base, "?mode=separate", "separate");
    const s = (R.separate = { boot: o.boot });
    await sleep(3000);
    await o.page.screenshot({ path: path.join(OUT, "4-separate-four-widgets.png") });
    s.alive = await alive(o.page);
    s.consoleBad = bad(o.log);
    await o.page.evaluate(() => window.__xq.widgets.c1.panic_next_frame());
    await sleep(500);
    s.afterPanicC1 = await alive(o.page);
    s.panickedAfter = await panicked(o.page);
    s.consoleAfterPanic = bad(o.log);
    o.save();
    await o.page.close();
  }

  // 5) 下載量（伺服器開 gzip）：共用 vs 各自 instance
  for (const [k, q] of [["shared", ""], ["separate", "?mode=separate"], ["single", "?only=table"]]) {
    const o = await open(browser, gbase, q, `download-${k}`);
    await sleep(1500);
    (R.download = R.download || {})[k] = {
      totalBytes: o.net.reduce((s, x) => s + x.bytes, 0),
      requests: o.net.map((x) => ({ url: x.url.replace(gbase, ""), bytes: x.bytes })),
    };
    await o.page.close();
  }

  // 6) 記憶體：1 個 runner vs 4 個（共用／各自 instance），各開新分頁、跑 5 秒後量
  R.memory = {};
  for (const [k, q] of [
    ["1 個（報價表）", "?only=table"],
    ["1 個（計數器）", "?only=c1"],
    ["4 個（共用 instance）", ""],
    ["4 個（各自 instance）", "?mode=separate"],
  ]) {
    const runs = [];
    for (let i = 0; i < 3; i++) {
      const o = await open(browser, base, q, `mem-${i}`);
      await sleep(5000);
      runs.push(await memory(o));
      await o.page.close();
    }
    const med = (f) => runs.map((r) => r[f]).sort((a, b) => a - b)[1];
    R.memory[k] = { runs, median: { jsHeapUsedMB: med("jsHeapUsedMB"), jsHeapTotalMB: med("jsHeapTotalMB"), wasmMemoryMB: med("wasmMemoryMB") } };
  }

  fs.writeFileSync(path.join(OUT, "results.json"), JSON.stringify(R, null, 2));
  console.log(JSON.stringify(R, null, 2));
  await browser.close();
  srv.close();
  gsrv.close();
})().catch((e) => {
  console.error(e);
  process.exit(1);
});

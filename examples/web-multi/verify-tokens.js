// tokens.html 驗證（無頭 Chromium／Playwright）：egui 元件和一般 HTML 讀同一份 token，畫出來的顏色、列高、字型是否一致。
// 用法：node verify-tokens.js <dist/web-multi 資料夾> <輸出資料夾>
// 輸出 tokens-results.json、tokens.png。比的是同一個位置的像素：左邊 egui canvas、右邊 HTML（右邊相對左邊的位移從 DOM 量）。
const http = require("http");
const fs = require("fs");
const path = require("path");
const { chromium } = require("playwright");

const DIST = path.resolve(process.argv[2] || "../../dist/web-multi");
const OUT = path.resolve(process.argv[3] || "out");
fs.mkdirSync(OUT, { recursive: true });
const TYPES = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".wasm": "application/wasm", ".otf": "font/otf" };

const srv = http.createServer((req, res) => {
  const u = new URL(req.url, "http://x");
  const f = path.join(DIST, decodeURIComponent(u.pathname));
  if (!f.startsWith(DIST) || !fs.existsSync(f) || fs.statSync(f).isDirectory()) return res.writeHead(404).end();
  res.writeHead(200, { "Content-Type": TYPES[path.extname(f)] || "application/octet-stream" }).end(fs.readFileSync(f));
});

(async () => {
  await new Promise((r) => srv.listen(0, "127.0.0.1", r));
  const port = srv.address().port;
  const browser = await chromium.launch({ args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"] });
  const page = await browser.newPage({ viewport: { width: 1000, height: 820 }, deviceScaleFactor: 1 });
  const logs = [];
  page.on("console", (m) => logs.push(`[${m.type()}] ${m.text()}`));
  await page.goto(`http://127.0.0.1:${port}/tokens.html`);
  await page.waitForFunction(() => window.__xq && window.__xq.readyMs, null, { timeout: 30000 });
  await page.waitForTimeout(2500);
  await page.screenshot({ path: path.join(OUT, "tokens.png") });

  // 兩邊的位置（CSS px，deviceScaleFactor=1 時等於截圖像素）
  const geo = await page.evaluate(() => {
    const r = (el) => { const b = el.getBoundingClientRect(); return { x: b.x, y: b.y, w: b.width, h: b.height }; };
    const tbl = document.getElementById("tbl");
    const rows = [...tbl.querySelectorAll(".r")].slice(0, 3).map(r);
    const cs = getComputedStyle(tbl.querySelector(".r span"));
    return {
      canvas: r(document.getElementById("egui")),
      tbl: r(tbl),
      head: r(tbl.querySelector(".head")),
      cols: r(tbl.querySelector(".cols")),
      rows,
      htmlFont: { family: cs.fontFamily, size: cs.fontSize },
      fontsOk: window.__xq.fontsOk,
      css: window.__xq.css,
    };
  });

  // 取截圖的像素：用 canvas 解 PNG（瀏覽器內做，不用額外套件）
  const png = fs.readFileSync(path.join(OUT, "tokens.png")).toString("base64");
  const px = async (pts) =>
    page.evaluate(async ({ png, pts }) => {
      const img = new Image();
      img.src = "data:image/png;base64," + png;
      await img.decode();
      const c = new OffscreenCanvas(img.width, img.height);
      const g = c.getContext("2d");
      g.drawImage(img, 0, 0);
      return pts.map(([x, y]) => { const d = g.getImageData(Math.round(x), Math.round(y), 1, 1).data; return "#" + [d[0], d[1], d[2]].map((v) => v.toString(16).padStart(2, "0")).join(""); });
    }, { png, pts });

  const tok = (name) => (geo.css.match(new RegExp(`--xq-${name}: ([^;]+);`)) || [])[1];
  const rowH = parseFloat(tok("layout-table-row"));
  const headH = parseFloat(tok("layout-table-header"));
  const colH = parseFloat(tok("layout-table-col-header"));
  // 取樣點（相對各自左上角，內框 1px 線寬已扣）：標頭底、選取列（第 1 列）底、第 6 列空白處（面板底）
  const spots = {
    header: [300, headH / 2],
    selectedRow: [455, headH + colH + rowH / 2],
    panel: [455, headH + colH + rowH * 5 + rowH / 2],
  };
  const out = { tokens: { rowH, headH, colH, header: tok("color-ui-header"), selected: tok("color-ui-selected"), panel: tok("color-ui-panel") }, spots: {} };
  for (const [k, [dx, dy]] of Object.entries(spots)) {
    const [e, h] = await px([[geo.canvas.x + 1 + dx, geo.canvas.y + 1 + dy], [geo.tbl.x + 1 + dx, geo.tbl.y + 1 + dy]]);
    out.spots[k] = { egui: e, html: h, same: e === h };
  }
  out.html = {
    headHeight: geo.head.h,
    colHeaderHeight: geo.cols.h,
    rowPitch: geo.rows.length > 1 ? geo.rows[1].y - geo.rows[0].y : null,
    font: geo.htmlFont,
    fontsLoaded: geo.fontsOk,
  };
  out.console = logs.filter((l) => /error|panic/i.test(l));
  fs.writeFileSync(path.join(OUT, "tokens-results.json"), JSON.stringify(out, null, 2));
  console.log(JSON.stringify(out, null, 2));
  await browser.close();
  srv.close();
})().catch((e) => { console.error(e); process.exit(1); });

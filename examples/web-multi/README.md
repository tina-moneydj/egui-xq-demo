# 一頁嵌多個 egui 元件（可行性範例）

一個 html 頁面（2×2 grid）放 egui 元件和一般 HTML：預設左上報價組合（50 列）、右上公告、左下下單匣（一般 HTML）、右下技術分析（K 線）；
`?layout=canvas4` 是 4 個 egui 元件（報價表、K 線、兩個計數器）。每個 egui 元件是一個 `<canvas>`＋一個 `eframe::WebRunner`。
`page.js`（版面、網頁格、自動壓測）與 gpui-xq-demo 的同名檔案內容相同。
結果與說明：[docs/web-multi.md](../../docs/web-multi.md)。

```bash
WASM_OPT=/path/to/wasm-opt ./build.sh                   # → ../../dist/web-multi/
python3 -m http.server -d ../../dist/web-multi 8080      # 瀏覽器開 http://127.0.0.1:8080/
node verify.js ../../dist/web-multi 輸出資料夾           # 無頭 Chromium 驗證（Playwright）
```

網址參數：`?layout=canvas4`、`?bench=1`（自動壓測，結果 POST 到 `/__bench`、也印在 console）、`?mode=separate`（每格各自一個 wasm instance）、`?isolate=0`（不換 panic hook，重現 eframe 原本「一格 panic 全停」）、`?only=table,c1`。

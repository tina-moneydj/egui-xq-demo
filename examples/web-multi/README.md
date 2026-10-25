# 一頁嵌 4 個 egui 元件（可行性範例）

一個 html 頁面（2×2 grid）放 4 個 `<canvas>`，各啟動一個 `eframe::WebRunner`：報價表（50 列）、K 線、兩個計數器。
結果與說明：[docs/web-multi.md](../../docs/web-multi.md)。

```bash
WASM_OPT=/path/to/wasm-opt ./build.sh                   # → ../../dist/web-multi/
python3 -m http.server -d ../../dist/web-multi 8080      # 瀏覽器開 http://127.0.0.1:8080/
node verify.js ../../dist/web-multi 輸出資料夾           # 無頭 Chromium 驗證（Playwright）
```

網址參數：`?mode=separate`（每格各自一個 wasm instance）、`?isolate=0`（不換 panic hook，重現 eframe 原本「一格 panic 全停」）、`?only=table,c1`。

# 三版功能對照表（wry ／ GPUI main ／ egui main）

2026-10-21 對齊後的狀態。基準是 **gpui-xq-demo main**（`tick-tape-scroll-anchor`、`ime-search-test` 分支都**沒有**合併，不算）；
GPUI 和 wry 一樣有的功能，egui 也要有。wry 獨有、GPUI 也沒有的（分頁、左側自選、右下內嵌網頁）不在對齊範圍。

圖例：✅ 有 ｜ 🆕 這次在 egui 補上 ｜ ≈ 有對應但做法不同（見備註） ｜ — 沒有／不適用

## 畫面與操作

| 功能 | wry | GPUI | egui | 備註 |
|---|---|---|---|---|
| 三格版面（上走勢圖／左下報價表／右下資訊格），分隔線可拖曳 | ✅ | ✅ | ✅ | wry 另有分頁、左側自選、右下網頁格（GPUI 沒有，不對齊） |
| 標題列、狀態列（連線狀態、FPS、RSS、封包/套用） | ✅ | ✅ | ✅ | |
| 週期 分時／日K／週K／月K、重設縮放 | ✅ | ✅ | ✅ | |
| 指標選單「指標 ▾」：主圖疊加 MA／EMA／布林／SAR（上限 20）、參數 −／+、移除 | ✅ | ✅ | ✅ | egui 🆕：Noto CJK 沒有 ▾ 字形（原本顯示成缺字方塊），改成自己畫三角形；名稱欄改靠左對齊（原本置中） |
| 指標選單：副圖 8 種（量、KD、MACD、RSI、%R、DMI、ATR、OBV，上限 10）、參數、均量 MA5/20、恢復預設 | ✅ | ✅ | ✅ | egui 🆕：勾選欄改固定寬靠左（原本置中，跟 GPUI 對不齊） |
| 十字線＋OHLC 圖例、滾輪縮放、拖曳平移 | ✅ | ✅ | ✅ | |
| 左側畫線工具列：游標＋8 種（趨勢線、射線、水平、垂直、平行通道、費波納契、矩形、文字）＋刪除選取＋全部清除 | ✅ | ✅ | ✅ | |
| 畫線選取／拖曳／編輯控制點、文字就地輸入（Enter／Esc） | ✅ | ✅ | ✅ | |
| 畫線與指標設定依代號＋週期存檔（背景寫檔） | ✅ | ✅ | ✅ | 壓測模式不讀不寫 |
| 畫線圖層（最多 200 層：新增、上下移、顯示／隱藏、鎖定、作用中圖層）＋上下順序（Ctrl+] 上移一層、Ctrl+[ 下移、Ctrl+Shift+] 移到最上、Ctrl+Shift+[ 移到最下；「圖層」面板按鈕） | ✅ | ✅ | ✅ | 2026-10-23 三版一起加；隱藏＝不畫也點不到，鎖定＝會畫但點不到；點選看最上面那條；JSON 加 `layers`，舊存檔照讀。差異：GPUI 同一區段內文字永遠在線條上面；wry 平移時水平線兩端延伸的部分會晚一幀補上 |
| 報價表 33 檔、跳價底色、虛擬化、點選切換走勢圖 | ✅ | ✅ | ✅ | |
| 壓力 5 萬（5 萬列合成表） | ✅ | ✅ | ✅ | |
| 資訊／效能面板（引擎封包、報價筆數、UI 套用、套用耗時、走勢圖重畫） | ≈ | ✅ | ✅ | egui 那一列叫「走勢圖重建」：數的是快取 Mesh 重建次數（沒變的幀不算），意思比「重畫」精確 |
| **成交明細**面板（`XQ_TAPE=1`，右下多一欄 276px） | — | ✅ | 🆕 | 見下節 |
| 股名／代號搜尋框 | — | — | — | 只在 `ime-search-test` 分支（GPUI、egui），三版 main 都不放，壓測畫面相同 |

## 成交明細（tick tape）細項

| 行為 | GPUI main | egui | 備註 |
|---|---|---|---|
| 推導規則（總量差＝成交量、價升降＝外／內、換代號／量變少重設基準） | ✅ | ✅ | 同一個 `xq-tape` crate（gpui repo，同 rev c7ac536） |
| 背景執行緒：合併前每包看一眼，O(1)／筆進固定容量 Ring（4096） | ✅ | ✅ | 面板沒開時 `None`，熱路徑零成本 |
| UI 每幀最多取一次（雙緩衝 swap）、和報價表同節奏 | ✅ | ✅ | egui：整批只讓可見列快取失效一次（單元測試鎖住） |
| 保留最新 5000 筆、UI 落後時擠掉最舊並計數 | ✅ | ✅ | |
| 只看大單、門檻階梯 1→5→…→1000、大單底色＋單量黃字 | ✅ | ✅ | |
| 快捷鍵（面板有焦點時）：`b` 只看大單、`]`／`[` 門檻、`Home` 回最新 | ✅ | ✅ | GPUI 用 key_context；egui 點面板取得焦點、`consume_key` |
| 虛擬化：只格式化可見列 | ✅ uniform_list | ✅ | egui 再把可見列三角化成一個 Mesh 快取，沒有新成交／沒捲動就沿用 |
| 捲動：記住像素位移，捲離頂端時新成交會把內容往下推 | ✅ | ✅ | 「錨定」行為在 `tick-tape-scroll-anchor` 分支，未合併，兩版都不做 |
| 設定存檔：背景讀、防抖 450 ms 背景寫、壞檔回預設 | ✅ | ✅ | egui 存 `~/.local/share/egui-xq-demo/tick_tape.json`（或 `XQ_TAPE_STATE`） |
| 壓測模式不讀不寫設定 | ✅ | ✅ | |

## 環境變數（除錯／壓測開關）

| 變數 | wry | GPUI | egui | 備註 |
|---|---|---|---|---|
| `XQ_BENCH`、`XQ_PERIOD`、`XQ_METRICS`、`XQ_TOP_FRAC` | ✅ | ✅ | ✅ | wry 的圖面比例是 `XQ_CHART_TOP_RATIO` |
| `XQ_CHART_STRESS`、`XQ_CHART_DRAWINGS`、`XQ_CHART_*_MS` | ✅ | ✅ | ✅ | 重圖表壓測 |
| `XQ_GROUP_STRESS`、`XQ_STRESS_TICK_HZ／_MS`、`XQ_STRESS_SCROLL_MS` | ✅ | ✅ | ✅ | 5 萬列壓測 |
| `XQ_TICK_BENCH`、`XQ_FEED_ADDR` | ✅ | ✅ | ✅ | 高頻壓測（xq-feed） |
| `XQ_TAPE`、`XQ_TAPE_STATE` | — | ✅ | 🆕 | |
| `XQ_DEBUG` | ✅ | ✅ | 🆕 | 印滑鼠按下（按鍵、座標） |
| `XQ_NO_VIEW_CACHE` | — | ✅ | 🆕 ≈ | GPUI：關掉 view `.cached()`；egui：關掉走勢圖底圖／即時層、報價表、成交明細的 Mesh 快取，每幀重建（畫線層自己的 PaintCache 兩版都保留） |
| `XQ_LAYER_MODE=split` | — | ✅ | 🆕 ≈ | GPUI：quad／path／字分 scene layer（MSAA 中介貼圖）；egui：走勢圖三層各畫在自己的子圖層（`set_sublayer`），多幾批 draw call，沒有 MSAA 問題 |
| `XQ_DEBUG_REPAINT` | — | — | ✅ | egui 獨有：印每幀被誰叫醒 |
| `XQ_RENDERER=glow\|wgpu`、`XQ_EGUI_STATE`、`XQ_CJK_FONT` | — | — | ✅ | egui 獨有（GPUI 對應 `XQ_GPUI_STATE`、wry `XQ_STATE`） |

## 測試

| | wry | GPUI | egui |
|---|---|---|---|
| app 單元測試 | 2（push 節流） | 6（indicators 1、series 2、tick_tape 3） | **8**（indicators 1、series 2、tick_tape 5：與 GPUI 同 3 個＋整批合併只失效一次／大單重建／換代號、可見列範圍） |
| 共用 crate | — | xq-feed 3、xq-tape 9＋行為基準 1、micro-bench | 用同一份 xq-feed／xq-tape（git 依賴，測試在 gpui repo 跑；cargo 不讓測非 workspace 的依賴） |

`cargo test --release`：egui 8 passed。

## 截圖（2026-10-21，DISPLAY :1，同一個 WryFeedHost --demo-ticks）

| | GPUI main | egui（對齊後） |
|---|---|---|
| 主畫面＋成交明細（`XQ_TAPE=1`） | [gpui-tape.png](parity/gpui-tape.png) | [egui-tape.png](parity/egui-tape.png) |
| 指標選單 | [gpui-menu.png](parity/gpui-menu.png) | 對齊前 [egui-menu.png](parity/egui-menu.png) → 對齊後 [egui-menu2.png](parity/egui-menu2.png) |
| 只看大單（面板有焦點） | — | [egui-tape-big.png](parity/egui-tape-big.png) |
| `XQ_LAYER_MODE=split XQ_NO_VIEW_CACHE=1` | — | [egui-split-menu.png](parity/egui-split-menu.png) |

## 壓測一致性

三版 main 預設都**不開**成交明細、**沒有**搜尋框，壓測腳本也不設 `XQ_TAPE`／`XQ_NO_VIEW_CACHE`／`XQ_LAYER_MODE`，所以三方壓測畫面與 2026-10-21 那次相同；
egui 的熱路徑只多了「`tape` 是 None」的判斷和一次 OnceLock 讀取。

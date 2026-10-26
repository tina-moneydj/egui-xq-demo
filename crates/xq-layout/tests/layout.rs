//! 版面解析行為：範例檔、降級、上限、分割計算。
//! 本功能是 egui 版新加的（WPF 沒有對應），所以沒有 WPF 基準檔；行為以這裡的測試為準。

use xq_layout::*;

fn demo() -> Screen {
    parse(include_bytes!("../../../layouts/demo.json")).expect("demo.json 要能解析")
}

#[test]
fn demo_parses_without_warnings_except_unknown_widget() {
    let s = demo();
    assert_eq!(s.pages.len(), 3);
    assert_eq!(s.start, 0);
    assert_eq!(s.menu.len(), 3);
    // 只有「news」這個新版元件會被降級
    assert_eq!(s.warnings.len(), 1, "{:?}", s.warnings);
    assert!(s.warnings[0].contains("news"));
    let lite = &s.pages[2];
    assert!(matches!(&lite.panes[2], Pane::Unsupported(t) if t.contains("news")));
}

#[test]
fn demo_menu_resolves_pages_and_symbols() {
    let s = demo();
    let MenuEntry::Group { label, items } = &s.menu[0] else { panic!() };
    assert_eq!(label, "看盤");
    assert_eq!(items[1], MenuEntry::Item { label: "雙圖比較".into(), action: Action::Page(1) });
    assert_eq!(items[3], MenuEntry::Separator);
    let MenuEntry::Group { items: sub, .. } = &items[4] else { panic!() };
    assert_eq!(sub[0], MenuEntry::Item { label: "台積電".into(), action: Action::Symbol("2330".into()) });
    let MenuEntry::Group { items, .. } = &s.menu[2] else { panic!() };
    assert!(matches!(&items[0], MenuEntry::Item { action: Action::Url(u), .. } if u.starts_with("https://")));
}

#[test]
fn demo_chart_link() {
    let s = demo();
    assert_eq!(s.pages[1].panes[1], Pane::Chart { symbol: "2881".into(), period: "D".into(), link: true });
    assert_eq!(s.pages[1].panes[2], Pane::Chart { symbol: "2330".into(), period: "W".into(), link: false });
}

#[test]
fn demo_quote_columns() {
    let s = demo();
    let Pane::Quotes { title, symbols, columns } = &s.pages[1].panes[0] else { panic!() };
    assert_eq!(title, "金融");
    assert_eq!(symbols.len(), 4);
    assert_eq!(columns, &vec![1, 2, 4]); // name, price, pct
}

#[test]
fn broken_json_is_error() {
    assert!(parse(b"{ not json").is_err());
    assert!(parse(b"{}").is_err(), "沒有頁要回錯，讓呼叫端沿用上一份");
    assert!(parse(&vec![b' '; MAX_BYTES + 1]).is_err());
}

#[test]
fn bad_fields_degrade_not_fail() {
    // sizes 有字串 → serde 整份失敗是不對的；這裡 sizes 有 "bad"，要確認行為：整份格式錯誤
    // （sizes 型別錯是伺服器 bug，直接拒收，沿用上一份）
    let raw = include_bytes!("fixtures/broken.json");
    assert!(parse(raw).is_err());
    // 修掉型別錯之後，其他問題都降級
    let fixed = String::from_utf8_lossy(raw).replace(", \"bad\"", "");
    let s = parse(fixed.as_bytes()).expect("型別正確就要能開");
    assert_eq!(s.pages.len(), 1, "重複 id、沒有 root 的頁略過");
    assert_eq!(s.start, 0);
    let Node::Split { dir, sizes, .. } = &s.pages[0].root else { panic!() };
    assert_eq!(*dir, Dir::Col);
    assert_eq!(sizes, &vec![0.5, 0.5]);
    assert_eq!(s.pages[0].panes[0], Pane::Chart { symbol: String::new(), period: "T".into(), link: true });
    assert!(matches!(s.pages[0].panes[1], Pane::Unsupported(_)));
    // 選單：壞連結、危險網址、沒動作都略過；第 3 層以下的子選單略過
    assert_eq!(s.menu.len(), 1);
    let MenuEntry::Group { items, .. } = &s.menu[0] else { panic!() };
    let MenuEntry::Group { items: l2, .. } = &items[0] else { panic!() };
    assert!(l2.is_empty());
    let w = s.warnings.join("\n");
    for k in ["版本 9", "起始頁", "missing", "http(s)", "沒有 page", "超過 3 層", "diagonal", "週期", "重複", "沒有 root"] {
        assert!(w.contains(k), "警告少了「{k}」：\n{w}");
    }
}

#[test]
fn limits_are_enforced() {
    let charts: Vec<String> = (0..MAX_PANES + 4).map(|_| r#"{"widget":"chart","symbol":"2330"}"#.to_string()).collect();
    let syms: Vec<String> = (0..MAX_SYMBOLS + 10).map(|i| format!("\"{i}\"")).collect();
    let items: Vec<String> = (0..MAX_MENU_ITEMS + 5).map(|_| r#"{"label":"x","symbol":"2330"}"#.to_string()).collect();
    let json = format!(
        r#"{{"menu":[{}],"pages":[{{"id":"p","root":{{"split":"row","children":[{}]}}}},{{"id":"q","root":{{"widget":"quotes","symbols":[{}]}}}}]}}"#,
        items.join(","),
        charts.join(","),
        syms.join(",")
    );
    let s = parse(json.as_bytes()).unwrap();
    let p = &s.pages[0];
    assert_eq!(p.panes.len(), MAX_PANES);
    assert_eq!(p.panes.iter().filter(|x| matches!(x, Pane::Chart { .. })).count(), MAX_CHARTS);
    let Pane::Quotes { symbols, columns, .. } = &s.pages[1].panes[0] else { panic!() };
    assert_eq!(symbols.len(), MAX_SYMBOLS);
    assert_eq!(columns.len(), COLUMNS.len(), "沒給欄位＝全部");
    assert_eq!(s.menu.len(), MAX_MENU_ITEMS);
    let mut many = String::from(r#"{"pages":["#);
    for i in 0..MAX_PAGES + 3 {
        many += &format!(r#"{}{{"id":"p{i}","root":{{"widget":"info"}}}}"#, if i > 0 { "," } else { "" });
    }
    many += "]}";
    assert_eq!(parse(many.as_bytes()).unwrap().pages.len(), MAX_PAGES);
}

#[test]
fn deep_split_is_capped() {
    let mut j = r#"{"widget":"info"}"#.to_string();
    for _ in 0..MAX_DEPTH + 3 {
        j = format!(r#"{{"split":"row","children":[{j},{{"widget":"info"}}]}}"#);
    }
    let s = parse(format!(r#"{{"pages":[{{"id":"d","root":{j}}}]}}"#).as_bytes()).unwrap();
    fn depth(n: &Node) -> usize {
        match n {
            Node::Leaf(_) => 0,
            Node::Split { children, .. } => 1 + children.iter().map(depth).max().unwrap_or(0),
        }
    }
    assert!(depth(&s.pages[0].root) < MAX_DEPTH);
}

#[test]
fn solve_demo_page() {
    let s = demo();
    let (mut panes, mut gaps) = (Vec::new(), Vec::new());
    solve(&s.pages[0].root, Rect::new(0.0, 30.0, 1000.0, 601.0), 1.0, &mut panes, &mut gaps);
    assert_eq!(panes.len(), 3);
    assert_eq!(gaps.len(), 2);
    let (chart, quotes, info) = (panes[0], panes[1], panes[2]);
    assert_eq!(chart, Rect::new(0.0, 30.0, 1000.0, 336.0)); // (601-1)*0.56 = 336
    assert_eq!(gaps[0], Rect::new(0.0, 366.0, 1000.0, 1.0));
    assert_eq!(quotes.y, 367.0);
    assert_eq!(quotes.h + 367.0, 631.0, "最後一格吃到底");
    assert_eq!(quotes.w, 420.0); // 999*0.42 = 419.6 → 420
    assert_eq!(info.x, 421.0);
    assert_eq!(info.x + info.w, 1000.0);
    // 每幀重用：再解一次結果一樣、不會越疊越多
    solve(&s.pages[0].root, Rect::new(0.0, 30.0, 1000.0, 601.0), 1.0, &mut panes, &mut gaps);
    assert_eq!((panes.len(), gaps.len()), (3, 2));
}

#[test]
fn solve_tiny_area_never_negative() {
    let s = demo();
    let (mut panes, mut gaps) = (Vec::new(), Vec::new());
    for p in &s.pages {
        solve(&p.root, Rect::new(0.0, 0.0, 3.0, 2.0), 1.0, &mut panes, &mut gaps);
        assert!(panes.iter().all(|r| r.w >= 0.0 && r.h >= 0.0));
    }
}

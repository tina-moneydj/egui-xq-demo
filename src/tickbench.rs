//! 高頻進價壓測量測（`XQ_TICK_BENCH=1` 才啟用；搭配 xq-feed 的 xq-tickgen）。與 gpui 版輸出同一種 `TICKBENCH|{json}`。
//!
//! present 的定義：egui 沒有「這幀已送上螢幕」的回呼，所以用「下一幀開始」當 present——
//! eframe 在呼叫下一次 `App::ui` 之前，上一幀已經三角化、送進 GL/wgpu 並 swap/present 完。
//! 跟 GPUI 的 `on_next_frame`、wry 的兩次 rAF 一樣是上界（最多多算約 1 幀）。
//! 有待量的樣本時會 `request_repaint_after(16ms)`，保證下一幀一定會來。

use xq_feed::{unix_us, Stamp};

#[derive(Default)]
pub struct TickBench {
    /// 多視窗（XQ_WINDOWS）時的視窗編號；印在 TICKBENCH 行的 "win"
    pub win: usize,
    /// [seq_first, sent_first, seq_last, sent_last, recv_last, applied_us, present_us]
    samples: Vec<[u64; 7]>,
    switches: Vec<(String, u64, u64)>,
    /// 這一幀套用的（還沒 present）
    pending: Option<(Stamp, Stamp, u64)>,
    switch_pending: Option<(String, u64)>,
    pub inbox_rows: u64,
    pub applied_rows: u64,
    pub applies: u64,
    pub apply_us: u64,
}

impl TickBench {
    pub fn for_window(win: usize) -> Self {
        TickBench { win, ..Default::default() }
    }

    pub fn on_apply(&mut self, first: Option<Stamp>, stamp: Option<Stamp>, inbox_rows: usize, changed: usize, apply_us: u64) {
        self.inbox_rows += inbox_rows as u64;
        self.applied_rows += changed as u64;
        self.applies += 1;
        self.apply_us += apply_us;
        if let Some(st) = stamp {
            let first = first.unwrap_or(st);
            let first = match self.pending { Some((f, _, _)) => f, None => first };
            self.pending = Some((first, st, unix_us()));
        }
    }

    pub fn on_switch(&mut self, symbol: &str) {
        self.switch_pending = Some((symbol.to_string(), unix_us()));
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some() || self.switch_pending.is_some()
    }

    /// 每一幀開始時呼叫：上一幀套用的樣本此時已經畫上螢幕
    pub fn on_frame_start(&mut self) {
        let now = unix_us();
        if let Some((f, st, applied)) = self.pending.take() {
            self.samples.push([f.seq, f.sent_us, st.seq, st.sent_us, st.recv_us, applied, now]);
        }
        if let Some((sym, handled)) = self.switch_pending.take() {
            self.switches.push((sym, handled, now));
        }
    }

    pub fn report(&mut self, rows_in: u64, frames_in: u64, fps: u32, rss_mb: f64) {
        let lat: Vec<String> = self
            .samples
            .drain(..)
            .map(|s| format!("[{},{},{},{},{},{},{}]", s[0], s[1], s[2], s[3], s[4], s[5], s[6]))
            .collect();
        let sw: Vec<String> = self.switches.drain(..).map(|(s, h, p)| format!("[\"{s}\",{h},{p}]")).collect();
        eprintln!(
            "TICKBENCH|{{\"win\":{},\"now\":{},\"fps\":{fps},\"rssMb\":{rss_mb:.1},\"rowsIn\":{rows_in},\"framesIn\":{frames_in},\"inboxRows\":{},\"applied\":{},\"applies\":{},\"applyUs\":{},\"lat\":[{}],\"sw\":[{}]}}",
            self.win,
            unix_us(),
            self.inbox_rows,
            self.applied_rows,
            self.applies,
            self.apply_us,
            lat.join(","),
            sw.join(",")
        );
    }
}

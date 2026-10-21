#!/bin/bash
# 簡測 glow vs wgpu：同一份 feed（WryFeedHost --demo-ticks），各跑 N 秒，記 fps、CPU（全部 / 扣 llvmpipe）、RSS、首幀
cd /workspace/egui-xq-demo
export DISPLAY=:2
OUT=/workspace/bench/egui-renderer
for r in glow wgpu; do
 for env in "baseline" "stress"; do
  extra=""; [ $env = stress ] && extra="XQ_CHART_STRESS=1"
  t0=$(date +%s.%N)
  env XQ_RENDERER=$r XQ_BENCH=1 XQ_METRICS=1 XQ_PERIOD=D $extra ./target/release/egui-xq-demo > $OUT/$r-$env.log 2>&1 &
  pid=$!
  sleep 6
  W=$(xdotool search --onlyvisible --name "^XQ .*egui" | head -1); xdotool windowmove --sync $W 0 0 2>/dev/null; xdotool windowsize --sync $W 1240 760 2>/dev/null
  sleep 2
  read u1 s1 < <(awk '{print $14, $15}' /proc/$pid/stat)
  lp1=0; for t in /proc/$pid/task/*; do if grep -q llvmpipe $t/comm; then read a b < <(awk '{print $14,$15}' $t/stat); lp1=$((lp1+a+b)); fi; done
  sleep 15
  read u2 s2 < <(awk '{print $14, $15}' /proc/$pid/stat)
  lp2=0; for t in /proc/$pid/task/*; do if grep -q llvmpipe $t/comm; then read a b < <(awk '{print $14,$15}' $t/stat); lp2=$((lp2+a+b)); fi; done
  tk=$(getconf CLK_TCK)
  all=$(awk "BEGIN{printf \"%.1f\", ($u2+$s2-$u1-$s1)*100/$tk/15}")
  nolp=$(awk "BEGIN{printf \"%.1f\", ($u2+$s2-$u1-$s1-$lp2+$lp1)*100/$tk/15}")
  rss=$(awk '/VmRSS/{print $2/1024}' /proc/$pid/status)
  kill $pid; wait $pid 2>/dev/null
  fps=$(grep -o "fps=[0-9]*" $OUT/$r-$env.log | tail -12 | cut -d= -f2 | sort -n | awk '{a[NR]=$1}END{print a[int((NR+1)/2)]}')
  echo "$r $env cpu_all=$all% cpu_no_llvmpipe=$nolp% rss=${rss}MB fps_median=$fps" | tee -a $OUT/summary.txt
  grep CHARTPERF $OUT/$r-$env.log | tail -1 | cut -c1-300
  sleep 2
 done
done

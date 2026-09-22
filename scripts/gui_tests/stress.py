#!/usr/bin/env python3
"""xperf GUI 压力测试（长时稳定性基线；debug API 驱动）。

用法:
    python3 stress.py [--scenario s1,s3] [--serial <SS3>] [--s1-minutes 10]

前置：GUI 已启动（同机，setsid 方式见 GUI-TEST-PLAN；读 ~/.config/xperf/gui-debug.json），
目标设备在线，测试应用已安装（默认 gltf viewer，XPERF_IT_PACKAGE 覆盖）。
运行期间勿动物理鼠标/窗口（合成 hover 与真实事件竞争）。

场景（顺序执行，采样自 s1 起持续运行贯穿后续场景——叠加负载即压力）:
    s1 长时采样（默认 10min，CPU+内存+FPS @1000ms）——资源曲线主场景
    s2 高频数据（50ms 间隔，默认 3min）——图表绘制压力 + 悬停可用性
    s3 logcat 洪泛（全机 V + 设备端注入 XPERFFLOOD；可见/隐藏/切回补发三段）
    s4 trace+stack 并发（采样中同录 15s）——深挖不挤占采样节拍
    s5 多设备并行 + 快切页（三机同采，0.4s×150 次设备页切换）
    s6 浏览器按钮连点（open-perf ×10——防抖生效 + GUI 存活）
    s7 adb server 冻结注入（仅 ssh 模式：SIGSTOP 45s 半开信道，看门狗+重连自愈）

监测：全程 5s 粒度资源曲线（GUI 主进程/WebKit 子进程 RSS/CPU、DOM 节点数、
series 总点数、debug API 可达性）落 JSONL（/tmp/xperf-gui-stress-<ts>.jsonl），
结束后分段汇总成基线表。判据宽松（压测目的是基线 + 明显异常检出，非功能回归
——功能正确性归 regression.py）。
"""

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from harness import Client, Checker, proc_rss_cpu, webkit_children  # noqa: E402

PKG = os.environ.get("XPERF_IT_PACKAGE", "com.google.android.filament.gltf")
REMOTE_ADB = os.environ.get("XPERF_TEST_SSH", "hppc")  # SSH 远程模式下设备所挂宿主
EXTRA_SERIALS = [s for s in os.environ.get(
    "XPERF_TEST_EXTRA_SERIALS", "d1f39648c1f,localhost:5559").split(",") if s]

_MONITOR_INTERVAL = 5  # 资源曲线采样粒度（秒）


def page(serial):
    return f'.device-page[data-serial="{serial}"]'


def webkit_pids_for_gui(gui_pid):
    """WebKit 子进程 pid 集合（启动窗口归因，监测期内固定跟踪）。

    Linux/webkit2gtk：子进程直挂 GUI（ppid），harness.webkit_children 每次枚举。
    macOS/WKWebView：XPC 服务挂 launchd（ppid=1 且无 responsible 权限读取）——
    按「与 GUI 启动时刻相近」归因（etimes 差 ∈ [-5, 90]s；同机其他 WebView 应用的
    WebKit 进程启动时刻不同被排除）。WebContent 崩溃重spawn 会逃逸归因——但崩溃
    本身会致 eval 失败被 api 失联计数捕获，两通道互补。
    """
    if sys.platform != "darwin":
        return [p for p, _ in webkit_children(gui_pid)]
    # BSD ps 无 etimes，用 lstart（LC_ALL=C 防本地化日期串）
    try:
        gui_ls = subprocess.run(
            ["ps", "-o", "lstart=", "-p", str(gui_pid)],
            capture_output=True, text=True, timeout=5,
            env={**os.environ, "LC_ALL": "C"}).stdout.strip()
        gui_ts = time.mktime(time.strptime(gui_ls, "%a %b %d %H:%M:%S %Y"))
        out = subprocess.run(
            ["ps", "-Ao", "pid=,lstart=,comm="],
            capture_output=True, text=True, timeout=5,
            env={**os.environ, "LC_ALL": "C"}).stdout
    except Exception:
        return []
    pids = []
    for line in out.splitlines():
        m = line.split(None, 6)  # pid + lstart 5 词 + comm
        if len(m) != 7 or "WebKit" not in m[6]:
            continue
        try:
            pid = int(m[0])
            ts = time.mktime(time.strptime(" ".join(m[1:6]), "%a %b %d %H:%M:%S %Y"))
        except ValueError:
            continue
        if -5 <= ts - gui_ts <= 90:  # WebKit XPC 随 GUI 建 webview 后拉起
            pids.append(pid)
    return pids


class ResourceMonitor:
    """后台线程：5s 粒度采集 GUI/WebKit 进程资源 + 前端 DOM/series 规模，落 JSONL。"""

    def __init__(self, c: Client, out_path):
        self.c = c
        self.out_path = out_path
        self.gui_pid = c.pid
        self.web_pids = webkit_pids_for_gui(c.pid)
        self.scenario = "boot"
        self.t0 = time.time()
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self.records = []
        self.api_fails = 0

    def set_scenario(self, name):
        self.scenario = name

    def start(self):
        self._thread.start()

    def stop(self):
        self._stop.set()
        self._thread.join(timeout=10)

    def _sample_once(self):
        row = {"t": round(time.time() - self.t0, 1), "sc": self.scenario}
        gui = proc_rss_cpu([self.gui_pid] if self.gui_pid else [])
        web = proc_rss_cpu(self.web_pids)
        row.update(gui_rss_kb=gui["rss_kb"], gui_cpu=gui["cpu"],
                   web_rss_kb=web["rss_kb"], web_cpu=web["cpu"],
                   web_n=len(self.web_pids), web_alive=web["alive"])
        try:
            r = self.c.eval(
                "(()=>{let pts=0;for(const s of app.sessions.values())"
                "{for(const ch of s.allCharts){for(const k in ch.series)"
                "pts+=ch.series[k].length;}}"
                "return {dom:document.querySelectorAll('*').length,pts}})()")
            row.update(dom=r.get("dom"), pts=r.get("pts"), api=1)
        except AssertionError as e:  # HTTP 层通（JS 抛错），不算 API 失联
            row.update(dom=None, pts=None, api=1, err=str(e)[:80])
        except Exception as e:  # 网络级失败（req 已重试 3 次仍败）= 真失联
            row.update(dom=None, pts=None, api=0, err=str(e)[:80])
            self.api_fails += 1
        # 隧道/设备枚举快照（/api/status 走缓存非阻塞）：故障类事件的时间轴佐证
        try:
            st = self.c.status()
            row["remote_alive"] = bool((st.get("remote") or {}).get("alive", True))
            row["ndev"] = len([d for d in st.get("devices", []) if not d.get("is_gateway")])
        except Exception:
            row["remote_alive"] = None
            row["ndev"] = None
        return row

    def _run(self):
        with open(self.out_path, "a", encoding="utf-8") as f:
            while not self._stop.is_set():
                try:
                    row = self._sample_once()
                except Exception as e:  # 采样器自身异常不终止监测
                    row = {"t": round(time.time() - self.t0, 1),
                           "sc": self.scenario, "api": 0, "err": str(e)[:80]}
                    self.api_fails += 1
                self.records.append(row)
                f.write(json.dumps(row, ensure_ascii=False) + "\n")
                f.flush()
                self._stop.wait(_MONITOR_INTERVAL)

    def segment(self, scenario):
        return [r for r in self.records if r.get("sc") == scenario]


# ---------------------------------------------------------------------------
# 设备侧工具（本机/SSH 远程两态，与 regression.py 同套路）
# ---------------------------------------------------------------------------

_MODE = None
_REMOTE_ADB = None


def remote_mode(c):
    global _MODE
    if _MODE is None:
        _MODE = c.status().get("remote", {}).get("mode", "local")
    return _MODE


def _remote_adb():
    global _REMOTE_ADB
    if _REMOTE_ADB is None:
        r = subprocess.run(["ssh", REMOTE_ADB,
                            "command -v adb || echo $HOME/Android/Sdk/platform-tools/adb"],
                           capture_output=True, text=True, timeout=10)
        path = (r.stdout or "").strip().splitlines()[-1] if (r.stdout or "").strip() else "adb"
        chk = subprocess.run(["ssh", REMOTE_ADB, f"test -x {path}"],
                             capture_output=True, timeout=10)
        _REMOTE_ADB = path if chk.returncode == 0 else "adb"
    return _REMOTE_ADB


def adb_shell(c, serial, shell_cmd, timeout=30):
    """在设备上执行 shell 命令（ssh 模式经设备所挂宿主）。"""
    if remote_mode(c) == "ssh":
        full = f"{_remote_adb()} -s {serial} shell {shlex.quote(shell_cmd)}"
        return subprocess.run(["ssh", REMOTE_ADB, full],
                              capture_output=True, text=True, timeout=timeout)
    return subprocess.run([os.environ.get("XPERF_ADB", "adb"), "-s", serial,
                           "shell", shell_cmd],
                          capture_output=True, text=True, timeout=timeout)


class Flooder:
    """设备端 logcat 洪泛注入：循环 `log` 写入 XPERFFLOOD tag（stop 后有界收尾）。

    序号经 `/data/local/tmp/xflood_i` 跨调用单调递增（早期版本每次调用 0..299
    回绕，tail 里 max 序号可能回退——补发判据被回绕污染过一次）。
    """

    LOOP = ('i=$(cat /data/local/tmp/xflood_i 2>/dev/null || echo 0); '
            'end=$((i+300)); while [ $i -lt $end ]; do log -p i -t XPERFFLOOD '
            '"sf-$i-0123456789abcdef0123456789abcdef0123456789abcdef"; '
            'i=$((i+1)); done; echo $i > /data/local/tmp/xflood_i')

    def __init__(self, c, serial):
        self.c = c
        self.serial = serial
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def start(self):
        self._thread.start()

    def stop(self):
        self._stop.set()
        self._thread.join(timeout=30)  # 单次 LOOP 有界（300 行），随 adb 调用结束

    def _run(self):
        while not self._stop.is_set():
            try:
                adb_shell(self.c, self.serial, self.LOOP, timeout=60)
            except Exception:
                time.sleep(1)


# ---------------------------------------------------------------------------
# 采样/小工具
# ---------------------------------------------------------------------------

def ensure_visible(c, timeout=8):
    """macOS 全遮挡/锁屏冻结 WebKit rAF——hover 前拉回前台（见 GUI-TEST-PLAN #6）。"""
    if c.eval("document.visibilityState") == "visible":
        return True
    try:
        c.eval("(async()=>{await window.__TAURI__.window.getCurrentWindow().setFocus();return 1})()")
    except AssertionError:
        return False
    return c.poll(
        lambda: (c.eval("document.visibilityState") == "visible") or None,
        timeout=timeout) is not None


def cpu_count(c, serial):
    ser = (c.state(serial).get("charts") or {}).get("cpu", {}).get("series", {})
    return sum(v.get("count", 0) for v in ser.values())


def logcat_buf_len(c, serial):
    return c.eval(f"app.sessions.get('{serial}').logcatBuf.length")


def last_flood_idx(c, serial):
    """视图 tail 里最后一条 XPERFFLOOD 洪泛行的序号（无则 -1）。"""
    tail = c.state(serial, logcat_tail=300).get("logcatTail") or []
    idx = [int(m.group(1)) for line in tail
           if (m := re.search(r"sf-(\d+)-", str(line)))]
    return max(idx) if idx else -1


def start_sampling(c, ck, serial, name="开始监控"):
    p = page(serial)
    c.action("input", selector=f"{p} .package-input", value=PKG)
    _, r = c.action("click", selector=f"{p} .start-btn")
    ok = r.get("ok") and c.poll(lambda: c.state(serial).get("samplingRunning"), timeout=20)
    ck.check(name, bool(ok), str(r.get("error", ""))[:120])
    return bool(ok)


def stop_sampling(c, serial):
    """停止采样：优先走停止按钮（前端状态机一致）；按钮 disabled（未在跑）时
    后端幂等 invoke 兜底。直接 invoke 绕开前端会让 samplingRunning 滞留 true。"""
    btn = f"{page(serial)} .stop-btn"
    disabled = c.eval(f"document.querySelector({json.dumps(btn)}).disabled")
    if not disabled:
        c.action("click", selector=btn)
    else:
        c.invoke("stop_sampling", {"serial": serial})
    c.poll(lambda: not c.state(serial).get("samplingRunning"), timeout=12)


def launch_app(c, serial):
    p = page(serial)
    c.action("input", selector=f"{p} .package-input", value=PKG)
    c.action("click", selector=f"{p} .launch-btn")
    c.poll(lambda: (adb_shell(c, serial, f"pidof {PKG}").stdout or "").strip() or None,
           timeout=30)


def seg_stat(rows, key):
    vals = [r[key] for r in rows if r.get(key) is not None]
    if not vals:
        return None
    return {"first": vals[0], "last": vals[-1], "max": max(vals), "n": len(vals)}


# ---------------------------------------------------------------------------
# 场景
# ---------------------------------------------------------------------------

def scenario_s1(c, ck, mon, args):
    """长时采样：默认指标 @1000ms，持续 --s1-minutes。"""
    mon.set_scenario("s1")
    p = page(args.serial)
    launch_app(c, args.serial)
    if not start_sampling(c, ck, args.serial, "s1 开始监控"):
        return
    c0 = cpu_count(c, args.serial)
    deadline = time.time() + args.s1_minutes * 60
    last = c0
    stall_windows = 0
    while time.time() < deadline:
        time.sleep(min(60, max(1, deadline - time.time())))
        # 应用中途被杀时 series 停涨——环境事件非 GUI 缺陷，拉起续测
        if not (adb_shell(c, args.serial, f"pidof {PKG}").stdout or "").strip():
            launch_app(c, args.serial)
        now = cpu_count(c, args.serial)
        if now <= last:  # 60s 窗口零增长 = 采样管线停滞
            stall_windows += 1
        last = now
    ck.check("s1 采样全程无 60s 停滞窗", stall_windows == 0,
             f"stall_windows={stall_windows} cpu {c0}→{last}")
    ck.check("s1 末态仍在采样", bool(c.state(args.serial).get("samplingRunning")))
    # 恢复默认速率留给后续场景；s1 不停止采样（贯穿压测）


def scenario_s2(c, ck, mon, args):
    """高频数据：重启到 50ms 间隔，图表绘制压力 + 悬停读数。"""
    mon.set_scenario("s2")
    p = page(args.serial)
    stop_sampling(c, args.serial)
    c.action("select", selector=f"{p} .interval-select", value="50")
    if not start_sampling(c, ck, args.serial, "s2 50ms 开始监控"):
        return
    c0 = cpu_count(c, args.serial)
    # 高频绘制下悬停仍可读数（窗口须可见——锁屏/全遮挡 rAF 冻结为环境 SKIP）
    time.sleep(5)  # 攒若干点再悬停
    tip = None
    if ensure_visible(c):
        for _ in range(15):
            g = c.eval(
                f"(()=>{{const el=document.querySelector('{p} canvas.chart-cpu');"
                f"const r=el.getBoundingClientRect();"
                f"return {{x:r.x+r.width/2,y:r.y+r.height/2,w:r.width}}}})()")
            if g.get("w", 0) <= 0:
                time.sleep(0.5)
                continue
            c.action("hover", selector=f"{p} canvas.chart-cpu", x=g["x"], y=g["y"])
            time.sleep(0.4)
            tip = (c.state(args.serial).get("charts") or {}).get("cpu", {}).get("hover")
            if tip and tip.get("rows"):
                break
        ck.check("s2 高频下悬停出数", bool(tip and tip.get("rows")),
                 f"tooltip={str(tip)[:120]}")
    else:
        ck.skip("s2 高频下悬停出数", "窗口不可见（rAF 冻结，GUI-TEST-PLAN #6）")
    time.sleep(args.s2_minutes * 60)
    ck.check("s2 50ms series 高速增长", cpu_count(c, args.serial) >= c0 + 600,
             f"cpu {c0}→{cpu_count(c, args.serial)}（50ms 单 PID 应 ~20/s）")
    stop_sampling(c, args.serial)
    c.action("select", selector=f"{p} .interval-select", value="1000")
    start_sampling(c, ck, args.serial, "s2 恢复 1000ms 监控")


def scenario_s3(c, ck, mon, args):
    """logcat 洪泛：全机 V + XPERFFLOOD 注入；可见→隐藏（后端暂停事件）→切回补发。"""
    mon.set_scenario("s3")
    p = page(args.serial)
    c.action("click", selector=f"{p} .tab.subtab[data-tab='logcat']")
    c.poll(lambda: c.state(args.serial).get("activeTab") == "logcat", timeout=8)
    c.action("input", selector=f"{p} .logcat-text", value="")
    c.action("select", selector=f"{p} .logcat-level", value="V")
    c.action("check", selector=f"{p} .logcat-bypkg", value=False)  # 全机
    _, r = c.action("click", selector=f"{p} .logcat-btn")
    ck.check("s3 开始抓取", r.get("ok") and bool(c.poll(
        lambda: c.state(args.serial).get("logcatRunning"), timeout=15)))
    flood = Flooder(c, args.serial)
    flood.start()
    try:
        # 可见段：洪泛下行进 ring buffer
        hit = c.poll(lambda: "XPERFFLOOD" in str(
            c.state(args.serial, logcat_tail=50).get("logcatTail")), timeout=30)
        ck.check("s3 洪泛行进入视图", bool(hit))
        time.sleep(args.s3_seconds)
        buf = logcat_buf_len(c, args.serial)
        ck.check("s3 ring buffer 封顶 2000", buf <= 2000, f"buf={buf}")
        # 隐藏段：切走 tab，后端暂停推送
        c.action("click", selector=f"{p} .tab.subtab[data-tab='perf']")
        c.poll(lambda: c.state(args.serial).get("activeTab") == "perf", timeout=8)
        mon.set_scenario("s3-hidden")
        time.sleep(3)
        i0 = last_flood_idx(c, args.serial)
        b0 = logcat_buf_len(c, args.serial)
        time.sleep(args.s3_seconds)
        b1 = logcat_buf_len(c, args.serial)
        ck.check("s3 隐藏段 buf 冻结（后端暂停事件）", b1 - b0 <= 20,
                 f"buf {b0}→{b1}（洪泛中 Δ={b1 - b0}）")
        # 切回：pending 补发——buf 已满（ring 2000），以洪泛序号前进佐证新行到达
        c.action("click", selector=f"{p} .tab.subtab[data-tab='logcat']")
        mon.set_scenario("s3")
        flushed = c.poll(lambda: last_flood_idx(c, args.serial) > i0 + 50 and
                         last_flood_idx(c, args.serial), timeout=15)
        ck.check("s3 切回补发（洪泛序号前进）", bool(flushed),
                 f"flood_idx {i0}→{last_flood_idx(c, args.serial)}")
    finally:
        flood.stop()
        if c.state(args.serial).get("logcatRunning"):
            c.action("click", selector=f"{p} .logcat-btn")
            c.poll(lambda: not c.state(args.serial).get("logcatRunning"), timeout=10)
    ck.check("s3 停止抓取", not c.state(args.serial).get("logcatRunning"))


def scenario_s4(c, ck, mon, args):
    """trace 15s + stack 15s 并发录制，期间采样节拍不受挤占。"""
    mon.set_scenario("s4")
    p = page(args.serial)
    if not c.state(args.serial).get("samplingRunning"):
        start_sampling(c, ck, args.serial, "s4 采样兜底启动")
    c.action("click", selector=f"{p} .tab.subtab[data-tab='trace']")
    c.action("select", selector=f"{p} .trace-seconds", value="15")
    _, r1 = c.action("click", selector=f"{p} .trace-start-btn")
    c.action("click", selector=f"{p} .tab.subtab[data-tab='stack']")
    c.action("select", selector=f"{p} .stack-seconds", value="15")
    _, r2 = c.action("click", selector=f"{p} .stack-start-btn")
    ck.check("s4 trace+stack 并发发起", r1.get("ok") and r2.get("ok"),
             f"trace={r1.get('error', '')} stack={r2.get('error', '')}"[:120])
    # 录制期间采样 series 持续增长（节拍不被深挖饿死）
    c0 = cpu_count(c, args.serial)
    grew = c.poll(lambda: cpu_count(c, args.serial) >= c0 + 8 and cpu_count(c, args.serial),
                  timeout=30, interval=2)
    ck.check("s4 录制期间采样持续增长", bool(grew), f"cpu {c0}→{cpu_count(c, args.serial)}")
    # 报告文本 >200 字符 = done（录制中进度文案远短于此）
    rep_ok = c.poll(lambda: len(c.eval(
        f"document.querySelector('{p} .trace-report').textContent")) > 200,
        timeout=180, interval=3)
    srep_ok = c.poll(lambda: len(c.eval(
        f"document.querySelector('{p} .stack-report').textContent")) > 200,
        timeout=120, interval=3)
    ck.check("s4 trace 报告产出", bool(rep_ok))
    ck.check("s4 stack 报告产出", bool(srep_ok))


def scenario_s5(c, ck, mon, args):
    """多设备并行采样 + 0.4s 快切设备页 ×150。"""
    mon.set_scenario("s5")
    online = {d["serial"] for d in c.status().get("devices", [])}
    extras = [s for s in EXTRA_SERIALS if s in online]
    if not extras:
        ck.skip("s5 多设备并行", "无在线副设备（XPERF_TEST_EXTRA_SERIALS）")
        return
    serials = [args.serial] + extras
    counts0 = {}
    for s in serials:
        launch_app(c, s)
        if not c.state(s).get("samplingRunning"):
            start_sampling(c, ck, s, f"s5 {s} 开始监控")
        counts0[s] = cpu_count(c, s)
    # 快切页（eval 直调 switchDevice，比 click 更密；渲染路径同）
    n_sw, sw_err = 0, 0
    t_end = time.time() + 60
    i = 0
    while time.time() < t_end and n_sw < 150:
        tgt = serials[i % len(serials)]
        try:
            c.eval(f"app.switchDevice('{tgt}')")
            if c.eval("app.active") != tgt:
                sw_err += 1
        except AssertionError:
            sw_err += 1
        n_sw += 1
        i += 1
        time.sleep(0.4)
    ck.check("s5 快切页无错乱", sw_err == 0, f"switches={n_sw} err={sw_err}")
    ok_growth = all(cpu_count(c, s) > counts0[s] for s in serials)
    ck.check("s5 各设备 series 隔离增长", ok_growth,
             str({s: cpu_count(c, s) - counts0[s] for s in serials}))
    for s in extras:
        stop_sampling(c, s)
    c.eval(f"app.switchDevice('{args.serial}')")


def scenario_s6(c, ck, mon, args):
    """浏览器按钮连点：open-perf ×10——首次后 disabled+冷却挡住，GUI 保持存活。"""
    mon.set_scenario("s6")
    p = page(args.serial)
    c.eval(f"app.switchDevice('{args.serial}')")
    c.action("click", selector=f"{p} .tab.subtab[data-tab='trace']")
    en = c.eval(f"!document.querySelector('{p} .open-perf-btn').disabled")
    if not en:  # 无 trace 产物（s4 未跑过）——录 5s 兜底
        c.action("select", selector=f"{p} .trace-seconds", value="5")
        c.action("click", selector=f"{p} .trace-start-btn")
        c.poll(lambda: c.eval(f"!document.querySelector('{p} .open-perf-btn').disabled"),
               timeout=120, interval=3)
    c.action("click", selector=f"{p} .open-perf-btn")  # 第一击真实触发（开一次浏览器）
    disabled_now = c.eval(f"document.querySelector('{p} .open-perf-btn').disabled")
    for _ in range(9):  # 冷却/禁用期内连点
        c.action("click", selector=f"{p} .open-perf-btn")
        time.sleep(0.15)
    ck.check("s6 首击后即禁用（防抖）", bool(disabled_now))
    ck.check("s6 连点后 GUI 存活", c.alive() and bool(c.status().get("frontend_ready")))


def scenario_s7(c, ck, mon, args):
    """adb server 冻结注入（仅 ssh 模式）：SIGSTOP 宿主 adb server 45s——信道
    「无数据亦无 EOF」半开；恢复后经入向静止看门狗（15s）→ 重连自愈。

    2026-09-22 压测实锤的野外观测：adb server 短暂卡死曾致 SS3 采样永久静默
    （series 冻结、前后端均无感知），本场景是该回归的确定性守卫。
    """
    mon.set_scenario("s7")
    if remote_mode(c) != "ssh":
        ck.skip("s7 adb 冻结注入", "非 ssh 模式（adb server 在本机，冻结影响面过大）")
        return
    if not c.state(args.serial).get("samplingRunning"):
        start_sampling(c, ck, args.serial, "s7 采样兜底启动")
    r = subprocess.run(["ssh", REMOTE_ADB, "pgrep -f fork-server | head -3"],
                       capture_output=True, text=True, timeout=15)
    pids = r.stdout.split()
    if not pids:
        ck.skip("s7 adb 冻结注入", "远端找不到 adb server 进程")
        return
    frozen = False
    c_pre = cpu_count(c, args.serial)
    c_mid = c_pre
    try:
        subprocess.run(["ssh", REMOTE_ADB, f"kill -STOP {' '.join(pids)}"],
                       capture_output=True, timeout=15)
        frozen = True
        time.sleep(3)  # 等冻结生效（在途事件落地）后再取基线，判据只看纯冻结窗口
        c_pre = cpu_count(c, args.serial)
        time.sleep(45)  # > 看门狗 15s + daemon 30s 无 ping 断连窗口
        # 冻结期间 series 应完全静止（adb server 停泵零字节流动；少量积压容忍）
        c_mid = cpu_count(c, args.serial)
        ck.check("s7 冻结期间 series 静止", c_mid - c_pre <= 3,
                 f"cpu {c_pre}→{c_mid}")
    finally:
        if frozen:
            subprocess.run(["ssh", REMOTE_ADB, f"kill -CONT {' '.join(pids)}"],
                           capture_output=True, timeout=15)
    recovered = c.poll(
        lambda: cpu_count(c, args.serial) > c_mid + 3 and cpu_count(c, args.serial),
        timeout=90, interval=5)
    ck.check("s7 恢复后自愈（看门狗→重连→数据流恢复）", bool(recovered),
             f"cpu {c_mid}→{cpu_count(c, args.serial)}（90s 内增长即过）")


SCENARIOS = {"s1": scenario_s1, "s2": scenario_s2, "s3": scenario_s3,
             "s4": scenario_s4, "s5": scenario_s5, "s6": scenario_s6,
             "s7": scenario_s7}


# ---------------------------------------------------------------------------
# 汇总：分段基线 + 资源判据
# ---------------------------------------------------------------------------

def summarize(c, ck, mon, args):
    mon.set_scenario("done")
    print("\n" + "=" * 78)
    print(f"资源基线（{os.path.basename(mon.out_path)}；5s/点，rss 单位 KB）")
    print(f"{'场景':<10} {'gui_rss first→last':>20} {'web_rss first→last':>20} "
          f"{'dom first→last':>16} {'pts last':>9} {'api失败':>6}")
    for sc in [s for s in dict.fromkeys(r["sc"] for r in mon.records)]:
        rows = mon.segment(sc)
        if not rows:
            continue
        g, w, d, pt = (seg_stat(rows, k) for k in ("gui_rss_kb", "web_rss_kb", "dom", "pts"))
        api_bad = sum(1 for r in rows if r.get("api") == 0)
        f = lambda s: f"{s['first']}→{s['last']}" if s else "-"
        print(f"{sc:<10} {f(g):>20} {f(w):>20} {f(d):>16} "
              f"{(pt['last'] if pt else '-'):>9} {api_bad:>6}")

    ck.check("全程 debug API 无失联", mon.api_fails == 0, f"fails={mon.api_fails}")
    s1 = mon.segment("s1")
    if len(s1) >= 4:
        d = seg_stat(s1, "dom")
        if d:
            ck.check("s1 DOM 节点稳定（canvas 渲染不堆 DOM）",
                     d["last"] <= d["first"] + 100, f"dom {d['first']}→{d['last']} max={d['max']}")
        w = seg_stat(s1, "web_rss_kb")
        if w and w["first"] > 0:
            ratio = w["last"] / w["first"]
            ck.check("s1 WebKit RSS 增长 <50%", ratio < 1.5,
                     f"{w['first']}→{w['last']} KB（×{ratio:.2f}，max={w['max']}）")
        pt = seg_stat(s1, "pts")
        if pt:
            ck.check("s1 series 总点数增长", pt["last"] > pt["first"] + 100,
                     f"pts {pt['first']}→{pt['last']}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--scenario", default="all",
                    help="逗号分隔场景 id（默认 all=s1..s6）")
    ap.add_argument("--serial", default="6eb792dfb0f", help="主测试设备（默认 SS3）")
    ap.add_argument("--s1-minutes", type=float, default=10)
    ap.add_argument("--s2-minutes", type=float, default=3)
    ap.add_argument("--s3-seconds", type=float, default=60, help="logcat 可见/隐藏每段秒数")
    ap.add_argument("--keep-running", action="store_true", help="结束后不停止主设备采样")
    args = ap.parse_args()

    c = Client()
    ck = Checker()
    assert c.wait_frontend(30), "前端未就绪"
    online = {d["serial"] for d in c.status().get("devices", [])}
    assert args.serial in online, f"主设备不在线: {args.serial}（在线: {online}）"

    ts = time.strftime("%Y%m%d_%H%M%S")
    mon = ResourceMonitor(c, f"/tmp/xperf-gui-stress-{ts}.jsonl")
    mon.start()
    print(f"资源曲线: {mon.out_path}")
    t0 = time.time()
    try:
        # 幂等清场：主/副设备采样与 logcat 全停
        for s in [args.serial] + [x for x in EXTRA_SERIALS if x in online]:
            stop_sampling(c, s)
        which = list(SCENARIOS) if args.scenario == "all" else args.scenario.split(",")
        for name in which:
            print(f"\n--- {name} ---", flush=True)
            SCENARIOS[name](c, ck, mon, args)
    finally:
        mon.set_scenario("teardown")
        if not args.keep_running:
            for s in [args.serial] + [x for x in EXTRA_SERIALS if x in online]:
                stop_sampling(c, s)
        time.sleep(_MONITOR_INTERVAL)  # teardown 段留一个采样点
        summarize(c, ck, mon, args)
        mon.stop()
    print(f"\n总时长: {time.time() - t0:.0f}s")
    ck.summary("stress")
    ck.exit()


if __name__ == "__main__":
    main()

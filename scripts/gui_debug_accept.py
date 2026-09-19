#!/usr/bin/env python3
"""xperf GUI 调试接口验收——全程 API 驱动，零 CGEvent/截图。

前置：GUI 已启动（可带 --remote <host> --device <serial>），目标设备在线，
测试应用已安装（默认 gltf viewer，XPERF_IT_PACKAGE 覆盖，与集成测试同变量）。
流程：发现文件 → 鉴权 → status → dom → 填包名 → 打开应用（冷启动）→ 开始监控
→ series 增长 → hover 图表读 tooltip → series 取值 → 切日志 tab → 开 logcat
→ 文本过滤热切换 → 停 logcat → eval 一致性 → 停止监控 → 确认序列停增。
注意：运行期间勿动物理鼠标/窗口（真实鼠标事件与合成 hover 竞争会干扰读数）。
"""
import json, os, sys, time, urllib.request, urllib.error

if len(sys.argv) < 2:
    sys.exit("用法: gui_debug_accept.py <device serial>  # 前置：GUI 已启动且该设备在线")
SERIAL = sys.argv[1]
CFG = json.load(open(os.path.expanduser("~/.config/xperf/gui-debug.json")))
BASE = f"http://127.0.0.1:{CFG['port']}"
TOK = CFG["token"]
PKG = os.environ.get("XPERF_IT_PACKAGE", "com.google.android.filament.gltf")
PAGE = f'.device-page[data-serial="{SERIAL}"]'

fails = []
def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name + (f"  | {detail}" if detail else ""))
    if not cond: fails.append(name)

def req(method, path, body=None):
    r = urllib.request.Request(BASE + path, method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"X-Xperf-Token": TOK, "content-type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=15) as resp:
            return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read())

def poll(name, fn, timeout=25, interval=0.5):
    t0 = time.time()
    while time.time() - t0 < timeout:
        v = fn()
        if v: return v
        time.sleep(interval)
    return None

# 0. 鉴权
r = urllib.request.Request(BASE + "/api/status")
try:
    urllib.request.urlopen(r, timeout=5); code = 200
except urllib.error.HTTPError as e: code = e.code
check("无 token → 401", code == 401, f"code={code}")

# 1. 等前端就绪
st = poll("frontend_ready", lambda: req("GET", "/api/status")[1].get("data", {}).get("frontend_ready"), 30)
check("frontend_ready", bool(st))

# 2. status：设备在线（后端会话在首次命令触达时才建，采样启动后再断言）
_, st = req("GET", "/api/status")
d = st["data"]
devs = [x for x in d["devices"] if not x["is_gateway"]]
check("设备在线", any(x["serial"] == SERIAL for x in devs), f"devices={[x['serial'] for x in devs]}")
check("remote=ssh", d["remote"]["mode"] == "ssh" and d["remote"]["alive"])

# 3. dom：设备页结构 + rect
code, dom = req("GET", f"/api/dom?selector={PAGE.replace('[', '%5B').replace(']', '%5D').replace('\"', '%22')}&depth=2")
ok = dom.get("ok") and dom["data"]["tree"]["rect"][2] > 100
check("dom 设备页 rect", bool(ok), f"rect={dom['data']['tree']['rect'] if dom.get('ok') else dom}")

# 4. 填包名 + 打开应用（冷启动）
req("POST", "/api/action", {"op": "input", "selector": f"{PAGE} .package-input", "value": PKG})
code, r = req("POST", "/api/action", {"op": "click", "selector": f"{PAGE} .launch-btn"})
check("点击打开应用", r.get("ok"), str(r)[:120])
cs = poll("coldStarts", lambda: (req("GET", f"/api/state?serial={SERIAL}")[1]["data"].get("session") or {}).get("coldStarts"), 30)
check("冷启动面板出数", bool(cs), f"coldStarts={cs}")

# 5. 开始监控 → series 增长
req("POST", "/api/action", {"op": "click", "selector": f"{PAGE} .start-btn"})
def cpu_count():
    _, s = req("GET", f"/api/state?serial={SERIAL}")
    sess = s["data"].get("session") or {}
    ser = (sess.get("charts") or {}).get("cpu", {}).get("series", {})
    return sum(v.get("count", 0) for v in ser.values())
check("samplingRunning", bool(poll("sampling", lambda: (req("GET", f"/api/state?serial={SERIAL}")[1]["data"].get("session") or {}).get("samplingRunning"), 15)))
_, st2 = req("GET", "/api/status")
ss = {s["serial"]: s for s in st2["data"]["sessions"]}
check("后端会话已建且采样中", SERIAL in ss and ss[SERIAL]["sampling"] and not ss[SERIAL]["agent_building"], str(ss.get(SERIAL))[:120])
c0 = cpu_count()
grown = poll("series 增长", lambda: cpu_count() >= max(3, c0 + 2) and cpu_count(), 20)
check("CPU series 增长", bool(grown), f"count {c0} → {cpu_count()}")

# 6. hover CPU 图表 → tooltip（rAF 合帧；真实用户鼠标是连续移动的——轮询式反复 hover 模拟）
# 前置：确保 perf tab 激活（上轮运行可能停在日志 tab——隐藏 tab 的 canvas 零尺寸，hover 无意义）
req("POST", "/api/action", {"op": "click", "selector": f"{PAGE} .tab.subtab[data-tab='perf']"})
time.sleep(0.5)
def tooltip():
    _, s = req("GET", f"/api/state?serial={SERIAL}")
    return ((s["data"].get("session") or {}).get("charts", {}).get("cpu", {}) or {}).get("hover")
tip = None
for _ in range(20):
    _, r = req("POST", "/api/eval", {"expr": f"(()=>{{const c=document.querySelector('{PAGE} canvas.chart-cpu');const r=c.getBoundingClientRect();const s=app.sessions.get('{SERIAL}').charts.cpu;return {{x:r.x+r.width/2,y:r.y+r.height/2,w:r.width,vis:r.width>0,plot:!!s.plot,active:'{SERIAL}'===app.active}}}})()"})
    g = r["data"]
    if not g.get("vis"):
        time.sleep(0.5); continue
    req("POST", "/api/action", {"op": "hover", "selector": f"{PAGE} canvas.chart-cpu", "x": g["x"], "y": g["y"]})
    time.sleep(0.4)
    tip = tooltip()
    if tip and tip.get("rows"):
        break
check("悬停 tooltip 出数", bool(tip and tip.get("rows")), f"geom={g} tooltip={str(tip)[:160]}")

# 7. series 端点取值
code, sr = req("GET", f"/api/series?serial={SERIAL}&metric=cpu&tail=3")
ok = sr.get("ok") and any(len(v) >= 1 for v in sr["data"].values())
check("/api/series tail", bool(ok), str(sr.get("data"))[:150])
if tip:
    code, sr2 = req("GET", f"/api/series?serial={SERIAL}&metric=cpu&at={int(tip['t'])}")
    ok = sr2.get("ok") and all(v.get("point") for v in sr2["data"].values())
    check("/api/series at(tooltip 时刻)", bool(ok), str(sr2.get("data"))[:150])

# 8. 切日志 tab
req("POST", "/api/action", {"op": "click", "selector": f"{PAGE} .tab.subtab[data-tab='logcat']"})
tab = poll("activeTab", lambda: (req("GET", f"/api/state?serial={SERIAL}")[1]["data"].get("session") or {}).get("activeTab") == "logcat", 5)
check("切到日志 tab", bool(tab))

# 9. 开 logcat → 尾行非空 → 文本过滤热切换
req("POST", "/api/action", {"op": "click", "selector": f"{PAGE} .logcat-btn"})
run = poll("logcatRunning", lambda: (req("GET", f"/api/state?serial={SERIAL}")[1]["data"].get("session") or {}).get("logcatRunning"), 10)
check("logcat 启动", bool(run))
lines = poll("logcat 行", lambda: (req("GET", f"/api/state?serial={SERIAL}&logcat_tail=5")[1]["data"].get("session") or {}).get("logcatTail"), 15)
check("logcat 出行", bool(lines), f"tail={str(lines)[-120:]}" if lines else "")
req("POST", "/api/action", {"op": "input", "selector": f"{PAGE} .logcat-text", "value": "GC"})
# ring buffer 保留过滤前行、respawn 前后有在途行竞争——轮询到「尾部 3 行非标记行全命中」
# （过滤词用 GC：该应用 GC 日志密集，filament 词太稀疏会超时）
def filter_ok():
    _, s = req("GET", f"/api/state?serial={SERIAL}&logcat_tail=2000")
    tail = (s["data"].get("session") or {}).get("logcatTail") or []
    last_marker = max((i for i, l in enumerate(tail) if l.startswith("#")), default=-1)
    after = [l for l in (tail[last_marker + 1:] if last_marker >= 0 else tail)]
    if len(after) < 3:
        return None
    return all("gc" in l.lower() for l in after[-3:]) and after[-3:]
fok = poll("过滤生效", filter_ok, 20)
check("文本过滤热切换生效", bool(fok), f"尾 3 行={str(fok)[-150:]}" if fok else "超时")

# 10. 停 logcat + eval 一致性
req("POST", "/api/action", {"op": "click", "selector": f"{PAGE} .logcat-btn"})
poll("logcat 停止", lambda: not (req("GET", f"/api/state?serial={SERIAL}")[1]["data"].get("session") or {}).get("logcatRunning"), 8)
_, ev = req("POST", "/api/eval", {"expr": "({size: app.sessions.size, has: app.sessions.has('" + SERIAL + "')})"})
check("eval 前端会话一致性", ev.get("data", {}).get("has") and ev["data"]["size"] == len(devs), f"{ev} devs={len(devs)}")

# 11. 停止监控 → 序列停增
req("POST", "/api/action", {"op": "click", "selector": f"{PAGE} .stop-btn"})
stop = poll("停止监控", lambda: not (req("GET", f"/api/state?serial={SERIAL}")[1]["data"].get("session") or {}).get("samplingRunning"), 10)
check("停止监控", bool(stop))
# 停止确认后在途事件仍可到（后端下一轮节拍才退出）——等 2s 再开干净观察窗
time.sleep(2)
c1 = cpu_count(); time.sleep(2.5); c2 = cpu_count()
check("停止后序列停增", c1 == c2, f"{c1} vs {c2}")

print()
print("=" * 50)
print("FAIL:", fails if fails else "无 —— 全部通过")
sys.exit(1 if fails else 0)

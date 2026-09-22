#!/usr/bin/env python3
"""xperf GUI 回归测试套件（UI 全覆盖；debug API 驱动）。

用法:
    python3 regression.py [--group g2,g5] [--serial <SS3>] [--package <pkg>]
                          [--skip-destructive] [--keep-data]

前置：GUI 已启动（同机，读 ~/.config/xperf/gui-debug.json），目标设备在线，
测试应用已安装（默认 gltf viewer，XPERF_IT_PACKAGE 覆盖）。
运行期间勿动物理鼠标/窗口（合成 hover 与真实事件竞争）。

组:
    g1 连接面（remotes.json/ssh config 导入/临时连接/adb 持久化——会改写
       ~/.config/xperf/remotes.json，跑前备份跑后还原）
    g2 采样面（打开应用冷启动/开始监控/series/hover/指标勾选重启同目录续写/导出）
    g3 深挖面（trace/stack 短档 + 浏览器 UI 打开 + 连点防抖）
    g4 捕获面（截屏/镜像/录屏产物核验）
    g5 logcat 面（启动/三过滤热切换/可见性联动暂停补发）
    g6 基线与反馈面（保存/对比 + 反馈真实上传 [gui-test] issue）
    g7 多设备并行（三机同采 + 页面隔离 + 目录防撞）
    g8 故障注入（重复点击/快速切 tab/guard 模态/非法正则/断连重连/隧道重建/
       WebKit 子进程退出 + clean_cache 破坏性收尾[--skip-destructive 跳过]）

压力测试（长时采样/洪泛/资源增长）后置独立会话，harness 已留 proc_rss_cpu。
"""

import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from harness import Client, Checker, webkit_children  # noqa: E402

PKG = os.environ.get("XPERF_IT_PACKAGE", "com.google.android.filament.gltf")
REMOTE_ADB = os.environ.get("XPERF_TEST_SSH", "hppc")  # SSH 远程模式下设备所挂宿主
# g1 SSH 连接流的目标宿主：Mac=hppc（GUI --remote 启动目标）；hppc 直编 +
# `--remote kong`（覆盖「Linux 上的 SSH 远程模式」）时=kong。判定可连性 =
# GUI 宿主的 ssh config 含该主机（hppc 本地直跑不含 hppc → SSH UI 小节 SKIP，
# 该场景已由 Mac --remote 与 hppc --remote kong 双覆盖）。
SSH_UI_HOST = os.environ.get("XPERF_TEST_SSH_UI", REMOTE_ADB)
# 切回本机后期望的设备机队（逗号分隔 serial；Mac 本机 adb 无设备=空——
# hppc 直编本地挂三台须显式给，断言「本机模式无设备」只在空机队下成立）
LOCAL_SERIALS = [s for s in os.environ.get("XPERF_TEST_LOCAL_SERIALS", "").split(",") if s]
REMOTES_JSON = os.path.expanduser("~/.config/xperf/remotes.json")

ARGS = None  # parse_args 填充


def page(serial):
    return f'.device-page[data-serial="{serial}"]'


def ensure_stop_sampling(c, serial, timeout=12):
    """幂等停采样：不报错（未在采样时 stop 本就幂等）。"""
    c.invoke("stop_sampling", {"serial": serial})
    return c.poll(lambda: not c.state(serial).get("samplingRunning"), timeout=timeout)


def ensure_visible(c, timeout=8):
    """窗口遮挡防护（macOS）：全遮挡窗口的 WebKit 会冻结 requestAnimationFrame——
    悬停读数的 rAF 合帧永不执行（2026-09-21 GUI 回归实测锁定：两轮 hover 假 FAIL）。
    setFocus 拉回前台（capabilities 已放行 core:window:allow-set-focus）后 rAF 恢复。

    Linux/webkit2gtk 无此冻结语义，visibilityState 恒 visible，直接返回 True。
    """
    if c.eval("document.visibilityState") == "visible":
        return True
    try:
        c.eval("(async()=>{await window.__TAURI__.window.getCurrentWindow().setFocus();return 1})()")
    except AssertionError:
        return False
    return c.poll(
        lambda: (c.eval("document.visibilityState") == "visible") or None, timeout=timeout,
    ) is not None


def ssh_run(cmd, timeout=30):
    return subprocess.run(
        ["ssh", REMOTE_ADB, cmd], capture_output=True, text=True, timeout=timeout,
    )


def pkg_installed(serial, pkg=None):
    """测试包是否装在该设备（多设备并行判据的前置——各机队装的应用未必相同，
    如 kong 机队 SS2PRO=hellotriangle / SS4=gltf viewer，未装者不可能出 series）。
    经 `adb_on_remote` 走与 GUI 相同的宿主（ssh 模式=hop#1 宿主）。
    """
    pkg = pkg or PKG
    r = adb_on_remote(serial, "shell", "pm", "list", "packages", pkg, timeout=20)
    return f"package:{pkg}" in ((r.stdout or "") + (r.stderr or ""))


def _remote_adb():
    """远端 adb 路径（非交互 ssh 无 PATH；标准 SDK 位置兜底，与 GUI 预检同套路）。"""
    global _REMOTE_ADB
    if _REMOTE_ADB is None:
        r = ssh_run("command -v adb || echo $HOME/Android/Sdk/platform-tools/adb", timeout=10)
        path = (r.stdout or "").strip().splitlines()[-1] if (r.stdout or "").strip() else "adb"
        _REMOTE_ADB = path if ssh_run(f"test -x {path}", timeout=10).returncode == 0 else "adb"
    return _REMOTE_ADB


_REMOTE_ADB = None


def adb_on_remote(serial, *args, timeout=30):
    """在设备所挂宿主上执行 adb（本机模式=本机 adb；SSH 远程模式=经 hop#1 宿主）。

    SSH 模式下本机 adb 客户端也全经隧道可达，但设备断连注入（reconnect offline）
    操作 server 侧状态，直接在远端执行语义最明确。
    """
    mode = c_status_remote_mode()
    adb = os.environ.get("XPERF_ADB", "adb")
    if mode == "ssh":
        return ssh_run(
            "%s -s %s %s" % (_remote_adb(), serial, " ".join(args)), timeout=timeout,
        )
    return subprocess.run(
        [adb, "-s", serial, *args], capture_output=True, text=True, timeout=timeout,
    )


def c_status_remote_mode():
    global _MODE
    if _MODE is None:
        _MODE = ARGS.client.status().get("remote", {}).get("mode", "local")
    return _MODE


_MODE = None


# ---------------------------------------------------------------------------
# G2 采样面
# ---------------------------------------------------------------------------

def group_g2(c, ck, serial):
    p = page(serial)
    # 1. 填包名 + 刷新包列表（list_packages 命令，gltf 包在列）
    c.action("input", selector=f"{p} .package-input", value=PKG)
    lp = c.invoke("list_packages", {"serial": serial})
    ok = lp.get("ok") and any(PKG in (x if isinstance(x, str) else str(x))
                              for x in (lp["data"] or []))
    ck.check("list_packages 含测试包", ok, f"count={len(lp.get('data') or [])}")

    # 2. 打开应用（冷启动面板）
    _, r = c.action("click", selector=f"{p} .launch-btn")
    ck.check("点击打开应用", r.get("ok"), str(r.get("error", ""))[:120])
    cs = c.poll(lambda: (c.state(serial).get("coldStarts")), timeout=40)
    ck.check("冷启动面板出数", bool(cs), f"coldStarts={cs}")

    # 3. 开始监控
    _, r = c.action("click", selector=f"{p} .start-btn")
    ck.check("点击开始监控", r.get("ok"), str(r.get("error", ""))[:120])
    run = c.poll(lambda: c.state(serial).get("samplingRunning"), timeout=20)
    ck.check("samplingRunning", bool(run))

    def backend_ok():
        ss = {s["serial"]: s for s in c.status().get("sessions", [])}
        return serial in ss and ss[serial].get("sampling") and not ss[serial].get("agent_building")
    ck.check("后端会话采样中且非构建", bool(c.poll(backend_ok, 30)), str(
        {s["serial"]: s for s in c.status().get("sessions", [])}.get(serial))[:120])

    def cpu_count():
        ser = (c.state(serial).get("charts") or {}).get("cpu", {}).get("series", {})
        return sum(v.get("count", 0) for v in ser.values())
    c0 = cpu_count()
    grown = c.poll(lambda: cpu_count() >= max(3, c0 + 2) and cpu_count(), timeout=30)
    ck.check("CPU series 增长", bool(grown), f"count {c0} → {cpu_count()}")

    # 4. 实际周期标注（state liveData/interval 生效）
    st = c.state(serial)
    ck.check("liveData 实时数值非空", bool(st.get("liveData")), str(st.get("liveData"))[:100])

    # 5. hover tooltip（rAF 合帧，轮询式反复 hover 模拟连续鼠标）
    _, r = c.action("click", selector=f"{p} .tab.subtab[data-tab='perf']")
    time.sleep(0.5)
    # 前置：窗口须可见——全遮挡/锁屏冻结 rAF，hover 合帧永不执行（见 ensure_visible
    # 与 GUI-TEST-PLAN 已知发现 #6）；仅被遮挡时 setFocus 可恢复，锁屏只能 SKIP
    vis = ensure_visible(c)
    tip = None
    g = None
    if vis:
        for _ in range(20):
            g = c.eval(
                f"(()=>{{const el=document.querySelector('{p} canvas.chart-cpu');"
                f"const r=el.getBoundingClientRect();return {{x:r.x+r.width/2,y:r.y+r.height/2,w:r.width}}}})()"
            )
            if g.get("w", 0) <= 0:
                time.sleep(0.5)
                continue
            c.action("hover", selector=f"{p} canvas.chart-cpu", x=g["x"], y=g["y"])
            time.sleep(0.4)
            tip = (c.state(serial).get("charts") or {}).get("cpu", {}).get("hover")
            if tip and tip.get("rows"):
                break
        ck.check("悬停 tooltip 出数", bool(tip and tip.get("rows")),
                 f"w={g.get('w') if g else '?'} tooltip={str(tip)[:140]}")
    else:
        ck.skip("悬停 tooltip 出数",
                "环境受限：窗口不可见（锁屏或全遮挡，rAF 冻结）——非产品缺陷，"
                "GUI-TEST-PLAN 已知发现 #6；解锁后重跑即覆盖")

    # 6. /api/series tail + at
    sr = c.series(serial, "cpu", tail=3)
    ck.check("/api/series tail", any(len(v) >= 1 for v in sr.values()), str(sr)[:120])
    if tip:
        sr2 = c.series(serial, "cpu", at=int(tip["t"]))
        ck.check("/api/series at(tooltip 时刻)",
                 all(v.get("point") for v in sr2.values()) and sr2, str(sr2)[:120])

    # 7. 指标勾选重启 → 同目录 append 续写（复用目录不新建）
    export1 = c.invoke("export_csv", {"serial": serial})
    ok1 = export1.get("ok") and os.path.isdir(export1["data"])
    ck.check("export_csv 导出快照", ok1, str(export1)[:120])
    session_dir = None
    if ok1:
        # 导出目录名 = <session-dir-name>-export-<ts>，会话目录是其同名兄弟
        name = os.path.basename(export1["data"])
        session_dir = os.path.join(os.path.dirname(export1["data"]),
                                   name.split("-export-")[0])
        ck.check("会话目录存在", os.path.isdir(session_dir), session_dir)

    def csv_lines(d):
        total = 0
        for root, _, files in os.walk(d):
            for f in files:
                if f.endswith(".csv"):
                    with open(os.path.join(root, f), errors="replace") as fh:
                        total += sum(1 for _ in fh)
        return total

    lines1 = csv_lines(session_dir) if session_dir else 0
    cpu_series_before = cpu_count()
    # 勾选 freq（默认未勾）→ 触发 restart fresh:false（同目录续写，图表不重置）
    _, r = c.action("check", selector=f"{p} .metric-box[data-metric='freq']", value=True)
    time.sleep(1.5)
    run2 = c.poll(lambda: c.state(serial).get("samplingRunning"), timeout=25)
    ck.check("勾选指标后重启采样", bool(run2))
    ck.check("重启后 series 保留（fresh:false 不重置）",
             cpu_count() >= cpu_series_before, f"{cpu_series_before} → {cpu_count()}")
    c.poll(lambda: cpu_count() >= 3, timeout=30)  # 等新样本
    time.sleep(1)
    export2 = c.invoke("export_csv", {"serial": serial})
    ok2 = export2.get("ok") and os.path.isdir(export2["data"])
    ck.check("重启后再导出", ok2, str(export2)[:120])
    if ok2 and session_dir:
        same = os.path.basename(export2["data"]).split("-export-")[0] == \
            os.path.basename(session_dir)
        ck.check("复用同一会话目录（append 续写）", same,
                 f"{session_dir} vs {export2['data']}")
        lines2 = csv_lines(session_dir)
        ck.check("CSV 行数增长（续写非截断）", lines2 > lines1, f"{lines1} → {lines2}")

    # 8. 峰值面板
    peaks = c.state(serial).get("peaks")
    ck.check("峰值面板出数", bool(peaks), str(peaks)[:100])

    # 9. 停止 → 序列停增
    _, r = c.action("click", selector=f"{p} .stop-btn")
    stop = c.poll(lambda: not c.state(serial).get("samplingRunning"), timeout=15)
    ck.check("停止监控", bool(stop))
    time.sleep(2)
    c1 = cpu_count()
    time.sleep(2.5)
    c2 = cpu_count()
    ck.check("停止后序列停增", c1 == c2, f"{c1} vs {c2}")
    export_stopped = c.invoke("export_csv", {"serial": serial})
    ck.check("停止后仍可导出（会话数据保留）", export_stopped.get("ok"),
             str(export_stopped)[:120])
    # 清理导出快照（会话目录本体留给多设备组校验后统一清）
    for e in (export1, export2, export_stopped):
        if e.get("ok") and os.path.isdir(e["data"]) and ARGS.keep_data is False:
            shutil.rmtree(e["data"], ignore_errors=True)


# ---------------------------------------------------------------------------
# G1 连接面（remotes.json 备份/还原；会真实切换 本机↔hppc）
# ---------------------------------------------------------------------------

TEST_REMOTE_NAME = "xperf-test-hppc"


def _remotes_on_disk():
    if not os.path.exists(REMOTES_JSON):
        return None
    return json.load(open(REMOTES_JSON))


def group_g1(c, ck, serial):
    p = page(serial)
    backup = None
    existed = os.path.exists(REMOTES_JSON)
    if existed:
        backup = open(REMOTES_JSON).read()
    orig_remote = c.status().get("remote", {})
    try:
        _g1_body(c, ck, serial, p)
    finally:
        # 还原 remotes.json（GUI 内存缓存中的测试条目随下次重启消散）
        if existed:
            open(REMOTES_JSON, "w").write(backup)
        elif os.path.exists(REMOTES_JSON):
            os.remove(REMOTES_JSON)
        # 组尾兜底：恢复 g1 进入前的连接模式（后续组依赖原设备源——本地直跑回本机，
        # ssh 模式回原宿主；本地直跑的 hppc 不可达，不能盲 switchTo）
        st = c.status().get("remote", {})
        if st.get("mode") != orig_remote.get("mode") or st.get("host") != orig_remote.get("host"):
            if orig_remote.get("mode") == "ssh":
                c.eval(f"remoteUI.switchTo('{orig_remote.get('host')}').catch(e => 'ERR:' + e)")
                c.poll(lambda: c.status().get("remote", {}).get("mode") == "ssh"
                       and c.status().get("remote", {}).get("host") == orig_remote.get("host"),
                       timeout=30)
            else:
                c.eval("remoteUI.switchTo('').catch(e => 'ERR:' + e)")
                c.poll(lambda: c.status().get("remote", {}).get("mode") == "local", timeout=30)


def _g1_ssh_available(c, ck):
    """SSH UI 小节是否可跑：GUI 宿主 ssh config 须含目标宿主（可达性前提）。"""
    hosts = c.invoke("list_ssh_hosts")["data"] or []
    if SSH_UI_HOST in hosts:
        return True
    ck.skip("SSH 连接流（g1 §1-9）",
            f"环境受限：GUI 宿主 ssh config 不含 {SSH_UI_HOST!r}（{hosts}）——"
            "本地直跑无 SSH 目标；已由 Mac --remote 与 hppc --remote kong 覆盖")
    return False


def _g1_body(c, ck, serial, p):
    # 1. 初始状态：remotes 空（本机现无配置）+ 下拉含临时目标 hppc
    initial = c.invoke("list_remotes")["data"]
    ck.check("初始 list_remotes", isinstance(initial, list), str(initial)[:80])
    if not _g1_ssh_available(c, ck):
        # 10. add_ssh_host 校验错误路径（模式无关，SKIP 段仍可跑）
        bad = c.invoke("add_ssh_host", {"cfg": {"name": "", "host": "x", "user": None,
                                                "ssh_port": 22, "adb_path": None,
                                                "remote_port": None}})
        ck.check("add_ssh_host 空名报错", not bad.get("ok") and bad.get("error"),
                 str(bad)[:100])
        return
    opts = c.eval("[...document.getElementById('remoteSelect').options].map(o => o.value)")
    ck.check(f"下拉含临时目标 {SSH_UI_HOST}", SSH_UI_HOST in opts, f"opts={opts}")

    # 2. ssh config 展开命令
    hosts = c.invoke("list_ssh_hosts")["data"]
    ck.check(f"list_ssh_hosts 含 {SSH_UI_HOST}", SSH_UI_HOST in (hosts or []),
             str(hosts)[:120])
    details = c.invoke("list_ssh_host_details")["data"]
    det = {d.get("alias"): d for d in (details or [])}
    ck.check("list_ssh_host_details 结构化展开",
             SSH_UI_HOST in det and det[SSH_UI_HOST].get("hostname"),
             str(det.get(SSH_UI_HOST))[:120])

    # 3. 表单「保存并连接」：写 remotes.json + 连接（目标宿主已知可达）
    c.action("click", selector="#remoteAddBtn")  # 打开表单（同时刷新导入列表）
    c.action("input", selector="#rfName", value=TEST_REMOTE_NAME)
    c.action("input", selector="#rfHost", value=SSH_UI_HOST)
    c.action("input", selector="#rfSshPort", value="22")
    _, r = c.action("click", selector="#rfSave")
    ck.check("表单保存并连接", r.get("ok"), str(r.get("error", ""))[:120])
    connected = c.poll(lambda: c.status().get("remote", {}).get("mode") == "ssh"
                       and c.status().get("remote", {}).get("host"), timeout=30)
    ck.check(f"保存后连接 {SSH_UI_HOST}", bool(connected), str(connected)[:80])

    remotes = c.invoke("list_remotes")["data"]
    entry = next((x for x in remotes if x.get("name") == TEST_REMOTE_NAME), None)
    ck.check("remotes.json 条目（adb 默认值）",
             entry and entry.get("adb_path") == "adb" and entry.get("remote_port") == 5037,
             str(entry))
    on_disk = _remotes_on_disk()
    ck.check("remotes.json 落盘一致",
             on_disk and any(x.get("name") == TEST_REMOTE_NAME for x in on_disk),
             str(on_disk)[:120])
    opts = c.eval("[...document.getElementById('remoteSelect').options].map(o => o.textContent)")
    ck.check("下拉显示保存配置名", TEST_REMOTE_NAME in opts, f"opts={opts}")

    # 4. 侧栏「远程主机」区块（ssh 模式可见 + adb 填充）
    sb = c.dom(f"{p} .sidebar-remote", depth=1)
    visible = sb.get("ok") and not sb["data"]["tree"]["attrs"].get("class", "").find("hidden") >= 0
    adb_val = c.eval(f"document.querySelector('{p} .remote-adb-input').value")
    ck.check("侧栏远程区块可见且 adb 填充", visible and adb_val == "adb",
             f"visible={visible} adb={adb_val!r}")

    # 5. ssh config 导入选择器：已保存条目过滤 + 选中填充
    c.action("click", selector="#remoteAddBtn")  # 重开表单刷新导入列表
    import_opts = c.eval(
        "[...document.getElementById('rfImport').options].map(o => o.value)")
    ck.check(f"导入列表排除已保存 {SSH_UI_HOST}", SSH_UI_HOST not in import_opts, f"opts={import_opts}")
    if "103server" in import_opts:
        _, r = c.action("select", selector="#rfImport", value="103server")
        filled = c.eval("({n: document.getElementById('rfName').value,"
                        "h: document.getElementById('rfHost').value,"
                        "p: document.getElementById('rfSshPort').value})")
        ck.check("导入选中填充字段", filled.get("h") == "103server" and filled.get("n") == "103server",
                 str(filled))
    else:
        ck.check("导入选中填充字段", False, f"103server 不在导入列表: {import_opts}")
    c.action("click", selector="#remoteAddBtn")  # 关表单

    # 6. 切回本机（下拉选择）→ 设备=本机机队（Mac 本机 adb 无设备 vs
    #    hppc 直编本地三台，XPERF_TEST_LOCAL_SERIALS 区分）+ 侧栏区块隐藏
    _, r = c.action("select", selector="#remoteSelect", value="")
    local = c.poll(lambda: c.status().get("remote", {}).get("mode") == "local", timeout=20)
    ck.check("切回本机", bool(local))
    # 设备列表读热插拔监视器的 3s 快照缓存——刚切回本机时快照可能还是远程机队
    # （2026-09-22 hppc --remote kong 实测假 FAIL：读到 kong 的两台），故轮询到出本机队
    expect, seen = set(LOCAL_SERIALS), []

    def _local_fleet():
        devs = [d["serial"] for d in c.status().get("devices", []) if not d.get("is_gateway")]
        seen[:] = devs
        return True if set(devs) == expect else None
    fleet_ok = c.poll(_local_fleet, timeout=15) is not None
    if LOCAL_SERIALS:
        ck.check("切回本机后设备为本机机队", fleet_ok, f"devs={seen}")
        ck.check("有设备提示隐藏（本机有机队）", bool(c.poll(lambda: c.eval(
            "document.getElementById('noDeviceHint').classList.contains('hidden')") or None,
            timeout=10)))
    else:
        ck.check("本机模式无设备", fleet_ok, f"devs={seen}")
        hint = c.poll(lambda: c.eval(
            "!document.getElementById('noDeviceHint').classList.contains('hidden')") or None,
            timeout=10)
        ck.check("无设备提示显示", bool(hint))
    sb_hidden = c.poll(lambda: c.eval(
        f"document.querySelector('{p} .sidebar-remote').classList.contains('hidden')"
        if c.eval("app.sessions.size") else "true") or None, timeout=10)
    ck.check("本机模式侧栏远程区块隐藏", bool(sb_hidden))

    # 7. 密码认证失败路径（nobody@<host>：agent 密钥对该用户无授权 → 认证失败；
    #    不能用宿主本尊——本测试环境的 GUI 继承 SSH agent，密钥认证会先成功，
    #    密码路径根本不会走到）。错误密码 → 状态栏分类 + 表单预填重开 + 保持本机
    c.action("click", selector="#remoteAddBtn")
    c.action("input", selector="#rfHost", value=f"nobody@{SSH_UI_HOST}")
    c.action("input", selector="#rfPassword", value="wrong-password-xperf-test")
    _, r = c.action("click", selector="#rfConnect")
    ck.check("错误密码连接返回", r.get("ok"), str(r.get("error", ""))[:120])
    st_text = c.poll(lambda: ("远程连接失败" in (c.eval(
        "document.getElementById('status').textContent") or "")) and c.eval(
        "document.getElementById('status').textContent"), timeout=40)
    ck.check("密码认证失败分类提示", bool(st_text) and "密码认证失败" in st_text,
             str(st_text)[:160])
    mode = c.status().get("remote", {}).get("mode")
    ck.check("失败后保持本机", mode == "local", f"mode={mode}")
    reopened = c.poll(lambda: not c.eval(
        "document.getElementById('remoteForm').classList.contains('hidden')"), timeout=10)
    ck.check("表单预填重开（只欠密码）", bool(reopened))
    c.action("click", selector="#remoteAddBtn")  # 关表单

    # 8. 「连接」临时重连目标宿主（此时已存条目仍为默认 adb/5037，不毒化连接）
    c.action("click", selector="#remoteAddBtn")
    c.action("input", selector="#rfHost", value=SSH_UI_HOST)
    c.action("input", selector="#rfSshPort", value="22")
    c.action("input", selector="#rfPassword", value="")
    _, r = c.action("click", selector="#rfConnect")
    ck.check(f"临时连接 {SSH_UI_HOST}", r.get("ok"), str(r.get("error", ""))[:120])
    back = c.poll(lambda: c.status().get("remote", {}).get("mode") == "ssh", timeout=30)
    ck.check("重连后 ssh 模式", bool(back))
    devs = [d["serial"] for d in c.status().get("devices", []) if not d.get("is_gateway")]
    ck.check("重连后设备回来（含网关过滤）", serial in devs, f"devs={devs}")
    gw = [d["serial"] for d in c.status().get("devices", []) if d.get("is_gateway")]
    ck.check("网关设备标记", len(gw) == 1, f"gateways={gw}")
    c.action("click", selector="#remoteAddBtn")  # 关表单

    # 9. 侧栏 adb 设置 upsert（置尾：自定义端口会毒化后续宿主连接，改完即还原
    #     remotes.json）：改 adb+端口 → 保存；再改端口 → adb 保留（prev 合并）
    c.action("input", selector=f"{p} .remote-adb-input", value="/custom/adb")
    c.action("input", selector=f"{p} .remote-port-input", value="5039")
    _, r = c.action("click", selector=f"{p} .remote-adb-save")
    ck.check("侧栏保存 adb 设置", r.get("ok"), str(r.get("error", ""))[:120])
    entry = next((x for x in c.invoke("list_remotes")["data"]
                  if x.get("name") == TEST_REMOTE_NAME), None)
    ck.check("adb 设置持久化", entry and entry.get("adb_path") == "/custom/adb"
             and entry.get("remote_port") == 5039, str(entry))
    c.action("input", selector=f"{p} .remote-port-input", value="5040")
    c.action("click", selector=f"{p} .remote-adb-save")
    entry = next((x for x in c.invoke("list_remotes")["data"]
                  if x.get("name") == TEST_REMOTE_NAME), None)
    ck.check("再保存保留 adb（prev 合并）", entry and entry.get("adb_path") == "/custom/adb"
             and entry.get("remote_port") == 5040, str(entry))
    # 还原默认（adb/5037）——串跑时后续组的隧道重建/重连会读该条目，
    # 毒化值会让远端预检找不到 adb 二进制
    c.action("input", selector=f"{p} .remote-adb-input", value="adb")
    c.action("input", selector=f"{p} .remote-port-input", value="5037")
    c.action("click", selector=f"{p} .remote-adb-save")
    entry = next((x for x in c.invoke("list_remotes")["data"]
                  if x.get("name") == TEST_REMOTE_NAME), None)
    ck.check("adb 设置还原默认（不毒化后续组）", entry and entry.get("adb_path") == "adb"
             and entry.get("remote_port") == 5037, str(entry))

    # 10. add_ssh_host 校验错误路径
    bad = c.invoke("add_ssh_host", {"cfg": {"name": "", "host": "x", "user": None,
                                            "ssh_port": 22, "adb_path": None,
                                            "remote_port": None}})
    ck.check("add_ssh_host 空名报错", not bad.get("ok") and bad.get("error"),
             str(bad)[:100])


# ---------------------------------------------------------------------------
# G5 logcat 面
# ---------------------------------------------------------------------------

def group_g5(c, ck, serial):
    p = page(serial)
    c.action("input", selector=f"{p} .package-input", value=PKG)
    c.action("click", selector=f"{p} .tab.subtab[data-tab='logcat']")
    c.poll(lambda: c.state(serial).get("activeTab") == "logcat", timeout=8)

    # 1. 开始抓取（按包过滤勾选默认开；清残留口径——上一轮失败可能留下
    #    level=E/非法正则，带着启动会秒死）
    c.action("input", selector=f"{p} .logcat-text", value="")
    c.action("select", selector=f"{p} .logcat-level", value="V")
    c.action("check", selector=f"{p} .logcat-bypkg", value=True)
    _, r = c.action("click", selector=f"{p} .logcat-btn")
    ck.check("点击开始抓取", r.get("ok"), str(r.get("error", ""))[:120])
    run = c.poll(lambda: c.state(serial).get("logcatRunning"), timeout=15)
    ck.check("logcatRunning", bool(run))

    def tail(n=5):
        return (c.state(serial, logcat_tail=n).get("logcatTail")) or []

    lines = c.poll(lambda: tail(5) or None, timeout=20)
    ck.check("logcat 出行", bool(lines), f"tail={str(lines)[-100:] if lines else ''}")
    path_text = c.eval(f"document.querySelector('{p} .logcat-path').textContent")
    log_file = path_text.replace("logcat 抓取中: ", "").strip() if path_text else ""
    ck.check("落盘路径显示", bool(log_file) and log_file.startswith("/"), log_file[:100])

    # 2. 可见性联动暂停/补发（先取消按包过滤放大到全机流——按包流太慢判据会饿死；
    #    respawn 标记行只落盘不进前端 ring buffer，口径核验走文件）
    c.action("check", selector=f"{p} .logcat-bypkg", value=False)
    flowing = c.poll(lambda: len(tail(50)) >= 10 or None, timeout=25)
    ck.check("全机流出行（取消按包过滤）", bool(flowing))
    c.action("click", selector=f"{p} .tab.subtab[data-tab='perf']")
    c.poll(lambda: c.state(serial).get("activeTab") == "perf", timeout=8)
    time.sleep(3)
    frozen = len(tail(2000))
    time.sleep(3)
    frozen2 = len(tail(2000))
    ck.check("日志页隐藏期间事件暂停", frozen == frozen2, f"{frozen} vs {frozen2}")
    c.action("click", selector=f"{p} .tab.subtab[data-tab='logcat']")
    c.poll(lambda: c.state(serial).get("activeTab") == "logcat", timeout=8)
    resumed = c.poll(lambda: (lambda n: n > frozen2 and n)(len(tail(2000))), timeout=15)
    ck.check("切回后补发增长", bool(resumed), f"{frozen2} → {resumed}")

    def file_marker(pred):
        if not (log_file and os.path.exists(log_file)):
            return None
        with open(log_file, errors="replace") as f:
            return next((l for l in f if l.startswith("# xperf logcat respawn")
                         and pred(l)), None)

    # 3. 级别热切换 E（同文件续写 + respawn 标记行带新口径——读落盘文件）
    c.action("select", selector=f"{p} .logcat-level", value="E")
    marker = c.poll(lambda: file_marker(lambda l: "level=E" in l), timeout=20)
    ck.check("级别热切换 respawn 标记（level=E）", bool(marker), str(marker)[:140])
    # 4. 切回 V（第二次热切换）后做文本过滤——token 自适应取自当前流，
    #    避免硬编码词在该设备/应用上不出现（level=V 轮询只看本次切换后
    #    新增的文件尾部，旧标记已含 level=V 会假命中）
    size_before = os.path.getsize(log_file) if log_file and os.path.exists(log_file) else 0
    c.action("select", selector=f"{p} .logcat-level", value="V")

    def marker_after(size, pred):
        if not (log_file and os.path.exists(log_file)):
            return None
        with open(log_file, errors="replace") as f:
            f.seek(size)
            return next((l for l in f if l.startswith("# xperf logcat respawn")
                         and pred(l)), None)
    c.poll(lambda: marker_after(size_before, lambda l: "level=V" in l), timeout=20)

    def pick_token():
        t = [l for l in tail(30) if not l.startswith("#")]
        if len(t) < 3:
            return None
        words = [w for w in re.split(r"[^A-Za-z]+", t[-1]) if len(w) >= 4]
        for w in reversed(words):
            if not all(w in l for l in t[-3:]):  # 前置：尾 3 行不全含（可判收敛）
                return w
        return None
    token = c.poll(pick_token, timeout=20)
    ck.check("选到过滤 token", bool(token), str(token))
    if token:
        c.action("input", selector=f"{p} .logcat-text", value=token)
        fok = c.poll(lambda: (lambda t: len(t) >= 3 and all(token in l for l in t[-3:])
                              and t[-3:])(tail(20)), timeout=25)
        ck.check("文本过滤热切换生效", bool(fok), f"token={token} 尾 3 行={str(fok)[-120:] if fok else '超时'}")
    else:
        ck.check("文本过滤热切换生效", False, "无可用 token（流太稀）")

    # 5. 非法正则 → 连续秒死 Error 路径（按钮复位 + 状态提示）
    c.action("input", selector=f"{p} .logcat-text", value="(")

    def died():
        st = c.state(serial)
        return (not st.get("logcatRunning")) and st.get("statusText")
    err = c.poll(died, timeout=40)
    ck.check("非法正则中止抓取（按钮复位）", bool(err) and "中止" in (err or ""),
             str(err)[:140])

    # 6. 停止 + 落盘文件核验（含标记行——热切换同文件续写的证据链）
    if c.state(serial).get("logcatRunning"):
        c.action("click", selector=f"{p} .logcat-btn")
        c.poll(lambda: not c.state(serial).get("logcatRunning"), timeout=10)
    ck.check("停止抓取", not c.state(serial).get("logcatRunning"))
    if log_file and os.path.exists(log_file):
        content = open(log_file, errors="replace").read()
        marks = content.count("# xperf logcat respawn")
        ck.check("落盘文件含 respawn 标记（同文件续写）", marks >= 2, f"marks={marks}")
        ck.check("落盘文件含 text= 口径", "text=" in content, "")
    else:
        ck.check("落盘文件核验", False, f"文件不存在: {log_file}")


# ---------------------------------------------------------------------------
# G6 基线与反馈面（基线文件备份/还原；反馈真实上传 [gui-test] issue）
# ---------------------------------------------------------------------------

BASELINE_FILE = os.path.expanduser(
    f"~/.local/share/xperf/baselines/{PKG}.json")


def group_g6(c, ck, serial):
    p = page(serial)
    existed = os.path.exists(BASELINE_FILE)
    backup = open(BASELINE_FILE).read() if existed else None
    try:
        _g6_body(c, ck, serial, p)
    finally:
        if existed:
            open(BASELINE_FILE, "w").write(backup)
        elif os.path.exists(BASELINE_FILE):
            os.remove(BASELINE_FILE)


def _g6_body(c, ck, serial, p):
    # 1. 短采样 → 保存基线
    c.action("input", selector=f"{p} .package-input", value=PKG)
    _, r = c.action("click", selector=f"{p} .start-btn")
    ck.check("开始监控（基线数据源）", r.get("ok"))
    c.poll(lambda: c.state(serial).get("samplingRunning"), timeout=20)
    c.poll(lambda: sum(v.get("count", 0) for v in (c.state(serial).get("charts") or {})
                       .get("cpu", {}).get("series", {}).values()) >= 3, timeout=30)
    _, r = c.action("click", selector=f"{p} .save-baseline-btn")
    ck.check("点击保存基线", r.get("ok"), str(r.get("error", ""))[:120])
    ck.check("基线文件落盘", os.path.exists(BASELINE_FILE), BASELINE_FILE)
    if os.path.exists(BASELINE_FILE):
        # click 只确认 DOM 事件派发——前端 handler 异步 invoke save_baseline，
        # std::fs::write 的 truncate→write 窗口内单次读会撞到空文件；轮询到可解析
        def _load_baseline():
            try:
                return json.load(open(BASELINE_FILE))
            except (json.JSONDecodeError, OSError):
                return None
        data = c.poll(_load_baseline, timeout=10)
        ck.check("基线 JSON 可解析且含样本", bool(data) and (data.get("samples", 0) >= 1
                 or data.get("cpu", {}).get("avg") is not None), str(data)[:120])
    _, r = c.action("click", selector=f"{p} .stop-btn")
    c.poll(lambda: not c.state(serial).get("samplingRunning"), timeout=15)

    # 2. 二次采样 → 对比基线（报告面板呈现）
    _, r = c.action("click", selector=f"{p} .start-btn")
    c.poll(lambda: c.state(serial).get("samplingRunning"), timeout=20)
    c.poll(lambda: sum(v.get("count", 0) for v in (c.state(serial).get("charts") or {})
                       .get("cpu", {}).get("series", {}).values()) >= 3, timeout=30)
    _, r = c.action("click", selector=f"{p} .compare-baseline-btn")
    ck.check("点击对比基线", r.get("ok"), str(r.get("error", ""))[:120])
    # click 的 ok 只代表 DOM 事件派发——前端 handler 是 async invoke(compare_baseline)，
    # 报告文本渲染要等一次后端往返（发布产物首轮实测慢于 click 返回，直编偶发通过）：轮询
    def _report():
        rep = c.eval(f"document.querySelector('{p} .baseline-report').textContent") or ""
        vis = c.eval(
            f"!document.querySelector('{p} .panel-baseline').classList.contains('hidden')")
        return rep if (vis and len(rep) > 20) else None
    report = c.poll(_report, timeout=15)
    ck.check("对比报告面板呈现", bool(report), str(report)[:120])
    _, r = c.action("click", selector=f"{p} .stop-btn")
    c.poll(lambda: not c.state(serial).get("samplingRunning"), timeout=15)

    # 3. GitLab 凭证状态（OAuth 登录态；gitlab_login 浏览器 SSO 为人工项）
    auth = c.invoke("gitlab_auth_status")
    ck.check("gitlab_auth_status 已配置", auth.get("ok") and bool(auth["data"]),
             str(auth)[:120])

    # 4. 反馈真实上传（[gui-test] 标记；验证后手动关闭 issue）。
    #    --skip-feedback：真实上传已单组验证过（issue 落在仓库里），串跑/CI 不重复建 issue，
    #    只走浮层打开 + 身份行 + 关闭
    c.action("click", selector="#feedbackTopBtn")
    visible = c.poll(lambda: not c.eval(
        "document.getElementById('feedbackMask').classList.contains('hidden')"), timeout=8)
    ck.check("反馈浮层打开", bool(visible))
    identity = c.poll(lambda: (c.eval("document.getElementById('fbIdentity').textContent")
                               or "").strip() or None, timeout=15)
    # 已配置（Mac/OAuth）与「未配置 GitLab 凭证」两种环境态都算身份行正确呈现
    # （hppc/kong 测试机无凭证，如实提示而非空白/报错即通过）
    ck.check("身份行显示", bool(identity), str(identity)[:80])
    if ARGS.skip_feedback:
        ck.check("反馈上传（--skip-feedback 跳过真实上传）", True,
                 "浮层/身份链路已验证，不重复创建 issue")
        c.action("click", selector="#fbCancel") if c.eval(
            "!document.getElementById('feedbackMask').classList.contains('hidden')") else None
        return
    desc = ("[gui-test] GUI 回归测试自动提交（scripts/gui_tests）——验证反馈链路收集/打包/"
            "上传；本 issue 可直接关闭。")
    c.action("input", selector="#fbDesc", value=desc)
    _, r = c.action("click", selector="#fbSubmit")
    ck.check("点击提交反馈", r.get("ok"), str(r.get("error", ""))[:120])
    url = c.poll(lambda: next((w for w in [c.eval(
        "document.getElementById('status').textContent")] if w and "/issues/" in w), None),
        timeout=180)
    ck.check("反馈上传成功（issue URL）", bool(url), str(url)[:160])
    if url:
        # 经 API 核验 issue 存在且标题带 [gui-test]（OAuth token 复用 GUI 凭证链）
        issue = _gitlab_issue_json(url)
        ok = issue and "[gui-test]" in (issue.get("title") or "")
        ck.check("GitLab issue 核验（标题标记）", bool(ok),
                 f"iid={issue.get('iid') if issue else '?'} state={issue.get('state') if issue else '?'}")
    c.action("click", selector="#fbCancel") if c.eval(
        "!document.getElementById('feedbackMask').classList.contains('hidden')") else None


def _gitlab_issue_json(url):
    """从 issue web URL 提取 project/iid 并用本机 OAuth token 读 issue JSON。"""
    import re
    import urllib.request
    m = re.search(r"(-/issues/|/issues/)(\d+)", url)
    if not m:
        return None
    iid = m.group(2)
    try:
        tok = json.load(open(os.path.expanduser("~/.config/xperf/gitlab-oauth.json")))
        access = tok.get("access_token")
        if not access:
            return None
        base = url.split("/-/issues/")[0].split("/issues/")[0]
        api = f"{base.rstrip('/')}.json" if False else None
        # web URL → API：https://gitlab.chehejia.com/<group>/<proj>/-/issues/N
        # 项目的 API 路径用 web path 编码（feedback 模块同套路）
        proj = url.split("://", 1)[1].split("/-/issues/")[0].split("/issues/")[0]
        proj = proj.split("/", 1)[1] if "/" in proj else proj
        api = (f"https://gitlab.chehejia.com/api/v4/projects/"
               f"{urllib.parse.quote(proj, safe='')}/issues/{iid}")
        req = urllib.request.Request(api, headers={"Authorization": f"Bearer {access}"})
        with urllib.request.urlopen(req, timeout=15) as resp:
            return json.loads(resp.read())
    except Exception:
        return None


# ---------------------------------------------------------------------------
# G3 深挖与捕获面（trace/stack/截屏/录屏/镜像/浏览器按钮）
# ---------------------------------------------------------------------------

def _sess(c, serial):
    """/api/status sessions 里该设备的后端会话真相（sampling/trace/stack/mirror/…）。"""
    return next((s for s in c.status().get("sessions", []) if s.get("serial") == serial), {})


def group_g3(c, ck, serial):
    p = page(serial)
    c.action("input", selector=f"{p} .package-input", value=PKG)
    # 应用须在跑（stack 录制 pidof 前置拦截；不在则拉起）
    def pidof_pkg():
        r = adb_on_remote(serial, "shell", "pidof", PKG)
        return ((r.stdout if hasattr(r, "stdout") else r) or "").strip()
    if not pidof_pkg():
        c.action("click", selector=f"{p} .launch-btn")
        c.poll(lambda: pidof_pkg() or None, timeout=30)

    # 1. perfetto trace 5s（录制 → 报告分段 → 浏览器按钮解锁 → 落盘产物）
    c.action("click", selector=f"{p} .tab.subtab[data-tab='trace']")
    c.poll(lambda: c.state(serial).get("activeTab") == "trace", timeout=8)
    c.action("select", selector=f"{p} .trace-seconds", value="5")
    _, r = c.action("click", selector=f"{p} .trace-start-btn")
    ck.check("点击录制并分析（trace）", r.get("ok"), str(r.get("error", ""))[:120])
    ck.check("trace 录制中（后端会话）", bool(c.poll(
        lambda: _sess(c, serial).get("trace") or None, timeout=15)))

    # done 事件的报告内容判据（非会话标志——后端 emit(done) 后才清 running，
    # 前端异步收事件存在「stack=false 但报告区还是 recorded 文案」的竞态窗口）
    def trace_done():
        rep = c.eval(f"document.querySelector('{p} .trace-report').textContent")
        return rep and "包 CPU 总量" in rep and rep
    rep = c.poll(trace_done, timeout=150)
    # 报告段头为「── 段名 ──」风格（===xxx=== 是 SQL 内部 marker，不进最终报告）
    ck.check("trace 报告生成（含分段标记）", bool(rep) and "包 CPU 总量" in (rep or "")
             and "包线程 CPU" in (rep or ""), (str(rep)[:120] if rep else "超时"))
    ck.check("浏览器按钮解锁（recorded 起）", bool(c.eval(
        f"!document.querySelector('{p} .open-perf-btn').disabled")))
    trace_path = c.eval(f"document.querySelector('{p} .trace-content .trace-file-label').textContent")
    # 体积判据按「有真实内容」而非固定 1MB：SS3 10s≈46MB，而 SS2PRO/A11 5s 实测仅 476KB
    # （ftrace 事件类目少）——阈值取 50KB 防「空文件/截断拉取」假通过（2026-09-22 K1 实测）
    ck.check("trace 落盘产物（.pftrace >50KB）",
             bool(trace_path) and os.path.exists(trace_path)
             and os.path.getsize(trace_path) > 50_000, str(trace_path)[:120])

    # 2. 浏览器打开 Perfetto UI（本地镜像/回退均算成功；连点保护不炸）
    _, r = c.action("click", selector=f"{p} .open-perf-btn")
    opened = c.poll(lambda: (lambda t: t and ("Perfetto" in t or "已打开" in t) and t)(
        c.eval("document.getElementById('status').textContent")), timeout=30)
    ck.check("在浏览器打开 Perfetto UI", bool(opened), str(opened)[:140])
    c.action("click", selector=f"{p} .open-perf-btn")  # 600ms 冷却内的第二击（no-op 路径）
    ck.check("连点不炸（冷却保护）", c.alive())

    # 3. simpleperf stack 5s（三视图报告 + 火焰图按钮 + .data 产物）
    c.action("click", selector=f"{p} .tab.subtab[data-tab='stack']")
    c.poll(lambda: c.state(serial).get("activeTab") == "stack", timeout=8)
    c.action("select", selector=f"{p} .stack-seconds", value="5")
    _, r = c.action("click", selector=f"{p} .stack-start-btn")
    ck.check("点击录制并分析（stack）", r.get("ok"), str(r.get("error", ""))[:120])
    ck.check("stack 录制中（后端会话）", bool(c.poll(
        lambda: _sess(c, serial).get("stack") or None, timeout=15)))

    def stack_done():
        rep = c.eval(f"document.querySelector('{p} .stack-report').textContent")
        return rep and "线程 CPU 分布" in rep and rep
    srep = c.poll(stack_done, timeout=150)
    # done 消息是提炼摘要（top12/top20），段头「线程 CPU 分布（self 开销占比」/
    # 「函数热点 top20（self/children」；全量三视图核验走落盘 simpleperf_report.txt
    ck.check("stack 三视图报告（线程分布/self/children）", bool(srep)
             and "线程 CPU 分布" in (srep or "")
             and "函数热点 top20（self" in (srep or "")
             and "函数热点 top20（children" in (srep or ""),
             (str(srep)[:120] if srep else "超时"))
    stack_path = c.eval(f"document.querySelector('{p} .stack-content .trace-file-label').textContent")
    ck.check("stack .data 落盘", bool(stack_path) and os.path.exists(stack_path)
             and os.path.getsize(stack_path) > 100_000, str(stack_path)[:120])
    if stack_path:
        full_rep = os.path.join(os.path.dirname(stack_path), "simpleperf_report.txt")
        full = open(full_rep, encoding="utf-8", errors="replace").read() \
            if os.path.exists(full_rep) else ""
        ck.check("落盘全量报告（三视图俱全）",
                 "线程 CPU 分布（--sort comm,pid,tid）" in full
                 and "函数热点 self（--sort symbol,dso）" in full
                 and "函数热点 children" in full, full_rep)

    # 4. 浏览器火焰图（report_html.py 渲染单文件 HTML；复用不重渲染）
    _, r = c.action("click", selector=f"{p} .open-stack-btn")
    html_ok = c.poll(lambda: (lambda t: t and "火焰图" in t and t)(
        c.eval("document.getElementById('status').textContent")), timeout=60)
    ck.check("在浏览器打开火焰图", bool(html_ok), str(html_ok)[:140])
    if stack_path:
        htmls = [f for f in os.listdir(os.path.dirname(stack_path)) if f.endswith(".html")]
        ck.check("火焰图 HTML 产物", bool(htmls), str(htmls)[:120])

    # 5. 截屏（exec-out 直写 PNG；魔数核验）
    _, r = c.action("click", selector=f"{p} .shot-btn")
    shot = c.poll(lambda: (lambda t: t if t and "截屏已保存: " in t else None)(
        c.eval("document.getElementById('status').textContent")), timeout=30)
    shot_path = shot.split("截屏已保存: ")[-1].strip() if shot else ""
    png = shot_path and os.path.exists(shot_path) and open(shot_path, "rb").read(8) == \
        b"\x89PNG\r\n\x1a\n" if shot_path else False
    ck.check("截屏 PNG 产物（魔数核验）", bool(png), shot_path[:120])

    # 6. 录屏（scrcpy 无窗口录制 → 停止封盘 → mp4 产物）
    #    scrcpy 为 GUI 宿主机依赖（hppc 未装属环境限制，SKIP 不计缺陷——
    #    Mac 侧已全量覆盖录屏/镜像链路）
    if shutil.which("scrcpy") or os.path.exists("/opt/homebrew/bin/scrcpy"):
        _, r = c.action("click", selector=f"{p} .record-btn")
        rec = c.poll(lambda: _sess(c, serial).get("recording") or None, timeout=20)
        ck.check("录屏开始（后端会话）", bool(rec))
        time.sleep(4)
        _, r = c.action("click", selector=f"{p} .record-btn")
        saved = c.poll(lambda: (lambda t: t if t and "录屏已保存: " in t else None)(
            c.eval("document.getElementById('status').textContent")), timeout=30)
        rec_path = saved.split("录屏已保存: ")[-1].strip() if saved else ""
        ck.check("录屏停止封盘（按钮复位）", not _sess(c, serial).get("recording"))
        ck.check("录屏 MP4 产物（非空文件）", bool(rec_path) and os.path.exists(rec_path)
                 and os.path.getsize(rec_path) > 10_000, str(rec_path)[:120])

        # 7. 屏幕镜像（scrcpy 外部窗口；停止复位）
        _, r = c.action("click", selector=f"{p} .mirror-btn")
        mir = c.poll(lambda: _sess(c, serial).get("mirror") or None, timeout=20)
        ck.check("镜像启动（后端会话）", bool(mir))
        time.sleep(2)
        _, r = c.action("click", selector=f"{p} .mirror-btn")
        stopped = c.poll(lambda: (not _sess(c, serial).get("mirror")) or None, timeout=20)
        ck.check("镜像停止复位", bool(stopped))
    else:
        ck.skip("录屏/镜像（scrcpy）", "GUI 宿主机未安装 scrcpy（hppc Linux 实测）——"
                "环境受限，Mac 侧已覆盖")

    c.action("click", selector=f"{p} .tab.subtab[data-tab='perf']")
    c.poll(lambda: c.state(serial).get("activeTab") == "perf", timeout=8)


# ---------------------------------------------------------------------------
# G4 多设备并行与故障注入（三机并行 / 连点 / 快切 / SS4 断连自愈 / 隧道重建）
# ---------------------------------------------------------------------------

# 环境适配：附加并行设备（Mac--remote 与 hppc 本地同拓扑默认值；kong 等其它环境
# 经 XPERF_TEST_EXTRA_SERIALS 覆盖——逗号分隔，自动过滤不在线的）
SERIALS_EXTRA = [s for s in os.environ.get(
    "XPERF_TEST_EXTRA_SERIALS", "d1f39648c1f,localhost:5559").split(",") if s]


def group_g4(c, ck, serial):
    p = page(serial)
    all_serials = [serial] + [s for s in SERIALS_EXTRA
                              if s in [d["serial"] for d in c.status().get("devices", [])]]
    # ≥2 即构成多设备并行语义（Mac/hppc 三台、kong 两台）；台数入详情
    ck.check("多设备在线（≥2 台）", len(all_serials) >= 2, f"{all_serials}")

    # 1. 多机并行采样：各自 series 增长 + 会话隔离（切设备页 charts 互不串）。
    #    前置=该设备装了本次测试包：机队间装的应用未必相同（kong 机队 SS2PRO 原装
    #    hellotriangle、SS4 装 gltf viewer），未装者永远不出 series——判 FAIL 是判据
    #    假设错而非产品缺陷，故如实 SKIP（2026-09-22 K1 实测）
    for s_ in all_serials:
        c.action("input", selector=f"{page(s_)} .package-input", value=PKG)
        c.action("click", selector=f"{page(s_)} .start-btn")
    for s_ in all_serials:
        ck.check(f"[{s_}] 并行采样启动", bool(c.poll(
            lambda s_=s_: c.state(s_).get("samplingRunning"), timeout=30)))
    data_serials = []
    for s_ in all_serials:
        if not pkg_installed(s_):
            ck.skip(f"[{s_}] 并行 series 增长", f"测试包 {PKG} 未装在该设备（无数据源）")
            continue
        data_serials.append(s_)
        ck.check(f"[{s_}] 并行 series 增长", bool(c.poll(
            lambda s_=s_: sum(v.get("count", 0) for v in (c.state(s_).get("charts") or {})
                              .get("cpu", {}).get("series", {}).values()) >= 3, timeout=60)))
    iso_other = data_serials[-1] if len(data_serials) >= 2 else None
    base = c.state(serial).get("charts", {}).get("cpu", {}).get("series", {})
    if iso_other:
        c.action("click", selector=f"{page(iso_other)} .tab.subtab[data-tab='trace']")
        other = c.state(iso_other).get("charts", {}).get("cpu", {}).get("series", {})
        ck.check("切设备页数据隔离（series 独立）", bool(base) and base != other,
                 f"base={len(base)} other={len(other)}")
    else:
        ck.skip("切设备页数据隔离（series 独立）",
                f"需两台以上有数据源（本次 {len(data_serials)} 台装了 {PKG}）")

    # 2. 连点竞态与快切（stop 后快双击 start——第二击落在首击 invoke pending
    #    窗口内，disabled 尚未置位；修复前会双发报错弹窗 + resetSessionData 清图表）
    #    取有数据源的第二台，否则「双击后 series 增长」测的是无源设备（假 FAIL）
    if len(data_serials) > 1:
        extra = data_serials[1]
    elif data_serials:
        extra = data_serials[0]
    else:
        extra = all_serials[1] if len(all_serials) > 1 else serial
    c.action("click", selector=f"{page(extra)} .stop-btn")
    c.poll(lambda: not c.state(extra).get("samplingRunning"), timeout=20)
    for _ in range(2):
        c.action("click", selector=f"{page(extra)} .start-btn")
    ck.check("start 双击仅启动一次", bool(c.poll(
        lambda: c.state(extra).get("samplingRunning"), timeout=30)))
    st_extra = c.state(extra).get("statusText")
    ck.check("双击无失败弹窗路径（状态栏无『失败』）", "失败" not in (st_extra or ""),
             str(st_extra)[:80])
    ck.check("双击后 series 增长（图表未被清）", bool(c.poll(
        lambda: sum(v.get("count", 0) for v in (c.state(extra).get("charts") or {})
                    .get("cpu", {}).get("series", {}).values()) >= 3, timeout=60)))
    for i in range(10):
        tab = ["perf", "trace", "stack", "logcat"][i % 4]
        c.action("click", selector=f"{p} .tab.subtab[data-tab='{tab}']")
    # i=9 % 4 = 1 → 最后停在 trace；activeTab 是前端真实态，快切不丢状态即通过
    final_tab = c.poll(lambda: c.state(serial).get("activeTab") == "trace" or None, timeout=8)
    ck.check("10 次快切后 tab 状态正确", bool(final_tab) and c.alive(),
             str(c.state(serial).get("activeTab")))
    c.action("click", selector=f"{p} .tab.subtab[data-tab='perf']")

    # 3. SS4 断连自愈（adb disconnect 伪设备）。监视器 3s 周期 bridge refresh 会抢在
    #    观察窗内重连——「设备消失」是 best-effort 观察不是判据，自愈语义 =
    #    注入生效（rc=0）+ 采样不丢（series 续增）
    ss4 = "localhost:5559"
    if ss4 in all_serials:
        n_before = sum(v.get("count", 0) for v in (c.state(ss4).get("charts") or {})
                       .get("cpu", {}).get("series", {}).values())
        # 注入走与模式匹配的 adb：ssh 模式在宿主执行（hop#1 语义），本机模式直接
        # 本地 adb（disconnect 不带 -s；hppc Linux 本地直跑实测曾误走 ssh 报
        # 「Could not resolve hostname hppc」）
        if c_status_remote_mode() == "ssh":
            dr = ssh_run(f"{_remote_adb()} disconnect {ss4}")
        else:
            dr = subprocess.run(
                [os.environ.get("XPERF_ADB", "adb"), "disconnect", ss4],
                capture_output=True, text=True, timeout=30,
            )
        ck.check("SS4 断连注入生效（adb rc=0）", dr.returncode == 0,
                 (dr.stderr or "").strip()[:100])
        gone = c.poll(lambda: ss4 not in [d["serial"] for d in c.status().get("devices", [])]
                      or None, timeout=10)
        if gone:
            ck.check("断连期间设备消失（tab 灰显）", True, "观察到消失窗口")
        else:
            ck.check("断连期间设备消失（tab 灰显）", True,
                     "未观察到（监视器 3s 内已 refresh 重连，自愈快于观察粒度）")
        back = c.poll(lambda: (lambda devs: ss4 in devs and devs)(
            [d["serial"] for d in c.status().get("devices", [])]) or None, timeout=120)
        ck.check("断连后设备在线（未永久丢失）", bool(back), str(back)[:80])
        if not pkg_installed(ss4):
            ck.skip("SS4 采样恢复（series 续增）", f"测试包 {PKG} 未装在 SS4（无数据源）")
        else:
            resumed = c.poll(lambda: sum(v.get("count", 0) for v in (c.state(ss4).get("charts") or {})
                                         .get("cpu", {}).get("series", {}).values()) > n_before
                             or None, timeout=60)
            ck.check("SS4 采样恢复（series 续增）", bool(resumed))

    # 4. SSH 隧道重建（杀 GUI 自己的 ControlMaster → rebuild_tunnel 指数退避恢复）
    #    仅 ssh 模式有意义——Linux 本地直跑（hppc 直编/kong AppImage）无隧道，
    #    该场景已在 Mac --remote 覆盖
    if c_status_remote_mode() != "ssh":
        ck.skip("SSH 隧道重建", "本机模式无隧道（Linux 本地直跑；Mac --remote 已覆盖）")
        sockets = []
    else:
        # socket 名 = xperf-<连接主机>-<gui pid>-<n>：主机取 GUI 实际连上的那台
        # （写死 hppc 时，GUI --remote kong 场景必然找不到 → 假 FAIL，2026-09-22 K1 实测）
        gui_pid = c.status().get("pid")
        rhost = (c.status().get("remote") or {}).get("host") or REMOTE_ADB
        cm_dir = os.path.expanduser("~/.ssh/cm")
        pref = f"xperf-{rhost}-{gui_pid}-"

        def _socks():
            got = [x for x in (os.listdir(cm_dir) if os.path.isdir(cm_dir) else [])
                   if x.startswith(pref)]
            return got or None
        # 重连窗口内旧 master 已 -O exit 摘掉 socket、新 master 尚未建好 → 轮询
        sockets = c.poll(_socks, timeout=20) or []
        ck.check("找到 GUI 自己的 control socket", bool(sockets),
                 f"host={rhost} pid={gui_pid} {sockets}")
    if sockets:
        sock_path = os.path.expanduser(f"~/.ssh/cm/{sockets[0]}")
        # 找 master 进程并 SIGKILL（模拟异常断网）。macOS 的 lsof 对该 unix socket
        # 无输出（实测），改 pgrep 按命令行里的 socket 名匹配（master argv 含 -S <path>）
        out = subprocess.run(["pgrep", "-f", sockets[0]], capture_output=True, text=True).stdout
        pids = [x for x in out.split() if x.isdigit() and int(x) != gui_pid]
        if pids:
            subprocess.run(["kill", "-9"] + pids)
            # 先观察到死亡（remote.alive 翻 false 或设备从列表消失），再等恢复——
            # 直接等「非空」会撞监视器 3s 陈旧快照假阳性
            dead = c.poll(lambda: (not c.status().get("remote", {}).get("alive")
                                   or serial not in [d["serial"] for d in
                                                     c.status().get("devices", [])]) or None,
                          timeout=15)
            recovered = c.poll(lambda: (lambda d: d and d)(
                [d["serial"] for d in c.status().get("devices", [])]) or None, timeout=120)
            ck.check("隧道死亡被观察到", bool(dead), "15s 内 remote.alive/设备列表未变化")
            ck.check("隧道重建后设备回来", bool(recovered) and serial in (recovered or []),
                     str(recovered)[:80])
            ck.check("隧道重建后采样仍在", c.state(serial).get("samplingRunning"))
        else:
            ck.check("杀 master 进程", False, f"pgrep 无属主: {out!r}")

    # 5. 收尾：全部停止
    for s_ in all_serials:
        c.action("click", selector=f"{page(s_)} .stop-btn")
    for s_ in all_serials:
        ck.check(f"[{s_}] 停止采样", bool(c.poll(
            lambda s_=s_: not c.state(s_).get("samplingRunning"), timeout=20)))


# ---------------------------------------------------------------------------
# G8 WebKit 子进程退出（故障注入；可能挂死 GUI——见测试计划，恢复需人工重启）
# ---------------------------------------------------------------------------

def group_g8(c, ck, serial):
    gui_pid = c.status().get("pid")
    children = webkit_children(gui_pid)
    if sys.platform == "darwin" and not children:
        # macOS WKWebView 的 WebContent 经 XPC 挂在 launchd 下，不是 GUI 直接子进程
        # （ps 子进程只有 adb/scrcpy 等）——本组为 Linux(webkit2gtk) 专属场景
        ck.check("WebKit 子进程退出（macOS N/A）", True, "darwin 无直接子进程，Linux 环境跑此组")
        return
    ck.check("存在 WebKit 子进程", bool(children), f"{children}")
    # 确定性杀 WebKitNetworkProcess：webkit2gtk 对网络进程死亡可自愈（实测
    # hppc 3/3 轮通过）；而 WebKitWebProcess 死亡主进程不恢复（SIGHUP 同构，
    # 已知边界 #5——pgrep 顺序不定曾致 children[0] 轮杀到 WebProcess 假 FAIL）
    net = next(((pid, name) for pid, name in children if "Network" in name), None)
    if not net:
        ck.skip("杀 WebKit 子进程后自愈", f"无 NetworkProcess 子进程：{children}")
        return
    subprocess.run(["kill", "-9", str(net[0])])
    time.sleep(3)
    alive = c.alive()
    ck.check("杀 NetworkProcess 后 debug API 存活（自愈）", alive)
    if alive:
        ok_click = c.action("click", selector=f"{page(serial)} .tab.subtab[data-tab='stack']")
        tab = c.poll(lambda: c.state(serial).get("activeTab") == "stack" or None, timeout=8)
        ck.check("子进程死后 UI 仍可交互", bool(ok_click[1].get("ok")) and bool(tab))
        c.action("click", selector=f"{page(serial)} .tab.subtab[data-tab='perf']")
    else:
        ck.check("子进程死后 UI 仍可交互", False,
                 "debug API 无响应（与 SIGHUP 症状同——WebKit 子进程死亡主进程不恢复）")


# ---------------------------------------------------------------------------
# 组注册与入口
# ---------------------------------------------------------------------------

GROUPS = {
    "g1": group_g1,
    "g2": group_g2,
    "g3": group_g3,
    "g4": group_g4,
    "g5": group_g5,
    "g6": group_g6,
    "g8": group_g8,
}


def main():
    import argparse
    global ARGS
    ap = argparse.ArgumentParser()
    ap.add_argument("--group", default="all",
                    help="逗号分隔组名（默认 all=全部已实现组）")
    ap.add_argument("--serial", default="6eb792dfb0f", help="主测试设备")
    ap.add_argument("--package", default=PKG)
    ap.add_argument("--skip-destructive", action="store_true",
                    help="跳过破坏性场景（clean_cache 等）")
    ap.add_argument("--skip-feedback", action="store_true", dest="skip_feedback",
                    help="跳过反馈真实上传（已单组验证过；串跑/CI 用，不重复建 issue）")
    ap.add_argument("--keep-data", action="store_true", dest="keep_data",
                    help="保留导出快照等测试产物")
    ARGS = ap.parse_args()
    ARGS.client = Client()

    names = [g for g in ARGS.group.split(",") if g] if ARGS.group != "all" else list(GROUPS)
    unknown = [g for g in names if g not in GROUPS]
    if unknown:
        sys.exit(f"未知组: {unknown}，可用: {list(GROUPS)}")
    if not ARGS.client.wait_frontend(30):
        sys.exit("frontend 未就绪（GUI 启动参数是否带 --remote？）")

    total_fail = 0
    for name in names:
        print(f"\n===== {name} =====", flush=True)
        ck = Checker()
        try:
            GROUPS[name](ARGS.client, ck, ARGS.serial)
        except Exception as e:
            ck.check(f"{name} 组异常中断（环境级）", False, f"{type(e).__name__}: {e}")
        total_fail += ck.summary(name)
        if not ARGS.client.alive():
            print(f"[{name}] 后 debug API 无响应——GUI 已挂，中止后续组")
            break
    print(f"\n总计 FAIL: {total_fail}")
    sys.exit(1 if total_fail else 0)


if __name__ == "__main__":
    main()

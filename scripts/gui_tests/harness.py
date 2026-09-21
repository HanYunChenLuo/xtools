#!/usr/bin/env python3
"""xperf GUI 回归测试共享库（harness）。

被 `regression.py` 导入；也可独立运行做环境自检（`python3 harness.py --check`）。

设计要点：
- 被测 GUI 与本脚本**同机**（读该机 `~/.config/xperf/gui-debug.json` 发现文件）；
  Mac/Linux 通用——hppc 直编 GUI、kong AppImage、本机 Mac GUI 均同一套脚本。
- GUI 启动器用 `os.setsid`（preexec_fn）脱离会话——macOS 无 setsid 命令，且
  nohup 不足以保护 WebKit 子进程（会话清理的 SIGHUP 会杀它，2026-09-20 教训）。
- 任意 Tauri command 可经 `/api/eval` 前端 `invoke()` 直调（无按钮绑定的命令
  如 startup_sessions/diag_log 以此覆盖）；注意命令参数 snake_case → camelCase。
- 进程资源采样（GUI + WebKit 子进程 RSS/CPU）：故障注入前后对比用；压力测试
  会话复用同一接口。
- 运行期间勿动物理鼠标/窗口（合成 hover 与真实事件竞争）。

api 端点语义见 xperf-gui/src/debugsrv.rs 与 docs/DESIGN-gui-debug.md。
"""

import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

GUI_DEBUG_JSON = os.path.expanduser("~/.config/xperf/gui-debug.json")


class Client:
    """debug API 客户端：发现文件 → 鉴权 → req/poll/action/eval/invoke。"""

    def __init__(self, timeout=20):
        self.timeout = timeout
        cfg = json.load(open(GUI_DEBUG_JSON))
        self.base = f"http://127.0.0.1:{cfg['port']}"
        self.token = cfg["token"]
        self.pid = cfg.get("pid")

    def req(self, method, path, body=None, timeout=None):
        """带 token 的 JSON 请求；HTTPError 也返回 (code, json) 不抛。

        网络级错误（超时/连接拒绝）重试 3 次：连接切换（connect_remote 建 SSH
        隧道）期间 debug API 可被阻塞数十秒（实测 rfSave 后 /api_status 超时
        20s+ 但最终恢复）——瞬态失败不应中断整组。
        """
        last = None
        for attempt in range(3):
            r = urllib.request.Request(
                self.base + path,
                method=method,
                data=json.dumps(body).encode() if body is not None else None,
                headers={"X-Xperf-Token": self.token, "content-type": "application/json"},
            )
            try:
                with urllib.request.urlopen(r, timeout=timeout or self.timeout) as resp:
                    return resp.status, json.loads(resp.read())
            except urllib.error.HTTPError as e:
                try:
                    return e.code, json.loads(e.read())
                except Exception:
                    return e.code, {}
            except (urllib.error.URLError, TimeoutError, OSError) as e:
                last = e
                if attempt < 2:
                    time.sleep(2 + attempt * 3)
        raise ConnectionError(f"debug API 连续 3 次网络级失败: {path}: {last}")

    def poll(self, fn, timeout=25, interval=0.5, desc=""):
        """轮询直到 fn() 真值或超时；返回最终值（None=超时）。"""
        t0 = time.time()
        while time.time() - t0 < timeout:
            v = fn()
            if v:
                return v
            time.sleep(interval)
        return None

    def status(self):
        _, r = self.req("GET", "/api/status")
        return r.get("data", {})

    def state(self, serial, **params):
        qs = f"?serial={serial}"
        for k, v in params.items():
            qs += f"&{k}={v}"
        _, r = self.req("GET", f"/api/state{qs}")
        return r.get("data", {}).get("session") or {}

    def action(self, op, selector=None, **kwargs):
        body = {"op": op}
        if selector is not None:
            body["selector"] = selector
        body.update(kwargs)
        return self.req("POST", "/api/action", body)

    def eval(self, expr):
        """任意 JS（async 自动 await）；返回 data 或抛 AssertionError（前端错误）。"""
        _, r = self.req("POST", "/api/eval", {"expr": expr})
        if not r.get("ok"):
            raise AssertionError(f"eval 失败: {r}")
        return r["data"]

    def invoke(self, cmd, args=None):
        """经前端 invoke() 直调 Tauri command（参数键用 camelCase）。

        返回 dict：{"ok": true, "data": <命令返回>} 或 {"ok": false, "error": <Err 文本>}。
        """
        return self.eval(
            f"(async () => {{ try {{ return {{ok: true, data: await window.__TAURI__.core.invoke({json.dumps(cmd)}, {json.dumps(args or {})})}}; }}"
            f" catch (e) {{ return {{ok: false, error: String(e && e.message ? e.message : e)}}; }} }})()"
        )

    def dom(self, selector, depth=2, max_nodes=400):
        sel = urllib.parse.quote(selector, safe="")
        _, r = self.req("GET", f"/api/dom?selector={sel}&depth={depth}&max_nodes={max_nodes}")
        return r

    def series(self, serial, metric, tail=3, at=None):
        qs = f"?serial={serial}&metric={metric}&tail={tail}"
        if at is not None:
            qs += f"&at={at}"
        _, r = self.req("GET", f"/api/series{qs}")
        return r.get("data", {})

    def wait_frontend(self, timeout=30):
        return self.poll(
            lambda: self.status().get("frontend_ready"), timeout=timeout, interval=0.5,
            desc="frontend_ready",
        )

    def alive(self):
        try:
            self.req("GET", "/api/ping", timeout=3)
            return True
        except Exception:
            return False


class GuiProc:
    """被测 GUI 进程生命周期：setsid 启动 / 探活 / 收尾。

    `os.setsid`（preexec_fn）等价 Linux `setsid`：脱离控制终端与会话，宿主 shell
    退出不再向进程组发 SIGHUP——WebKit 子进程初始化后恢复默认信号处理，nohup
    保护不了它（SESSION 2026-09-20 实测）。
    """

    def __init__(self, argv, log_path):
        self.argv = argv
        self.log_path = log_path
        self.proc = None

    def start(self):
        log = open(self.log_path, "ab")
        self.proc = subprocess.Popen(
            self.argv,
            stdout=log,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            preexec_fn=os.setsid,
        )
        return self.proc

    def stop(self, timeout=8):
        """SIGTERM（=窗口正常关闭路径不触发，此处仅进程级收尾）→ 宽限 SIGKILL。"""
        if not self.proc or self.proc.poll() is not None:
            return
        try:
            os.killpg(self.proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            return
        try:
            self.proc.wait(timeout)
            return
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
            self.proc.wait(timeout)
        except ProcessLookupError:
            pass

    def discover_client(self, timeout=20):
        """等待发现文件出现且 pid 活着，返回 Client。"""
        t0 = time.time()
        while time.time() - t0 < timeout:
            if os.path.exists(GUI_DEBUG_JSON):
                cfg = json.load(open(GUI_DEBUG_JSON))
                pid = cfg.get("pid")
                if pid and _pid_alive(pid):
                    return Client()
            time.sleep(0.5)
        raise TimeoutError(f"GUI 发现文件未出现: {GUI_DEBUG_JSON}")


def _pid_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def webkit_children(gui_pid):
    """GUI 进程的 WebKit 子进程（WebKitWebProcess/WPIDWebProcess 等）列表。

    跨平台：ps -o pid,comm 遍历子进程（macOS 无 /proc）。名字按 WebKit 发行版
    变化，宽松匹配 web 关键字 + 排除自身。
    """
    try:
        out = subprocess.run(
            ["ps", "-o", "pid=,ppid=,comm=", "-x"],
            capture_output=True, text=True, timeout=5,
        ).stdout
    except Exception:
        return []
    kids = []
    for line in out.splitlines():
        parts = line.split(None, 2)
        if len(parts) != 3:
            continue
        pid, ppid, comm = parts
        if int(ppid) == gui_pid and "web" in comm.lower():
            kids.append((int(pid), comm))
    return kids


def proc_rss_cpu(pids):
    """进程 RSS(KB)/CPU(%) 采样快照（ps 一次往返，多 pid）。"""
    if not pids:
        return {"rss_kb": 0, "cpu": 0.0, "alive": 0}
    args = ["ps", "-o", "rss=,pcpu=", *[str(p) for p in pids]]
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=5).stdout
    except Exception:
        return {"rss_kb": 0, "cpu": 0.0, "alive": 0}
    rss, cpu, alive = 0, 0.0, 0
    for line in out.splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        alive += 1
        rss += int(parts[0])
        cpu += float(parts[1])
    return {"rss_kb": rss, "cpu": round(cpu, 2), "alive": alive}


class Checker:
    """统一 PASS/FAIL/SKIP 汇总（沿用 gui_debug_accept.py 输出协议）。

    SKIP：环境受限（如 macOS 锁屏/全遮挡冻结 WebKit rAF——hover 类用例无法执行），
    不计入失败也不计入通过数；summary 单列，与产品缺陷严格区分。
    """

    def __init__(self):
        self.fails = []
        self.skips = []
        self.count = 0

    def check(self, name, cond, detail=""):
        self.count += 1
        line = ("PASS " if cond else "FAIL ") + name + (f"  | {detail}" if detail else "")
        print(line, flush=True)
        if not cond:
            self.fails.append(name)
        return bool(cond)

    def skip(self, name, note=""):
        """环境受限用例：不计 PASS/FAIL，summary 单列（区别于产品缺陷）。"""
        self.skips.append(name)
        print("SKIP " + name + (f"  | {note}" if note else ""), flush=True)
        return False

    def summary(self, group):
        print(flush=True)
        print("=" * 50)
        print(f"[{group}] {self.count - len(self.fails)}/{self.count} 通过"
              + (f"（SKIP {len(self.skips)}：{'; '.join(self.skips)}）" if self.skips else ""))
        print("FAIL:", self.fails if self.fails else "无")
        return len(self.fails)

    def exit(self):
        sys.exit(1 if self.fails else 0)


if __name__ == "__main__":
    # 环境自检：发现文件 → 鉴权 → ping → frontend_ready → 设备清单
    if "--check" not in sys.argv:
        sys.exit("用法: harness.py --check  # 前置：GUI 已在运行")
    cfg = json.load(open(GUI_DEBUG_JSON))
    print(f"发现文件: {GUI_DEBUG_JSON}")
    print(f"  port={cfg['port']} pid={cfg.get('pid')} alive={_pid_alive(cfg.get('pid', 0))}")
    cli = Client()
    r = urllib.request.Request(cli.base + "/api/status")
    try:
        urllib.request.urlopen(r, timeout=5)
        print("无 token → 200（异常：应 401）")
    except urllib.error.HTTPError as e:
        print(f"无 token → {e.code}（应 401）")
    print("ping:", cli.req("GET", "/api/ping")[1])
    st = cli.status()
    print("frontend_ready:", st.get("frontend_ready"))
    print("remote:", st.get("remote"))
    print("devices:", [
        (d["serial"], d.get("is_gateway")) for d in st.get("devices", [])
    ])
    print("sessions:", [(s["serial"], s.get("sampling")) for s in st.get("sessions", [])])
    print("webkit 子进程:", webkit_children(cfg.get("pid", 0)))

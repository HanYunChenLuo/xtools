---
name: xperf-gui-debug
description: "xperf GUI 可编程调试接口：读取/操控运行中的 xperf-gui。当需要验证 xperf GUI 行为（采样状态、图表数值、悬停读数、tab 切换、logcat 视图）、自动化目验 GUI 改动、或以编程方式点击/填值/悬停 GUI 元素时使用。Use when asked to verify, drive, or regression-test the xperf-gui desktop app programmatically (DOM/geometry/state reads + click/input/hover injection), instead of screenshots or CGEvent mouse simulation."
metadata:
  requires:
    bins: ["python3"]
---

# xperf-gui-debug — 运行中 GUI 的可编程操控

xperf-gui 内嵌**默认开启**的 loopback 调试接口（`XPERF_GUI_DEBUG=0` 才关闭）。
它替代三类脆弱目验手段：CGEvent 坐标模拟 / screencapture 截图比对 / macOS
AX 树（WKWebView AX 冻结于启动早期快照，不可靠）。设计与完整语义见仓库
`docs/DESIGN-gui-debug.md` 与 CLAUDE.md「GUI 可编程调试接口」节。

## 启动方式

GUI 的 WebKit 子进程不能依赖当前终端会话存活。终端、SSH 或 agent shell 启动时使用独立会话，避免宿主会话结束发送 `SIGHUP` 导致主窗口保留但 DOM/点击无响应：

```bash
setsid nohup xperf-gui > /tmp/xperf-gui.log 2>&1 < /dev/null &
```

仅使用 `nohup` 不足以保护 WebKit 子进程。桌面启动器直接启动不受该问题影响。若接口突然连续返回前端超时，先检查 `WebKitWebProcess` 子进程和启动会话；不要先归因于 GPU 或 AppImage 沙箱。

## 发现与鉴权

```bash
# 端口与 token（每启动随机；多开时取 pid 存活且 started_at 最新者）
cat ~/.config/xperf/gui-debug.json        # {pid, port, token, version, started_at}
# 全部请求带 header:  X-Xperf-Token: <token>   （缺失/错误 → 401，含 404 路径）
```

统一应答：成功 `{ok:true, data:…}`；失败 `{ok:false, error:…}`（400 参数 /
401 鉴权 / 404 selector 未命中 / 503 前端未就绪或无 AppHandle / 504 前端超时 5s）。

## 端点速查

| 端点 | 用途 |
|---|---|
| `GET /api/status` | 后端真相：版本/uptime/remote/设备列表（含 platform）/每设备 sampling·trace·stack·logcat·mirror·recording·agent_building·csv_dir。`frontend_ready=false` 时前端类端点 503——**先轮询它** |
| `GET /api/dom?selector=<css>&depth=4&max_nodes=400` | DOM 子树：tag/attrs/text/**rect[x,y,w,h]**/visible |
| `GET /api/state?serial=<s>` | 设备会话快照：statusText/activeTab/samplingRunning/liveData/peaks/coldStarts/每图表 series 摘要/悬停 tooltip/logcatTail |
| `GET /api/series?serial=<s>&metric=cpu&tail=100` | 单图表全分辨率读数；`at=<epoch_ms>` 定点二分取值 |
| `POST /api/action` | `{"op":"click|input|select|check|hover|scroll|key", "selector":"…", "value":…, "x":…,"y":…, "scrollX":…,"scrollY":…}` |
| `POST /api/eval` | `{"expr":"…"}` 任意 JS（async 自动 await），前端顶层作用域（`app`/`DeviceSession` 可达） |

设备页内元素选择器须带作用域：`.device-page[data-serial="<serial>"] .start-btn`。

## 操作语义与坑（都踩过）

- action 全部真实 DOM 事件序列：click=mousedown+mouseup+click；input=focus+
  原生 setter 写值+显式 input+change（不会不发 change）；hover=mouseover+
  mouseenter+mousemove（带 clientX/Y，直驱图表悬停读数）。
- **悬停 tooltip 是 rAF 合帧**：hover 后必须轮询 `/api/state` 的
  `charts.<metric>.hover`；且宜**轮询式反复 hover**（模拟真实鼠标连续移动）。
- **隐藏 tab 的 canvas 零尺寸**：hover/点击图表前先 click 对应
  `.tab.subtab[data-tab=...]` 切 tab；dom 快照的 `visible`/`rect` 可判可见性。
- **自动化运行期间勿动物理鼠标/窗口**：真实 mousemove/mouseleave 与合成
  hover 竞争会清掉读数。
- 采样「停止」后在途事件可再到一个节拍，断言序列停增要错开观察窗。
- logcat ring buffer 保留过滤口径切换前的行；判定过滤生效只看最后一条
  `# xperf logcat respawn` 标记之后的行。

## 全链路验收

```bash
# 前置：GUI 已启动（可带 --remote <host> --device <serial>），设备在线
python3 <repo>/scripts/gui_debug_accept.py <serial>
# 覆盖：鉴权/就绪/dom/打开应用+冷启动/开始监控/series 增长/悬停读数/series
# 取值/切 tab/logcat 出行与文本过滤热切换/eval 一致性/停止停增
```

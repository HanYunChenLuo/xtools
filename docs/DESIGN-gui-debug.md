# DESIGN: GUI 可编程调试接口（debug API）

2026-09-19 v1（方案经用户评审拍板：axum+tokio / token 鉴权 / eval 逃逸舱一期 / 默认开启含 release）

## 背景与目标

GUI 行为验证目前只有两条路，都很脆：

- CGEvent 鼠标模拟 + `screencapture` 截图 + 按布局估算坐标（布局变即失效）；
- macOS AX 树——WKWebView 实例的 AX 子树冻结于启动早期快照，新插入内容不可达
  （见 CLAUDE.md「AX 目验教训」）。

目标：GUI 内嵌一个 **默认开启** 的本机调试接口，让脚本/coding agent 能
①读取 UI 结构与几何位置（DOM 树 + bounding rect）②读取性能监控状态
（状态栏/实时数值/图表 series/悬停 tooltip/logcat 行）③注入操作
（click/input/hover/scroll/key 等真实 DOM 事件）④任意 JS 逃逸舱。
用途：GUI 改动的自动化目验回归，替代截图比对。

非目标：服务端推送（调用方轮询）、跨设备 adb 操作（那是 Tauri 命令的事，
eval 可直接 invoke）、Windows（GUI 仅 macOS/Linux）。

## 架构

```
agent/curl ──HTTP──▶ 127.0.0.1:<随机端口>（axum server，GUI 进程内）
                        │
        ┌───────────────┴────────────────┐
        │ 后端直接答（进程内真相）        │ 前端往返答（DOM/JS 状态）
        │ /api/status                   │ emit 'xperf-debug' {id, op, params}
        │                               │   → main.js 调试钩子执行
        │                               │   → invoke('debug_respond', {id, result})
        │                               │   → pending map oneshot 唤醒 → HTTP 响应
        └───────────────────────────────┘
```

- **HTTP 库 = axum + tokio**：tokio "full" 已是 GUI/core 直接依赖；独立
  std 线程里起 current_thread runtime，不耦合 Tauri 内部 runtime。
  handler panic 逐请求隔离成 500；`tower::ServiceExt::oneshot` 支持不绑
  端口的进程内路由层单测。WebSocket 被否：推送非需求、curl 不可调试、
  请求/响应配对要自建（详见会话记录，WebDriver 先例 = 纯 HTTP REST）。
- **前端通信走 Tauri 事件/命令**（不引 WebSocket、不改 CSP、前端零网络
  代码）：后端 `emit` 请求、前端钩子执行后 `invoke('debug_respond')` 回传。
  钩子在 main.js 顶部注册（早于一切业务 listener，无「事件早于注册」窗口）。
  前端单线程天然串行化；后端每请求挂 5s 超时 → 504。
- **WebKit 主线程阻塞风险**：钩子里禁止同步 XHR/长循环；所有操作
  O(节点数) 有上限保护。

## 默认开启与安全

默认开启是用户拍板（含 release DMG），安全底线三件套：

1. **仅绑 `127.0.0.1`**（loopback，不暴露局域网）；
2. **每启动随机 token**：32 字节随机 hex，请求须带 `X-Xperf-Token` 头，
   缺失/错误 → 401。token 只落 0600 权限的发现文件（仅属主可读）——
   本机其他进程无法无感操控 GUI（调试接口间接可 invoke 任意 Tauri 命令，
   等价于能对设备 force-stop，必须挡）；
3. **发现文件**：`~/.config/xperf/gui-debug-<pid>.json`（0600，原子写：
   tmp+rename）+ `gui-debug.json` 指向最新实例（支持多开，消费方按
   pid 活性/启动时间挑）；内容 `{pid, port, token, version, started_at}`。
   退出时清理（仅删自己那份 + latest 指针指向自己时）；启动时顺带清扫
   pid 已死的陈旧文件。
   `XPERF_GUI_DEBUG=0` 显式关闭（其余值/未设 = 开启）。

## API 表面

统一 JSON；成功 `{ok:true, data:…}`，失败 `{ok:false, error:…}`（HTTP
状态码同步：400 参数错 / 401 未授权 / 404 目标不存在 / 500 内部错 /
504 前端超时）。

| 端点 | 应答方 | 说明 |
|---|---|---|
| `GET /api/status` | 后端 | 版本/pid/运行时长；remote 状态；设备列表（serial/model/version/platform/offline）；每设备采样·trace·stack·logcat·镜像·录屏 running、package、csv_dir、agent_building |
| `GET /api/dom?selector=<css>&depth=<n>&max_nodes=<n>` | 前端 | DOM 子树快照：tag/id/class/全部 attributes（截断）/text（截断）/rect [x,y,w,h]/visible；默认 root=body depth=4 max_nodes=400（防爆上限硬顶 2000） |
| `GET /api/state?serial=<s>` | 前端 | 设备会话状态快照：statusText+progress、activeTab、samplingRunning、realtime 数值面板、peaks、每图表 series 摘要（点数/末值/末时间戳/窗口 min-max）、悬停 tooltip（null 或 {t, rows}）、冷启动面板、logcatBuf 尾 N 行 |
| `GET /api/series?serial=<s>&metric=<m>&tail=<n>` | 前端 | 单图表全分辨率读数（series 名 → 尾 n 点 [{t,v}]；at=<ms> 单点取值走二分同悬停口径） |
| `POST /api/action` | 前端 | `{op, selector?, x?, y?, value?, key?}`：op ∈ click / input / select / check / hover / scroll / key / tap_xy。selector 命中失败 404；坐标模式经 elementFromPoint |
| `POST /api/eval` | 前端 | `{expr}` 任意 JS（async 表达式自动 await）；结果 JSON 序列化回传（循环引用/undefined 兜底为字符串）。逃逸舱，覆盖 DOM/state 长尾 |

action 事件语义（根治 AX 时代的坑，全部真实 DOM 事件）：

- `click`：`mousedown`+`mouseup`+`click`（bubbles，含 clientX/Y 于元素中心
  或指定偏移）——按钮/设备 tab/tab 页切换通用；
- `input`：`focus()` → setter 写值（用原生 setter 绕过框架拦截）→
  `input` 事件 → `change` 事件（AX 教训：change 随 blur 派发、合成事件
  不一定触发，这里显式全发）；
- `select`：同上写 `<select>.value` + change；
- `check`：checkbox/radio 设 checked + change；
- `hover`：`mouseover`+`mouseenter`+`mousemove`（clientX/Y）——直接驱动
  图表悬停读数（LineChart.onHover 用 `e.clientX - rect.left`，合成事件兼容）；
- `scroll`：写 scrollTop/scrollLeft + scroll 事件；
- `key`：keydown+keyup（key/code/修饰键）派发到焦点元素或指定 selector。

## 落点与文件

- 新增 `xperf-gui/src/debugsrv.rs`：server 启动/发现文件/token 中间件/
  路由/pending map/`debug_respond` 命令。
- `xperf-gui/src/main.rs`：setup 里启动 debug server（返回句柄存 AppState
  供状态聚合）；`generate_handler` 加 `debug_respond`。
- `xperf-gui/frontend/main.js`：顶部调试钩子（listen 'xperf-debug' +
  op 分发 + DOM walker + 状态采集器 + action 执行器），~200 行。
- 单测：debugsrv.rs 内（token 中间件/发现文件原子写与清扫/路由层
  tower oneshot 进程内测试/status 序列化）。
- 文档：CLAUDE.md 加「GUI 调试接口」节；CHANGELOG [Unreleased]；
  WORKSPACE I 节勾销。

## 验证（目的即手段）

启动 GUI（`--remote <host> --device <serial> --package gltf --cpu --memory
--fps`）后全程 API 驱动，零 CGEvent/screencapture：

1. 发现文件可读 → token 取到；无 token 请求 401；
2. `/api/status`：设备在线、采样 running、agent_building=false；
3. `/api/dom`：设备页结构 + rect 非零；
4. 轮询 `/api/state`：series 点数增长、realtime 数值非空；
5. `/api/action hover` CPU 图 canvas 中心 → `/api/state` tooltip 非 null
   且含数值；
6. click 切 tab（日志/Perfetto）→ `/api/dom` 反映激活态；
7. input 填 logcat 过滤框 → state 反映；
8. `/api/eval` 取 `app.sessions.size` 与 status 一致；
9. 全程 UI 无卡死（采样事件流正常，state 点数持续增长）。

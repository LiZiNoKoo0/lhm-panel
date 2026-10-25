# HANDOVER · PC 侧与打包（2026-10-25 18:30）

> 这份文档只管**上位机（Python/Web/EXE）**。
> 固件侧看 `firmware/HANDOVER.md`，移植标准看 `firmware/SPEC.md`，
> 每个 bug 的「症状 → 根因 → 改法」看 `firmware/BACKLOG.md`。
>
> 读的顺序：本文 → `TASKS.md`（M/P 卡状态）→ `PROTOCOL.md`（线协议）。

---

## 0. 一句话状态

**上位机功能全部可用，打包成 EXE 时卡在「窗口里是空白页」。**

- daemon、webui、布局编辑器、真采集、回放、桌面终端——都已验过
- EXE 能构建、能启动、三个部件（daemon / webui / bridge）都跑起来了
- **但 EXE 打开的那个窗口没有显示仪表盘**，这是唯一未解决的问题

---

## 1. 已经做完并验过的（不要重做）

| 卡 | 交付物 | 怎么验的 |
|---|---|---|
| M1 | `pcmon/protocol.py` | `python -m pcmon.protocol` → `ALL OK`（10/10） |
| M2 | `fake_daemon.py` | 板子 / `term_tk` / webui 三种终端同时接入 |
| M3 | `term_tk.py` | `--selftest` → **22 项全过** |
| M4 | `replay.py` | 探针抓帧逐帧比对 → **60/60 完全相同** |
| M5 | `pcmon/lhm.py` | 杀 `lhm_bridge.exe` → **0.3 秒**全部变 `null`，自动重启恢复 |
| P1 | `preview_st7735.py` | 与固件 CRC 逐字节相等 |
| P2 | `pcmon/webui.py` + `webstatic/` | 布局编辑器端到端上板（apply → 屏上真的变了） |
| — | `pcmon/layouts.py` | 已应用布局持久化，`ver` 只在 apply 时 +1 |

工具（新加，可复用）：

| 文件 | 用途 |
|---|---|
| `tools/watch_nulls.py` | 每个 data 帧的 live/null 计数 |
| `tools/capture_tape.py` | 抓一个客户端收到的原始字节 |
| `tools/check_replay_parity.py` | 录像 vs 回放逐帧比对 |
| `tools/check_flash_layout.py` | 解析 `espflash read-flash` 的转储，验布局落盘 |
| `tools/serial_tolerant.py` | 跨 CH343 掉线重试的串口读取器 |
| `start-daemon.ps1` | 从 `daemon.token` 读 token 启 daemon（避免手打） |

---

## 2. 打包（未完成）—— 这是接手的第一件事

### 2.1 已经解决的三个问题

**① `%TEMP%` 不可写**
```
[PYI-11136:ERROR] Could not create temporary directory!
```
单文件模式要解压到 `%TEMP%`。启动器里可显式指定，或保证 TEMP 可写。

**② `daemon.token` 写进了临时解压目录**
PyInstaller 下 `fake_daemon.__file__` 指向 `_MEIPASS`，daemon 就把 token 写在那儿，进程一退就没了 → webui 报 `no token`。
**已改**：启动器自己读写 token，显式传给 daemon 和 webui，握手里不再有文件。

**③ 桥接路径硬编码 `H:\tools\lhm\lhm_bridge.exe`**
换台机器就没有。**已改**：`pcmon/lhm.py` 加 `_default_bridge_path()`，冻结时查 `sys._MEIPASS`。
日志里能看到证据：`source : lhm bridge ...\_MEI00000f702\lhm_bridge.exe (found)`。

### 2.2 崩因已查明：`0xC0000409`

事件日志（应用程序）里有两次：
```
15:55:22  异常代码 0xc0000409  错误模块 ucrtbase.dll
17:50:29  异常代码 0xc0000409  错误模块 ucrtbase.dll
```

`0xC0000409` = fail-fast，**不是 Python 异常**，所以控制台一行都不留、窗口瞬间消失。

**因果链（已确认）**：
1. token 竞态 → webui 启动失败并 `SystemExit` → `main()` 提前返回
2. daemon 线程还活着，仍持有 stdout
3. 解释器退出时抢锁 → `_enter_buffered_busy` → CRT invalid parameter → fail-fast
4. 控制台窗口消失 = 用户看到的「双击没反应」

**已改**：启动器持有 token（断掉第 1 步）；主线程 `while True` 保活（断掉第 2 步）。

### 2.3 当前卡点：窗口是空白页

窗口起来了（Edge `--app=` 模式，6 个进程带 `--user-data-dir=…\browser-profile`），
**但页面是空白**，用户描述「长得跟 edge 的似的」——即 Edge 自己的空白页，不是仪表盘。

**已验证不是服务端问题**（全部返回 200）：
```
/               200  17158
/tokens.css     200  15623
/editor.js      200  22009
/app.js         200  29424
/api/font       200   4074
/api/state      200   1491
/api/layout     200     21
```

**当前假设**：全新的 `--user-data-dir` 触发 Edge 首次运行流程，忽略 `--app=` 的导航。

**已改但未验证**：加了
```
--no-first-run
--no-default-browser-check
--disable-features=msEdgeIdentityFRE,EdgeCollectionsFRE
--noerrdialogs
```
并把就绪判定改成「必须拿到 HTTP 200 且读到字节」，失败会在 `PC-Monitor.log` 里留 `webui ready=false`。

### 2.4 下一步（按顺序）

1. **重新构建并验证窗口** —— `launcher.py` 已改好，只差一次 build + 看一眼
2. 若仍空白：在那个窗口按 `Ctrl+Shift+I`，看 Console 报什么
3. 若 Edge app 模式始终不听话，备选：
   - `--windowed` + 修好 pywebview 的 .NET 后端（需要 `pythonnet` + WebView2 程序集正确打包，
     第一次尝试在 `webview/platforms/winforms.py` 导入阶段就失败）
   - 或者干脆放弃内嵌，做成「托盘程序 + 打开默认浏览器」，把预期讲清楚
4. 构建命令（当前用的）：
```powershell
pyinstaller --onefile --noconfirm --name PC-Monitor `
  --add-data "pcmon/webstatic;pcmon/webstatic" `
  --add-binary "H:\tools\lhm\lhm_bridge.exe;." `
  launcher.py
```

---

## 3. 两条测试方法论（血泪）

### 3.1 从 exec 会话里测「exe 能不能活」是测不出来的

我起的所有子进程都会**随会话结束被终止**，于是「进程没了、端口空了」被我误读成「exe 崩了」，
查了很久。

**正确做法**：用 WMI 启动，进程不属于当前会话：
```powershell
Invoke-CimMethod -ClassName Win32_Process -MethodName Create `
  -Arguments @{ CommandLine = 'H:\newwork\dist\PC-Monitor.exe --source fake' }
```

### 3.2 ⚠️ 绝不要 `Stop-Process msedge`

我为了「干净地重测」写了：
```powershell
Get-Process PC-Monitor,msedge | Stop-Process -Force   # ← 把用户自己的浏览器也杀了
```
**这会关掉李子正在用的所有 Edge 窗口。** 2026-10-25 已经犯过一次。

**正确做法**：只杀带我们 profile 的那些：
```powershell
Get-CimInstance Win32_Process -Filter "Name='msedge.exe'" |
  Where-Object { $_.CommandLine -match 'browser-profile' } |
  ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
```

> 同类教训：`Stop-Process` 打到的可能不是你以为的那个进程
> （固件侧也踩过：`Stop-Process` 杀的是 shell 包装进程，真正的 python 还在跑）。

---

## 4. 配置与路径

| 项 | 值 |
|---|---|
| Python | `C:\Users\Administrator\AppData\Local\Programs\Python\Python312\python.exe` |
| 桥接 | `H:\tools\lhm\lhm_bridge.exe`（打包时内置） |
| daemon 端口 | 7802 |
| webui 端口 | 7803 |
| token 文件 | 项目根 `daemon.token`（板子 flash 里是 `0123456789abcdef`） |
| 已应用布局 | `layouts/applied.json`（已 gitignore，不入库） |
| 打包产物 | `dist/PC-Monitor.exe`（已 gitignore） |

---

## 5. 还没做的（PC 侧）

| 项 | 说明 |
|---|---|
| **EXE 窗口显示** | 见 §2.3 / §2.4，唯一阻塞项 |
| 协议 v0.5 的 PC 义务 | `catalog` 带 `source: local\|remote`、不对 `local.*` 发 `data`、`link.alive`。**要等固件 L 系列有 `peripherals` 才有东西可测** |
| 编辑器「可选中」勾选框 | 现在造 `focusable` 页面只能用 API（`POST /api/layout/apply`） |
| `daemon.capabilities()` | 已写好（读终端声明的能力），但**没有任何调用者**——是一段没接线的代码 |
| M3 的 Tk 窗口 | `--selftest` 全过，但**没人亲眼看过那个窗口长什么样** |

---

## 6. 一句话给下一个人

上位机是能用的，只有「打包成 EXE 后窗口里是空白」这一件事没解决。
崩因（`0xC0000409`）已经查清并修掉，现在只是 Edge app 模式不导航。
**别再杀 `msedge` 全部进程。**

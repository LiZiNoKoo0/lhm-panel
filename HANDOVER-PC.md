# HANDOVER · PC 侧与打包（2026-10-25 18:30）

> 这份文档只管**上位机（Python/Web/EXE）**。
> 固件侧看 `firmware/HANDOVER.md`，移植标准看 `firmware/SPEC.md`，
> 每个 bug 的「症状 → 根因 → 改法」看 `firmware/BACKLOG.md`。
>
> 读的顺序：本文 → `TASKS.md`（M/P 卡状态）→ `PROTOCOL.md`（线协议）。

---

## 0. 一句话状态

**上位机全部可用；打包成 EXE 的内嵌 WebView2 窗口已验通（2026-10-25 20:02）。**

- daemon、webui、布局编辑器、真采集、回放、桌面终端——都已验过
- EXE 用 pywebview/WebView2 开自己的窗口，不再依赖 Edge；`--window-selftest` 把渲染结果写进日志作验收
- 当前已知问题清单见 §2.3（四个坑均已修）和 §5

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

## 2. 打包（已解决，2026-10-25 20:02）

**`dist\PC-Monitor.exe` 现在用内嵌 WebView2 显示仪表盘，不再依赖 Edge。**
自检证据（`dist\PC-Monitor.log`）：

```
window probe: {"title":"PC-Monitor · 上位机仪表盘","kpis":4,"badge":"实时","chars":328}
=== exit: code=0 ===
```

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

### 2.3 为什么曾经是空白页，以及四个真坑（都已修）

当初的空白页其实是 **Edge 的首启/同步确认页**：`--app=` 页面确实加载了（CDP 能看到
`PC-Monitor · 上位机仪表盘`），但新 profile 弹的 `edge://sync-confirmation-dialog/` 盖在上面。
结论：不再和 Edge 的 profile 机制搏斗，改用 **pywebview 内嵌 WebView2**。接手时踩过的四个坑：

| # | 症状 | 真因 | 修法 |
|---|---|---|---|
| 1 | `Failed to resolve Python.Runtime.Loader.Initialize from ...\_MEI...\pythonnet\runtime\Python.Runtime.dll` | pythonnet 默认选 .NET Framework，onefile 包里加载不了 netstandard2.0 的 Python.Runtime.dll（源码环境是好的，所以「刚才明明好了」） | `launcher._prepare_dotnet()`：写 `PC-Monitor.exe.config`，按 `netfx→coreclr→default` 顺序试，探针是 `clr.AddReference('System.Windows.Forms')` + `from Microsoft.Win32 import SystemEvents` |
| 2 | `Could not load file or assembly 'System.Windows.Forms'`（coreclr 下） | WinForms 在 `Microsoft.WindowsDesktop.App` 里，pythonnet 自动生成的 runtimeconfig 只声明了 `Microsoft.NETCore.App` | 生成 runtimeconfig 同时声明两个 framework，版本号从 `%ProgramFiles%\dotnet\shared\Microsoft.WindowsDesktop.App` 现场读 |
| 3 | `You must have pythonnet installed in order to use pywebview`（误导性最强） | `webview.guilib.try_import` 只 catch `ImportError`，把 .NET 异常吞了；webview 子模块缺失也报这条 | launcher 自己先 `import webview.platforms.winforms`（真异常进日志）；spec 用 `collect_submodules('webview')` |
| 4 | `AttributeError: 'NoneType' object has no attribute 'GetMethod'`（winforms.py 668/680） | **pywebview 6.2.1 的 bug，与打包无关**：`OpenFolderDialog` 在类定义时反射 `FileDialogNative+IFileDialog`，.NET Core WinForms 没有这个类型，源码下一样炸 | `tools/fix_pywebview.py`：幂等补丁，把该块包进 try/except，文件夹选择器不可用时 `show()` 报清晰错误；`--revert` 可还原；留 `.bak` |

**重新构建前必须先跑** `python tools\fix_pywebview.py`（site-packages 被重装就会退回未补丁状态）。

### 2.4 构建与自检（现在用这个）

```powershell
python tools\fix_pywebview.py                       # 补丁 site-packages（幂等）
pyinstaller --noconfirm --clean --distpath dist --workpath build PC-Monitor.spec
# 自检：窗口自动打开、把渲染结果写进日志、然后自己关闭
dist\PC-Monitor.exe --source fake --window webview --window-selftest
Get-Content dist\PC-Monitor.log     # 期望 window probe: {"title":"PC-Monitor · 上位机仪表盘",...}
```

自检探针是**验收的唯①证据**：没有它，脚本无法区分「窗口在工作」和「窗口是白的」——
正是本项目第一次翻车的地方。spec 里的 upx 保持开着（这台机器 PATH 上没有 upx，实际未压缩；
若将来启用了 upx，必须把 .NET 程序集排除，否则会复现坑 1 的症状）。

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
| 协议 v0.5 的 PC 义务 | `catalog` 带 `source: local\|remote`、不对 `local.*` 发 `data`、`link.alive`。**要等固件 L 系列有 `peripherals` 才有东西可测** |
| 编辑器「可选中」勾选框 | 现在造 `focusable` 页面只能用 API（`POST /api/layout/apply`） |
| `daemon.capabilities()` | 已写好（读终端声明的能力），但**没有任何调用者**——是一段没接线的代码 |
| M3 的 Tk 窗口 | `--selftest` 全过，但**没人亲眼看过那个窗口长什么样** |
| 配对/多设备（李子的提案） | 多 token 注册表（按 token 作主键、MAC 作显示名）→ 串口配对工具 → 固件生成 token；设计讨论见会话记录，未动工 |
| console=True 可改 windowed | launcher 已兜住 `sys.stdout is None`，想去掉黑控制台时改 spec 的 `console=False` 即可 |

---

## 6. 一句话给下一个人

EXE 的窗口问题已经结案：**pywebview + WebView2，`--window-selftest` 是验收证据，重建前先跑 `tools/fix_pywebview.py`**。
认证还是静态共享 token（不是挑战应答），且 exe 与板子的 token 必须一致——用 `--token ***。**别再杀 `msedge` 全部进程。**

## 顶层状态必须声明在 boot() 调用点之上（2026-10-27，连踩三次）

`pcmon/webstatic/app.js` 的 `boot()` 在文件中部就执行；任何被它链条用到、却声明在它下面的
`const` / `let` 都处在**临时死区**。抛出的 `ReferenceError` 会被 `pairedKey()` 之类自带的
`try/catch` 吞掉，所以症状不是异常，而是"请求没带 key / 页面没数据"，极难反推。

这一条已连踩三次：`keyStoreError`（ba7a628）、`memoryKey`（806b87b）、`KEY_STORE`（8b6bd4f）。
最后一次的表现最迷惑：同一个页面里，服务端看到 `token=***`（没带 key），而文件末尾的自检却报
`hasKey:true`——因为自检运行时变量早已初始化。

规矩：**所有顶层状态集中在 `boot()` 调用点之上**，新增一条也不许放下面。
改完必须跑 `node --check pcmon\webstatic\app.js`，并确认 `const KEY_STORE` 与 `let pendingPair`
的位置都在 `boot().catch` 之前。
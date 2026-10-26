# 任务卡

> 给编码 agent 的任务规格。**动手前先通读 `PROTOCOL.md`**，它是唯一真实来源。
> 语言：Python 3.12（`C:\Users\Administrator\AppData\Local\Programs\Python\Python312\python.exe`）。
> 只用标准库，不要装第三方包（M5 真采集阶段再说）。

---

## 状态总览（2026-10-25 07:20）

| 卡 | 交付物 | 状态 | 证据 |
|---|---|---|---|
| M1 | `pcmon/protocol.py` | ✅ 完成 | `python -m pcmon.protocol` → `ALL OK`（10/10 用例） |
| M2 | `fake_daemon.py` | ✅ 完成 | 一直在跑；板子/桌面/网页三种终端同时接入 |
| M3 | `term_tk.py` | ✅ 完成 | `--selftest` 22 项全过（2026-10-25） |
| M4 | `replay.py` | ✅ 完成 | 探针抓帧逐帧比对 60/60 相同 → `PARITY OK`（2026-10-25） |
| M5 | `pcmon/lhm.py` + `--source lhm` | ✅ 完成 | 真采集上屏；杀桥接 → **0.3 秒**变 `null`，自动重启恢复（2026-10-25） |
| P1 | `preview_st7735.py` | ✅ 完成 | 与固件逐字节 parity 已钉进 `panel-core/tests/parity.rs` |
| P2 | `pcmon/webui.py` + `webstatic/` | ✅ 完成（含新增编辑器） | 布局编辑器端到端上板验证（2026-10-25） |
| — | `pcmon/layouts.py`（新增） | ✅ 完成 | 已应用布局持久化；`ver` 只在 apply 时 +1 |

### C 系列（v0.6 认证 / 配对）

| 卡 | 交付物 | 状态 | 证据 |
|---|---|---|---|
| C1 | `PROTOCOL.md` v0.6：§2 重写（三帧挑战应答）、§2.1 握手、§2.2 配对、§2.3 串口配对、§2.4 密钥生命周期、§2.5 威胁模型、§2.6 服务模式；§4 消息总表、§5 hello、§18/§19 样例、§20 兼容表同步 | ✅ 完成（2026-10-25） | 文档已落盘；`hello` 不再带 `token`，残留提及只剩变更摘要与兼容表 |
| C2 | PC 侧实现：`pcmon/protocol.py` 的 mac/srv/challenge/auth、`fake_daemon.py` 握手门 + `--auth hmac\|static\|any` + 失败锁定、`term_tk`/`webui`/`launcher` 应答挑战、`pcmon/devices.py` 设备表 + `pcmon/pair.py`、`tools/check_auth.py` | ✅ 完成（2026-10-25） | `tools/check_auth.py` → **AUTH OK (12 checks)**（含重放拒绝、错 mac、跨 client_id 不通用、认证前零泄漏、5 s 超时、第 6 次 locked、static 模式两态）；`python -m pcmon.protocol` → **ALL OK 12/12**（含两条钉死的 mac/srv 向量）；`term_tk.py --selftest` → **22/22 PASS**（现在走的是真握手，不再是 --lenient 旁路） |
| C3 | 固件：`sha256.rs` + `auth.rs`、`wire` hello 去 token、`session` 的 `Challenged` 态、`net_link` 验 srv + 回 mac、三条串口行；串口 `KW` 配对（固件写盘已验，工具回执未验） | 🟡 1–3 上板验过；4 固件侧验过、工具侧未验（2026-10-26 13:01） | `cargo test` 207 绿、零 warning；板上 `AUTH ok` 用的是**真实 128 位 key** `5f2c1b7a…`；`STORE ok slot=A seq=8`（曾 B/5）证明 `KW` 写盘成功 |
| C5 | 手机：webui 放开 + HTTPS 自签 + 6 位配对码 + cookie 会话 | ⬜ 未开工 | |
| C6 | 指标源可插拔：`pcmon/sources.py` 一份契约 + 注册表，新增 `proc`（Linux /proc + /sys）；`fake_daemon` 里四处 `source_kind == SOURCE_LHM` 全部改成问接口 | ✅ 完成（2026-10-27） | `tools/check_sources.py` → Windows **SOURCES OK (36 checks)**、Linux **SOURCES OK (22 checks)**；banner 跟着源变；`python -m pcmon.protocol` 仍 **ALL OK 12/12** |


#### C3 现场状态（2026-10-26 01:20，睡觉前留档）

**已经成立的**（不是推断，是 daemon 日志原文）：

```
[01:14:20] discover from 192.168.1.53 -> here
[01:14:20] connection from 192.168.1.53:51190
[01:14:20] 192.168.1.53:51190: challenge sent (hmac-sha256/128)
[01:14:20] 192.168.1.53:51190: AUTH ok
[01:14:20] hello from st7735-160x80 role=thin 160x80 charset=ascii prims=rect,text,num fonts=16,32,48 auth=challenge-response
[01:14:40] closed st7735-160x80 (done) sent=15
[01:14:41] connection from 192.168.1.53:51190     <- 复位后自己重连
[01:14:41] challenge sent (hmac-sha256/128)
[01:14:41] AUTH ok
```

- 新固件已烧进 **A 板**（MAC `b0:3f:d3:42:c7:58`，COM10），烧写时间 01:11，App 717,376 B
- hello 里**没有 token**（`prims=rect,text,num`、`auth=challenge-response` 是新固件的签名）
- daemon 用严格 `--auth hmac` 跑的，不是 `any`

**现在的弱点，必须在 C3 第 4 步收掉**：

- 板子 flash 里的 key 仍是 v0.5 继承下来的 **`0123456789abcdef`**（16 字符 / 64 位，而且是仓库公开值）
- `devices.json` 里 `st7735-160x80` 就是这条；`webui` 那条是仪表盘自动配的（128 位随机）
- `launcher.py` 仍默认 `--auth any`（给旧固件留窗口），板子换成随机 key 之后应改成 `hmac`

**一个会误导人的现象**（已确认，不是 bug）：

复位后串口在 `STORE ok creds=yes slot=B seq=5` 之后就断，`WIFI init` 都不出现——
是 **WiFi 射频上电瞬间 CH343 整块掉出设备树**（HANDOVER §7.1 / §11）。
所以 `LINK challenge` / `LINK auth tx n=` 这两行抓不到；它们确实执行了，
证据是 daemon 回了 `AUTH ok`（mac 不对不可能通过）。

**下一步（按顺序）**

1. **修 KOK 捕获**：配对成功后板子立刻 `software_reset()`，CH343 随之重新枚举，
   回执跟着句柄一起没了。推荐改法：**配对后不自动复位** —— 先打 `KOK`，等工具发
   `KBOOT` 再复位。把等待时间从 200 ms 加到 1.5 s 也试过，不够，说明丢的是 USB 侧字节。
2. `pair --serial` 的"掉线重开"要等第 1 条生效才有意义（现在回执根本读不到）。
3. `launcher.py` 默认 `--auth any` 可以改成 `hmac` 了（板子不再需要旧路径）。
4. `daemon.token` 里那个公开值彻底删掉，别再有地方引用它。

**2026-10-26 13:01 的现场（已验证可用）**

- 板子 key = `5f2c1b7a9e3d4c6081a2b3c4d5e6f708`（128 位随机，由 `KW` 写进 flash）
- `devices.json` 里 `st7735-160x80` 就是它；`webui` 是仪表盘自配对的那条
- daemon 以 `--auth hmac` 跑，板子 `AUTH ok`，连接保持
- **仓库里那个公开的 `0123456789abcdef` 已经作废**

**这一轮学到的三条**

1. **开串口会复位板子**（DTR=IO0、RTS=EN）。所以"打开端口立刻发一行再等回执"在这块
   板子上天生不可靠：板子重启、CH343 重新枚举，回执和句柄一起消失。
2. **写盘成功 ≠ 工具知道**。`STORE ok slot=… seq=…` 的推进才是"写进去了"的证据；
   板子手里是哪把 key，可以用"把候选 key 逐个写进设备表、看哪一把 `AUTH ok`"反推
   （表是每次读盘的，改一行就生效，不用重启 daemon）。
3. **.NET 的 CWD 不是 shell 的 CWD**：`[System.IO.File]::ReadAllBytes('相对路径')` 会去
   `C:\Users\Administrator\.openclaw\workspace` 找文件。一律用绝对路径。

---|---|---|
| **M3** 桌面终端 | ✅ **PASS** | `term_tk.py --selftest` → `SELFTEST PASS (22 checks, 0 failed)`，1.4 s |
| **M4** 回放一致性 | ✅ **PASS** | `tools/check_replay_parity.py` → `PARITY OK`，逐帧比对 60/60 完全相同（除 `s` 序号） |
| **M5** 桥接掉线 | ✅ **PASS** | 杀 `lhm_bridge.exe` → **0.3 秒**全部 6 个值变 `null`（要求 3 秒内）；桥接自动重启 → `live=6` 恢复 |

#### M3 的注意事项（下次别再踩）

`term_tk.py --selftest` 是拿**当前生效的布局**做断言的。我先应用过一个「FONT TEST」页面
（3 个 text 节点、故意让 32/48 号字重叠），自检立刻报了 3 项失败：

```
FAILED no_text_overlap: overlaps=[('B32', 'C48')]
FAILED num_updated: num texts=[]
FAILED bind_w_scaled: rects=0
```

**那不是 term_tk 的 bug，是测试环境被污染。** 把 `layouts/applied.json` 移开、跑在内置布局上，
22 项全过。以后跑这个自检前先确认没有自定义布局生效。

#### M4 的两次错误方法（值得记住）

1. **解析回放器的日志而不是线上的字节**。日志把每个说过 hello 的客户端都混在一起，
   板子中途重连看起来就像「帧顺序错乱」——错的其实是我的解析器。
2. **用字节数当证据**。两个不同的帧可以长度相同，只有内容能证明一致。

现在的做法：一个探针连一次、抓自己那份原始字节，规范化 JSON（忽略字段顺序和 `s`）后逐帧比。
**时间不参与比较**——`--speed` 存在的意义就是改时间。

#### M5 的测量方式

用 `tools/watch_nulls.py` 直接看线上 `data` 帧，不看界面：

```
t= 28.3s  live=6  null=0   cpu.temp=38.0, cpu.total=58.98 ...
t= 29.3s  live=0  null=6                        ← 桥接被杀后 0.3 秒
```

daemon 日志同一时刻：`[10:11:03] lhm bridge restart in 1s (attempt 1)`。
之后桥接自动重启（新 pid），值恢复 `live=6`。

> `null` 与「省略 key」是两件事（PROTOCOL 9）：省略会让屏上保留旧值，`null` 才会变 `--`。
> 这条验证的正是 daemon 发的是 `null`。

---

## M1 · `pcmon/protocol.py` —— 协议库

### 输入
`PROTOCOL.md` 全文。

### 输出
一个可被 `import` 的模块，**不含任何 I/O**（不发不连不打印，纯计算）。

必须提供：

```python
# 编码：dict -> 单行 bytes（b'...\n'）
def encode(msg: dict) -> bytes

# 解码：单行 bytes/str -> dict；非法返 None（不要抛异常）
def decode(line) -> dict | None

# 分类：返回 'hello'|'catalog'|'ui'|'data'|'cmd'|'event'|... 或 None
def kind(msg: dict) -> str | None

# 能力裁剪：按客户端的 hello 限制 nodes
def fit_nodes(nodes: list, hello: dict) -> list
# - nodes 数量 <= hello['limits']['max_nodes']
# - type 必须在 hello['primitives'] + hello['widgets'] 里，否则丢弃
# - text/num 的 size 必须在 hello['fonts'] 里，否则取最接近的
# - hello['charset'] == 'ascii' 时，含非 ASCII 的 text 节点丢弃

# 构造器（返回 dict，方便测试）
def hello(client_id, role, w, h, **kw) -> dict
def catalog(ver, metrics) -> dict
def ui(scene, ver, nodes, bg="#000000") -> dict
def data(values: dict, colors: dict | None = None) -> dict
def cmd(action, target="*", **kw) -> dict

# 超时/新鲜度判定
def is_stale(entry: dict, now: float, factor: float = 3.0) -> bool
# entry = {"value":..,"ts":..,"interval_ms":..}
```

另外必须带一个 **`__main__` 自测**：

```
python -m pcmon.protocol
```
跑完打印 `ALL OK` 或失败的用例名。至少覆盖：
- 每种消息 encode→decode 往返相等
- 损坏的 JSON 返回 None
- 未知 `t` 返回 None（不崩）
- `fit_nodes` 在 max_nodes=2 时只留 2 个
- `fit_nodes` 在 primitives 只给 `["rect","text"]` 时丢掉 `path`
- `fit_nodes` 在 fonts=[12,16] 时把 size=28 改成 16
- `is_stale` 边界

### 禁止
- ❌ 不要开 socket、不要读文件、不要 print（`__main__` 里的自测除外）
- ❌ 不要引入第三方依赖

---

## M2 · `fake_daemon.py` —— 假数据 daemon

### 输入
`pcmon/protocol.py`

### 输出
一个能直接 `python fake_daemon.py` 跑起来的程序。

- 监听 `0.0.0.0:7802`，TCP
- 接受**多个**终端同时连接
- 每个连接**独立线程**，send 带 **200ms 超时**（见协议 §16 红线）
- 发送队列**只保留最新一帧**（新数据覆盖旧数据，不排队）

启动后自动生成假数据（不需要任何插件）：

| 指标 | 假数据 |
|---|---|
| `cpu.total` | 正弦波 20~80 |
| `cpu.temp` | 正弦波 45~75，周期更长 |
| `mem.used` | 缓慢上升 + 锯齿回落 |
| `net.down` | 随机尖峰 0~8000 |

每秒推一帧 `data`，200ms 抖动一次（可选，模拟真实波动）。

### 必须实现的行为

1. 收到 `hello` → 校验 → 立刻回 `catalog` + `scenes` + 当前 scene 的 `ui` + 一帧 `data`
2. 收到 `hello` 后**无条件重发**（不要假设终端缓存对）
3. 收到 `event` `{"k":"page","want":"next"|int}` → 切页并推对应 `ui`
4. 收到 `event` `{"k":"key","id":"focus","v":n}` → 在控制台打印 `FOCUS n`（不做别的）
5. 心跳：收到 `ping` 回 `pong`
6. **`--record <file>`** 参数：把所有**发出去的** NDJSON 原样写进文件（给回放器用）
7. **`--fake`** 是默认行为，不用加参数就是假数据
8. 同一个 `client_id` 重连 → **踢掉旧连接**（协议 §16 红线 5）

### UI 布局（服务端排版，240×320 竖屏）

至少 3 页：
- 页 0：CPU 标签 + 大数字 + 进度条 + 折线（`cpu.total`）
- 页 1：内存 + 网络
- 页 2：一个带 3 个 `focusable` 按钮的页（给 M3 验证焦点导航）

布局必须**按客户端的 hello 裁剪**（调 `fit_nodes`）。

### 验收
```
python fake_daemon.py --record recordings/session1.ndjson
```
另开一个窗口 `telnet 127.0.0.1 7802`，粘一条 hello 进去，应该立刻看到 catalog/ui/data 刷出来。

连两个终端，其中一个用 `timeout` 卡住不读，**另一个必须照常收到数据**。

### 禁止
- ❌ 不要真实采集任何系统数据（那是 M5）
- ❌ 不要用全局锁串行化所有发送（会复现"慢客户端堵死所有人"）

---

## M3 · `term_tk.py` —— 桌面模拟终端（thin）

### 输入
`pcmon/protocol.py`，连 M2 的 daemon。

### 输出
一个 tkinter 窗口，`python term_tk.py` 直接跑。

- 连接 `127.0.0.1:7802`
- 连上后发 `hello`：

```python
{"role":"thin","accept":["ui","data"],
 "display":{"w":240,"h":320,"depth":16},"charset":"utf8",
 "limits":{"max_nodes":48,"max_json_bytes":65536},
 "primitives":["rect","path","text","num"],
 "fonts":[12,16,24,32],"input":["key"]}
```

- 窗口按 `display.w/h` 定尺寸，`Canvas` 绘制
- 必须实现四个原语：`rect`（含 `fill`/`color`/`width`/`bind_w`）、`path`（`pts` 点串）、`text`、`num`
- 收到 `data` 只更新数值，**不要整屏重绘**（帧率会很惨）
- 收到 `ui` 重建整页
- 收到 `cmd`：`show` / `clear` / `reset` 都要处理，`brightness` 忽略

### 焦点导航（必须做，用来验证协议 §11.5）
- 从 `nodes` 里筛出 `focusable: true` 的，按 `focus` 字段（没有就按数组顺序）编号
- 方向键移动当前焦点，**画一个高亮框**在选中项上
- 回车 → 发 `{"t":"event","k":"key","id":"focus","v":<序号>}`
- ⚠️ 收到新 `ui` 后，焦点索引越界要重置为 0

### 状态提示（协议 §13.5）
- 15s 没收到任何数据 → 窗口右下角画 `LINK LOST`
- 右上角常驻小字：`N ms ago`（距上次 data 的秒数）

### 验收
1. `python fake_daemon.py` + `python term_tk.py` → 窗口里看到 CPU 数字和进度条在动
2. 按方向键能切焦点，回车在 daemon 控制台看到 `FOCUS n`
3. 按 `n` 键发 `{"k":"page","want":"next"}` 能翻页
4. 关掉 daemon → 15s 后出现 `LINK LOST`
5. 重新开 daemon → 3s 内自动重连并恢复画面

### 禁止
- ❌ 不要在终端里解析任何指标语义（除 `num` 的 `dec`/`unit` 格式化）
- ❌ 不要写死布局（一切从 `ui` 消息来）

---

## M4 · `replay.py` —— 离线回放器

### 输入
M2 `--record` 录的 NDJSON 文件。

### 输出
```
python replay.py recordings/session1.ndjson [--speed 1.0] [--loop]
```
- 起一个监听 7802 的假服务端，等终端连上
- 连上后按记录的顺序/时间间隔把消息灌进去
- 让 M3 拿到**完全一样**的画面，不用开真 daemon

### 用途
开发 ESP32 固件时，PC 端不用跑、不用真采集、不用等 CPU 波动。
**这是 M6 能快速迭代的前提。**

### 验收
用同一份录制，`fake_daemon` 和 `replay` 喂出来的 M3 画面一致。

---

## M5 · 真实采集 —— `pcmon/lhm.py` + `--source lhm`

### 输入
`lhm_bridge.exe`（源码在 `tools/lhm_bridge/`，说明见那里的 README）。每秒一行 NDJSON **数组**：

```json
[{"id":"/intelcpu/0/load/0","path":"Intel Xeon E3-1226 v3 / CPU Total",
  "name":"CPU Total","hw":"Intel Xeon E3-1226 v3","hwtype":"Cpu",
  "kind":"Load","value":37.5,"unit":"%"}, ...]
```

失效的传感器**不会出现在数组里**（不是给 null）。

### 输出

**`pcmon/lhm.py`** —— 一个 `LhmSource` 类，只管"把桥接的当前值拿出来"：

- `start()` 起子进程（stdout 管道），后台线程逐行读，**绝不阻塞调用方**
- 保留"最新一帧 + 它的到达时间"，不存历史
- `snapshot(now)` → `{metric_id: value 或 None}`
- 桥接 **3 秒**没有新帧 → 全部指标 `None`
- 桥接进程退出 → 全部 `None`，并按协议 §1 的退避阶梯（1/2/4/8/10s）重连
- 所有异常吞掉并计数，不许把 daemon 带崩

**`fake_daemon.py`** 增加两个参数：

- `--source fake|lhm`（默认 `fake`，保持现有行为不变）
- `--bridge PATH`（默认 `H:\tools\lhm\lhm_bridge.exe`）

lhm 模式下：`catalog` 只列真实可用的指标；`data` 帧里本帧拿不到或已失效的指标发 **`null`**（协议 §9：null = 显示 `--`，不是省略 key）。

### 指标映射

| 协议 id | 传感器（`hwtype` / `name` / `kind`） | 换算 |
|---|---|---|
| `cpu.total` | `Cpu` / `CPU Total` / `Load` | 原值 |
| `cpu.temp` | `Cpu` / `CPU Package` / `Temperature`，没有就 `Core Max` | 原值 |
| `mem.used` | `Memory` / `Total Memory` / `Load` | 原值 |
| `net.down` | `Network` / `Download Speed` / `Throughput`（取第一块有流量的网卡） | B/s → KB/s |
| `gpu.load` | `GpuNvidia` / `GPU Core` / `Load`（可选，没有就不进 catalog） | 原值 |
| `gpu.temp` | `GpuNvidia` / `GPU Core` / `Temperature`（可选） | 原值 |

匹配按 `hwtype` + `name` 精确匹配，**不要按 `path` 字符串匹配**（多显卡、多网卡会串）。

### 验收

```
python fake_daemon.py --source lhm
python term_tk.py
```

1. 屏幕上的 CPU 数字应随真实负载变化（打开任务管理器对照）
2. 跑个吃满 CPU 的脚本 → `cpu.total` 明显上升，`cpu.temp` 跟着上
3. 杀掉 `lhm_bridge.exe` → **3 秒内**数值变 `--`
4. 桥接重启 → 自动恢复，不需要重启 daemon
5. 拿 `recordings/` 里录的一段回放，画面与实时录制时一致（验证"采数换了但协议没变"）

### 禁止

- ❌ 不要在 Python 里另搞一套 WMI / 性能计数器采集（M5 只认 LHM 这一个源）
- ❌ 不要启动 `LibreHardwareMonitor.exe`，不要碰它的 GUI 配置
- ❌ 不要让桥接的读取阻塞 daemon 主循环或发送循环
- ❌ 不要在 lhm 模式下伪造数值填空

---

## P1 · `preview_st7735.py` —— ST7735 160×80 技术预览

**目标**：在写固件之前，先在 PC 上看见这块屏到底显示成什么样。不做 UI，一页只打一行字。

面板事实（写死，不做协商）：

| 项 | 值 |
|---|---|
| 分辨率 | 160 × 80（横屏） |
| 颜色 | RGB565，16 位 |
| 字模 | 8×16 点阵，`pcmon/font8x16.py` |
| 一行容量 | 20 字符 × 5 行（本工具只用一行） |

### 交付物

1. `preview_st7735.py` —— 声明 160×80，用 8×16 点阵渲染出**真实 framebuffer**，窗口放大显示（默认 4 倍），1/2/4/6/8 切换，`s` 存盘，`q`/Esc 退出
2. `pcmon/font8x16.py` —— 95 个可打印字符的字模表（第 32–126 号），每字 16 字节，最高位=最左像素，1=墨
3. `tools/font8x16_gen.py` —— 字模生成器（GDI 抠 1 位点阵），`--c FILE` 直接导出固件用的 C 头文件
   （`--rust FILE` 直接导出固件用的 Rust 字模表）
4. `firmware/st7735-text/` —— 真固件（**哑终端**）：手写 ST7735 SPI 驱动 + `src/font8x16.rs`，
   UART0 收一行 `<上排>|<下排>`，在 RAM 里拼出整帧 160×80（正好 25600 字节），
   先把这帧的 CRC32 打到串口，再把同一份字节推上屏
5. `firmware/check_parity.py` —— 用预览渲出同一帧、算同一个 CRC32，和板子报的比
6. `firmware/flash.ps1` —— 一条命令编译+烧录
7. `pcmon/serialport.py` —— ctypes 写的串口（读+写，不装 pyserial）
8. `live_serial.py` / `live.bat` —— 把**真实** CPU 占用率和温度每秒推到屏上（`pcmon/lhm.py` 供数）

固件不认识 "CPU" 这两个字：它只画收到的字节，排版（标签、右对齐、`--`）全在 PC 上。
这条分工和 M6 的 daemon 排版、终端零语义是同一条规矩，只是先跑在串口上。

### 为什么字模要自己抠

Tk 画出来的是 TrueType + 抗锯齿，和单片机的点阵**不是一回事**，预览会撒谎。
所以字模从固定宽度字体用 GDI 取**单色位图**，`pcmon/font8x16.py` 和固件里的
`font8x16[95][16]` 是**同一份字节**——预览和屏上一致是靠构造保证的，不是靠对眼睛。

### 验收

```
python preview_st7735.py --selftest                       # 6 项检查
python preview_st7735.py --text "CPU 33.5% 34C"           # 看窗口
python preview_st7735.py --text "CPU 33.5% 34C" --raw out.bin --no-window
```

1. framebuffer 必须是 160×80，`--raw` 必须正好 **25600 字节**（160×80×2）
2. 超过 20 字符要截断并留 `>` 标记，绝不撑宽画面
3. 非 ASCII 变 `?`，不允许出现字模表里没有的字形
4. 颜色要按 RGB565 量化后再显示；量化后跟你要的不一样时必须**明说**（例：`#010101` 会变成黑）
5. 固件侧：`python tools/font8x16_gen.py --c font8x16.h` 出来的表，与预览用的表逐字节相同
6. 烧进去之后 **`PARITY OK`**：板子报的 CRC32 必须等于预览渲出的那份字节的 CRC32
7. 接上实时数据：`live_serial.py` 每帧都打 `ok`，数字要和任务管理器对得上

```
powershell -ExecutionPolicy Bypass -File firmware\flash.ps1          # 编译 + 烧录（COM10）
python firmware\check_parity.py --port COM10                         # 比对像素，期望 PARITY OK
python live_serial.py --source demo                                  # 不装 LHM 也能试
live.bat                                                             # 真 CPU 占用 / 温度，1 Hz
```

屏幕上一共两行：`CPU  33.5%` / `TEMP   34C`。没有数据就是 `--`，绝不显示上一次的旧值。

### 串口复位这块的坑（下次别重踩）

- 板上 **DTR 接 IO0、RTS 接 EN**，而且是反相电路。复位时只能脉冲 RTS，
  **DTR 必须保持不置位**，否则 IO0 被拉低，芯片直接进 ROM 下载模式，串口一个字都不出（看起来像"板子没烧进去"）。
- `DCB.fDtrControl` / `fRtsControl` 必须是 ENABLE(1)，否则驱动会覆盖线状态，
  `EscapeCommFunction` 调了也白调。
- 这块 CH343 的 EN 脉冲大约三成会被吞掉，所以 `pcmon/serialport.py` 里是**重试式**复位：最多 4 次，每次等 2.5 秒。
- 更省事的结论：**实时画面根本不需要复位**。固件是被动渲染的，PC 打开串口直接发帧就行；
  `--reset` 只是用来看启动横幅（boot banner）。别为了"让它刷新"去反复拉 EN。

### 禁止

- ❌ 不要为了好看用 Tk 字体渲染文字——那是预览在撒谎
- ❌ 不要把这块屏接进 daemon 的布局体系（P1 不碰 `pcmon/protocol.py`，也不发 hello）
- ❌ 不要做多页/翻页/焦点（那是 M3 的事，M6 固件之前不做）

---

## 编码规范（四条，别省）

1. **纯 ASCII 的 .py 文件**（中文注释也不要——这台机器 PowerShell 5.1 会踩 GBK/UTF-8 的坑）
   要写中文就写进 `.md`，不要写进 `.py`。
2. 每个文件顶部写清楚它**不做什么**（边界比功能重要）
3. 异常一律吞掉并计数，**不要让单个连接的错误杀掉整个 daemon**
4. 所有魔数（端口、超时、间隔）提到文件顶部常量区

#### C3 收尾（2026-10-26 13:14）—— 串口配对端到端跑通

**链路**：`pcmon.pair --serial` → `firmware/pan.ps1`（.NET SerialPort）→ 板子写 flash → 回 `KRX`/`KOK` → 工具更新设备表。

**实测一轮**（工具自己 mint、自己写、自己记账）：

```
PAIR mint client=st7735-160x80 key=719f2d21d9c1d4edaafda95469293037
serial: KOK slot=A seq=24 len=32
PAIR ok (board stored the key, table updated)
```

板子侧旁证：复位后 `STORE ok creds=yes slot=A seq=24` + `TOKEN ok len=32`；
daemon 侧 `AUTH ok`（13:13:31）。**板子现在用的是一把 128 位随机 key，仓库里那个公开值彻底退休。**

**两个必须记住的判断**

1. **`pcmon.serialport.Port.read()` 在这台机器上读不到板子的输出**（同一时刻
   `pan.ps1` 能读到 `KRX`/`KOK`）。所以 `pair --serial` 走 `pan.ps1`，不是走自家封装——
   工具的可信度比代码的整洁度重要。`serialport.Port` 仍被 check_parity / live_serial 使用。
2. **板子配对后不自己复位**，它打 `KOK` 后等 `KBOOT`（60 秒超时才兜底复位）。
   原因：复位会让 CH343 重新枚举，回执跟着句柄一起消失——这正是之前
   "写进去了但工具不知道"的根因。

**排查时最有用的两个手法**

- **候选 key 探测**：把可疑 key 逐个写进 `devices.json`（每次读盘，改一行就生效），
  等板子重连，看哪一把 `AUTH ok`。不用重启 daemon，也不碰板子。
- **`STORE ok … seq=` 是写盘的唯一证据**：`seq` 推进 = 真的写进 flash 了；
  `KOK` 只证明"板子说它写了"。
**第三个"工具会丢数据"的例子（同一天，我自己）**

排查用的 `probe_board_key.py`（把候选 key 逐个写进设备表、看哪一把 `AUTH ok`）
在"一个都没对上"时，把设备表**留在了最后一个候选**——于是它自己制造了一次不一致：
板子明明连得上，daemon 却开始报 `auth timeout`。

规矩：**任何会改设备表的诊断/修复工具，退出前必须把原值放回去**，
除非它明确成功并说明了新值。`pair --serial` 已经改成"先写板子、成功了才记账"，
探测脚本现在也加了"没找到就复原"。

#### 收尾：公开密钥这条线闭合（2026-10-26 14:17，选项 B）

按"只改生产路径"做的，开发期便利路径留到 C5：

| 改动 | 文件 | 验证 |
|---|---|---|
| `--auth` 默认 `any` → `hmac` | `launcher.py` | 冒烟：`auth: hmac`、`AUTH ok`、页面 `link_up=True metrics=4` |
| hmac 模式下**不再生成** `daemon.token` | `launcher.py` | 冒烟后 `daemon.token` 不存在 |
| hmac 模式下 daemon 不写 token 文件 | `fake_daemon.py` | banner 显示 `token: (not used)` |
| 删掉硬编码的公开 token | `run-lhm.bat` | 改为 `--auth hmac --devices devices.json` |
| 删掉两个含公开值的 token 文件 | `daemon.token`、`dist/daemon.token` | 已删除 |

**仍然保留、但不是漏洞的**：`term_tk.py` / `pcmon/webui.py` 还能读 `daemon.token`——
那是"不碰板子快速看布局"的开发路径，等 C5（手机/网页）统一改成"配对拿 key"。
现在 hmac 模式下该文件根本不会被生成，所以这条路会**明确地**报"没有 token"，
而不是悄悄用一个旧值连上去。

#### C5 第一步：网页侧的配对与鉴权（2026-10-26）

浏览器不能预共享密钥，也不能在网络上证明“人就在 PC 前”，所以按 PROTOCOL §2.2 走 6 位配对码：
**码只出现在操作员的控制台**，两边各自用 HKDF 派生同一把 key，key 不上线。

| 新增/改动 | 内容 |
|---|---|
| pcmon/pairing.py（新） | 纯逻辑：6 位码、60 s、一次性、只存 sha256(code)、3 次错码锁 10 分钟、同时只允许一个待配对 |
| pcmon/webui.py | POST /pair/begin（码打到控制台）、POST /pair/claim（派生并登记）、/api/* 与 /events 要 Authorization: Bearer <key>、--tls-cert/--tls-key、--sessions |
| 	ools/check_pairing.py（新） | 12 项规则验收 → PAIRING OK |
| 	ools/check_web_pairing.py（新） | 起真 webui 走完整流程 → WEB PAIRING OK (12 checks) |

**鉴权范围（重要）**：**loopback 免鉴权，对外监听必须配对**。桌面仪表盘 pp.js 从不发
Authorization 头，而能访问 loopback 的进程本来就在这台机器上；一起拦掉只会破坏现有仪表盘。
--require-auth 可强制打开，验收脚本走的就是这条严格路径。

已验证：严格模式 12 项全过；loopback 默认 /api/state 无 token 仍 200。

**还没做（C5 剩余）**：前端配对表单 + WebCrypto HKDF + Bearer 头；自签证书生成脚本；手机真机验证。

#### C5 第三步：前端配对界面（2026-10-26）

| 改动 | 内容 |
|---|---|
| `webstatic/index.html` | 配对浮层：默认隐藏，收到 401 才出现；只要求 6 位数字 |
| `webstatic/app.js` | 配对流程（`/pair/begin` → 用户输入码 → WebCrypto HKDF → `/pair/claim`）、key 存 localStorage、`fetch` 带 `Authorization`、`EventSource` 带 `?key=` |
| `pcmon/webui.py` | `/events` **唯一**接受查询参数 key 的路由（EventSource 不能设 Header） |
| `tools/check_web_pairing.py` | 增加到 14 项：`/events?key=<正确>` 200、`<错误>` 401 |

```
python tools/check_web_pairing.py             -> WEB PAIRING OK (14 checks)
python tools/check_web_pairing.py --tls ...   -> WEB PAIRING OK (14 checks)
```

**踩到并修掉的一个真 bug**：给 `/events` 加查询参数回退时，我按位置插入代码，把原来的
`return None` 挤进了 `if parsed.path == "/events"` 里面 —— 于是**没有 Header 的请求一律 401**，
查询参数根本没被用上。症状很迷惑：`/api/state` 带 Header 正常，`/events?key=` 却 401。
用最小复现打印真实状态码才定位到（不是猜）。

**前端还没验的部分**：JS 只在"静态资源已更新、服务端行为已测"这个层面验证过，
真正的浏览器/手机端没跑过。WebCrypto 的 HKDF 与 Python 的 `derive_key` 是否逐字节一致，
必须靠一次真机配对来证明——那是下一步。

#### C5 第四步：浮层为什么"没有框"、配对码到底在哪（2026-10-26）

第一次点"获取配对码"时，浮层只有裸文字没有框。两个前端 bug，都不是协议问题：

| 改动 | 内容 |
|---|---|
| `webstatic/tokens.css` | `.hidden` 在第 66 行，`.pairing-overlay` 的 `display:flex` 在其后、同为单类选择器——同权重后来者赢，`hidden` 永远压不住它。补 `.pairing-overlay.hidden { display:none }` |
| `webstatic/index.html`、`app.js` | 提示写"在控制台上读取配对码"，可仪表盘是 `cmd /c ... > tmp\webui_live.out 2>&1` 起的，**没有控制台窗口**。文案改成指向那行 `PAIR code=` 的实际落点 |

**上一节遗留的必验项已验**（WebCrypto 与 Python 的 HKDF 是否逐字节一致）：

```
node tools/check_web_hkdf.mjs
  ok   /pair/begin 200
  ok   code is NOT in the response
  ok   code reached the operator log  (H:\newwork\tmp\webui_live.out)
  ok   /pair/claim 200 (server derived the same key)
  ok   claimed key opens /api/font
  ok   wrong key is refused
  WEB HKDF OK - WebCrypto and Python derive the same key
```

脚本里的 `deriveKeyHex` 是从 `app.js` **原样搬过来**的，所以 salt 顺序、`info` 标签、
128 位输出任何一处对不上就会红——要验的本来就是参数字符串，不是算法本身。

**这仍然不是真浏览器**：跑的是 Node 的 `crypto.subtle`（同一套 API 与参数面），
真机/真手机那次配对依然没做。差别是它从"完全没验"变成了"参数已验证、只剩环境没验"。

#### C3 补记（2026-10-26 22:52）—— key 轮换：真板子再配一次

前端那轮改动之后，串口链路要确认没被带坏，所以又真配了一次（不是模拟）：

```
python -m pcmon.pair --serial COM10 --client st7735-160x80 --timeout 15

PAIR mint client=st7735-160x80 key=5fe7d82aac517025552dec50f1333d48
serial: KOK slot=A seq=32 len=32
serial: FRAME 160x80 crc=0xf817bec1 bytes=25600
PAIR ok (board stored the key, table updated)
```

- **`seq` 24 → 32** 才是写进 flash 的证据（`KOK` 只证明板子说它写了）。这条判据到现在没失效过。
- 板子随后自己重启（串口上能看完整 boot 日志到 `READY`），用新 key 连回 daemon：
  `AUTH ok` + `hello from st7735-160x80 role=thin ... auth=challenge-response`，
  `devices.json` 的 `last_seen` 推到 22:54:55。旧 key `719f2d21…` 作废。
- 读串口时 `pan.ps1` 会抛一次"端口被关闭"——那是板子复位导致 CH343 重新枚举的副作用，
  **不是失败**。别拿这个当判据，判据仍是 `KOK`/`seq`。

**手边留个教训**：查"板子有没有连回来"时，我用 `Select-String -Last` 抓 `192.168.`，
第一次匹配到的是几小时前的旧行，看起来像"已经连上了"——差点读成成功。
时间戳属于输出、不属于结论：筛日志要先按本次事件的起始时间过滤，再取最后一行。

#### C5 收尾：临时诊断收进 `--debug-http`（2026-10-27）

`ba7a628` 里留了四处 **TEMPORARY** 探针，注释原话是“unattended 之前必须删掉或加闸”：

| 探针 | 原来的位置 |
|---|---|
| 每个 JSON 响应打一行 `REQ` | `webui.py::_json` |
| 每个静态文件打一行 `STATIC` | `webui.py::_static` |
| 401 时打 token 指纹（`len` + `sha8`，不打印 token） | `webui.py::_need_auth` |
| `POST /debug/client-error` 免鉴权接收浏览器错误 | `webui.py::do_POST` |
| 页面侧把 window.onerror / unhandledrejection 上报上去 | `webstatic/index.html` |

**改法**：新增 `--debug-http`（**默认关**），四处服务端输出全部包进 `if DEBUG_HTTP:`；
页面侧的上报改成**打开 `/?debug=1` 才武装**（`URLSearchParams` 守卫），
`app.js` 早就有 `if (window.pcmonReport)` 守卫，所以默认关掉不会报错，也不会发出请求。

**怎么用**（手机白屏、连不上时才需要）：

```
python -m pcmon.webui --host 0.0.0.0 --debug-http --tls-cert tls\server.crt --tls-key tls\server.key
手机打开 https://<本机IP>:7803/?debug=1
```

两边都开才有日志——这是故意的：`/debug/client-error` 是一条**免鉴权写路径**，
不能默认开在局域网上。

**验收证据**（不是“看过一眼”）：

- `python tools\check_pairing.py` → exit 0
- `python tools\check_web_pairing.py` → **WEB PAIRING OK (14 checks)**，
  且整份输出里 `REQ` / `STATIC` / `AUTH 401` 行数**都是 0** ⇒ 默认关是被证明的
- `python -m pcmon.protocol` → **ALL OK**
- `python -m py_compile pcmon\webui.py` → 通过

**一条可推广的教训（补丁脚本）**：一次性补丁脚本用“整段替换 + 命中次数必须为 1”是对的做法，
但**单行模式会被算两次**——它的 LF 与 CRLF 两个“变体”是同一个字符串，`count` 各自 +1，
于是命中数 2、脚本自爆。这次第一趟就栽在最后一条替换上，前五条已经落盘；第二趟先检查
“已落盘标记”，只补剩下的。以后写这类脚本：**变体去重**，并且**先自检再动手**。

#### C6：指标源可插拔（2026-10-27）

原本排在最后，理由是"PC 上有 LHM"。现在成了硬需求：她日常这台是 **Linux**（手机 chroot），
`lhm_bridge.exe` 那条路在这儿根本不存在。

**改之前的形状（问题）**：`FakeSource` 有 `tick()` / `values()`，`LhmSource` 有
`snapshot(now)` / `metrics()`，`fake_daemon` 用 `source_kind` 记着自己手里是哪一个，
四处 `if ... == SOURCE_LHM`。加第三个源 = 把那四个判断再抄一遍（B-2 的老毛病）。

**现在的形状**

| 件 | 内容 |
|---|---|
| `pcmon/sources.py`（新） | 一份契约 + `REGISTRY` + `create()` + `snapshot_of()` / `has_metrics()` + `platform_reason()` |
| `ProcSource`（新） | `/proc/stat`（CPU 占用）、`/proc/meminfo`（内存）、`/proc/net/dev`（下行速率）、`/sys/class/{hwmon,thermal}`（温度） |
| `fake_daemon.py` | `--source fake\|lhm\|proc`；`_make_source()` 变成一次 `sources.create(...)`；banner 按源说话 |
| `tools/check_sources.py`（新） | 对每个源跑**同一套**契约检查 |

契约只有五个名字：`start() stop() tick(now) snapshot(now) metrics()`。
`metrics()` **可选**——fake 的固定表由 daemon 拥有，`has_metrics()` 是"问"它，而不是猜。

**三条判断（值得记）**

1. **"未知"不是 0**。`cpu.total` / `net.down` 是差值量，进程刚起来没有前值 ⇒ 返回 `None`，
   屏上 `--`。给个 `0.0` 会让人以为机器闲着——那是最贵的一种谎。
2. **一个意思只能有一个名字**。旧的两套 API（`values()` 与 `snapshot(now)`）正是
   `source_kind` 这层"记忆"的成因。`FakeSource` 现在也答 `snapshot(now)`，`values()` 只留给 M5 时代的调用者。
3. **跑不了的源要当场吼**。`--source proc` 在 Windows 上不是"启动后没有指标"，而是启动就
   `SystemExit`：`proc reads /proc and /sys, which only exist on Linux (this is nt)`。

**验收（可复现）**

```
# Windows：fake + lhm 走契约；proc 按平台 SKIP
python tools\check_sources.py                       -> SOURCES OK (36 checks, 0 failed)

# Linux：proc 的原生环境
python3 tools/check_sources.py --source proc         -> SOURCES OK (22 checks, 0 failed)

# banner 跟着源说话（构造 daemon 不发 socket）
--- --source fake ---   PC-Monitor fake daemon ... metrics: cpu.total, cpu.temp, mem.used, net.down
--- --source lhm  ---   PC-Monitor daemon (live lhm data) / source: lhm bridge H:\tools\lhm\lhm_bridge.exe (found)

# 没有回归
python -m pcmon.protocol                             -> ALL OK (12/12)
```

Linux 实测（这台手机）：`cpu.total` 从 24.4% 变 4.4%、`mem.used` 91.4%、
`net.down` 0.0 KB/s、`cpu.temp` 从 thermal zone 读到。

**还没做（写在这儿防忘）**：`catalog.source = local | remote`（PROTOCOL §6.1）仍未接线，
现在所有源都算 `remote`；等 L 系列终端能自己测指标时才真有东西可标。

**一条自嘲但真的教训**：这轮我自己的补丁脚本连炸三次——单行模式被算两次、
"`SOURCE_LHM` 一处都不许剩"的断言（banner 那行本来就该留）、`def values()` 出现次数的假设错。
三次都是"断言比事实更严"。补丁脚本的自检要检**必须成立的事**，不是**我以为的事**。

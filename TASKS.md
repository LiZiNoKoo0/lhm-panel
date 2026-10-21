# 任务卡

> 给编码 agent 的任务规格。**动手前先通读 `PROTOCOL.md`**，它是唯一真实来源。
> 语言：Python 3.12（`C:\Users\Administrator\AppData\Local\Programs\Python\Python312\python.exe`）。
> 只用标准库，不要装第三方包（M5 真采集阶段再说）。

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

## 编码规范（四条，别省）

1. **纯 ASCII 的 .py 文件**（中文注释也不要——这台机器 PowerShell 5.1 会踩 GBK/UTF-8 的坑）
   要写中文就写进 `.md`，不要写进 `.py`。
2. 每个文件顶部写清楚它**不做什么**（边界比功能重要）
3. 异常一律吞掉并计数，**不要让单个连接的错误杀掉整个 daemon**
4. 所有魔数（端口、超时、间隔）提到文件顶部常量区

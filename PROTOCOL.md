# PC-Monitor 显示协议 v0.4

> 上位机（PC daemon）与显示终端之间的线协议。
>
> **核心思想：协议是唯一的耦合点。** 终端可以是 ESP32、树莓派、手机网页、
> 桌面窗口、甚至命令行里的 ASCII 画屏 —— 只要它遵守本协议，就能当显示器。
>
> 固件/客户端可以随便写，PC 端不需要知道屏幕是什么。

---

## 0. 设计原则

1. **结构与数据分离**
   结构（有哪些图元、摆在哪）变化少 → 低频下发
   数据（数值）变化快 → 高频下发

2. **终端能力决定职责**
   - **thin（瘦终端）**：屏小算力弱 → PC 排版，它照着画
   - **thick（胖终端）**：算力强 → 它只要数据，界面自己决定
   由终端在 `hello` 里自己声明。

3. **固件只实现原语**
   终端只要会画 `rect` / `path` / `text`，就能画出任何界面。
   语义控件（进度条、折线）是原语的组合，能画原语的固件就能「将就」，有富余算力的固件可以做得更好看。
   → **"该实现多少种控件"这个问题从此不存在。**

4. **终端零语义**
   终端不知道 "CPU" 是什么，也不知道 "这个按钮该关机"。
   单位换算、小数位、颜色、按钮含义，全部由 PC 端决定。

5. **终端只上报事件，不发命令**
   设备说"我按了第 3 个键"，PC 端决定"第 3 个键干什么"。
   任何能连上端口的设备都无法直接操纵你的电脑。

6. **缺数据就是缺数据**
   值为 `null` 时显示 `--`，绝不显示上一次的值。

7. **向前兼容靠"忽略"**
   未知 `t` / 未知 `type` / 未知字段，一律忽略。

---

## 1. 传输层（可插拔）

| 层 | 约定 |
|---|---|
| 消息层 | **NDJSON**：一行一个 JSON，`\n` 结尾，UTF-8 |
| 传输层 | TCP ｜ WebSocket ｜ 串口 ｜（未来任意） |

- ESP32 → 裸 TCP
- 浏览器 → WebSocket（浏览器不能开裸 TCP）
- 串口屏 → 串口

### 角色与发现

- **终端永远是 client**，主动连 daemon（默认 `7802`）
- 不知道 PC IP 时：UDP 广播 `{"t":"discover"}` 到 `255.255.255.255:7802`
  → PC 回 `{"t":"here","ip":"192.168.1.100","port":7802}`
- ⚠️ PC 有多张网卡时，**每张网卡都要广播一遍**
- 成功后缓存 IP 到 flash，下次直连，失败再广播
- 重连指数退避：1s → 2s → 4s → 8s → 最大 10s
- 心跳：终端发 `ping`，PC 回 `pong`；15s 无消息判定断开

---

## 2. 认证

**基于 token，不基于设备 ID**（`client_id` 是终端自报的，可以伪造）。

```json
{"t":"hello","token":"a3f9c2...","client_id":"esp32-a", ...}
```

- daemon 首次启动生成一个随机 token，存在配置里
- 终端在第一帧 hello 里带上
- token 不匹配 → daemon 回 `{"t":"deny","reason":"bad token"}` 并**立刻断开**

宽松模式（开发期可开）：daemon 收到未知 token 时，在管理界面弹出"是否信任此设备"，
批准后把 token 加入白名单。行为类似蓝牙配对。

⚠️ **daemon 只监听局域网地址**，不要绑 `0.0.0.0` 暴露到公网。

---

## 3. 客户端模型（thin / thick）

| | **thin** | **thick** |
|---|---|---|
| 典型设备 | ESP32 + 小 SPI 屏、串口屏、墨水屏 | 树莓派、手机网页、桌面窗口 |
| PC 发给它 | `catalog` + `ui` + `data` + `cmd` | `catalog` + `data` + `cmd` |
| 谁决定布局 | **PC**（绝对坐标算好） | **终端自己** |

thick 终端也可以在 `accept` 里带上 `"ui"`，表示"我也想看看你排的版"。

---

## 4. 消息总表

| 方向 | `t` | 说明 |
|---|---|---|
| H→P | `discover` | （UDP 广播）请求地址 |
| P→H | `here` | （UDP）回应地址 |
| H→P | `hello` | 上线，**声明能力 + 认证** |
| P→H | `deny` | 认证失败，随后断开 |
| P→H | `catalog` | 指标目录 |
| P→H | `scenes` | 页面列表 |
| P→H | `ui` | 界面结构（thin 专用） |
| P→H | `data` | 数值 |
| P→H | `cmd` | 控制指令（PC→H 唯一途径） |
| P→H | `time` | 时间同步 |
| P→H | `debug` | 往终端屏上打一行调试文字 |
| H→P | `event` | 上报按键/触摸/状态 |
| H→P | `stat` | 终端健康度（RSSI、堆、丢包） |
| H→P | `ping` / P→H `pong` | 心跳 |

PC→H 消息带 `"s":<seq>` 递增序号。

---

## 5. 终端上线：hello

```json
{
  "t": "hello",
  "proto": 1,
  "token": "a3f9c2...",
  "client_id": "esp32-a",
  "fw": "1.0.0",
  "role": "thin",
  "accept": ["ui", "data"],

  "display": { "w": 160, "h": 80, "depth": 16, "color": true },
  "charset": "ascii",

  "limits": {
    "max_nodes": 24,
    "max_line_points": 60,
    "max_text_len": 16,
    "max_json_bytes": 2048
  },

  "primitives": ["rect", "path", "text", "num"],
  "widgets": [],
  "fonts": [12, 16],
  "input": ["key"],
  "features": { "backlight": true, "rtc": true, "storage": true }
}
```

| 字段 | 必填 | 含义 |
|---|---|---|
| `proto` | ✅ | 协议主版本 |
| `token` | ✅ | 认证令牌 |
| `client_id` | ✅ | 终端自称的名字（日志/多终端区分，**不可作为身份凭证**） |
| `role` | ✅ | `thin` ｜ `thick` |
| `accept` | ✅ | 想收哪些消息：`ui` / `data` |
| `display` | ✅ | 旋转后的逻辑分辨率、位深 |
| `charset` | ✅ | `ascii` ｜ `utf8`（**决定 PC 端能不能用中文标签**） |
| `limits.max_nodes` | ✅ | 最多能画多少个图元 |
| `limits.max_json_bytes` | ✅ | 单条消息解析器容量上限 |
| `limits.max_line_points` | 建议 | 折线最多存多少点 |
| `primitives` | ✅ | 支持哪些原语 |
| `widgets` | 建议 | 额外支持的语义控件（可为空） |
| `fonts` | ✅ | 支持的字号像素高（PC 只能从这里挑） |
| `input` | 建议 | `key` / `touch` / `knob` / `none` |
| `features` | 建议 | `backlight` / `rtc` / `storage` |

### PC 端必须做的裁剪

- 图元数量 ≤ `max_nodes`
- 只用 `primitives` 里列出的原语
- 字号只能从 `fonts` 挑
- `charset` 是 `ascii` 时，`text` 里**不得出现非 ASCII 字符**
- 单条消息 ≤ `max_json_bytes`

---

## 6. 指标目录：catalog

**thick 终端必须靠它知道有哪些数据可画。**

```json
{"t":"catalog","s":1,"ver":3,"metrics":[
  {"id":"cpu.total","label":"CPU","unit":"%","type":"number","min":0,"max":100,"interval_ms":1000},
  {"id":"cpu.temp", "label":"TEMP","unit":"C","type":"number","min":0,"max":110,"interval_ms":1000},
  {"id":"music.title","label":"NOW","unit":"","type":"string","interval_ms":2000}
]}
```

**插件接入/退出 → catalog 变化 → 必须重发。**

这就是"加一个新词条，所有终端自动出现"的机制。

| 字段 | 含义 |
|---|---|
| `id` | `<plugin>.<metric>`，全小写，**发布后不改名** |
| `label` | 短标签（≤6 字符，给小屏用） |
| `unit` | 单位后缀（纯字符串，PC 端负责换算） |
| `type` | `number` / `string` / `bool` / `range` |
| `min` `max` | 数值范围 |
| `step` | 可选，步进/档位（亮度条那类）。省略或 `0` = 连续值 |
| `interval_ms` | 预期更新周期，终端据此判断超时 |

---

## 7. 界面结构：ui（thin 专用）

```json
{"t":"ui","s":2,"scene":0,"ver":7,"bg":"#000000","nodes":[
  {"type":"text","x":8,"y":6,"text":"CPU","size":12,"color":"#888888"},
  {"type":"rect","x":8,"y":54,"w":224,"h":6,"fill":"#222222"},
  {"type":"rect","x":8,"y":54,"w":112,"h":6,"fill":"#44DD88","bind_w":"cpu.total"},
  {"type":"num", "x":8,"y":22,"bind":"cpu.total","dec":0,"unit":"%","size":28,"color":"#FFFFFF"}
]}
```

- `scene`：页索引（整数）
- `ver`：结构版本。同 scene 同 ver 可跳过重建
- `nodes`：扁平数组，**顺序即绘制顺序**
- 绝对坐标，左上角原点

### 原语（primitives）

| type | 必填 | 可选 | 说明 |
|---|---|---|---|
| `rect` | `x y w h` | `fill color width r` | 填充或边框；`bind_w` 让宽度按某个值 0~1 比例变化 |
| `path` | `x y pts` | `color width closed fill` | **点串连成的折线**。`pts` 是相对 x,y 的坐标数组 `[dx,dy,dx,dy,...]` |
| `text` | `x y text size color` | `align` | 静态文字 |
| `num` | `x y bind` | `dec unit size color align` | 数值格式化显示 |

### 语义控件（可选，原语组合而成）

| type | 说明 | 不支持的终端怎么降级 |
|---|---|---|
| `bar` | 进度条 | ≈ 背景 rect + 填充 rect |
| `line` | 滚动折线（设备端维护环形缓冲） | ≈ 动态 `path` |
| `dot` | 状态点 | ≈ 小 rect |

**v0.3 最小合规集：`rect` + `path` + `text`**（三个就够画一切）。
颜色格式 `"#RRGGBB"`。

---

## 8. 折线的历史由设备端维护（重要）

```json
{"type":"line","x":8,"y":66,"w":224,"h":40,"bind":"cpu.total","min":0,"max":100,"points":60}
```

PC 端**每轮只发最新值**，设备自己压进环形缓冲（长度 = `points`）。

- 带宽从 1KB/s 降到几十字节/s
- 断线后曲线不会瞬间消失
- PC 端不需要维护历史

---

## 9. 数值：data

```json
{"t":"data","s":3,"ts":1729500234,"v":{"cpu.total":23.4,"cpu.temp":61.0,"gpu.temp":null}}
```

- key = 指标 id
- **未出现的 key** = 本轮无更新（终端保留旧值）
- **值为 `null`** = 指标无效（显示 `--`）

PC 端对"超时未上报的插件"应主动置 `null`。

可选颜色覆盖：`{"t":"data","v":{...},"c":{"cpu.total":"#FF4444"}}`

---

## 10. 页面：scenes + cmd

```json
{"t":"scenes","s":2,"list":[{"i":0,"title":"CPU"},{"i":1,"title":"GPU"}]}
```

**页面列表永远由 PC 端维护，终端只知道"第几页"。** 插件加页删页，终端完全无感。

```json
{"t":"cmd","s":4,"target":"*","action":"show","scene":1}
{"t":"cmd","s":5,"target":"esp32-a","action":"brightness","value":60}
{"t":"cmd","s":6,"target":"*","action":"clear"}
{"t":"cmd","s":7,"target":"*","action":"reset"}
{"t":"cmd","s":8,"target":"esp32-a","action":"reboot"}
```

| 字段 | 说明 |
|---|---|
| `target` | `"*"` 广播（默认）｜指定 `client_id` |
| `action` | `show` / `brightness` / `clear` / `reset` / `reboot` |

---

## 11. 终端上报：event（**唯一的上下行控制通道**）

```json
{"t":"event","k":"page","want":"next"}
{"t":"event","k":"page","want":2}
{"t":"event","k":"page","want":[3,4,5]}     ← 可选：滑动列表预加载
{"t":"event","k":"key","id":"focus","v":2}  ← 第 2 个可选中项被按下
{"t":"event","k":"tap","id":"btn1"}
{"t":"event","k":"state","id":"relay1","v":1}
```

### 页面请求：单页是默认，多页是可选

**绝大多数终端只需要单页**。按键盘翻页的屏，一次要一页就够：

```json
{"t":"event","k":"page","want":"next"}
{"t":"event","k":"page","want":3}
```

只有做**滑动列表预加载**的屏（触摸屏那类）才需要批量要：

```json
{"t":"event","k":"page","want":[2,3,4]}
```

| 形式 | 适用 |
|---|---|
| `"want": "next"` / `"want": 3` | 按键翻页，**单页，绝大多数终端走这条** |
| `"want": [2,3,4]` | 滑动列表预加载，**可选** |

PC 端**两种形式都要能解析**（成本很低，就一个类型判断），但**不需要任何能力声明** ——
设备不发批量请求，PC 端就当它只会单页。

PC 端回**多个独立的 `ui` 消息**（每个 scene 一条），不要塞成一条超长包。

**设备只报告"发生了什么事"，做什么由 PC 端决定。**

例：同一个 `boot` 键，PC 端可以配置成翻页、关机、或者什么都不做。
换设备不用改 PC 端策略，改策略不用改固件。

### ⚠️ 危险操作的处理

设备**没有** `shutdown` 这种动作。它只能发 `{"k":"key","id":"power"}`。
PC 端按配置处理，且对关机这类操作**必须先请求确认**（默认关闭）。

### ⚠️ event 不重传，也不保证送达

断线期间按下的事件会丢。**不做重传、不做序号确认。**

- 按键是人按的，丢一次再按一次就行
- 重传反而会造成「锁屏被按了两次」这类事故
- 所以**任何 event 的处理都必须是幂等的**，或被二次确认保护
- 设备重连后**不要补发**历史事件

代价可控，收益是整个协议少了一整层可靠性机制。

> 暗号发出去了，对方听没听见你无从知晓 —— 所以重要暗号必须有人复核。

---

## 11.5 焦点导航（按键 / 编码器终端的唯一交互方式）

设备**不需要知道按钮画在哪**，只需要知道**一共有几个可选中项、我现在选的是第几个**。

PC 端在 node 上标注：

```json
{"type":"rect","x":8,"y":200,"w":60,"h":24,"fill":"#333","focusable":true,"focus":0}
{"type":"rect","x":76,"y":200,"w":60,"h":24,"fill":"#333","focusable":true,"focus":1}
```

| 字段 | 含义 |
|---|---|
| `focusable: true` | 这一项可以被选中（设备通常画个高亮框） |
| `focus: n` | 显式序号。**不写就按 nodes 数组顺序自动编号**，两种可混用 |

设备端只保存「当前选中第几个」，方向键移动、确认键触发，然后上报：

```json
{"t":"event","k":"key","id":"focus","v":2}
```

PC 端查自己的映射表就知道第 2 项是什么。
**设备不知道第 2 项是「关机」还是翻页键，也不需要知道。**

### ⚠️ 焦点越界规则（必做）

收到新 `ui`（ver 变了、可选中项数量变了）后：

```
if 当前焦点索引 >= 可选中项数量:
    焦点 = 0
```

否则会出现「焦点停在一个已经不存在的按钮上，怎么按都没反应」——
而 PC 端日志完全正常，能查很久。

---

## 12. 触摸（可选，第一版不做点按）

第一版触摸屏按下面的方式处理，**不需要命中测试**：

- **交互** → 用焦点导航（§11.5），选中 + 确认，跟物理按键完全一样
- **浏览** → 用**滑动列表**：设备端上下各预加载一页，滑动时自己平移画面，
  滚到边界再向 PC 要新页（`{"k":"page","want":[3,4]}`）

滑动与预加载**完全在固件里做，PC 端只负责响应批量 page 请求**。

以后若确实需要「点哪块区域做什么」，再用可选字段：

```json
{"t":"ui",..., "hits":[{"id":"btn1","x":8,"y":200,"w":60,"h":24}]}
```

设备做命中测试，上报 `{"t":"event","k":"tap","id":"btn1"}`。
**只有 PC 端知道屏幕上画的是什么，所以可点区域必须由 PC 端下发。**

---

## 13. UI 持久化（写 flash 的规则）

终端可以把最后一个 `ui` 存进 flash，开机直接用。但必须：

| 规则 | 原因 |
|---|---|
| **只在结构版本变化时写** | flash 有寿命（NVS ≈ 10 万次擦写），不能每秒写 |
| **带 ver + CRC** | 写一半掉电会让 UI 损坏 |
| **CRC 不对就丢弃** | 回退到固件内置的最小 UI，并向上位机要一份新的 |
| **data 永远不写 flash** | 数值是易失的，写 flash 纯属自杀 |

---

## 13.5 固件端必须自己负责的事（PC 帮不了）

因为 **PC 自己也可能挂了**，下面这些必须在固件里，不能等指令：

| 责任 | 做法 |
|---|---|
| **断线检测** | 15s 无任何数据 → 屏上叠加 `LINK LOST` 角标。**不要黑屏、不要清屏** |
| **数据陈旧提示** | 建议右上角显示「数据已 N 秒未更新」，调试期极有用 |
| **焦点越界重置** | 收到新 UI 后索引超范围就归 0 |
| **UI 损坏回退** | CRC 不对 → 用固件内置最小 UI，并向上位机请求重发 |
| **重连退避** | 指数退避，别 100ms 一次狂连把 daemon 打满 |
| **写 flash 节流** | 只在 ver 变化时写，且最多每 N 秒一次 |

---

## 14. 时间同步

```json
{"t":"time","s":9,"epoch":1729500234,"tz":"+08:00"}
```

daemon 每 10 分钟发一次。终端有 RTC 就校时，没有就只存 epoch 偏移。

---

## 15. 调试与健康

```json
P→H  {"t":"debug","s":10,"text":"waiting for ui"}      ← 打在屏上一角
H→P  {"t":"stat","rssi":-52,"rx":1234,"drop":0,"heap":88000,"uptime":3600}
```

`debug` 在调固件时救命：能直接在屏上看到"收到几条、CRC 过没过"，
不用拔下来插串口。

---

## 16. daemon 端状态模型

```
全局一份
  ├─ 指标数据表     id → {value, ts, valid}
  ├─ catalog        所有已连插件汇总
  └─ 插件注册表     plugin → 连接、心跳、超时计数

每客户端一份
  ├─ 连接、role、能力（hello 内容）
  ├─ 当前 scene
  └─ 已下发 ui 的 ver
```

### ⚠️ 四条实现红线

1. **每客户端独立处理单元 + 写超时 200ms**
   一个终端不读，绝不能卡住其他终端。

2. **发送队列只保留最新一帧，不排队**
   监控数据旧值无价值。终端慢就跳帧，绝不积压。

3. **按能力裁剪布局**
   永远按 `hello` 的 `limits` / `primitives` / `charset` 裁剪。

4. **收到 hello 后无条件重发 ui + data**
   不要假设终端缓存的是对的（daemon 可能重启过）。省那几个字节不值得。

5. **同一个 `client_id` 只保留最新连接**
   设备断电重连时旧 socket 往往还没超时。不踢掉就会出现「两条连接各推一份、屏上乱跳」。

6. **批量请求要逐条回**
   设备要 `[2,3,4]` 就回三个独立的 `ui` 消息，别塞成一条超长包。

---

## 17. 网络环境注意事项（部署必读）

| 现象 | 原因 | 处理 |
|---|---|---|
| 终端连不上，一直超时 | **Windows 防火墙拦入站 TCP** | 给 daemon 加防火墙允许规则 |
| 同一 WiFi 下两台设备互相不通 | **路由器开启客户端隔离（AP isolation）** | 关掉该选项，或都走有线 |
| UDP 广播没反应 | PC 有多个网卡，播到了错误网段 | 每张网卡都广播一次 |
| 能连上但一会儿就断 | WiFi 信号弱 / 省电模式 | `stat` 里的 `rssi` 会露馅 |
| IP 变了之后连不上 | DHCP 重新分配 | 给 PC 设静态 IP，或靠广播发现恢复 |

---

## 18. 完整会话（thin 终端，首次配对）

```
H→P  {"t":"discover"}                                ← UDP 广播
P→H  {"t":"here","ip":"192.168.1.100","port":7802}

H→P  {"t":"hello","proto":1,"token":"a3f9...","client_id":"esp32-a","role":"thin",
      "accept":["ui","data"],"display":{"w":240,"h":320,"depth":16},"charset":"ascii",
      "limits":{"max_nodes":32,"max_json_bytes":4096},
      "primitives":["rect","path","text","num"],"fonts":[12,16],"input":["key"]}

P→H  {"t":"catalog","s":1,"ver":1,"metrics":[{"id":"cpu.total","label":"CPU",...}]}
P→H  {"t":"scenes","s":2,"list":[{"i":0,"title":"CPU"}]}
P→H  {"t":"ui","s":3,"scene":0,"ver":1,"bg":"#000","nodes":[...]}
P→H  {"t":"data","s":4,"v":{"cpu.total":23.4,"cpu.temp":61.0}}

H→P  {"t":"ping"}
P→H  {"t":"pong"}
H→P  {"t":"event","k":"page","want":"next"}
P→H  {"t":"ui","s":5,"scene":1,"ver":1,"nodes":[...]}
P→H  {"t":"cmd","s":6,"target":"esp32-a","action":"show","scene":1}
```

## 19. 完整会话（thick 终端）

```
H→P  {"t":"hello","proto":1,"token":"...","client_id":"phone","role":"thick",
      "accept":["data"],"display":{"w":1080,"h":2400,"depth":32},"charset":"utf8",
      "limits":{"max_json_bytes":65536},"primitives":[],"fonts":[12,16,24,32],"input":["touch"]}

P→H  {"t":"catalog","s":1,"ver":1,"metrics":[...]}   ← 告诉它有什么可画
P→H  {"t":"data","s":2,"v":{"cpu.total":23.4}}

（不发 ui，手机自己排版）
```

---

## 20. 版本兼容

| 变化 | 老终端怎么办 |
|---|---|
| 新增原语/控件类型 | 忽略未知 `type`（`primitives` 声明也能提前避开） |
| 新增消息类型 | 忽略未知 `t` |
| 新增字段 | 忽略未知字段 |
| `proto` 主版本不同 | 屏上显示 `PROTO MISMATCH`，daemon 日志报错 |

---

## 21. 待定 / 后续

- `path` 的贝塞尔曲线支持（v0.3 只做直线连接）
- 位图/图标下发（需另开二进制通道，会破坏 NDJSON 整洁性）
- `ui.patch` 局部更新（第一版整页重建）
- 精细订阅 `subscribe`
- 布局模板共享（一个 thin 排好版，另一个 thin 直接套用）
- OTA 固件下发（建议固件自己做，不放进本协议）
- 触摸点按命中测试（第一版走焦点导航 + 滑动列表，够用）
- 位图/图标一次性下发

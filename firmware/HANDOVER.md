# 交接文档 · st7735 160x80 文本面板

> **最近一次重写：2026-10-25 06:00**（T6c 数据超时置 `--` 上板验穿之后）。
> 读者是下一个人（或下一个我）。**读顺序**：本文件 → `BACKLOG.md`（每条 bug 的「症状 → 根因 → 改法」）→ `SPEC.md`（协议/存储/纪律编号 L-x、S-x、D-x）→ `ARCHITECTURE.md`（分层与理由）。
> 本文件负责「现在是什么样、怎么验、下一步做什么、什么别碰」；BACKLOG 负责「为什么长这样」。

---

## 0. 一句话状态

**T1–T4 + T5.1–T5.6 + T6a/T6b 完成并上板验穿。** 板子自己上电 → 自检 →
UDP 广播找 daemon → 握手（**带 flash 里的 token**）→ 拿指标目录 → 画出 daemon 的页面 →
**数据超时自动变 `--`** → 断线自己退避重连 → 重连后数字恢复。

`cargo test` **184 条全绿**，默认构建**零 warning**，`check_parity.py` 两种基准逐字节相等。

**原卡之外还做了三件，都上板验过**：PROTOCOL §9.1 的数据超时置 `--`、
多字号 16/32/48、以及 PC 端的布局编辑器（配置 → 应用 → 上屏）。

**PROTOCOL v0.5 的 §9.1 已在硬件上成立**：daemon 静默 3 秒后，屏上的数字变成 `--`，
而布局保留、不清屏。断开与重连循环已验证（`g=1 → g=0 → g=1`，`iv=1000` 全程保持）。

> ⚠️ **T6c 没做，这是当前最大的缺口。** T6c 的原定义是「布局二进制存 flash」，
> 现在 layout 只活在 RAM：断电重开 → 空白 → 必须等 daemon 重发 `ui`。**没有 PC 就是黑屏。**
> 这一条是 PROTOCOL v0.5 §22（离线运行模型）「布局持久 + 值易失」里那个「持久」的前提。

**下一张卡：T6c 布局存 flash**（先做它，见 §9）。固件侧的 `peripherals` 声明、
`link.alive` 内置指标、以及把 `LINK LOST` 从「固件强制」改成「PC 可配 UI」**也都还没做** ——
现在屏上同时存在旧的强制角标（v0.4 行为）和新的 `--` 超时（v0.5 行为），两者都正确，
但 v0.5 要求前者改成后者的一种可选排版。

---

## 1. 板子现在的物理状态

| 项 | 值 |
|---|---|
| 板子 | 经典 ESP32（**A 板**，rev v3.1，MAC `b0:3f:d3:42:c7:58`） |
| 串口 | **COM10**，115200 8N1，CH343（USB 转串口） |
| 固件 | 默认构建（`cargo build --release`，无 bench feature），2026-10-23 已烧入 |
| 屏幕 | ST7735S 0.96" 80x160 横屏用（逻辑 160x80）。**玻璃是 BGR：`MADCTL = 0x68`**（见 §7.4） |
| 接线 | SCLK=18 MOSI=23 CS=5 DC=4 RST=15 BL=2 |
| 当前画面 | **开机自检页**（`SELF TEST` + PANEL/STORE/WIFI/LINK 四行 ok）→ 收到内容后切正常页 |
| 板子 IP | `192.168.1.53`（rssi 约 -43~-52），家里路由器 `LiZiNoKoo` |
| 对端 PC | `192.168.1.128`（有线以太网，同一 /24） |
| daemon | 7802 TCP + **7802 UDP**（发现应答）；webui 7803；echo bench 7804 |
| daemon 启动参数 | **可以直接用严格模式**（T5.6 之后）：`--token 0123456789abcdef`。`--lenient` 只在板子还没有 token 时才需要 |
| 存储记录 | `creds` 记录 `ver = 2`，140 字节（含 32 字节 token 字段）；`ver = 1`（108 字节）仍可读 |
| 主任务栈 | 21,772 B（每次开机自己报；低于 16KB 会在 `sys::init_heap` 直接死） |

> ⚠️ 「flash 里是假凭据、只能在热点模式跑」这条**早已过期**。凭据是真的，能直连家里网。
> 开卡前先抓 20 秒串口确认，别照信仰推论：`powershell -NoProfile -File firmware\serial_cap.ps1 -Seconds 20 -Port COM10`

---

## 2. 本轮（10-22 夜 → 10-23 晨）到底变了什么

按时间顺序，**每一条都能在 BACKLOG 里找到完整取证**：

| # | 变化 | 影响面 |
|---|---|---|
| 1 | **T5.3 连接状态机**：`net_link::pump/serve` 全部改成调用 `panel-core/src/session.rs` 的 `Session`，删掉 pump 里另写的一份 | 修掉 **B-2**：退避阶梯被「TCP 连上」清零 → 被 deny 时每秒重试、永不上升 |
| 2 | **T5.4 `LINK LOST` 角标**：`panel-core/src/badge.rs`，72×16，贴右下角 (88,64)，白底黑字反相 | 链路断 → 出现；恢复 → 消失。串口有 `BADGE on|off` |
| 3 | **T5.4.1 重连阶梯**：`reconnect_probe.py` 一次开串口全程不关，验到 `1/2/4/8 s` 一字不差 | 证伪了「daemon 死后不重连」——那是测量错误 |
| 4 | **T5.5 UDP 发现**：新增 `panel-core/src/discover.rs`；`board::DAEMON_PEER` 变成 **`None`**；`FALLBACK_PEER` 只在整轮无人应答时兜底并打 `DISC none` | 换租约/换网段不用重新编译 |
| 5 | **开机自检页**：`panel-core/src/boot.rs` + `st7735-text/src/bootstate.rs` | 黑屏不再等于「板子死了」 |
| 6 | 修 **B-1**：静态内存吃主栈（栈被链接器摆在 `.bss` 正上方） | `sys::init_heap` 现在断言栈 ≥ `MIN_STACK_BYTES`(16 KB) |
| 7 | **颜色顺序修正**：`MADCTL` `0x60` → **`0x68`**（这块玻璃是 BGR）；新增 `bench-colours` 三色块构建；`preview_st7735.py` 加 `GLASS_BGR` | 樱花粉不再显示成淡蓝紫；帧 CRC 不变 |
| 8 | **T5.6 token 入 flash**：`creds` 记录升 `ver = 2`（140 字节，多 32 字节 token）；`hello` 从存储取 token（`n=304`）；配网页多一个 token 框（留空 = 保持）；`bench-seed-token` | 严格 daemon 直接能用；错 token → `LINK deny` + `BOOT LINK fail`（CRC `0x1EFEB12B`） |
| 9 | **串口工具**：`pan.ps1`（.NET），因为 pyserial 在这台机上读这个口会 ClearCommError | 抓日志/发内容都不再依赖 python |
| 10 | 掉线结案：换短粗线 + 主板后置口，连续 3 次零掉线 | 「READY 之后静默」= 供电/线材，不是固件 |

### 2.1 新增/重写的文件

```
panel-core/src/
  discover.rs     【新】DISCOVER 字节 + parse_here() + should_discover() 策略（8 条单测）
  session.rs      已有的唯一一份连接状态机（现在真的被 pump 用了）
  badge.rs        【新】LINK LOST 角标 72x16 + Canvas::fill_rect + Scene::has_layout
  boot.rs         【新】自检页：20 列 x 5 行，TITLE + 4 STEPS
st7735-text/src/
  bootstate.rs    【新】每行一个 AtomicU8，值变才打行/才重绘
  net_link.rs     pump 签名改成 initial: Option<(ip,port)>，连败两次先广播一轮
  diag.rs         + DISC/BOOT/BADGE 行，+ stack_total 的钉死测试
```

### 2.2 新增的验收工具

| 工具 | 干什么 | 期望输出 |
|---|---|---|
| `firmware/disc_probe.py` | 复位板子，看 `DISC tx` → `DISC here` → `LINK catalog`；顺带查 UDP 7802 防火墙规则 | `PASS the board found the daemon by itself` |
| `firmware/check_parity.py --boot` | 屏上自检页 vs `preview_st7735.py::boot_page()` | `BOOT PARITY OK (crc=0x188FDDDE, 25600 bytes, 4 frames)` |
| `firmware/badge_probe.py` | 自动停/起 daemon，验角标出现与消失 | `BADGE OK` |
| `firmware/reconnect_probe.py` | 杀 daemon 录退避阶梯，再起 daemon 看自愈 | `PASS`，阶梯 1/2/4/8 |
| `firmware/probe_catalog.py` | 用与固件逐字相同的 hello 直连 daemon | 打印原始 catalog 行（验证「n=0」是真 0 不是 bug） |
| `firmware/pan.ps1` | **.NET SerialPort 抓串口**（pyserial 在这台机上读这个口会 ClearCommError）。`-Seconds N` / `-Reset` / `-Lines "a|b"`。不加 `-Reset` 时**打开口不复位板子** |

---

## 3. 现在这套东西的架构（一句话版）

```
PC 侧：pcmon（LibreHardwareMonitor 指标）→ daemon（7802）
         daemon 只发 catalog/ui/data；不发像素
板子侧：main → 自检页 → net(WiFi 或 AP) → net_link(发现/连接/退避) → 渲染任务（永不 await）
        串口行是唯一「屏上发生了什么」的证据通道
```

**分层纪律（别破坏）**：

1. **渲染任务永不 await**（BUGLOG #1）。socket 归泵任务独占，`TcpLink::poll_line` 只 `channel::try_receive`。
2. **行切分只有一份**：`link::LineBuf<N>`。行为必须逐字节不变，用 `check_parity.py` 复核。
3. **同一规则只许有一份实现**（本轮最大教训，见 §6.1）。`session.rs` 有 12 条测试的状态机，`net_link::pump` 曾自己另写一份 → 两份真相，测试与上板行为第一次分歧，代价是一次重烧 + 一轮排查。
4. **`hello` 不能照抄 SPEC §5.1 原文**（缺 token/accept、字段名是 `v`）。已改 SPEC 并钉在 `wire::HELLO`。
5. **L-5**：只有 `catalog` 能清退避阶梯。`LINK up` 只是 socket 建起来，被 deny 不算好。
6. **屏上画的东西必须能听见**：`BADGE on|off`、`BOOT <LABEL> <word>`、`FRAME crc=...`。脚本看不见玻璃。

---

## 4. 验收清单（每次动完代码必须全绿）

```powershell
$py = "C:\Users\Administrator\AppData\Local\Programs\Python\Python312\python.exe"

# 1) 单元 + 钉死测试（126 条）
Set-Location H:\newwork\firmware\panel-core; cargo test

# 2) 两种构建都必须零 warning
Set-Location H:\newwork\firmware; cargo build --release
cargo build --release --features bench-echo

# 3) 内容页 parity
& $py H:\newwork\firmware\check_parity.py --port COM10        # PARITY OK 0xEE473F67

# 4) 自检页 parity
& $py H:\newwork\firmware\check_parity.py --port COM10 --boot # BOOT PARITY OK 0x188FDDDE

# 5) 角标（要动 daemon）
& $py H:\newwork\firmware\badge_probe.py --port COM10         # BADGE OK

# 6) 自动发现（要 daemon 活着、UDP 规则在）
& $py H:\newwork\firmware\disc_probe.py                       # PASS

# 7) 烧写（内部就是 cargo build + esptool，串口默认 COM10）
powershell -ExecutionPolicy Bypass -File H:\newwork\firmware\flash.ps1
```

**四个钉死的 CRC**（谁动字体/布局/颜色都会立刻红）：

| 画面 | CRC | 钉在哪 |
|---|---|---|
| 内容页 `CPU 33.5% / TEMP 34C` | `0xEE473F67` | `panel-core/tests/parity.rs` |
| 同上 + `LINK LOST` 角标 | `0x6A7B210F` | 同上 |
| 自检页 四行全 ok | `0x188FDDDE` | `panel-core/tests/boot_parity.rs` |
| 自检页 四段中间态 | `0x8e9722aa` / `0x5adc8dbd` / `0xaffeb207` | 同上 |

---

## 5. 设计约定：那套「粉色 UI」（半开玩笑，但已进代码）

李子原话是「总算是做了点面子工程了」。这套东西的**视觉基调确实已经固定下来**了，别再随手改：

| 角色 | 颜色 | 用在哪 |
|---|---|---|
| 背景 | `0x0000` 纯黑 | `Canvas::BG` —— 黑底在 ST7735 上对比度最好，也省电 |
| 正文 | `0xFFFF` 纯白 | `Canvas::FG`；四项 `ok`、内容页数据 |
| **强调 / 标题** | **`0xFDB8` 樱花粉** | `boot::TITLE_COLOUR`（`SELF TEST` 标题）；独立 RGB 灯那条线也是玫瑰粉，同一个审美 |
| 未开始 | `0x8410` 中灰 | `boot::PENDING_COLOUR` 的 `--` |
| 失败 | `0xF800` 纯红 | `boot::FAIL_COLOUR` 的 `fail` |

规矩：**粉只用于「标题/需要被眼睛第一眼抓到的那一个东西」，不用于数据正文**。上一轮的配色教训是
「淡粉 (255,192,203) 看着像白」—— 想「红润」要**压 G/B**，不是调亮度。屏是 565，别指望渐变。
想让自检页更花哨（进度`[>]`、耗时毫秒）先量高度：现在 5 行 16px 已经**占满 80px**，加行必须缩字号。

---

## 6. 这一轮学到的（能带到下一个工程的）

### 6.1 同一规则只许有一份实现
`session.rs` 早就有带 12 条测试的状态机，规矩也写对了（只有 catalog 清阶梯），但 `net_link::pump` 没接它、自己另写了一份。结果：测试全绿、上板行为错。**写完「可测的规则」立刻把板级代码接上去**，别让它变成第二份真相。

### 6.2 日志没时间戳，数量就不是证据
哑服务的日志没打时间戳时，我看到 73 行 `connection` 就断言「板子在狂连」，还据此推了半天。加时间戳后真相：每次会话老老实实待满 15 秒，73 次是 8 分钟累积的。**现在那个脚本第一列就是相对秒数。**

### 6.3 「没有日志」≠「没有行为」
`reconnect_probe.py` 的结论是：**没有「永久放弃重连」这个 bug**。那半小时的沉默是 CH343 的 EN 脉冲约 1/3 被忽略、板子根本没重启。探针必须先证明「我能听见你」，再判断「你说了什么」（`kick()`：脉冲后 8 s 没新行就再打，最多 4 次）。

### 6.4 「不出现」比「不消失」安全
角标最容易出的不是不出现（眼睛能看见），而是**不消失**——夹在健康链路里的 `LINK LOST` 会让用户以为数据过期。所以边沿触发 + 串口自述，两条都要。

### 6.5 黑屏和死板子长得一模一样
这一晚为这个歧义付过两次代价。**开机第一屏必须是「板子自己会说话」的样子**（自检页），且它要能被 CRC 校验。

### 6.6 发现与退避共用同一个「失败」定义
`should_discover()` 直接读 `Session::failures()`：没地址 → 必问；连续失败 ≥ 2 → 再问（DHCP 换租约）；只失败 1 次不问（电台还没醒）。**谁也不清谁的计数器**，L-5 原样保留。

---

## 7. 已知未解决 / 别乱动的

### 7.1 `READY` 之后整块板静默 —— **已结案：供电/线材（2026-10-23 16:40）**
- 历史现象：`READY` 后一个字节都没有，玻璃停住，只能复位。
- **真因**：`pan.ps1` 抓到时看得很清楚——停在 `STORE ok creds=yes slot=A seq=4` 之后，
  紧接着 `The port is closed.`、`Get-PnpDevice` 里 **COM10 整块消失**。不是程序停住，是
  **CH343 从设备树掉出去**，而那一刻正是 WiFi 射频启动、电流尖峰。
- **李子换了短粗线 + 主板后置 USB 口之后，连续 3 次零掉线**，每次都跑到 `BOOT LINK ok`（`0x188FDDDE`）。
- ⇒ 以后遇到「板子不说话」，先 `Get-PnpDevice -Class Ports -PresentOnly` 看口在不在，再抓串口；
  **不要一上来就怀疑代码**。

### 7.2 只有在凭据真的丢了才动配网
手机连 `st7735-c758` → `http://192.168.4.1` → 手输 SSID + 密码 → Save and reboot。
**任何写操作的目标扇区永远不能是当前生效的那一份**（T4 就是「先擦后写」把唯一副本擦没了）。

### 7.3 这台路由器每次开机必现的假故障
第一次关联会在 +4.19 s 报 `WIFI fail reason=Disconnected(AuthenticationExpired)`，重试才成功 → `WIFI up` 在 +6.1 s。**看起来像密码错，不是。** 别据此改凭据（固件已做 3 次重试，87% 的启动靠重试才连上）。

### 7.4 颜色顺序：这块玻璃是 BGR（2026-10-23）
- `board.rs` 的 `MADCTL` 是 **`0x68`**（bit3=1）。原来写 `0x60` 时红蓝是**反的**：樱花粉 `0xFDB8`
  出来是淡蓝紫。判据只能用**三色块**（`bench-colours` 构建：左 0xF800 / 中 0x07E0 / 右 0x001F），
  白字在两神模式下完全一样，盯屏幕看不出来。
- 帧字节与四个 CRC 基准**不受影响**（MADCTL 只影响玻璃的解释方式），已实测复验：
  内容页 `0xEE473F67`、自检页全 ok `0x188FDDDE`。
- PC 预览必须 `preview_st7735.py: GLASS_BGR = True`（出图时交换 R/B），否则预览图跟玻璃不一致。
- ⚠️ **别再凭「看着挺好看」把 `0x68` 改回 `0x60`**，想改先重跑 `bench-colours`。

---

## 8. 这台 PC 的坑（会伪装成固件 bug）

1. **防火墙是按「程序 + 协议」生效的**。`panel echo 7802` 只有 TCP → 板子的 UDP 广播**到了网卡却收不到**。已加 `panel discover 7802`（Inbound Allow, UDP, 7802, Profile Any）。**取证套路**：用 node（有 proto=Any 规则）绑同一个 UDP 口，能收到就说明包到了、锅在防火墙。
2. 历史上那两条 `Python.exe` 入站 **Block** 规则已在李子同意下禁用（规则保留，`Enabled=False`）。未禁用时症状：daemon 在 Listen，板子却三次 `LINK fail reason=Timeout`——SYN 到了被静默丢。取证看 `%windir%\System32\LogFiles\Firewall\pfirewall.log` 的 `DROP` 行。
3. **`ping` 不通不代表板子不在网**：固件没启 ICMP 回显，以串口 `WIFI up ip=` 为准。
4. **沙盒里 `sys.executable` 是 python 3.11 且 `PYTHONPATH` 被丢** → 子进程 `from pcmon import lhm` 失败，看着像「daemon 启动不了」。**一律用绝对路径** `...\Python312\python.exe`。
5. **打开 COM 口会动 DTR/RTS**（这块板 DTR=IO0、RTS=EN）→ 可能把板子重置进下载模式并吞掉第一行。对策：内容行每 2 秒重发直到收到 `FRAME`；**不要随便 `release_board()`**（实测之后板子完全不出声）。
6. **沙盒 exec 会话会被 SIGKILL（约 40~60 s）**：长任务要么丢后台，要么 `host=gateway` 直接在本机跑（李子 04:13 明说「想在我电脑上干嘛都可以」）。
7. **espflash 烧完不自启是它的 bug**（`reset_after_flash` 只拨 RTS 不碰 DTR）。本项目 `flash.ps1` 走
   **espflash**（`--non-interactive`），实测烧完能跑；若遇到不自启，用
   `python -m esptool --after hard_reset chip_id` 补一发硬复位。
8. **`cargo` 必须在项目目录里跑**：`.cargo/config.toml` 按 **cwd** 往上找，不看 `--manifest-path`。
9. **PowerShell 5.1 中文编码**：`Get-Content -Raw`/`Set-Content` 默认 GBK，会把 UTF-8 文件**永久写坏**。用 `[System.IO.File]::ReadAllText($p, [Text.Encoding]::UTF8)` / `WriteAllText(..., UTF8Encoding($false))`。`.ps1` 脚本本身要**纯 ASCII**。
10. ❗ **pyserial 3.5 读这个口会 ClearCommError**（`设备不识别此命令`）：它 open 时把 DTR/RTS 都拉高，
    等于把板子按在复位里，随后 read 就报 ERROR_INVALID_FUNCTION；再开又 `FileNotFoundError`。
    ⇒ **用 `firmware/pan.ps1`**。所以 `check_parity.py` / `disc_probe.py` 随时可能假失败，
    失败时先用 `pan.ps1` 手工看一眼串口，别直接认定固件坏了。
11. ⚠️ **CH343 会整块掉出设备树**（已结案：线/口）：跑到一半 `The port is closed.`、COM10 消失，
    几秒后又回来。换短粗线 + 主板后置 USB 口后零掉线（见 §7.1）。
12. ❗❗ **feature 构建会覆盖同一个 bin 路径**：`cargo build --release --features bench-echo` 和默认构建
    都写 `target/xtensa-esp32-none-elf/release/st7735-text`。刷之前**永远重建默认构建**，
    并确认 `init done: MADCTL=0x68` 、`LINK tcp peer=...:7802`（7804 = 刷进了 bench-echo）。我为此白查过一次。

---

## 9. 下一步（按顺序，别跳）

> 本节的旧版本（2026-10-23）写着「接下来 1/2/3」，那三步**已经全部做完**。
> 留在这里的坑说明一件事：进度写在两处（这里和 BACKLOG 的表）就一定会分叉，
> 2026-10-25 修正时两处都过期了一周。改动时请一起看。

### 已经做完的（不要再当待办）

| 项 | 完成日期 | 证据 |
|---|---|---|
| T6a `ui` 节点解析（`rect`/`text`/`num`，表 ≤32，`ver` 用 `!=`） | 2026-10-23 | 166 条测试；上板 `PAGE ui scene=0 ver=1 nodes=6` |
| T6b `data` 值更新 + 真页面上屏 | 2026-10-23 | `FRAME crc=0x85e823a9` 与 PC 预览逐字节相等 |
| §9.1 数据超时置 `--`（`interval_ms × 3`） | 2026-10-25 | daemon 静默后屏上数字变 `--`，布局保留不清屏 |
| 多字号 16/32/48（单字模整数倍放大） | 2026-10-25 | `hello ... fonts=16,32,48`；屏上三行明显不同大小 |
| PC 端布局编辑器（配置 → 应用 → 上屏） | 2026-10-25 | `apply from webui: ver=N -> broadcast` + 板子画面改变 |

### 下一张卡：**T6c 布局二进制存 flash**（SPEC §6，S-6）

**这是当前最大的缺口，而且之前的进度记录把它漏掉了。**

现状：`Page` 里的 layout 只活在 RAM。断电重开 → 空白 → 必须等 daemon 重发 `ui`。
**没有 PC 就是一块黑屏。**

这一条是 PROTOCOL v0.5 §22（离线运行模型）能不能成立的前提：
「玻璃上看到的 = 布局（持久） + 值（易失）」里的「持久」目前**不存在**。

要做：

- 二进制节点表落盘（**不存 JSON 原文**：开机不该跑解析器，且体积差 4 倍）
- 带 `ver` + CRC；写一半掉电要能识别并丢弃
- **只在 `ver` 变化时写**（NVS ≈ 10 万次擦写寿命）
- `data` 永远不写 flash
- 开机时序：读 flash → 立刻开画 → 后台再联网（不等网络）
- 验收：**拔电重开 → 布局还在、值全是 `--`**

> 排期理由：TLV 存储先于 T6 的其余部分，否则 L4 要再迁移一次格式。

### T6d · focus 表（按键导航的前提）

项数 + 每项 `x/y/w/h`，**序号 1 基**，≤32 项。
原因：按键导航必须有**看得见**的高亮框；没有它，按了键屏上没变化、串口却一切正常。

### T7 · 折线

`line` 节点 + 设备端环形缓冲 + 脏矩形合并推屏（D-39..D-43）。
顺带能解决 T6b 遗留的「整帧推屏」问题。

### L 系列 · 本地层（脱离 PC 也能用）

协议侧 v0.5 §22 已经把模型定完，**固件侧零实现**：

- `hello.peripherals` 外设声明（只声明有无/量纲/周期，**接线细节永不外流**）
- `catalog.source = local | remote`，`local.*` 由板子自己供数
- `link.alive` 内置指标（终端自供，不上报，可被布局绑定）
- **`LINK LOST` 从「固件强制」改成「PC 可配的 UI 节点」**

**L2 先于 L1**（只读页不依赖按键，`bench-local` 注入动词，先证明「板子自己会说话」再焊按键）。

### PC 侧的零碎（都不大，但别忘了）

- 编辑器 `PANEL_FONTS` 仍硬编码 `[16,32,48]`，没读固件 hello 的声明 —— 改固件就会不同步
- 编辑器是表单式，不是拖拽
- 三项验收没跑：M4 回放一致性、M5 桥接掉线 3 秒变 `--`、M3 `term_tk.py`

---

## 10. bench 编译开关（不需要手机就能验）

`Cargo.toml` 里，默认构建不含，零 warning。

| 开关 | 干什么 | 期望串口 |
|---|---|---|
| `seed-bad-psk` | 保留记录、只把密码换错 | `WIFI fail ...AuthenticationExpired` ×3 → `WIFI ap`（约 15 s） |
| `bench-save-on-boot` | 启动做一次真实 `store.save` | `WIFI save ok` + `STORE ok slot=<另一个> seq=<+1>` |
| `bench-powercut` | 擦目标扇区、写半帧、复位 | 下次启动仍是旧 slot/seq，可用 |
| `bench-legacy` | 往 A 扇区写 T3 老格式（假升级） | `STORE migrate ok` → `slot=B seq=1` |
| `bench-echo` | 去连写死的对端 `192.168.1.128:7804` 先发 `PING`；**不发 hello** | `LINK tcp peer=...` → `LINK tx n=5` → `LINK rx n=4 text=PING`（先起 `echo_server.js 7804`） |
| `bench-colours` | 画三块纯色条（左 0xF800 / 中 0x07E0 / 右 0x001F）然后**停住**，不进 WiFi | `COLOURS left=0xF800 mid=0x07E0 right=0x001F madctl=0x68 inv=0`；玻璃上应是**红|绿|蓝** |
| `bench-seed-token` | 没有 token 时把 `board::BENCH_TOKEN`（`0123456789abcdef`）写进 flash | `TOKEN ok len=16` + `STORE ok slot=<另一个> seq=+1`；之后 daemon 用 `--token 0123456789abcdef` 起就能严格握手 |

---

## 11. 一句话给下一个人

这套东西的信条是「**别信『它说 OK』**」：每个结论都要有**串口行、CRC、或一条钉死的测试**支撑。
这一晚所有 bug 都是这条信条抓出来的 —— 包括我自己写的那三个。
**看不见内部状态的东西（屏、角标、链接）必须让板子自己说出来**，否则你面对的就是「黑屏和死板子无法区分」。

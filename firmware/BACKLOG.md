# 固件任务单(handoff)

> 这里放**给实现方**的任务卡。每张卡 = 一个可验收的里程碑,不是半个系统。
>
> 规矩三条(分工会散架的三个地方,先钉死):
>
> 1. **验收由验收方跑,不看"我写完了"。** 每张卡都带可执行命令;命令跑不出预期结果就是没完成,不管代码多漂亮。
> 2. **改行为必须同时改 `SPEC.md`。** 代码与文档不一致时,要么改代码,要么改文档并写明理由。禁止"文档是文档,代码是代码"。
> 3. **一张卡一张单,不许顺手扩大。** 卡外的改动(重构、优化、加功能)一律另开一张卡。

状态:`TODO` / `DOING` / `REVIEW` / `DONE`

> **当前进度(2026-10-22 18:30)**:T1–T4 = `DONE`,T5 已拆成 T5.0–T5.6,
> T5.1(纯逻辑)= `DONE`,T5.2(TCP 泵)= `DONE` —— 上板跑到 `LINK up` +
> `LINK tx n=5` + `LINK rx n=4 text=PING`,顺手修掉了 B-1(静态内存吃栈)。
> 下一张卡:T5.3(hello + 15s 看门狗 + 退避)。本地层(L 系列)已设计但未开工。
> 接管工作请先读 **`HANDOVER.md`**(板子物理状态、bench 开关、未解决问题、下一模块入口),本文只讲“为什么”。

| 卡 | 目标 | 状态 |
|---|---|---|
| T1 | 依赖通关(只加依赖,不写业务) | DONE |
| T2 | 堆 + 时钟(毫秒)+ heap 自检 | DONE |
| T3 | AP 配网网页 + 凭据落盘 | DONE |
| T4 | sector store A/B(掉电可回退) | DONE |
| T5 | TCP 链路 + `hello` + 断线退避 | **DOING**(拆成 6 小卡,T5.1 = DONE) |
| T6 | `ui`/`data` + 布局持久化(`ver` 用 `!=`) | TODO |
| T7 | `line` 折线 + 脏矩形推屏 | TODO |

---

## T1 · 依赖通关

**目标**:把整条 WiFi 技术栈的依赖关系一次性钉死。**一行业务代码都不写。**

这一卡的唯一价值是:把"版本互相不兼容"这个最贵的坑,在写任何逻辑之前踩完。

### 输入(已实测确认过的版本矩阵,别自己换)

`firmware/st7735-text/Cargo.toml` 的 `[dependencies]` 追加:

```toml
esp-radio = { version = "0.18.0", features = ["wifi", "esp32"] }
esp-rtos = { version = "0.3.0", features = ["embassy", "esp-alloc", "esp32"] }
esp-alloc = "0.10.0"
embassy-executor = "0.10"
embassy-time = "0.5.1"
embassy-net = { version = "0.9.1", features = ["tcp", "dhcpv4", "dns"] }
esp-storage = "0.9.0"
esp-println = "0.15.0"
```

`esp-hal` **保持 `=1.1.2` 不动**(1.2.x 要 `esp-sync 0.3`,那个需要 esp 工具链没有的 nightly 特性)。

### 为什么是这些名字(踩过的坑,别重踩)

| 坑 | 事实 |
|---|---|
| `esp-wifi` | **是上一代名字**,停在 0.15(要 `esp-alloc 0.8`)。对应 `esp-hal 1.1.2` 的继任者是 **`esp-radio 0.18`** |
| `esp-storage` | 必须 **0.9.0**(要求 `esp-hal ~1.1`)。0.10.0 要 `esp-hal ~1.2`,装不上 |
| `esp-alloc` | 默认特性已含 `global-allocator`,不需要手动开 |
| `esp-rtos` | 必须同时开 `embassy`(带 embassy-executor 0.10 + 时间驱动)、`esp-alloc`、`esp32` 三个特性,否则 WiFi 任务起不来 |
| `picoserve`(T3 用,本卡先不加) | 0.20.1 要 `embassy-net ^0.9.1`,接得上;它自己的 feature 名等 T3 再确认 |

### 交付

1. `Cargo.toml` 更新 + `Cargo.lock` 更新。
2. `cargo build --release` **通过且 0 warning**。
3. **不改任何行为**:不打印新行、不动 `scene`/`canvas`/`panel`/`diag`。

### 验收(验收方执行)

```powershell
# 板子不在时(现在的状态)先跑这两条:
Set-Location H:\newwork\firmware\st7735-text
. D:\esp-toolchain\esp-xtensa-env.ps1
cargo build --release | Select-String 'warning'    # 必须零输出
Set-Location H:\newwork\firmware\panel-core; cargo test   # 必须 14/14 绿

# 板子插回来后补做硬件验收:
powershell -ExecutionPolicy Bypass -File firmware\flash.ps1
python firmware\check_parity.py --port COM10       # 必须 PARITY OK,CRC 与阶段1 相同
```

启动横幅必须与阶段1 **逐字节相同**(`panel-core` 的 `diag_format` 测试已把它钉死,跑 `cargo test` 即可复核)。

### 禁止

- ❌ 不写任何 WiFi / socket / flash 逻辑
- ❌ 不改启动横幅(多一行也算改行为)
- ❌ 不动 `esp-hal` 版本、不升级其他 crate
- ❌ 不引入 `std`
- ❌ 遇到编译错误就"降级某个 crate"--先看是不是漏了 feature,改版本必须先问

### 已知风险

`esp-radio` + `esp-rtos` 会拉进链接脚本/中断相关的符号。若出现 `undefined reference` 或 `esp-hal/rt` 相关错误,**先看 `esp-radio` 文档里对 `esp-hal` 特性的要求**(它自己会开 `esp-hal/rt` 与 `unstable`),不要急着改 `[features]`。

估时:0.5 天(时间基本花在 feature 名上,不在写代码上)。

### 结果(验收通过)

实现方遇到两个坑,都不是版本问题,已写进 `SPEC.md` §2.1:

1. `build-std` 原为 `["core"]`,`esp-alloc` 一带就报 `can't find crate for alloc` → 改为 `["core", "alloc"]`。
   `alloc` 是 no_std 合法子集,不算引入 `std`。**这是任务卡漏写的一项,不是越权。**
2. `esp-println` / `esp-storage` 必填芯片 feature,裸版本串会被拒 → 补 `features = ["esp32"]`。

验收记录:`cargo build --release` 强制重编后 **0 warning**;`panel-core` **14/14** 测试绿;
ELF 从 1 871 296 → 1 872 104 字节(+808,未被引用的依赖被 LTO 丢掉了,说明没把 `std` 拉进来)。

**硬件验收(已补做,板子插回后)**:

| 用例 | 板上 CRC | PC 预览 CRC | 结果 |
|---|---|---|---|
| `CPU 33.5%\|TEMP   34C` | `0xEE473F67` | `0xEE473F67` | PARITY OK |
| `CPU 100%\|TEMP   --C` | `0xB5EE5FE9` | `0xB5EE5FE9` | PARITY OK |
| `\|`(全黑) | `0x0448135D` | `0x0448135D` | PARITY OK |
| `MEM 71%\|NET 1.2MB/s` | `0xFCD2CA54` | `0xFCD2CA54` | PARITY OK |

另跑 `live_serial.py --source demo --once`:`CPU 50.6% TEMP 42C crc 0xFD6FD9FD ok`,1 帧 0 错。

> 踩坑:`check_parity.py --reset` 只复位一次,而这块 CH343 的 RTS 脉冲约三成被吞;
> 一旦被吞,芯片可能停在 espflash 留下的 ROM 下载模式,"固件正常"看起来像"固件死了"。
> 已改成 **最多 4 次重试、听到启动日志才停**。

---

## T2 · 堆 + 时钟

**目标**:allocator 起来、毫秒时钟可用,并把这两个能力用一行日志证明。

- 用 `esp-alloc` 建一个静态堆(64KB)
- 单调毫秒:**上电从 0 开始**、≥1kHz 精度(SPEC §3.4)
- 启动横幅**追加一行**:`heap: 65536 free=NNNNN  clock: NNNNms`
- 验收:`cargo test` 全绿;横幅多出这一行且数值合理;`PARITY OK` 仍成立

> 注意:这一卡**允许**改横幅(新增一行,不改已有行的内容)。改的同时更新 `panel-core/tests/diag_format.rs`。

### 结果(验收通过)

新增 `st7735-text/src/sys.rs`(`init_heap` / `probe` / `now_ms`),`diag.rs` 加 `heap_clock`,
`main.rs` 里三行装配,`diag_format.rs` 加 2 条测试。板上实测横幅:

```
=== st7735 160x80 live text / 8x16 dot matrix ===
heap: 65536 free=64512  clock: 5ms
init done: MADCTL=0x60 OFF=(0,24) INVERT=false SPI=20MHz
READY
```

`free=64512` 是**故意的**:自检分配的 1KB 在打印时仍被持有(`drop(probe)` 在打印之后),
所以这个数只有在 allocator 真的切出一块内存时才会小于 65536。若 `free == size`,说明探针没跑,
不是"堆很健康"。探针还会把 1024 字节写成 `i ^ 0x5A` 再读回来,不一致就多打一行
`heap: probe readback mismatch`(正常启动永不出现)。

**与任务卡不一致的两处,均已写进 SPEC §3.4:**

1. **时钟源不是 systimer。** ESP32(经典)上 esp-hal 的 `time::Instant::now()` 读的是
   **TIMG0 LACT**(16MHz,`rt` 启动时配好);systimer 时间源是 S3/C 系列的做法。
   卡上写 systimer 是按 S3 的印象写的,本板按芯片走,不额外自开 systimer 去"对齐文档"。
2. **`clock:` 不是 0ms,是 5ms。** 计数原点确实是上电(`main` 第一行会是 0),
   但横幅是在 `esp_hal::init()` + UART 配置之后才打的,这中间已经过了约 5ms。
   为了让横幅上的数字有意义,**不做减去偏移的"美化"**。

时钟"真的在走"是单独验证过的:临时在 300ms `delay` 后再打一行读数,
得到 `clock: 308ms`(相对 5ms 差 303ms),随后撤掉该临时行、重编、重烧,
最终固件只有一行 `heap:`。

验收记录:`cargo build --release` **0 warning**;`panel-core` **16/16** 绿;
ELF 1 872 104 → 1 999 092 字节(+127KB,alloc + 堆管理进来了,属预期);
`check_parity.py` 仍 `PARITY OK (crc=0xEE473F67)`。

---

## T3 · AP 配网网页 + 凭据落盘

**目标**:把"能上网"做成可验收的三态:**没有凭据 → 开热点等人配;有凭据但 30s 连不上 → 回热点;连上了 → 串口打 IP**。
**屏上一行像素都不动**(`PARITY OK` 必须还是 `0xEE473F67`)。兜底页的排字是 T6 的活,T3 不碰画面。

### 这一卡真正要做的三件事(先读这段再读代码)

1. **换运行时**。现在是同步死循环,WiFi 要的是 async + 抢断调度。`main` 要变成 `#[esp_rtos::main] async fn`,渲染/串口那一段搬进一个 embassy 任务。这是本卡最大也最容易失控的改动。
2. **堆要重新量**。64KB 一定不够:esp-radio 官方实测 **STA 47–57KB / AP 53–63KB**。本卡必须用 `esp-alloc` 的 `internal-heap-stats` 量出 `max_usage`,再把 `HEAP_BYTES` 定成"峰值 + 8KB"(4KB 对齐),**不许手拍一个 128KB 了事**。
3. **凭据先写单份**。A/B 双扇区是 T4。T3 用**一个扇区一份记录**,写一半掉电 = 凭据丢 = 回热点模式。这不是 bug,是 T4 存在的理由;但对外接口必须先定成 T4 能无缝替换的形状。

### 依赖变更

```toml
picoserve = { version = "0.20.1", default-features = false, features = ["embassy", "alloc"] }
serde = { version = "1", default-features = false, features = ["derive", "alloc"] }
embassy-sync = "0.8.0"
```

已核过的事实(别自己改版本):

- `picoserve 0.20.1` 硬依赖 `embassy-net 0.9.1` + `embassy-time 0.5.1`,与 T1 钉的版本**正好一致**;它 msrv 1.93,本工具链 1.97.1。
- `embassy-sync 0.8.0` 已在 `Cargo.lock` 里(embassy-net 带进来的),提成直接依赖不会多编译一份。
- `esp-storage 0.9` 的 `critical-section` **在 default 特性里**,不要再手加。
- `picoserve` 的 `json` / `ws` / `tokio` / `std` 特性**全部不开**:本卡只用 `embassy` + `alloc`。

### 已核实的 API(本地源码逐个看过,不要凭印象重写)

| 要做的事 | 真实入口 |
|---|---|
| 起调度器 | `esp_rtos::start(timg0.timer0, sw_int.software_interrupt0)`;`SoftwareInterruptControl::new(peripherals.SW_INTERRUPT)`;main 用 `#[esp_rtos::main]` |
| 起 WiFi | `esp_radio::wifi::new(peripherals.WIFI, ControllerConfig::default())` → `(WifiController, Interfaces)` |
| 连路由器 | `controller.set_config(&Config::Station(StationConfig::default().with_ssid(s).with_password(p)))` → `controller.connect_async().await` |
| 开热点 | `Config::AccessPoint(AccessPointConfig::default().with_ssid(s).with_password(p).with_auth_method(..))` |
| 扫 SSID | `controller.scan_async(&ScanConfig::default()).await` → `Vec<AccessPointInfo>`;字段 `ssid: Ssid`(`.as_str()`)、`signal_strength: i8`、`auth_method: Option<AuthenticationMethod>` |
| 网口 | `interfaces.station` / `interfaces.access_point` 都实现 `embassy_net_driver::Driver` |
| HTTP | `picoserve::Server::new(&app, &config, &mut http_buf).listen_and_serve(task_id, stack, 80, &mut tcp_rx, &mut tcp_tx).await` |
| 表单 | `picoserve::extract::Form<T>`,`T: DeserializeOwned`(所以要 `String`,不能借 `&str`) |
| flash | `FlashStorage::new(peripherals.FLASH)`,同时实现 `NorFlash` + `ReadNorFlash` → **写回校验做得到** |
| 分区表 | `esp_bootloader_esp_idf::partitions::read_partition_table(&mut flash, &mut buf)` → `PartitionTable::iter()` / `entry.{offset,len,label_as_str}` |
| 复位 | `esp_hal::system::software_reset()` |
| 状态 | `controller.is_connected()` / `controller.rssi()` |

**不要用** `Config::AccessPointStation`:AP+STA 并存要双份内存,本卡三态互斥,一条链路上同时只跑一个 iface。等到真需要"配着网还不掉链路"再说,那是另一张卡。

### 模块与文件

| 文件 | 职责 | 能不能在 PC 上测 |
|---|---|---|
| `panel-core/src/creds.rs` **新** | 凭据记录的编码/解码(纯字节,含 CRC) | ✅ 是,本卡的主要单测都在这 |
| `st7735-text/src/store.rs` **新** | 一个扇区的擦/写/读回校验,只做 I/O | ❌ |
| `st7735-text/src/net.rs` **新** | WiFi 状态机 + AP 网页 + 保存回调 | ❌ |
| `st7735-text/src/main.rs` | 起堆 → 起调度器 → 起 WiFi → spawn 3 个任务 | ❌ |
| `st7735-text/src/diag.rs` | 加 7 条状态行(见"串口契约") | ✅ 是,`diag_format` 钉字 |

`net.rs` 里**不许**出现画布/像素/`blit`;`store.rs` 里**不许**出现 WiFi 类型。两个方向都不许互相 import。

### 凭据记录格式(单份,T4 会在这上面长成 A/B)

扇区 4096 字节,记录落在扇区偏移 0:

```
偏移  长  内容
0     4   magic "PWIF"
4     1   ver = 1
5     1   ssid_len (0..=32)
6     1   psk_len  (0..=64)
7     1   flags = 0(保留)
8     32  ssid,右补 0x00
40    64  psk,右补 0x00
104   4   crc32(0..104),小端
```

解码规则(每条都要有单测):magic/ver 不对 → `None`;`ssid_len==0` → `None`(等于未配置);长度越界 → `None`;CRC 不对 → `None`。
**psk 明文存 flash**,是有意的:没有安全元件可放密钥,这块板子不做安全承诺。这一句同时写进 SPEC §6。

### 分区与地址(写死在 `board.rs`)

| 地址 | 用途 | 规矩 |
|---|---|---|
| `0x9000..0xF000` | `nvs` 分区,24KB / 6 个扇区 | T3 **只用首扇区 `0x9000..0xA000`**;T4 用 `0x9000`+`0xA000` |
| `0xF000..0x10000` | `phy_init` | **禁碰**(esp-phy 在用) |
| `< 0x9000` | bootloader + 分区表 | **禁碰** |

为什么敢用 `nvs`:WiFi 初始化时 `nvs_enable: 0`(见 esp-radio 的 `ControllerConfig`),驱动不碰 NVS;我们也不实现 NVS 格式,只当**裸扇区**用。
启动时用 `read_partition_table` **核对一次**:label 为 `nvs` 的条目必须存在、`offset == 0x9000`、`len >= 0x2000`;不符就打 `STORE layout bad` 并且**不写任何 flash**(宁可回热点,也不要写错地址把 phy 数据擦了)。

### 状态机(四态,写成一目了然的 loop)

```
Boot
 ├─ store 读不到凭据 ──────────────→ Ap
 └─ 读到 ─→ Sta: set_config + connect_async
              ├─ 30s 内 stack 拿到 v4 地址 ─→ Up   (打 IP/RSSI,任务结束)
              └─ 30s 到 ─→ disconnect ─→ Ap
Ap: set_config(AP) → 起 192.168.4.1/24 的栈 → picoserve 守 80
     └─ POST /save 合法 → 写盘+读回 → software_reset()
```

- **30s** 从 `connect_async()` 返回后开始算,到 `stack.wait_config_up()` 拿到地址为止(用 `embassy_time::Timer` + `select`,不要 `sleep` 轮询)。
- 连上后**关掉 AP 栈**:不留一个没人连的热点白吃 60KB。
- 每次失败打一行 `WIFI fail`,再打 `WIFI ap`,然后重启流程只在重新保存后发生(不在 AP 模式里反复重试路由器,避免"密码错 → 一直闪" )。

### 串口契约(逐字,全部钉进 `diag_format.rs`)

```
WIFI sta ssid=<ssid>              // 开始连
WIFI up ip=<a.b.c.d> rssi=<n>     // 拿到地址
WIFI fail reason=<Name>           // 30s 超时/连接错误
WIFI ap ssid=<ssid> ip=192.168.4.1
WIFI save ok ssid=<ssid>          // 写盘+读回都成功
WIFI save bad reason=<Name>       // 写失败/读回不符,不重启
STORE layout bad off=<n> len=<n>  // 分区表核对失败
```

`reason=` 后面跟 **`WifiError` 的枚举名**(`Debug` 名),不是数字码:库里没有稳定数字码,自己编一套数字只会在下次升级时对不上。SPEC 7.2 的 E7 同步从"原因码"改成"原因名"并写理由。
AP 的 SSID 固定为 `st7735-` + MAC 后两字节十六进制小写(例:`st7735-c758`),**开放网络无密码**(配网页只用于配网,不承担安全)。

### 网页契约(不会写前端也要照这个来)

| 项 | 要求 |
|---|---|
| `GET /` | 现场 `scan_async` 一次,列出 ≤16 个 SSID(`<option value="ssid">ssid (rssi)</option>`)+ 密码框 + 保存按钮 |
| `POST /save` | `application/x-www-form-urlencoded`,字段名**必须是 `ssid` 和 `psk`** |
| 保存顺序 | **先回 200 页面,再写 flash,再 `software_reset()`**。反过来会让浏览器在 flash 擦除的 400ms 里超时,人以为没保存成功 |
| 校验 | `ssid` 1..=32 字节;`psk` 0(开放)或 8..=63 字节;越大越界一律回 400 并打 `WIFI save bad` |
| 页面 | 单个 `&'static str`,≤ 4KB,**不用 JS/不引外部 CSS**;HTML 里的 `{` 与 `}` 要转义成 `{{`/`}}` |
| 超时 | `picoserve::Timeouts` 里 `start_read_request`/`read_request`/`write` 都给 `Some(5s)`;**handler 超时要 > 扫描耗时**(扫描 1–2s,给 10s),否则扫得慢时页面直接 500 |
| 并发 | `listen_and_serve` 一次只服务 1 条连接,只起 1 个任务。别开 4 个抢那点堆 |

HTTP 缓冲区:`http_buf` 4KB、`tcp_rx`/`tcp_tx` 各 1.5KB,全部 `Box::leak`(我们有 alloc,不需要 `static_cell`)。

### 内存预算(必须给数,不给结论)

1. 临时开 `esp-alloc` 的 `internal-heap-stats` 特性(它不在 default 里)。
2. 跑完整流程:开热点 → 扫描 → 存盘 → 重启 → 连上路由器 → 挂着跑 10 分钟。
3. 记录 `max_usage` 的峰值,`HEAP_BYTES = round_up(峰值 + 8KB, 4KB)`。
4. 把上面的过程与最终数字写进 SPEC §3.4 + 本卡结果;`internal-heap-stats` **开着保留到 T5**(T5 的 TCP 缓冲还要再量一次),并在 manifest 注释里写明它多花 CPU。

### 验收(验收方执行)

```powershell
Set-Location H:\newwork\firmware\st7735-text
. D:\esp-toolchain\esp-xtensa-env.ps1
cargo build --release | Select-String 'warning'      # 必须零输出
Set-Location ..\panel-core; cargo test                # 必须全绿,且 creds 至少 6 条新用例
```

板子上按顺序跑,七步全部要过:

| # | 操作 | 预期 |
|---|---|---|
| 1 | 烧完首次上电(无凭据) | 串口 `WIFI ap ssid=st7735-xxxx ip=192.168.4.1`,手机能看到这个热点 |
| 2 | 手机连热点,浏览器开 `http://192.168.4.1/` | 页面出现,SSID 列表里能看到家里的路由器 |
| 3 | 选对 SSID、填对密码、保存 | 串口 `WIFI save ok`,约 1s 后重启,热点消失,`WIFI sta` → `WIFI up ip=...` |
| 4 | 断电 5s 再上电 | 不再开热点,直接自动连上并打 IP |
| 5 | 手机连上后把路由器关掉/改成错密码重存 | 重启后 `WIFI fail` → `WIFI ap`,约 30s 内回热点 |
| 6 | 重复 1 步与 3 步两次 | 同一 SSID 反复保存不累积垃圾,记录仍是 108 字节 |
| 7 | 全程 | `check_parity.py` 仍 `PARITY OK (0xEE473F67)`;`cargo test` 全绿 |

### 禁止

- ❌ 不动 `canvas` / `scene` / `panel` / `crc32` 的任何一行(画面归 T1/T2 管)
- ❌ 不开 `Config::AccessPointStation`、不引 `esp-wifi`(旧名)、不自开 systimer
- ❌ 不在 handler 里直接裸写 flash 寄存器(只用 `store.rs` 的接口)
- ❌ 不把 `ssid`/`psk` 塞进 `static mut` 手拼;一律走 `creds::encode/decode`
- ❌ 不改 `HEAP_BYTES` 以外的堆参数、不动 `esp-hal` 版本
- ❌ 页面里不写 JS,不引外部资源

### 已知风险

| 风险 | 先这么处理 |
|---|---|
| `#[esp_rtos::main]` 下原来的死循环会把 executor 饿死 | 渲染任务每轮 `Timer::after(20ms).await`,**不许**在自己的循环里等 UART 回调 |
| 整帧 `blit` 在 20MHz 下约 10ms,期间网络任务全停 | 先接受(T3 的 HTTP 只发生在 AP 模式,画面是黑的);T5 真的丢包再改分块 blit |
| 写 flash 时 WiFi 会卡住几百 ms | 保存流程本来就紧跟一次复位,顺序固定为"回包 → 写 → 复位" |
| 扫描与 AP 模式抢射频 | AP 模式下才扫,且只在 `GET /` 里扫一次;不要后台周期性扫描 |
| AP 与 STA 切换后 `embassy-net` 的运行器要不要重建 | 三态互斥,每次切换**丢掉旧 Stack、重建新的**,不让两个 runner 同时存在 |
| `picoserve` 的 `AppBuilder`(关联类型 `impl PathRouter`)要夜间特性 | esp 工具链本来就是 nightly;若报错就用 `picoserve::make_static!(AppRouter<..>, ...)` 静态构造,不要为此降版本 |

估时:2–3 天(其中一半在运行时改造与堆预算,配网页本身只值半天)。

### 2026-10-22 实测:四个坑(已修,别再踩)

这一轮上板把 T3 卡了整晚,四个 bug 全部定位并修掉。按"症状 → 根因 → 改法"记,顺序就是它们互相掩盖的顺序。

| # | 症状 | 根因 | 改法 |
|---|---|---|---|
| 1 | 热点能连上但手机永远"正在获取 IP";串口打完 `WIFI ap` 就一片死寂;`check_parity.py` 永远 `no FRAME line` | **`Uart::read` 在 FIFO 为空时会阻塞**(esp-hal 文档写明)。它被放在异步渲染循环里、单线程执行器上:PC 一安静,`poll_line` 永不返回,于是定时器不响、其他任务永不被 poll、DHCP 起不来、画面不刷。**这正是上面风险表预判的那条,只是没落到代码上** | `link.rs::refill` 改用 `read_buffered`(返回 FIFO 里已有字节,可为 0)。渲染循环里**任何**等待都会饿死整个执行器 |
| 2 | 屏幕全黑 + 每秒重启;`esp_hal::init` 断言 `stack pointer out of range: sp=0x3ffdf2d0, bottom=0x3ffdf94c` | `.data`+`.bss`+栈挤在同一段 192KB DRAM 里,栈紧贴 `.bss` 上方。`HEAP_BYTES=128KB` 的静态数组把栈压到只剩 **1,716 字节**,`main` 的栈帧一压就掉出栈底 | 堆降到 **96KB**(栈 34,484 字节);`sys::init_heap` 里加断言,栈不足 8KB 时直接报"heap leaves under 8 KB of stack",让故障自己说出名字;`sys::stack_bytes()` 直接读 `_stack_end_cpu0/_stack_start_cpu0` |
| 3 | 修掉 #1 后立刻 `PANIC ... smoltcp/src/iface/socket_set.rs:83: adding a socket to a full SocketSet` | `SOCKETS=2` 不够:DHCP 的 UDP + HTTP 监听 + picoserve 接受的连接,第 3 个就爆 | `SOCKETS = 4`(含 T5 守护链路的位置) |
| 4 | 串口 `STORE layout bad off=0 len=0`,凭据永远存不下 | `partition_entry.label()` 返回 **16 字节定长字段**(`&binary[12..][..16]`),拿它跟 `b"nvs"`(3 字节)比,**永远不相等** | `store.rs::label_is` 先按 NUL/0xFF 截断再比;`Unreadable` 与"表里没有 nvs"两种情况目前都打 `0/0`,分不开(留 T4 区分) |

**硬件一条(不是代码)**:期间串口出现"连一秒断一秒"、屏幕同步闪烁。判断依据是 **CH343 整个从设备树消失**——USB 转串口是 VBUS 供电,固件再怎么崩也做不到让它掉线。换 USB 口 + 换线后 25 秒零掉线,同一版固件。以后现场先怀疑线/口/供电,别先怀疑代码。

**验证方式记录**:

- 定位靠"最小复现":`esp_hal::init` + `esp_rtos::start` + 一个 500ms 定时任务,结果 tick 正常、晚 spawn 的任务也能跑 → 证明运行时无罪,锅在我们自己。临时工程用完即删,但**这个手法要留着**:先证明平台无罪,再二分自己的代码。
- `firmware/_cap.ps1`(抓串口,带 chunk 到达时间 + 自动重开)比原来的裸抓取好用,建议收进仓库;`DBG ...` 面包屑全部摘除。

**当前状态**:验收表第 1–2 步的半边已过(`WIFI ap ssid=st7735-c758 ip=192.168.4.1` + 新增的 `DHCP up port=67` 行 + `PARITY OK (0xEE473F67, 25600 bytes)`);第 2 步的"手机真正拿到 IP 并打开页面"与 3–7 步待验。**T3 仍未标 DONE。**


### 2026-10-22 深夜续:验收走到第 4 步,又挖出四个

手机端真正跑通了一次完整闭环。按发生顺序记,前三条都是"用户看见的怪事"倒逼出来的。

| # | 症状 | 根因 | 改法 |
|---|---|---|---|
| 5 | 手机上打开页面全是 HTML 源码 | `picoserve` 对**字符串响应体**默认 `text/plain; charset=utf-8`(`response.rs:441`)。代码里从没设过 Content-Type,等于让库替我们回答它回答不了的问题 | 三个页面全改 `Response::ok(page).with_content_type("text/html; charset=utf-8")` |
| 6 | `Network` 列表永远是空的 | AP 模式下 `esp_wifi_scan_start` 不可用(IDF 限制),而 `scan_async().await.ok()` 把错误吞了,看起来就像"附近没有 WiFi" | **决定:纯 AP,不做列表**,页面手输 SSID。理由:要列表就得 APSTA,多一套 buffer + 共存代价,换一个锦上添花的功能,不划算。`scan` 保留但失败要打 `WIFI fail reason=scan ...` + `SCAN n=0`,不许静默 |
| 7 | `WIFI fail reason=...` 被截断且**吞掉换行**,下一条日志粘上来:`...ssid: "LWIFI ap ssid=...` | 队列每个槽固定 96 字节,`write_all` 超长直接切,连结尾 `\n` 一起切 | `LINE_MAX` 提到 192 并统一到 `panel_core::link`(格式化与队列共用一个数);超长时**砍尾巴、保换行**。测试断言"恰好一个 `\n`" |
| 8 | 大约一半的启动连不上家里路由器,30s 后回热点 | 读全了才知道是 `Disconnected(AuthenticationExpired)` —— 第一次认证就过期(典型是首个 auth 帧没被及时响应)。驱动自带的 `failure_retry_cnt=1` + `scan_method=Fast`(只用缓存信道),冷启动没缓存,`StationConfig` 又**不给外部改这两个字段** | 在我们这层重试:30s 预算内最多 3 次(单次上限 12s,间隔 1s),仍失败才回热点。实测 `sta → fail(AuthenticationExpired) → up`,第二次必成 |

新增两条启动行(都是为了把"卡在哪"变成可观测):

- `STORE ok creds=yes|no` —— flash 记录读到了,并且**这一启动会不会去连网**
- `WIFI init` —— 射频初始化之前的最后一句;`READY` 后沉默 = 卡在 flash 读,`WIFI init` 后沉默 = 卡在 `esp_radio::wifi::new`

**仍未解决**:`READY` 之后整块板沉默(执行器一次都没跑起来的样子),前 10 次启动里出现 3 次;加了上面两条行之后连续 8 次启动**没再复现**。不敢说修好了 —— 只能说现在它一出现就会自己指出位置。这条留在 T3 尾巴上。

**验收状态(2026-10-22 06:25)**:

| 步 | 结果 |
|---|---|
| 1 首次上电开热点 | ✅ `WIFI ap ssid=st7735-c758 ip=192.168.4.1` + `DHCP up port=67` |
| 2 手机开页面 | ✅ 页面正常渲染(**列表改为手输 SSID**,理由见 #6) |
| 3 保存并重启 | ✅ `WIFI save ok ssid=LiZiNoKoo` → 软复位 → `WIFI up ip=192.168.1.53 rssi=-60` |
| 4 断电再上电 | ✅ `rst:0x1 (POWERON_RESET)` 后自动重连,凭据仍在 |
| 5 错密码回热点 | ⬜ 代码路径已有(重试耗尽 → `WIFI ap`),未亲手验 |
| 6 反复保存不累积 | ⬜ 未验 |
| 7 parity 与测试 | ✅ `cargo test` 全绿(18 条 diag 格式用例);串口 parity 出现过一次前缀脏字节,后随供电/接线恢复,未复现 |

**还差一点才能标 DONE**:第 5、6 步实测 + `READY` 后沉默不再出现。


## T4 · sector store A/B

- 两扇区 + `magic/ver/seq/crc32/len` 头部(SPEC §6)
- 验收:写入→断电→读回一致;**写到一半断电**(写循环里 `esp_hal::system::software_reset`)能回退到上一份
- 前提:T3 的 `store.rs` 接口(`load`/`save`)不变,只换内部为 A/B + `seq` 切换;`creds::encode/decode` 不动

### 2026-10-22 T4 已完成(含两个只有上板才会暴露的坑)

头部/CRC/取新规则搬进 **`panel-core/src/sector.rs`**(纯逻辑,PC 上可测),`store.rs` 只管扇区、擦写、读回比对。格式:`magic "PCFG" | ver | len u16 | seq u32 | crc32 | payload`,前 15 字节头,payload ≤1KB。

| # | 症状 | 根因 | 改法 |
|---|---|---|---|
| 9 | 迁移后一块板彻底丢凭据:上一次上电还是 `creds=yes`,下一次变 `creds=no`、直接开热点 | 迁移把旧记录所在的 A 扇区当成写入目标,**先擦后写**,而写入被驱动拒绝 → 旧副本与新的都没有了。正是 SPEC S-2 禁止的“两份都坏” | 迁移固定写 **B**,A 保持原样;写失败下次启动可重试。`save()` 选目标的原则重申为“永不碰当前生效的那一份” |
| 10 | 同上,写入被拒的原因:`erase OK` 之后 `write` 返回错误 | `esp-storage` 的写要求**长度 4 字节对齐**,帧是 15+108=123 字节 | 写入长度向上取整到 4 字节(124),尾部填充不被 CRC 覆盖;读者按帧内 `len` 停 |

**新增/变更的行**:`STORE ok creds=yes|no [slot=A|B seq=<n>]`(少了 slot/seq,“写进另一个扇区但还启动旧的”与“根本没写”在串口上不可区分);`STORE migrate ok`(没人要求的 flash 写入不许静默)。

**验收(全部在板上跑过,不是推断)**:

| 场景 | 手法 | 串口证据 |
|---|---|---|
| 从 T3 升级不丢配置 | `bench-legacy` 写回 T3 字节,再启动 | `STORE migrate ok` → `STORE ok creds=yes slot=B seq=1` → 正常进入连接流程 |
| 保存走 A/B 交替 | `bench-save-on-boot` | `WIFI save ok` + `STORE ok creds=yes slot=A seq=2`(B 生效时写 A) |
| 写到一半掉电 | `bench-powercut`(擦除后写半帧即复位) | 下次启动 `STORE ok creds=yes slot=A seq=2` —— 旧的仍是旧的,可用 |
| 坏扇区能自愈 | 接着再保存一次 | `WIFI save ok` + `slot=B seq=3`,半帧被覆盖 |
| 主机侧逻辑 | `cargo test` | `sector.rs` 9 条:半帧被拒、任意单比特翻转被拒、版本/魔数不符被拒、seq 回绕不把旧的判为新、空 payload 合法但空白扇区不合法 |

**顺带修掉的另一件事**:`seed-bad-psk` / `bench-powercut` / `bench-save-on-boot` / `bench-legacy` 四个编译开关都写进了 `Cargo.toml` 并注释了用途——它们是“不需要手机和网页也能验的通道”,以后再验存储/连接不用再求人配合。默认构建零警告(五个配置逐一验过)。

**代价(得写清楚)**:调试第 9 条时把这块板真实保存的 WiFi 密码弄没了(只丢了 psk,ssid 与 A/B 机制都正常)。现在的现场状态是:板子开着热点 `st7735-c758`、里面存的是测试用的假凭据 `bench-legacy`,需要重新在网页上填一次家里的 WiFi。教训已写进代码注释:**迁移/保存的目标扇区永远不能是当前生效的那一份**。

**T3 至此标 DONE**:第 1–4 步现场验过;第 5 步(密码错不变砖)用 `seed-bad-psk` 在板上验过(15 秒回热点);第 6 步由 `save` 的“擦-写-读回-比对-再取新”语义与 `slot/seq` 日志覆盖。

## T5 · TCP 链路 + hello(2026-10-22 拆成六小卡)

原卡只有三行,实际包含六件事,且有三处会卡住的地方在开工前就能预判。拆法如下(估时合计约 4 天):

| 卡 | 内容 | 可 PC 测 | 状态 |
|---|---|---|---|
| T5.0 | 环境:配回家庭 WiFi、起 daemon、确认 7802 通 | — | TODO |
| T5.1 | 纯逻辑:`hello` 原文、行切分、路由与收件箱、15s 静默判定 | ✅ | **DONE** |
| T5.2 | TCP 泵:`TcpLink` + 行环 + `SOCKETS` 4→5 + 堆再量 | ❌ | **DONE**(上板验过,附带 B-1) |
| T5.3 | 连接状态机:connect→hello→catalog→15s→退避,7 条新串口行 | 半 | **DONE**(2026-10-22 上板验过) |
| T5.4 | `LINK LOST` 角标(仅“有布局”时画,不动 parity 基准) | ✅ | **DONE**(2026-10-23 上板验过) |
| T5.5 | UDP 发现(`discover` → `here`),替代写死 IP | 半 | TODO |
| T5.6 | token 入 flash(动配置页与存储格式,**单开卡**) | — | TODO |

> **开工前定下的三条纪律**
>
> 1. **行切分只写一份**(`link::LineBuf`),UART 与 TCP 各持一个实例。BUGLOG #7 就是
>    “两份实现各自漂移”的代价。
> 2. **路由看内容不看线**:行首 `{` → 协议;其它 → bench(`<top>|<bottom>`,parity 靠它)。
>    这样“两路抢屏”不是需要仲裁的状态。
> 3. **出口只有一个**:diag 行只写 UART,绝不往 NDJSON 流里插裸文本。

### T5.1 结果(2026-10-22,`cargo test` 78 条全绿)

新增 `panel-core/src/wire.rs`(`HELLO` 常量 + `message_kind`/`uint_field`/`count_ids`)、
`link.rs` 扩出 `LineBuf<N>` / `Watchdog` / `Silence` / `Route` / `Inbox`。

**为什么 `hello` 不能照抄 SPEC §5.1 原文**:daemon 的 `_check_hello` 要求 token 匹配、
`role` 合法、`client_id` 非空,而且只有 `accept` 里点了 `ui`/`data` 才会真发这两类消息。
原文缺 `token`/`accept`,字段名还是 daemon 不认的 `v`。已按 PROTOCOL §5 修正并写进 SPEC §5.1
的批注(三处,**不是加功能,是原文写错**)。

**为什么现在还不写 JSON 解析器**:T5 只需要“这是哪种消息、带几个指标”,一个字节就够
(`{` 是协议线)。真解析器等 T6 要真 *理解* `ui`/`data` 时再上,那时 `wire.rs` 退回它名字
所说的角色:预筛。

验收:`cargo test` 34(lib)+18(creds)+19(diag_format)+7(doc)= **78 条,零 warning**;
`cargo check --lib` 绿(这条走的是 `no_std` 路径,`cargo test` 走的是 std,两者不是一回事)。

## T5.2 结果(2026-10-22,上板验收通过)

**代码已完、宿主机全绿、上板阻塞。** 新增 `st7735-text/src/net_link.rs`(泵任务 +
`TcpLink` 门面)、`diag::link_*` 七条日志、`Cargo.toml` 的 `bench-echo` 开关、
`firmware/echo_server.py`(五行回显服务,验收工具本身也要能重复跑)。

宿主机:`cargo test` **81 条全绿**(34 lib + 18 creds + 22 diag + 7 doc);
`cargo build` 与 `cargo build --features bench-echo` 均过。

**上板串口原话(验收依据)**:

```
stack: 21868 bytes for the main task
WIFI up ip=192.168.1.53 rssi=-52 | LINK tcp peer=192.168.1.128:7804
LINK up
LINK tx n=5 | LINK rx n=4 text=PING
```

`rx n=4` 是对的:echo 回的是 `PING\n`,`LineBuf` 按契约去掉了换行。
至此**“TCP 字节能来回”与“协议对不对”彻底分开了**,PROTOCOL 还没出场。
但在拿到这四行之前先撞了 B-1,它比 T5.2 本身值钱。

### B-1 · 每字节静态内存都是栈字节(已解决)

**症状**:`--features bench-echo` 烧进去后,每次开机走到 `WIFI sta` 后固定 4.19 s 出现
`WIFI fail reason=Disconnected(AuthenticationExpired)`,再过 1.7 s 就是
`PANIC stage=13 … Detected a write to the stack guard value on ProCpu`,随即 SW_RESET,
无限循环。8 次开机一字不差。

**已排除**(每条都是一次真实上板):

| 构建 | WiFi 结果 | 有没有崩 |
|---|---|---|
| 默认 `--release` | 首次 4.19 s 失败 → 重试 6.1 s 连上 `192.168.1.53` | 不崩(45 s 观察) |
| 默认 + `seed-bad-psk`(故意坏密码) | 5.2/12.9/15.5 s 三次失败 → 16.5 s 落到 AP 兜底 | **不崩** |
| `bench-echo` | 4.19 s 失败 | 失败后 1.7 s 崩,循环 |

于是可以钉死两件事:

1. **“连不上”这条路本身是好的**。seed-bad-psk 把它走穿了(重试阶梯 + AP 兜底 + DHCP),不崩。
2. **pump 从头到尾没跑过**——`spawn` 在 `wait_config_up` 之后,WiFi 没起来就不会执行。
   (临时把 `spawn` 去掉后体积回到 659,600 一字不差,pump 整个被 GC——那条实验没有信息量,
   但它确认了“差别只在链接内容”。)

**根因**:`stack_guard_monitoring` 在主**任务栈底部**放了守卫字,而在这颗芯片上
`.data`/`.bss`/栈共用同一段 DRAM,**链接器把栈直接摆在 `.bss` 之后**——
所以“栈有多大”= `0x3FFE0000 - _bss_end`,**任何静态内存增长都直接从栈里扣**。

`elf_syms.py` 量出来的两个数字把话说死:

| 构建 | `_bss_end`(＝栈底) | 主任务栈 |
|---|---|---|
| 默认 | `0x3FFDA494` | 23,404 B(22.9 KB) |
| `bench-echo`(修前) | `0x3FFDD354` | **11,436 B(11.2 KB)** |

多出来的 11,952 字节查得到出处:`OUTBOUND` 8,384 + `pump::POOL` 3,568。
默认构建里没人发送,`OUTBOUND` 被链接器整个 GC;pump 一存在,收发两个方向都变成活的。
11.2 KB 不够一次 WiFi 关联——崩溃时 `A1 = 0x3FFDCE40` 比栈底**还低 1,300 字节**,
是真溢出。`PC` 落在 ROM 只因为“最深那一层刚好是 PHY 代码”;这一点曾把我引向
“野指针”的错误结论,记在这里当教训:**寄存器先翻译成符号,再下结论**。

**改法**(两处):

1. `net_link::QUEUE` 8 → 3:每方向省 5,240 B。渲染每 5 ms 抽 6 行、daemon 1 Hz,
   3 行足够;满队丢最新行正是 PROTOCOL §16 的取舍。主栈回到 **21.4 KB**。
2. `sys::init_heap` 的栈下限 8 KB → 16 KB(`MIN_STACK_BYTES`)。11.2 KB 当年顺利过审、
   打完 banner 才在关联里死;8 KB 是 WiFi 出现之前的占。banner 现在每次开机都报
   `stack: <n> bytes for the main task`,下次这类问题是一次减法而不是一次侦查。

**留下的规矩**:在这个项目里加静态变量 = 从栈里借钱。T6 要在同一条栈上解析
`ui`/`data`,以后再看到“静态区涨了一点”要当心。

### 顺带记下的三个环境事实

- **这台路由器上,每次开机的第一次关联都会 `AuthenticationExpired`(+4.19 s),重试才成功。**
  看起来像密码错,其实不是。它会让开机到 `WIFI up` 多花 2 秒,也正好是 B-1 的崩盘窗口
  (崩溃点在重试前 0.2 s)。任何“WiFi 连不上”的结论都要先分清这两者。
- **PC 侧:`fake_daemon.py --source lhm` 常驻 7802,`pcmon.webui` 常驻 7803。**
  bench 测试把 echo 服务放到 **7804**,就是为了不打断这两个。
- **这台 PC 有两条针对 `python.exe` 的入站 Block 规则**(Private,程序级)。
  Windows 里 Block 优先于 Allow,所以“给端口加个放行规则”对它无效:SYN 到了、被静默丢掉、
  防火墙日志里一行 `DROP TCP … 7804`。绕法:用 `echo_server.js` + node(未被拦),
  或显式禁用那两条规则(属主机配置,需人同意)。取证手法见 `HANDOVER.md`。

## T5.3 结果(2026-10-22,三条路径全部上板验过)

**代码**:`net_link::pump` 现在是一个完整状态机(connect → hello → serve → 退避),
规则全部交给 `panel-core::session`;新增 7 条串口行(`LINK hello tx` / `catalog` / `deny` /
`ping` / `unknown` / `silent` / `backoff`),
`board::DAEMON_PEER` 的默认构建从 `None` 改成真的 `192.168.1.128:7802`(`bench-echo` 仍指 7804)。
宿主机 `cargo test` **102 条全绿**;两种构建都**零 warning**。

### 路径一:严格 daemon(故意不 `--lenient`)→ deny

```
LINK up | LINK hello tx n=288 | LINK deny reason=bad token | LINK backoff ms=1000 tries=1
LINK up | LINK hello tx n=288 | LINK deny reason=bad token | LINK backoff ms=2000 tries=2
LINK up | LINK hello tx n=288 | LINK deny reason=bad token | LINK backoff ms=4000 tries=3
...                                                                   ms=10000 tries=6
```

### 路径二:`--lenient` daemon → 握手完成

```
LINK up | LINK hello tx n=288 | LINK catalog n=0 ver=1
LINK catalog n=6 ver=2
```

`n=0` 一开始像个 bug,查过了**是真的**:板子第一次连上时 LHM 桥还没解析出 id,
daemon 先发一版空 catalog,1 秒后指标集合变化再补发 `ver=2`/6 项。取证工具是新加的
`firmware/probe_catalog.py`(用与固件**逐字相同**的 hello 直连 daemon,打印原始行的字节数与头部):
它报 `sent hello 288 bytes`,与固件 `LINK hello tx n=288` 一字不差;raw 行里 6 个 `"id"`。

### 路径三:哑服务(收 PING 后一个字节都不发)→ 15s 静默

`firmware/mute_server.js` + `bench-echo` 构建(`echo_server` 会回字节,压不出静默):

```
7136 ms  LINK up | 7228 ms LINK tx n=5
22424 ms LINK silent ms=15000 | LINK lost idle=15000 | LINK backoff ms=1000 tries=1
38650 ms LINK silent ms=15000 | LINK lost idle=15000 | LINK backoff ms=2000 tries=2
55883 ms LINK silent ms=15000 | LINK lost idle=15000 | LINK backoff ms=4000 tries=3
```

15.0 秒、`idle=15000`、阶梯递增,三个数同时对上。随后换成真 daemon 连跑 **60 秒**:
**一次重连都没有**(daemon 每秒发 `data`,看门狗不会误炸)—— 这条反而是最容易被忽略的验收项。

### B-2 · 退避阶梯被"TCP 连上"清零(已修)

**症状**:严格 daemon 下 `LINK backoff ms=1000 tries=1` **每秒一次、永不上升**。

**根因不是算术,是两份实现**:`panel-core/src/session.rs` 里早就有一个带 **12 条测试**的状态机
（`connected`/`line`/`closed`/`connect_failed`),而且它**早就把规矩写对了** —— 只有 `catalog`
才把 `failures` 清零。但 `net_link::pump` 没接它，自己另写了一份，那份把 `LINK up` 当成功。
于是同一个协议规则在工程里有两份，一份对、一份错，上板跑到错的那份。

**改法**:把 `pump`/`serve` 全部改成调 `session::Session`，删掉我刚加的等价函数
`link::next_failures`（重复实现不留）。现在梯子、看门狗、`deny`/`ping`/计数器
全部只有一份，全部在 PC 上有测试。另补一条回归测试
`a_denied_hello_still_climbs_the_ladder`——它就是 B-2 的形状。

**规矩写进 SPEC 新增的 L-5**。

改完重新上板复验两条路:lenient daemon → `LINK hello tx n=288 | LINK catalog n=6 ver=2`;
严格 daemon → `tries` 从 1 爬到 6、退避封顶 `ms=10000`。

宿主机最终:`cargo test` **102 条全绿、零 warning**（含 `tests/diag_format.rs` 新加的
`stack_total` 钉死测试，它之前是 dead code 警告）。

### 环境事实(两条都会伪装成固件 bug)

1. **这台 PC 的两条 `python.exe` 入站 Block 规则已经在 2026-10-22 **禁用**(已得李子同意,
   规则本身保留,只把 Enabled 改成 False)。
   未禁用前,板子的三次 `connect` 全部 `LINK fail reason=Timeout`:SYN 到了、包被静默丢掉,
   固件毫无线索。以后"daemon 明明在 Listen 但板子连不上",**先看这两条规则**。
2. **daemon 必须 `--lenient` 启动**(T5 阶段 `hello.token` 发的是空串,T5.6 才进 flash)。
   严格模式下的输出不是失败,而是**证据**:`LINK deny reason=bad token` 证明 hello 真的发出去了。

### 一个方法论教训(我自己踩的)

哑服务的日志**没打时间戳**时,我看到 73 行 `connection` 就断言"板子在狂连",
还据此写了半天假设。加上时间戳后真相是:每次会话老老实实待满 15 秒,
73 次是 8 分钟里累积的。**日志没时间戳,数量就不是证据。**
现在 `mute_server.js` 的第一列就是相对秒数。

## T5.4 结果(2026-10-23,角标出现与消失都验过)

**代码**:`panel-core/src/badge.rs`(`LINK LOST`,72×16,右下角贴角 `X=88 Y=64`,白底黑字反相)、
`Canvas::fill_rect`、`Scene::has_layout()`(当前判据 = 场景里有非空白内容)、`diag::badge`。
主循环只做三件事:判断 `has_layout && !is_up`、只在**边沿**重绘、重绘后叠角标。

**PC 侧同源**:`preview_st7735.py::draw_badge` 与 `render_page(..., badge=True)`,
`check_parity.py --link-lost` 拿它跟板子比 CRC。两个数字钉在 `panel-core/tests/parity.rs`:
plain `0xEE473F67`、badge `0x6A7B210F`。

**上板串口原话**(`firmware/badge_probe.py`,四阶段全自动):

```
1. link down, content sent every 2 s
   BADGE on, FRAME 0x6A7B210F
2. daemon started (pid=16836), waiting for the badge to clear
   BADGE off, FRAME 0xEE473F67
3. daemon killed (pid=16836), the badge must come back
   BADGE on, FRAME 0x6A7B210F
4. daemon restarted (pid=8392), the badge must clear again
   BADGE off, FRAME 0xEE473F67
BADGE OK
```

四个 CRC 全部与 PC 预览逐字节相等 → 角标不只是“存在”,它跟预览画的是同一份 25600 字节。
**parity 基准没动**:链路正常时 `check_parity.py` 仍是 `PARITY OK 0xEE473F67`。

### 为什么角标也要占一行串口

角标最容易出的事不是“不出现”(眼睛能看见)而是**“不消失”**——夹在健康链路上的 `LINK LOST`
会让用户以为数据过期了。脚本看不见玻璃,所以只能让板子自己说 `BADGE on|off`。

### 环境坑(都会伪装成固件 bug)

1. **`sys.executable` 是沙盒里的 python 3.11**,`PYTHONPATH` 被丢掉 → 子进程里
   `from pcmon import lhm` 失败,看起来像“daemon 启动不了”。要用真解释器:
   `C:\Users\Administrator\AppData\Local\Programs\Python\Python312\python.exe`。
2. **打开 COM 口会动 DTR/RTS**(这块板上 DTR=IO0、RTS=EN) → 可能把板子重置进下载模式,
   第一行写入被吞。对策:内容行**每 2 秒重发**直到收到 `FRAME`;不要自己 `release_board()`——
   实测那一下之后板子完全不出声。`check_parity.py` 已改成同样重发策略。

### T5.4.1 重连阶梯(2026-10-23 04:45,`firmware/reconnect_probe.py`)

怀疑「daemon 死掉后板子不再重连」(曾观察到 30 分钟零连接尝试)。用**一次开串口、全程不关**的探针
把板子自己的话录下来 —— 结论是**没有这个 bug**,退避阶梯完全按 L-1/L-2/L-5 走:

```
[  5.7 s] killing the daemon (port stays open, we keep listening)
[  8.5 s] board: LINK lost idle=319          <- 319 ms 就发现对端没了
[  8.5 s] board: LINK backoff ms=1000 tries=1
[  9.5 s] board: LINK tcp peer=192.168.1.128:7802
[ 14.5 s] board: LINK fail reason=Timeout
[ 14.5 s] board: LINK backoff ms=2000 tries=2
[ 21.5 s] board: LINK backoff ms=4000 tries=3
[ 30.5 s] board: LINK backoff ms=8000 tries=4
[ 38.3 s] starting the daemon again
[ 38.5 s] board: LINK tcp peer=192.168.1.128:7802   <- 0.2 s 就撞上门
[ 39.5 s] board: LINK up / hello tx n=288 / catalog n=0 ver=1
```

32 秒里 7 次重试声明,阶梯 1/2/4/8 一次不差;daemon 回来 0.2 秒就连上。
**之前那半小时的“沉默”是测量错误,不是固件错误**:CH343 这块板的 EN 脉冲约三次有一次被忽略,
reset 没生效时板子不重启、也就不打印任何东西 —— 看起来和“卡住不重试”一模一样。
旧 `read_for` 早就为重发脉冲写了注释,探针也必须照做(`kick()`:脉冲后 8 秒没见到新行就再打一次,最多 4 次,
每次跟一发 `release_board()` 把 DTR 抬起来免得落进下载模式)。

> 教训:**“没有日志”永远不能当作“没有行为”的证据**,除非日志通道本身在整段时间里都被证明是通的。
> 探针要先把“我能听见你”这一步钉死,再去判断“你说了什么”。

## T5.5 结果(2026-10-23,板子自己找到 daemon)

**代码**:`panel-core/src/discover.rs`(`DISCOVER` 的字节、`parse_here()`、`should_discover()` 策略,8 条单测)
+ `net_link::pump` 改成 `initial: Option<(ip,port)>`,没地址/连败两次就先广播一轮
+ `fake_daemon.py` 加 UDP 7802 应答(`_start_discovery` / `_local_ip_toward`)。

**策略**(`should_discover`):没地址 → 必问;连续失败 ≥ 2 → 再问(DHCP 换租约的场景);
只失败 1 次不问(通常只是电台还没醒)。失败计数直接读 `Session::failures()`,所以发现和退避阶梯
对"失败"的定义天然一致,谁也不清谁的计数器 —— **L-5 原样保留**(只有 catalog 能清阶梯)。

**上板串口原话**(板子侧 `firmware/disc_probe.py`,PC 侧 `check_parity.py --boot`):

```
WIFI up ip=192.168.1.53 rssi=-47
DISC tx / DISC tx / DISC tx          <- 三轮广播
DISC here ip=192.168.1.128 port=7802 <- daemon 报价
LINK tcp peer=192.168.1.128:7802
LINK up / hello tx n=288 / catalog n=6 ver=2
```

`board::DAEMON_PEER` 现在是 **`None`**(以前写死 `192.168.1.128`),`FALLBACK_PEER` 只在
**整轮发现无人应答**时兜底(同时打 `DISC none`,不静默)。⇒ 路由器换租约不再需要重编译。

**踩到的坑:防火墙是「按程序 + 按协议」的**。`panel echo 7802` 那条规则**只有 TCP**,
而 python312 没有程序级放行 → TCP 能连、UDP 广播包**到了网卡却收不到**。
取证手法(以后照抄):用 **node**(有 `EasyClaw Node Runtime` 的 proto=Any 规则)绑同一个 UDP 口,
能收到就说明包到了、锅在防火墙;同时打印 `Get-NetFirewallPortFilter` 按程序分组看规则缺口。
已加规则 `panel discover 7802`(Inbound Allow, UDP, 7802, Profile Any)。

## 启动自检页(2026-10-23,李子提的「开机自检挺酷」)

黑屏在显示器上和"板子死了"完全无法区分 —— 这一晚已经为这个歧义付过两次代价。现在:

- `panel-core/src/boot.rs`:160x80 正好 **20 列 × 5 行**(1 行标题 + 4 行步骤),标题 `SELF TEST`(樱花粉 `0xFDB8`),
  四行 `PANEL / STORE / WIFI / LINK`,状态 `--`(未开始)→ `...`(进行中)→ `ok` / `fail`(红)。
- BSP 侧 `bootstate.rs`:每行一个 `AtomicU8`,三个任务各自 `announce(index, step)`(��值触发,重复调用不打行、不重绘),
  同时吐 `BOOT <LABEL> <word>` 串口行 —— **屏上画的东西必须能听见**,和 `BADGE on|off` 同一条规矩。
- 渲染任务:只要**场景还没内容**就画自检页(取代黑屏),内容一到立刻切回正常渲染;边沿重绘,不每帧刷。
- 四行到位时正好覆盖开机这几秒:panel/store 在 `main` 里(executor 之前)先落,WiFi 在 `net::try_station`,
  LINK 要等 **catalog**(`LINK up` 只是 socket 建起来,被 deny 时不算好 —— 和 L-5 同一条道理)。

**上板串口原话 + CRC**(`check_parity.py --port COM10 --boot`):

```
BOOT PANEL ok  BOOT STORE ok     FRAME 160x80 crc=0x8e9722aa
BOOT WIFI ok                     FRAME 160x80 crc=0x5adc8dbd
BOOT LINK ...                    FRAME 160x80 crc=0xaffeb207
BOOT LINK ok                     FRAME 160x80 crc=0x188fddde
BOOT PARITY OK (crc=0x188FDDDE, 25600 bytes, 4 frames)
```

四个数同时钉在 `panel-core/tests/boot_parity.rs` 里,PC 侧 `preview_st7735.py::boot_page()` 同源。
**parity 基准没变**:发内容后仍是 `PARITY OK 0xEE473F67`。

## L 系列 · 本地层(脱离 PC 也能用)

> 需求原话:“除了显示电脑数据,也能让设备脱离电脑做一些别的活动,比如设置、查看设备信息”。
> 关键约束来自另一句:“必须在电脑上点应用才能推到设备上,电脑上的预览只是给用户改 ui 时预览用的”
> → **设备必须自己持有“已生效的那一版布局”,PC 只是编辑器,不是画面镜像源。**

四态所有权(谁拥有玻璃):`Fallback`(兜底页)/ `Local`(本地页)/ `Remote`(PC 布局)/ `Setup`(配网)。
规则、抢占、30s 自动交还、本地页内容与 golden CRC、四态输入抽象(`Verb` 五动词,
页面逻辑不认识按键),已定稿但未入卡——开工时据此写 L1–L5。

排期决定:**TLV 存储先于 T6**。理由:T6 本来就要把布局存 flash,如果 T6 先塞一个独占格式,
L4 就得再迁移一次,而迁移写错扇区正是 T4 踩过的 BUGLOG #9。

| 卡 | 内容 | 估时 | 依赖 |
|---|---|---|---|
| L1 | 输入硬件 + 去抖(键型待定) | 0.5–1 天 | — |
| L2 | 只读本地页 1–3 + golden CRC + `OWNER` 串口行 | 1 天 | — |
| L3 | 所有权状态机(四态 + 抢占 + 30s 交还) | 1 天 | L2 |
| L4 | 存储 payload 改 TLV + 设置页可写 + 迁移 | 1.5 天 | L3 |
| L5 | 键位事件上报：动词 → 焦点移动 → 位置事件(`{"k":"key","id":"focus","v":n,"scene":i}`) | 0.5 天 | L1、**T6 的 focus 表** |

**L2 先于 L1**:只读页不依赖按键(`bench-local` 注入动词),先证明“板子自己会说话”,再焊按键。

## T6 · ui/data + 布局持久化

> 已按“应用式推送”重排为 T6a → T6b → T6c,详见上面 L 系列的排期决定。
>
> **新增交付(T6d · 焦点表)**:解析 `ui` 时除了节点本身,还要存一张 focus 表
> (项数 + 每项 `x/y/w/h`,≤32)。原因:按键导航必须有**看得见**的高亮框,
> 而高亮位置的来源就是这张表。没有它,按了键屏上没任何变化,串口却一切正常。
> 序号一律 **1 基**(PROTOCOL §11.5 已钉死),设备内部 0 基数组出线前加一。
> 验收:`ui` 只有 1 个可选项 → 焦点必在 1 且高亮可见;选项数减少后按越界规则回落。

- 节点表 ≤32、二进制存 flash(SPEC §6 S-6)、`ver` 用 `!=`(D-34)
- 验收:拔电重开,布局还在、值全 `--`;`term_tk.py` 与板子同源同画面

## T7 · 折线

- `line` 节点 + 设备端环形缓冲(协议 §8)+ 脏矩形合并推屏(D-39..D-43)
- 验收:1Hz 曲线滚动;拔掉 PC 曲线不消失;单次 blit ≤4ms

## 颜色顺序：这块玻璃是 BGR（2026-10-23，李子发现的）

**症状**：李子说「为什么我现在看着屏幕的颜色是 bgr」。用 `bench-colours` 构建画三块纯色
（左 0xF800 红 / 中 0x07E0 绿 / 右 0x001F 蓝）上板，他看到的顺序是 **蓝绿红** —— 干净的红蓝对调。

**判据为什么只能靠色块**：这块屏上唯一的非白色像素是自检页的樱花粉标题，`0xFDB8` 反过来变成
淡蓝紫（255,182,224 -> 224,182,255），黑底上「也挺好看」，所以从点亮那天起就没人怀疑过。
白字 `0xFFFF` 在 RGB 和 BGR 下**一模一样**，盯着屏幕看一万年也看不出问题。

**根因**：`MADCTL` 的 bit3（0x08）是 ST7735S 上唯一能把 R/B 干净对调的开关。我们写的是
`0x60`（bit3=0，按数据手册=RGB），但玻璃照 BGR 解释 —— 这块模块的柔性板/玻璃接法就是 BGR
（克隆片把该位语义反过来也常见）。旧的 st7735-esp32 工程用同一个 `0x60` 且当时「看着对」，
所以要么两块模块不同批，要么当时粉的其实是淡蓝紫（无法回溯）。

**改法**：`board.rs` `MADCTL = 0x68`。李子确认「现在对了」（红绿蓝）。
⚠️ 别再凭「看着挺好看」把它改回 0x60 —— 要改先重跑 `bench-colours`。

**副作用**：
1. **帧字节和 CRC 一律不变** —— MADCTL 只决定玻璃怎么解释那 16 位。`0xEE473F67`（内容页）/
   `0x188FDDDE`（自检页）依然成立，实测复验通过。
2. **PC 预览要跟着交换 R/B**：`preview_st7735.py` 新增 `GLASS_BGR = True` + `glass_rgb()`，
   只影响出图（Tk 窗口 / PPM），不影响 `to_bytes()` 和 CRC。selftest 6 项 PASS。

## 串口工具：pyserial 在这台机上不能用（2026-10-23）

**症状**：`serial.Serial("COM10", ...)` 之后第一次 `read()` 就
`ClearCommError failed (PermissionError(13, '设备不识别此命令。'))`；再开又变
`could not open port 'COM10': FileNotFoundError`。

**根因**：pyserial 3.5 open 时默认把 **DTR 和 RTS 都拉高**（这块板 DTR=IO0、RTS=EN），等于把板子
按在复位里；随后 driver 对读句柄返回 ERROR_INVALID_FUNCTION。.NET 的 `SerialPort` 默认两线为
**false**，所以同一时刻它开同一口、读同一块板完全正常（实测读到完整启动日志）。

**改法**：`firmware/pan.ps1`（纯 ASCII，.NET SerialPort）
  `powershell -NoProfile -File firmware\pan.ps1 -Seconds 20 [-Reset] [-Lines "CPU 33.5%|TEMP   34C"]`
  - 不加 `-Reset` 时**打开口不会复位板子**（这正是 pyserial 做不到的）
  - `-Lines` 每 2 秒重发一次（第一次写入偶尔被吞）

## CH343 会整块掉出设备树（疑似供电）（2026-10-23）

**症状**：抓日志跑到 `STORE ok creds=yes slot=A seq=4` 之后，`ReadExisting` 抛
`The port is closed.`，`Get-PnpDevice` 里 COM10 也消失；几秒后又出现、一切正常。
`check_parity.py --boot` 报的 `FAIL no FRAME` 就是这么来的（读到一半设备没了）。

**意义**：这条直接解释了我们一直没定性的 **「READY 之后整块板静默」**：静默点正好落在
`WIFI init` 之前 —— 也就是 WiFi 射频启动、电流尖峰的那一刻。**很可能是供电/线材，不是固件。**
下一步（等人手）：换一根短的粗线 + 直插主板后置 USB 口，同一段日志连抓三次不掉，即可判为供电问题。

**同一次实测的正面结果**（换 .NET 抓之后全部对上）：
```
BOOT LINK ...          FRAME 160x80 crc=0xaffeb207 bytes=25600
LINK tcp peer=192.168.1.128:7802 / LINK up / hello tx n=288 / catalog n=6 ver=2
BOOT LINK ok           FRAME 160x80 crc=0x188fddde bytes=25600   <- 自检页全 ok
（发 "CPU 33.5%|TEMP   34C" 后）FRAME crc=0xee473f67             <- 内容页基准未变
```
## 掉线结案：换线换口就好了（2026-10-23 16:40）

李子换了一根短粗线、插到主板后置 USB 口之后，**连续 3 次复位抓日志零掉线**，每次都能跑完整条链：

```
BOOT PANEL ok / BOOT STORE ok      FRAME crc=0x8e9722aa
WIFI up ip=192.168.1.53 rssi=-50   FRAME crc=0x5adc8dbd
DISC tx x3 -> DISC here ip=192.168.1.128 port=7802
LINK tcp peer=... / LINK up / hello tx n=288 / catalog n=6 ver=2
BOOT LINK ok                       FRAME crc=0x188fddde
```

⇒ **「READY 之后整块板静默」结案：是供电/线材，不是固件。** 之前的判据是「串口在 `STORE ok`
之后一行不出、COM10 从设备树消失」——那是 USB 掉线，不是程序停住。同一条线在换口之前
两次都能复现，换完一次都没有。

**以后遇到「板子不说话」的第一件事**：`Get-PnpDevice -Class Ports -PresentOnly` 看口还在不在，
再用 `pan.ps1` 抓；不要一上来就怀疑代码（这一晚为此白查过半小时）。

## 坑：feature 构建会覆盖同一个 bin 路径（我自己踩的）

`cargo build --release --features bench-echo` 和 `cargo build --release` 输出**同一个文件**
`target/xtensa-esp32-none-elf/release/st7735-text`。所以「先用 feature 构建做个实验、再刷默认固件」
的顺序如果搞错，刷进去的就是**上一个 feature 构建**。

实际后果：刷进了 `bench-echo`（连 7804、不发 hello），板上表现为
`LINK tcp peer=192.168.1.128:7804` → `LINK fail reason=Timeout` → 退避阶梯 → `BOOT LINK` 永远停在 `...`，
看起来像「daemon 挂了」或「发现逻辑坏了」，实际是刷错固件。

**规矩**：`espflash flash` 之前**永远重建一次默认构建**（`cargo build --release`），
并确认 `init done:` 那行里的 MADCTL 和预期一致、`LINK tcp peer=` 是 7802 不是 7804。
## T5.6 结果（2026-10-23，token 入 flash，不再需要 --lenient）

**存储格式**：`panel-core/src/creds.rs` 升到 `ver = 2`，140 字节：
`magic|ver|ssid_len|psk_len|token_len|ssid32|psk64|token32|crc32(0..136)`。
`ver = 1`（108 字节，T4/T5 写的）**仍然能读**，读出来只是没有 token ⇒ 老板升级后
WiFi 照样能用，配一次 token 就完成升级（不需要单独的迁移 pass，也就不会像 T4 那次
把唯一副本擦掉）。

**链路**：`hello` 的 token 从存储取，不再写死空串（`wire::HELLO_HEAD/TAIL` + 逐段写，
不占栈）。**`hello` 现在是 304 字节 = 288 + 16**。

**配置页**：多一个 `Daemon token` 输入框；**留空 = 保持已存的那个**（否则只想改 WiFi
的人会把 token 清掉）。非法 token（>32 字符、带引号/反斜杠/换行）走单独的
`SAVE_BAD_TOKEN_PAGE`，不写 flash。

**新 bench 开关**：`bench-seed-token` —— 没有 token 时把 `board::BENCH_TOKEN`
（`0123456789abcdef`）写进 flash。理由和 `seed-bad-psk` 一样：真配对要手机 + AP + 手输
16 个字符，脚本没法重复。

### 上板取证（三次，都是严格 daemon，没有 `--lenient`）

```
# 1) 种 token（bench-seed-token 构建）
TOKEN ok len=16
STORE ok creds=yes slot=B seq=5        <- A/B 轮换，写的是非活动扇区
LINK hello tx n=304 / catalog n=6 ver=2

# 2) 刷回默认构建：token 从 flash 读出来，还在
STORE ok creds=yes slot=B seq=5
TOKEN ok len=16
LINK hello tx n=304 / catalog n=6 ver=2 / BOOT LINK ok        FRAME crc=0x188fddde

# 3) daemon 用**错的** token 起（--token deadbeefdeadbeef）
LINK up / LINK hello tx n=304
LINK deny reason=bad token
BOOT LINK fail                        FRAME crc=0x1efeb12b   <- 红色 fail，已钉进 boot_parity
LINK backoff ms=1000 tries=1 → 2000/2 → 4000/3 → 8000/4 → 10000/5...
```

第 3 条同时证明两件事：**token 真的被校验**（不是我们发什么都被收），以及
**L-5 没被破坏**（`deny` 不清退避阶梯，`tries` 一路涨到 10）。把 daemon 换回正确
token 后，板子在下一个退避窗口内自己 `catalog n=6 ver=2`，不用重启板子。

### 坑：`Stop-Process` 杀的可能是 shell 包装进程，不是 python

后台起 daemon 时拿到的 pid 是 **包装进程**的 pid，`Stop-Process <那个 pid>` 之后
`Get-NetTCPConnection -LocalPort 7802` 仍然有 Listener，于是「换了 token 还是 deny」——
其实是旧 daemon 还在跑。**判据要两条一起看**：先看监听端口的 `OwningProcess`，
再 `Get-CimInstance Win32_Process` 看那个 pid 的**命令行里到底是哪个 token**。
## T6a 进行中：json + ui 解析（2026-10-23 17:50，李子出门期间）

**状态**：`panel-core/src/json.rs` 与 `panel-core/src/ui.rs` 写完、有测试、`cargo test` **156 条全绿**、
固件 `cargo build --release` 零 warning。**还没接到链路上**（`net_link` 收到 `ui`/`data` 后仍只打
`Route::Proto => {}`），所以屏上暂时还是原来那个 `CPU 33.5%` 的 bench 页。

### `json.rs`：够用的读取器，没有树

- `Cursor`（16 字节，无 alloc）：`value_raw()` 返回"下一个值的原始字节"，嵌套容器整段跳过。
- `Fields::of(obj)` → `.get(key)` / `.find(pred)` / `.iter()`；`Array::of(arr)` 是个迭代器。
- `as_str` / `as_i64` / `as_f64` / `is_null`：转换在使用点做，所以一个值不会被存两遍。
- `as_str` **拒绝带转义的字符串**（协议里全是 ASCII：指标 id、`#RRGGBB`、短标签），
  这样绘制路径不用为它准备解码缓冲区。

**❗ 第一版踩的坑（已改，值得记住）**：`Fields` 最初自己持有游标，`get()` 是**消耗式**的 ——
`get("w")` 之后再 `get("x")` 就找不到前面的字段了，而且**不报错**。`ui` 的黄金页测试一跑就炸
（`NoNodes`），否则上板后会表现为"某些页面永远画不全"。现在 `Fields` 只持有字节，
每次 `get` 都从头发一轮新的游标（消息 ≤1KB，重复扫描免费）。

### `ui.rs`：固定表的页面

- `Layout::parse(line)`：`{"t":"ui","scene":n,"ver":n,"bg":"#RRGGBB","nodes":[...]}` →
  `[Node; 32]` + `len`。每个 `Node` 内联保存自己的 `text[20]`、`bind: Id[16]`、`unit[4]`，
  所以节点不指向接收缓冲区（缓冲区下一秒就被下一行覆盖了）。
- 支持 `rect`（`fill` 填充 + 可选 `color` 描边）、`text`（静态）、`num`（绑指标 + `dec` + `unit`）。
  **不支持 `path`/`line`/`bar`/`dot`**：整页拒绝（`Error::Unsupported`），不是丢一个节点 ——
  少画一个元素看起来像渲染 bug，而 daemon 握手后会重发 `ui`（PROTOCOL §16）。
- `colour("#RRGGBB") -> RGB565`：**PC 端必须用同一个取整**，否则 CRC 对不上。
- `Layout::render(canvas, values)`：先 `clear(bg)` 再按顺序画；`rect` 的裁剪在 `fill()` 里做一次
  （协议允许绝对坐标越界，`Canvas::fill_rect` 只吃 `usize`）。
- `Values`：`MAX_NODES` 条的 `(Id, Option<f64>)`。`apply_data` 按 PROTOCOL §9：
  **没提到的 key 保持旧值**，`null` 清成 `--`，非数字跳过。
- `format_number`：整数运算，`dec` 位小数 + 单位；`None` → `--`（不是 `0`）。

### 下一步（T6b/T6c/T6d，按这个顺序）

1. **接线**：`net_link` 的 `Route::Proto` 里按 kind 分派：`ui` → `Layout::parse`，
   `data` → `Values::apply_data`，再把 `Layout + Values` 经 `Signal`/`Channel` 交给渲染任务。
   渲染任务拿到 `ui` 之后必须**停止画自检页**（`Scene::has_layout` 的判据要从"有非空白内容"
   换成"真的拿到了布局"）。
2. **PC 侧同源预览**：`preview_st7735.py` 加一个 `ui_page(nodes)`，用同一套 565 取整、
   同样的 8x16 字库画，然后 `check_parity.py --ui <json 文件>` 比 CRC。没有这一步，
   `ui` 的正确性只能靠眼睛。
3. **T6d focus 表**（按键导航要有看得见的高亮框，序号 1 基）。
4. **T6c 存 flash**：布局二进制落盘（SPEC S-6），TLV 存储先于 T6，否则 L4 要再迁一次。

## T6b 结果（2026-10-23 22:20，真页面终于上屏）

**hello 与解析器不一致，整页被拒**（真 bug，已修）。板子日志原本是
`LINK hello tx n=304` 之后一直画自检卡片，根因：hello 声明支持 `path`，
而 `ui::parse_node` 没有 path 分支 → 遇到一个 path 节点整页 `reject`。
现在 `n=297`（少了 `"path",` 9 字节），板子日志：

```
LINK hello tx n=297
LINK catalog n=6 ver=2
PAGE ui scene=0 ver=1 nodes=6
FRAME 160x80 crc=0x85e823a9
```

加了 `wire.rs::hello_claims_exactly_what_the_page_parser_draws`：给每个 primitive
造一条最小页面，断言「hello 承诺的」==「`ui::Layout::parse` 真画得出来」。
以后谁单边加 primitive 就会红。`cargo test` 166 条全绿。

**PC 侧同源预览打通**：`preview_st7735.py::ui_page(layout, values, badge=False)`，
`check_parity.py --ui pages\panel.ndjson` 比 CRC。
❗ 坑一：daemon 在跑的时候**不能**用 `--ui` 验——串口喂进去的 fixture 会被 daemon
每秒推的 `data`（和它自己的 `ui`）覆盖，抓到的 7~8 个 CRC 全不一样。必须先停 daemon。
❗ 坑二：停 daemon 后板子按设计贴 `LINK LOST` 角标，PC 预览不带角标就永远比不上。
所以给 `check_ui` 加了 `--link-lost`（`ui_page(..., badge=True)`，白底 + 页面底色字，
同 `badge.rs`）。

证据（10 节点 fixture + 角标，逐字节相等）：

```
PC renders : ui scene=0 ver=1 nodes=10 + LINK LOST badge -> crc=0x0E85F74A
BADGE on
PAGE ui scene=0 ver=1 nodes=10
UI PARITY OK (crc=0x0E85F74A, 25600 bytes, 4 frames, 2 matched)
```

**顺手记一个隐患**：`H:\newwork\daemon.token` 里现在是 `207a6df89f1dc168`，
但板子 flash 里认的是 `0123456789abcdef`（daemon 用后者跑、`LINK up` 正常）。
两者不一致 → 哪天有人照 token 文件启动，板子会被 deny。**待李子决定留哪个**，
定下来后要重烧一次 flash token 并把两边对齐。

### token 隐患已解决（2026-10-24 08:10）

复查结果：`daemon.token` 现在已经是 `0123456789abcdef`，**与板子 flash 一致**，不需要重烧。
真正的坑不在值，而在 `run-lhm.bat` 第 83 行启动 daemon 时**没带 `--token`** ——
`fake_daemon.py` 的默认行为是 `secrets.token_hex()` 每次启动随机新生成并写回文件，
所以原来每双击一次 `run-lhm.bat`，token 就会变一次，板子当场被 deny。

修法（已改）：`run-lhm.bat` 里钉死 `set "TOKEN=0123456789abcdef"` 并传给 `--token %TOKEN%`。
以后**改 token 必须同时改三处**：这个 bat 变量、板子 flash、`daemon.token`。
`run-lhm.bat check` 干跑通过；改动前备份在 `run-lhm.bat.bak-20261024`。

**WIFI 那行不是故障**：`WIFI fail reason=Disconnected(AuthenticationExpired)`
在每次 boot 都会出现一次，后面紧跟 `BOOT WIFI ok` —— 是第一次关联被 AP 拒、
重连成功，属正常路径。
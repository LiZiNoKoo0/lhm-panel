# 固件任务单(handoff)

> 这里放**给实现方**的任务卡。每张卡 = 一个可验收的里程碑,不是半个系统。
>
> 规矩三条(分工会散架的三个地方,先钉死):
>
> 1. **验收由验收方跑,不看"我写完了"。** 每张卡都带可执行命令;命令跑不出预期结果就是没完成,不管代码多漂亮。
> 2. **改行为必须同时改 `SPEC.md`。** 代码与文档不一致时,要么改代码,要么改文档并写明理由。禁止"文档是文档,代码是代码"。
> 3. **一张卡一张单,不许顺手扩大。** 卡外的改动(重构、优化、加功能)一律另开一张卡。

状态:`TODO` / `DOING` / `REVIEW` / `DONE`

| 卡 | 目标 | 状态 |
|---|---|---|
| T1 | 依赖通关(只加依赖,不写业务) | DONE |
| T2 | 堆 + 时钟(毫秒)+ heap 自检 | DONE |
| T3 | AP 配网网页 + 凭据落盘 | TODO |
| T4 | sector store A/B(掉电可回退) | TODO |
| T5 | TCP 链路 + `hello` + 断线退避 | TODO |
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


## T4 · sector store A/B

- 两扇区 + `magic/ver/seq/crc32/len` 头部(SPEC §6)
- 验收:写入→断电→读回一致;**写到一半断电**(写循环里 `esp_hal::system::software_reset`)能回退到上一份
- 前提:T3 的 `store.rs` 接口(`load`/`save`)不变,只换内部为 A/B + `seq` 切换;`creds::encode/decode` 不动

## T5 · TCP 链路 + hello

- `TcpLink` 实现 `panel_core::link::Transport`
- 发 §5.1 的 hello 原文;15s 无数据 → `LINK LOST`;退避 1/2/4/8/10
- 验收:连 `fake_daemon.py` 后串口能看到 daemon 的 `catalog`;拔网线 15s 出角标

## T6 · ui/data + 布局持久化

- 节点表 ≤32、二进制存 flash(SPEC §6 S-6)、`ver` 用 `!=`(D-34)
- 验收:拔电重开,布局还在、值全 `--`;`term_tk.py` 与板子同源同画面

## T7 · 折线

- `line` 节点 + 设备端环形缓冲(协议 §8)+ 脏矩形合并推屏(D-39..D-43)
- 验收:1Hz 曲线滚动;拔掉 PC 曲线不消失;单次 blit ≤4ms

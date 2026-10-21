# 固件任务单（handoff）

> 这里放**给实现方**的任务卡。每张卡 = 一个可验收的里程碑，不是半个系统。
>
> 规矩三条（分工会散架的三个地方，先钉死）：
>
> 1. **验收由验收方跑，不看"我写完了"。** 每张卡都带可执行命令；命令跑不出预期结果就是没完成，不管代码多漂亮。
> 2. **改行为必须同时改 `SPEC.md`。** 代码与文档不一致时，要么改代码，要么改文档并写明理由。禁止"文档是文档，代码是代码"。
> 3. **一张卡一张单，不许顺手扩大。** 卡外的改动（重构、优化、加功能）一律另开一张卡。

状态：`TODO` / `DOING` / `REVIEW` / `DONE`

| 卡 | 目标 | 状态 |
|---|---|---|
| T1 | 依赖通关（只加依赖，不写业务） | DONE |
| T2 | 堆 + 时钟（systimer 毫秒）+ heap 自检 | TODO |
| T3 | AP 配网网页 + 凭据落盘 | TODO |
| T4 | sector store A/B（掉电可回退） | TODO |
| T5 | TCP 链路 + `hello` + 断线退避 | TODO |
| T6 | `ui`/`data` + 布局持久化（`ver` 用 `!=`） | TODO |
| T7 | `line` 折线 + 脏矩形推屏 | TODO |

---

## T1 · 依赖通关

**目标**：把整条 WiFi 技术栈的依赖关系一次性钉死。**一行业务代码都不写。**

这一卡的唯一价值是：把"版本互相不兼容"这个最贵的坑，在写任何逻辑之前踩完。

### 输入（已实测确认过的版本矩阵，别自己换）

`firmware/st7735-text/Cargo.toml` 的 `[dependencies]` 追加：

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

`esp-hal` **保持 `=1.1.2` 不动**（1.2.x 要 `esp-sync 0.3`，那个需要 esp 工具链没有的 nightly 特性）。

### 为什么是这些名字（踩过的坑，别重踩）

| 坑 | 事实 |
|---|---|
| `esp-wifi` | **是上一代名字**，停在 0.15（要 `esp-alloc 0.8`）。对应 `esp-hal 1.1.2` 的继任者是 **`esp-radio 0.18`** |
| `esp-storage` | 必须 **0.9.0**（要求 `esp-hal ~1.1`）。0.10.0 要 `esp-hal ~1.2`，装不上 |
| `esp-alloc` | 默认特性已含 `global-allocator`，不需要手动开 |
| `esp-rtos` | 必须同时开 `embassy`（带 embassy-executor 0.10 + 时间驱动）、`esp-alloc`、`esp32` 三个特性，否则 WiFi 任务起不来 |
| `picoserve`（T3 用，本卡先不加） | 0.20.1 要 `embassy-net ^0.9.1`，接得上；它自己的 feature 名等 T3 再确认 |

### 交付

1. `Cargo.toml` 更新 + `Cargo.lock` 更新。
2. `cargo build --release` **通过且 0 warning**。
3. **不改任何行为**：不打印新行、不动 `scene`/`canvas`/`panel`/`diag`。

### 验收（验收方执行）

```powershell
# 板子不在时（现在的状态）先跑这两条：
Set-Location H:\newwork\firmware\st7735-text
. D:\esp-toolchain\esp-xtensa-env.ps1
cargo build --release | Select-String 'warning'    # 必须零输出
Set-Location H:\newwork\firmware\panel-core; cargo test   # 必须 14/14 绿

# 板子插回来后补做硬件验收：
powershell -ExecutionPolicy Bypass -File firmware\flash.ps1
python firmware\check_parity.py --port COM10       # 必须 PARITY OK，CRC 与阶段1 相同
```

启动横幅必须与阶段1 **逐字节相同**（`panel-core` 的 `diag_format` 测试已把它钉死，跑 `cargo test` 即可复核）。

### 禁止

- ❌ 不写任何 WiFi / socket / flash 逻辑
- ❌ 不改启动横幅（多一行也算改行为）
- ❌ 不动 `esp-hal` 版本、不升级其他 crate
- ❌ 不引入 `std`
- ❌ 遇到编译错误就"降级某个 crate"——先看是不是漏了 feature，改版本必须先问

### 已知风险

`esp-radio` + `esp-rtos` 会拉进链接脚本/中断相关的符号。若出现 `undefined reference` 或 `esp-hal/rt` 相关错误，**先看 `esp-radio` 文档里对 `esp-hal` 特性的要求**（它自己会开 `esp-hal/rt` 与 `unstable`），不要急着改 `[features]`。

估时：0.5 天（时间基本花在 feature 名上，不在写代码上）。

### 结果（验收通过）

实现方遇到两个坑，都不是版本问题，已写进 `SPEC.md` §2.1：

1. `build-std` 原为 `["core"]`，`esp-alloc` 一带就报 `can't find crate for alloc` → 改为 `["core", "alloc"]`。
   `alloc` 是 no_std 合法子集，不算引入 `std`。**这是任务卡漏写的一项，不是越权。**
2. `esp-println` / `esp-storage` 必填芯片 feature，裸版本串会被拒 → 补 `features = ["esp32"]`。

验收记录：`cargo build --release` 强制重编后 **0 warning**；`panel-core` **14/14** 测试绿；
ELF 从 1 871 296 → 1 872 104 字节（+808，未被引用的依赖被 LTO 丢掉了，说明没把 `std` 拉进来）。
**硬件验收（烧录 + `PARITY OK`）待板子插回后补做。**

---

## T2 · 堆 + 时钟

**目标**：allocator 起来、毫秒时钟可用，并把这两个能力用一行日志证明。

- 用 `esp-alloc` 建一个静态堆（建议 64KB，SPEC §5 预算）
- 单调毫秒：systimer 读数，**上电从 0 开始**
- 启动横幅**追加一行**：`heap: 65536 free=NNNNN  clock: 0ms`
- 验收：`cargo test` 全绿；横幅多出这一行且数值合理；`PARITY OK` 仍成立

> 注意：这一卡**允许**改横幅（新增一行，不改已有行的内容）。改的同时更新 `panel-core/tests/diag_format.rs`。

---

## T3 · AP 配网网页 + 凭据落盘

- `esp-radio` STA+AP 并存；连不上路由器 **30s 自动开热点**（D12）
- `picoserve` 提供一页：扫到 SSID 列表 + 密码框 + 保存
- 保存后写 store，重启自动连；连上串口打 `WIFI up ip=...`
- 验收：手机连热点能打开页面；填密码→重启自动连；改错密码→30s 回热点

## T4 · sector store A/B

- 两扇区 + `magic/ver/seq/crc32/len` 头部（SPEC §6）
- 验收：写入→断电→读回一致；**写到一半断电**（写循环里 `esp_hal::reset::software_reset`）能回退到上一份

## T5 · TCP 链路 + hello

- `TcpLink` 实现 `panel_core::link::Transport`
- 发 §5.1 的 hello 原文；15s 无数据 → `LINK LOST`；退避 1/2/4/8/10
- 验收：连 `fake_daemon.py` 后串口能看到 daemon 的 `catalog`；拔网线 15s 出角标

## T6 · ui/data + 布局持久化

- 节点表 ≤32、二进制存 flash（SPEC §6 S-6）、`ver` 用 `!=`（D-34）
- 验收：拔电重开，布局还在、值全 `--`；`term_tk.py` 与板子同源同画面

## T7 · 折线

- `line` 节点 + 设备端环形缓冲（协议 §8）+ 脏矩形合并推屏（D-39..D-43）
- 验收：1Hz 曲线滚动；拔掉 PC 曲线不消失；单次 blit ≤4ms

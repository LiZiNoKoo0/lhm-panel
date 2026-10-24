# PC-Monitor

一台电脑（daemon）把系统/游戏数据推给**任意**显示终端（ESP32、树莓派、手机网页、桌面窗口）。

**核心契约是 `PROTOCOL.md`。** 固件和客户端随便写，只要遵守它就能当显示器。

```
插件(CPU/天气/音乐) ──TCP 7801──▶ daemon ──TCP 7802──▶ 终端们
                                    │
                                    └──HTTP 7803──▶ 管理界面
```

## 里程碑（按顺序做，每步可独立验收）

| # | 交付物 | 验收 |
|---|---|---|
| M1 | `pcmon/protocol.py` | 编解码往返测试通过、坏消息被拒 |
| M2 | `fake_daemon.py` | 终端连上能看到画面，数字在动 |
| M3 | `term_tk.py`（桌面模拟终端） | 窗口里画出 rect/path/text/num，数值实时更新 |
| M4 | `replay.py`（离线回放器） | 用 M2 录的流量喂 M3，表现一致 |
| M5 | 真采集插件 + Go/Python daemon 主体 | CPU/内存/GPU 数字和任务管理器对得上 |
| M6 | ESP32 固件（thin 终端） | 屏上出现同样的画面 |
| P1 | `preview_st7735.py`（ST7735 160×80 预览） | 一行 8×16 字按面板真实像素显示，可导出 RGB565 裸字节 |
| P2 | 上位机 Web UI（`pcmon/webui.py` + `pcmon/webstatic/`） | 浏览器看到 KPI / 实时曲线 / 160×80 预览 / 指标表；`LINK LOST` 15s 出角标 |

> **固件侧进度（2026-10-22）**：`firmware/` 下 T1–T4 已完成并上板验过（面板、链路、配置页 + WiFi、A/B 掉电安全存储），T5 未开始。
> 接手前先读 **`firmware/HANDOVER.md`**（板子状态、bench 开关、未解决项），再看 `firmware/BACKLOG.md`（每张卡的任务与踩坑记录）。

**先做 M1 → M2 → M3，这三步跑通，协议就算验证过了。**

## 上位机 Web UI（2026-10-22）

浏览器仪表盘。**它是一个 `thick` 终端**：走和板子完全相同的 TCP/NDJSON（7802），收 `catalog`/`ui`/`data`，再把状态用 HTTP + SSE 转给页面。daemon 不需要任何改动。

```powershell
# 先起 daemon（另开一个窗口）
python fake_daemon.py --source lhm --port 7802
# 再起页面（读同目录的 daemon.token，daemon 重启换 token 会自动重读）
python -m pcmon.webui --port 7803 --open
# 检查页面是否还符合 Dashboard Layout 风格规范
python tools/check_dashboard_style.py
```

三点要记住：

1. **不引 Tailwind CDN**。`tokens.css` 只实现本页用到的 token 子集，类名与风格提示词逐字一致；**新加类必须同时加进 CSS**，否则类名在 HTML 里静默失效。
2. **偏离规范一处（已记录）**：风格提示词要求搜索框在顶栏，实际放在“指标目录”卡片头部——只有一个列表可搜，顶栏摆“全局搜索”是骗人的。`check_dashboard_style.py` 里写明了这条。
3. **浏览器不能开裸 TCP**，所以中间必须有 `webui.py` 这层桥；将来 daemon 换成真实现（M5 完成态）也不用改。

## 目录约定（待定，由实现决定）

```
newwork/
  PROTOCOL.md          协议（唯一的真实来源）
  TASKS.md             任务卡
  pcmon/protocol.py    协议库
  fake_daemon.py       假数据 daemon
  term_tk.py           桌面模拟终端
  preview_st7735.py    ST7735 160×80 预览（一行点阵字）
  pcmon/font8x16.py    8×16 点阵字模（预览与固件共用同一份）
  replay.py            回放器
  pcmon/webui.py       上位机 Web UI（HTTP + SSE 桥，自己也是 thick 终端）
  pcmon/webstatic/     页面：index.html / app.js / tokens.css
  tools/check_dashboard_style.py  Dashboard Layout 风格自检（禁用词 + token 链 + 未定义类）
  recordings/          daemon 录制的 NDJSON
```

## 铁律

1. **协议优先于实现**。实现跟协议冲突，改实现。
2. **终端零语义**。终端不知道"CPU"是什么，只知道"有个 num 绑在 cpu.total"。
3. **缺数据显示 `--`**，绝不显示上一次的值。
4. **未知字段/未知 type/未知 t 一律忽略**，靠这个向前兼容。
5. **PC 端按 `hello` 的能力裁剪布局**，永远不要假设终端什么都能画。

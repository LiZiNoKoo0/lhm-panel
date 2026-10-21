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

**先做 M1 → M2 → M3，这三步跑通，协议就算验证过了。**

## 目录约定（待定，由实现决定）

```
newwork/
  PROTOCOL.md          协议（唯一的真实来源）
  TASKS.md             任务卡
  pcmon/protocol.py    协议库
  fake_daemon.py       假数据 daemon
  term_tk.py           桌面模拟终端
  replay.py            回放器
  recordings/          daemon 录制的 NDJSON
```

## 铁律

1. **协议优先于实现**。实现跟协议冲突，改实现。
2. **终端零语义**。终端不知道"CPU"是什么，只知道"有个 num 绑在 cpu.total"。
3. **缺数据显示 `--`**，绝不显示上一次的值。
4. **未知字段/未知 type/未知 t 一律忽略**，靠这个向前兼容。
5. **PC 端按 `hello` 的能力裁剪布局**，永远不要假设终端什么都能画。

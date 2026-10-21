# lhm_bridge

把 **LibreHardwareMonitor** 的传感器读出来，转成 NDJSON 打到 stdout，给 Python 那边的 daemon 用。

## 为什么不直接用 LibreHardwareMonitor.exe

它自带的 HTTP 服务器（`:8085/data.json`）有两个麻烦：

1. 那个开关**只能在 GUI 里点**（Options → Remote Web Server → Run），配置项藏在 .NET 的 `user.config` 里，写文件不生效——试过，写错会让它直接启动失败。
2. 它是个 WinForms 程序，要管理员权限，窗口/托盘常驻，跟"后台服务"的气质不合。

所以这里改成**直接引用它的库**（`LibreHardwareMonitorLib.dll`），传感器数据完全一样，但只是一个没有界面的控制台进程。

## 构建

需要先有 LibreHardwareMonitor 解压包（[releases](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases)，选 `LibreHardwareMonitor.zip`）。

```powershell
powershell -ExecutionPolicy Bypass -File build.ps1                       # 默认找 H:\tools\lhm
powershell -ExecutionPolicy Bypass -File build.ps1 -LhmDir 'D:\lhm'
```

只用 .NET Framework 4 自带的 `csc.exe`，不用装 SDK。

**必须编译到 LibreHardwareMonitor 的目录里**（默认就是），因为运行时要用同目录下的 `LibreHardwareMonitorLib.dll`、`HidSharp.dll`、`RAMSPDToolkit-NDD.dll` 等。

## 运行

```
lhm_bridge.exe [--interval=1000]
```

 stdout 每秒一行 JSON 数组：

```json
[{"id":"/intelcpu/0/load/0","path":"Intel Xeon E3-1226 v3 / CPU Total",
  "name":"CPU Total","hw":"Intel Xeon E3-1226 v3","hwtype":"Cpu",
  "kind":"Load","value":37.5,"unit":"%"}, ...]
```

| 字段 | 含义 |
|---|---|
| `id` | LHM 的传感器标识，稳定，可作主键 |
| `path` | 完整路径 `硬件 / 子硬件 / 传感器名` |
| `kind` | `Load` / `Temperature` / `Fan` / `Throughput` / `Data` / `SmallData` / `Clock` / `Voltage` / `Power` |
| `value` / `unit` | 数值和单位（`%` `C` `RPM` `V` `W` `MHz` `GB` `MB` `B/s`） |

无效的传感器（`Value` 为 null / NaN）**不输出**，不是输出 null。

## 注意

- **CPU 温度要管理员权限**（LHM 要装 ring0 驱动）。普通权限下 CPU 负载/内存/网络/硬盘都正常，但温度、风扇可能缺失。
- 第一次运行会加载内核驱动，慢一两秒。
- 它**不写文件、不开端口、不读配置**，唯一的输出就是 stdout。daemon 挂掉它也就跟着被回收最省事。

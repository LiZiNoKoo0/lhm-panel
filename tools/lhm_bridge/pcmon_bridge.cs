// pcmon_bridge.cs - publish LibreHardwareMonitor sensors as NDJSON on stdout.
//
// One JSON object per line, every second:
//   {"id":"/intelcpu/0/temperature/1","path":"CPU / Core #1","name":"Core #1",
//    "kind":"Temperature","value":57.0,"unit":"C","hw":"CPU","type":"Cpu"}
//
// This program deliberately does NOT: write files, listen on sockets, read
// config, or touch the GUI. It exists only so the Python daemon can get real
// sensors without being able to load a .NET assembly.
//
// Build:
//   csc /nologo /target:exe /out:lhm_bridge.exe /r:LibreHardwareMonitorLib.dll pcmon_bridge.cs

using System;
using System.Collections.Generic;
using System.Globalization;
using System.Text;
using LibreHardwareMonitor.Hardware;

internal sealed class UpdateVisitor : IVisitor
{
    public void VisitComputer(IComputer computer) { computer.Traverse(this); }

    public void VisitHardware(IHardware hardware)
    {
        hardware.Update();
        foreach (IHardware sub in hardware.SubHardware) sub.Accept(this);
    }

    public void VisitSensor(ISensor sensor) { }
    public void VisitParameter(IParameter parameter) { }
}

internal static class Program
{
    private static string Esc(string s)
    {
        if (s == null) return "";
        var sb = new StringBuilder(s.Length + 8);
        foreach (char c in s)
        {
            switch (c)
            {
                case '"': sb.Append("\\\""); break;
                case '\\': sb.Append("\\\\"); break;
                case '\n': sb.Append("\\n"); break;
                case '\r': sb.Append("\\r"); break;
                case '\t': sb.Append("\\t"); break;
                default:
                    if (c < 32) sb.Append(' ');
                    else sb.Append(c);
                    break;
            }
        }
        return sb.ToString();
    }

    private static void Walk(IHardware hw, string parent, StringBuilder sb,
                             ref bool first, ref int count)
    {
        string root = string.IsNullOrEmpty(parent) ? hw.Name : parent + " / " + hw.Name;
        foreach (ISensor s in hw.Sensors)
        {
            if (!s.Value.HasValue) continue;
            double v = s.Value.Value;
            if (double.IsNaN(v) || double.IsInfinity(v)) continue;
            if (!first) sb.Append(',');
            first = false;
            count++;
            sb.Append("{\"id\":\"").Append(Esc(s.Identifier.ToString()))
              .Append("\",\"path\":\"").Append(Esc(root + " / " + s.Name))
              .Append("\",\"name\":\"").Append(Esc(s.Name))
              .Append("\",\"hw\":\"").Append(Esc(hw.Name))
              .Append("\",\"hwtype\":\"").Append(hw.HardwareType.ToString())
              .Append("\",\"kind\":\"").Append(s.SensorType.ToString())
              .Append("\",\"value\":")
              .Append(v.ToString("0.###", CultureInfo.InvariantCulture))
              .Append(",\"unit\":\"");
            switch (s.SensorType)
            {
                case SensorType.Temperature: sb.Append("C"); break;
                case SensorType.Load: sb.Append("%"); break;
                case SensorType.Fan: sb.Append("RPM"); break;
                case SensorType.Voltage: sb.Append("V"); break;
                case SensorType.Power: sb.Append("W"); break;
                case SensorType.Clock: sb.Append("MHz"); break;
                case SensorType.Data: sb.Append("GB"); break;
                case SensorType.SmallData: sb.Append("MB"); break;
                case SensorType.Throughput: sb.Append("B/s"); break;
                default: sb.Append(""); break;
            }
            sb.Append("\"}");
        }
        foreach (IHardware sub in hw.SubHardware) Walk(sub, root, sb, ref first, ref count);
    }

    private static int Main(string[] args)
    {
        int intervalMs = 1000;
        foreach (string a in args)
        {
            if (a.StartsWith("--interval="))
            {
                int parsed;
                if (int.TryParse(a.Substring(11), out parsed) && parsed >= 200)
                    intervalMs = parsed;
            }
        }

        Console.OutputEncoding = new UTF8Encoding(false);
        var computer = new Computer
        {
            IsCpuEnabled = true,
            IsGpuEnabled = true,
            IsMemoryEnabled = true,
            IsMotherboardEnabled = true,
            IsControllerEnabled = true,
            IsNetworkEnabled = true,
            IsStorageEnabled = true
        };
        computer.Open();
        var visitor = new UpdateVisitor();
        var sb = new StringBuilder(65536);

        var clock = System.Diagnostics.Stopwatch.StartNew();
        while (true)
        {
            clock.Restart();
            computer.Accept(visitor);
            sb.Length = 0;
            sb.Append('[');
            bool first = true;
            int count = 0;
            foreach (IHardware hw in computer.Hardware)
                Walk(hw, null, sb, ref first, ref count);
            sb.Append(']');
            Console.Out.WriteLine(sb.ToString());
            Console.Out.Flush();
            int remain = intervalMs - (int)clock.ElapsedMilliseconds;
            if (remain > 0) System.Threading.Thread.Sleep(remain);
        }
    }
}

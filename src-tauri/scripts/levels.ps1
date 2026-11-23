$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$src = @'
using System;
using System.Globalization;
using System.Runtime.InteropServices;
[Guid("C02216F6-8C67-4B5B-9D00-D008E73E0064"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IAudioMeterInformation {
  int GetPeakValue(out float peak);
  int GetMeteringChannelCount(out uint n);
  int GetChannelsPeakValues(uint count, [In, Out, MarshalAs(UnmanagedType.LPArray, SizeParamIndex = 0)] float[] peaks);
  int QueryHardwareSupport(out uint mask);
}
[Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMMDevice {
  int Activate(ref Guid iid, uint clsCtx, IntPtr activationParams, [MarshalAs(UnmanagedType.IUnknown)] out object iface);
}
[Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMMDeviceEnumerator {
  int EnumAudioEndpoints(int flow, int mask, out IntPtr devices);
  int GetDefaultAudioEndpoint(int flow, int role, out IMMDevice endpoint);
}
[ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class MMDeviceEnumeratorComObject { }
public class Meter {
  public static Func<string> Extra;   // спектр (64 числа) от Spec.Cap, если захват звука запустился
  static IAudioMeterInformation Get() {
    IMMDeviceEnumerator en = (IMMDeviceEnumerator)(new MMDeviceEnumeratorComObject());
    IMMDevice dev;
    Marshal.ThrowExceptionForHR(en.GetDefaultAudioEndpoint(0, 1, out dev));
    Guid g = typeof(IAudioMeterInformation).GUID;
    object o;
    Marshal.ThrowExceptionForHR(dev.Activate(ref g, 23, IntPtr.Zero, out o));
    return (IAudioMeterInformation)o;
  }
  public static void Run() {
    IAudioMeterInformation m = null;
    int tick = 0;
    System.IO.TextWriter o = Console.Out;
    while (true) {
      string line;
      try {
        // устройство вывода могло смениться (наушники, Bluetooth) — переподключаемся раз в ~2 секунды
        if (m == null || (tick++ % 50) == 0) m = Get();
        float[] p = new float[2];
        float l = 0, r = 0;
        try { Marshal.ThrowExceptionForHR(m.GetChannelsPeakValues(2, p)); l = p[0]; r = p[1]; }
        catch { float v; Marshal.ThrowExceptionForHR(m.GetPeakValue(out v)); l = v; r = v; }
        line = string.Format(CultureInfo.InvariantCulture, "{0:0.000} {1:0.000}", l, r);
        if (Extra != null) { try { string ex = Extra(); if (ex != null && ex.Length > 0) line = line + " " + ex; } catch { } }
      } catch (Exception ex) { m = null; line = "0.000 0.000 E" + ex.HResult.ToString("X"); }
      try { o.WriteLine(line); } catch { return; }   // труба закрыта (остров закрыт) — выходим, чтобы не висеть в фоне
      System.Threading.Thread.Sleep(40);
    }
  }
}
'@
$spec = @'
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Threading;
namespace Spec {
  [Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IDev {
    int Activate(ref Guid iid, uint clsCtx, IntPtr p, [MarshalAs(UnmanagedType.IUnknown)] out object o);
    int OpenPropertyStore(int access, out IntPtr props);
    int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
  }
  [Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IEnum {
    int EnumAudioEndpoints(int flow, int mask, out IntPtr devices);
    int GetDefaultAudioEndpoint(int flow, int role, out IDev endpoint);
  }
  [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class EnumCom { }
  [Guid("C02216F6-8C67-4B5B-9D00-D008E73E0064"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IMeter { int GetPeakValue(out float peak); }
  [Guid("1CB9AD4C-DBFA-4c32-B178-C2F568A703B2"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IClient {
    int Initialize(int shareMode, int flags, long hnsBuffer, long hnsPeriod, IntPtr fmt, IntPtr session);
    int GetBufferSize(out uint n);
    int GetStreamLatency(out long latency);
    int GetCurrentPadding(out uint padding);
    int IsFormatSupported(int shareMode, IntPtr fmt, out IntPtr closest);
    int GetMixFormat(out IntPtr fmt);
    int GetDevicePeriod(out long defaultPeriod, out long minimumPeriod);
    int Start();
    int Stop();
    int Reset();
    int SetEventHandle(IntPtr handle);
    int GetService(ref Guid iid, [MarshalAs(UnmanagedType.IUnknown)] out object o);
  }
  [Guid("C8ADBD64-E71E-48A0-A4DE-185C395CD317"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface ICap {
    int GetBuffer(out IntPtr data, out uint frames, out uint flags, out ulong devicePos, out ulong qpcPos);
    int ReleaseBuffer(uint frames);
    int GetNextPacketSize(out uint frames);
  }
  public static class Cap {
    static volatile byte[] bins = new byte[64];
    static volatile bool active = false;
    static Thread th;
    static double[] win = new double[128];
    static double[] cs = new double[128];
    static double[] sn = new double[128];
    public static Func<string> Provider = Line;

    [DllImport("ole32.dll")] static extern int CoInitializeEx(IntPtr pv, uint coInit);
    [DllImport("ole32.dll")] static extern void CoUninitialize();

    public static void Start() {
      if (th != null && th.IsAlive) return;
      for (int i = 0; i < 128; i++) {
        win[i] = 0.42 - 0.5 * Math.Cos(2.0 * Math.PI * i / 127.0) + 0.08 * Math.Cos(4.0 * Math.PI * i / 127.0);
        cs[i] = Math.Cos(2.0 * Math.PI * i / 128.0);
        sn[i] = Math.Sin(2.0 * Math.PI * i / 128.0);
      }
      th = new Thread(Loop); th.IsBackground = true; th.Name = "Nexora WASAPI spectrum"; th.Start();
    }
    public static string Line() {
      if (!active) return "";
      byte[] b = bins;
      System.Text.StringBuilder sb = new System.Text.StringBuilder(260);
      for (int i = 0; i < b.Length; i++) { if (i > 0) sb.Append(' '); sb.Append((int)b[i]); }
      return sb.ToString();
    }
    static void Release(object o) { try { if (o != null && Marshal.IsComObject(o)) Marshal.FinalReleaseComObject(o); } catch { } }
    static void Loop() {
      while (true) {
        bool co = false;
        try { int hr = CoInitializeEx(IntPtr.Zero, 0); Marshal.ThrowExceptionForHR(hr); co = true; Run(); }
        catch { active = false; bins = new byte[64]; }
        finally { if (co) { try { CoUninitialize(); } catch { } } }
        active = false;
        Thread.Sleep(1000);
      }
    }
    static float Sample(IntPtr data, int sampleIndex, int bits, bool isFloat) {
      int off = sampleIndex * (bits / 8);
      if (isFloat) return BitConverter.ToSingle(BitConverter.GetBytes(Marshal.ReadInt32(data, off)), 0);
      if (bits == 8) return (Marshal.ReadByte(data, off) - 128) / 128f;
      if (bits == 16) return Marshal.ReadInt16(data, off) / 32768f;
      if (bits == 24) {
        int v = Marshal.ReadByte(data, off) | (Marshal.ReadByte(data, off + 1) << 8) | (Marshal.ReadByte(data, off + 2) << 16);
        if ((v & 0x800000) != 0) v |= unchecked((int)0xFF000000);
        return v / 8388608f;
      }
      return Marshal.ReadInt32(data, off) / 2147483648f;
    }
    static void Run() {
      IEnum en = null; IDev dev = null; IClient cl = null; ICap cap = null; object o = null; object co = null; IntPtr fmt = IntPtr.Zero; object mo = null; IMeter mtr = null;
      try {
        en = (IEnum)(new EnumCom());
        Marshal.ThrowExceptionForHR(en.GetDefaultAudioEndpoint(0, 1, out dev));
        string myId = ""; try { dev.GetId(out myId); } catch { }
        Guid ig = new Guid("1CB9AD4C-DBFA-4c32-B178-C2F568A703B2");
        Marshal.ThrowExceptionForHR(dev.Activate(ref ig, 23, IntPtr.Zero, out o));
        cl = (IClient)o;
        Marshal.ThrowExceptionForHR(cl.GetMixFormat(out fmt));
        int tag = Marshal.ReadInt16(fmt, 0) & 0xFFFF;
        int ch = Marshal.ReadInt16(fmt, 2);
        int bits = Marshal.ReadInt16(fmt, 14);
        int subtype = tag;
        if (tag == 0xFFFE) subtype = Marshal.ReadInt32(fmt, 24);
        bool isFloat = subtype == 3;
        bool isPcm = subtype == 1;
        bool okFmt = ch >= 1 && ch <= 8 && ((isFloat && bits == 32) || (isPcm && (bits == 8 || bits == 16 || bits == 24 || bits == 32)));
        if (!okFmt) throw new InvalidOperationException("Unsupported WASAPI mix format tag=" + tag + " subtype=" + subtype + " bits=" + bits + " channels=" + ch);
        int hr = cl.Initialize(0, 0x00020000, 10000000L, 0L, fmt, IntPtr.Zero);
        Marshal.FreeCoTaskMem(fmt); fmt = IntPtr.Zero; Marshal.ThrowExceptionForHR(hr);
        Guid cg = new Guid("C8ADBD64-E71E-48A0-A4DE-185C395CD317");
        Marshal.ThrowExceptionForHR(cl.GetService(ref cg, out co)); cap = (ICap)co;
        Marshal.ThrowExceptionForHR(cl.Start());
        try { Guid mg = new Guid("C02216F6-8C67-4B5B-9D00-D008E73E0064"); if (dev.Activate(ref mg, 23, IntPtr.Zero, out mo) == 0) mtr = (IMeter)mo; } catch { mtr = null; }
        float[] ring = new float[128]; int rp = 0; float[] sm = new float[64];
        Stopwatch sw = Stopwatch.StartNew(); long lastCalc = 0, lastDev = 0, lastPacket = 0, lastChk = 0;
        while (true) {
          Thread.Sleep(8);
          uint pk; Marshal.ThrowExceptionForHR(cap.GetNextPacketSize(out pk));
          while (pk > 0) {
            lastPacket = sw.ElapsedMilliseconds;
            IntPtr data; uint frames, flags; ulong pos, qpc;
            Marshal.ThrowExceptionForHR(cap.GetBuffer(out data, out frames, out flags, out pos, out qpc));
            try {
              bool silent = (flags & 2) != 0;
              int bps = bits / 8;
              for (int i = 0; i < (int)frames; i++) {
                float v = 0f;
                if (!silent) { for (int c = 0; c < ch; c++) v += Sample(data, i * ch + c, bits, isFloat); v /= ch; }
                ring[rp] = v; rp = (rp + 1) & 127;
              }
            } finally { Marshal.ThrowExceptionForHR(cap.ReleaseBuffer(frames)); }
            Marshal.ThrowExceptionForHR(cap.GetNextPacketSize(out pk));
          }
          long now = sw.ElapsedMilliseconds;
          // Loopback при паузе может не отдавать пакеты вообще; очищаем кольцо, но не завершаем capture.
          if (now - lastPacket > 180) { for (int i = 0; i < 128; i++) ring[i] = 0f; }
          // громкомер устройства показывает звук, а loopback молчит больше 1.2 с — захват «застрял»: выходим, внешний цикл создаст его заново
          if (mtr != null && now - lastPacket > 1200 && now - lastChk > 500) {
            lastChk = now; float pv = 0f;
            if (mtr.GetPeakValue(out pv) == 0 && pv > 0.02f) throw new InvalidOperationException("loopback stalled");
          }
          if (now - lastCalc >= 33) {
            lastCalc = now; byte[] nb = new byte[64];
            for (int k = 0; k < 64; k++) {
              double re = 0.0, im = 0.0;
              for (int t = 0; t < 128; t++) { double x = ring[(rp + t) & 127] * win[t]; int ph = (k * t) & 127; re += x * cs[ph]; im -= x * sn[ph]; }
              double mag = Math.Sqrt(re * re + im * im) / 64.0;
              sm[k] = (float)(0.72 * sm[k] + 0.28 * mag);
              double db = 20.0 * Math.Log10(Math.Max((double)sm[k], 1e-12));
              double v2 = 255.0 * (db + 90.0) / 75.0;
              nb[k] = (byte)(v2 < 0.0 ? 0.0 : (v2 > 255.0 ? 255.0 : v2));
            }
            bins = nb; active = true;
          }
          if (now - lastDev > 3000) {
            lastDev = now; IDev d2 = null;
            if (en.GetDefaultAudioEndpoint(0, 1, out d2) == 0) {
              string id2 = ""; try { d2.GetId(out id2); } catch { }
              Release(d2); if (id2 != myId) break;
            }
          }
        }
      } finally {
        active = false;
        try { if (cl != null) cl.Stop(); } catch { }
        if (fmt != IntPtr.Zero) { try { Marshal.FreeCoTaskMem(fmt); } catch { } }
        Release(mtr); Release(mo); Release(cap); Release(co); Release(cl); Release(o); Release(dev); Release(en);
      }
    }
  }

}
'@
Add-Type -TypeDefinition $src
# спектр необязателен: если захват звука не удался, остаются пики по каналам (как раньше)
try {
  Add-Type -TypeDefinition $spec
  [Spec.Cap]::Start()
  [Meter]::Extra = [Spec.Cap]::Provider
} catch { }
[Meter]::Run()

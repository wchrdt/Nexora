$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$src = @'
using System;
using System.Runtime.InteropServices;
[Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IAudioEndpointVolume {
  int RegisterControlChangeNotify(IntPtr p);
  int UnregisterControlChangeNotify(IntPtr p);
  int GetChannelCount(out uint n);
  int SetMasterVolumeLevel(float db, ref Guid ctx);
  int SetMasterVolumeLevelScalar(float level, ref Guid ctx);
  int GetMasterVolumeLevel(out float db);
  int GetMasterVolumeLevelScalar(out float level);
  int SetChannelVolumeLevel(uint ch, float db, ref Guid ctx);
  int SetChannelVolumeLevelScalar(uint ch, float level, ref Guid ctx);
  int GetChannelVolumeLevel(uint ch, out float db);
  int GetChannelVolumeLevelScalar(uint ch, out float level);
  int SetMute([MarshalAs(UnmanagedType.Bool)] bool mute, ref Guid ctx);
  int GetMute([MarshalAs(UnmanagedType.Bool)] out bool mute);
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
public class AudioCtl {
  [DllImport("user32.dll")] static extern void keybd_event(byte v, byte s, uint f, UIntPtr e);
  static IAudioEndpointVolume Vol() {
    IMMDeviceEnumerator en = (IMMDeviceEnumerator)(new MMDeviceEnumeratorComObject());
    IMMDevice dev;
    Marshal.ThrowExceptionForHR(en.GetDefaultAudioEndpoint(0, 1, out dev));
    Guid g = typeof(IAudioEndpointVolume).GUID;
    object o;
    Marshal.ThrowExceptionForHR(dev.Activate(ref g, 23, IntPtr.Zero, out o));
    return (IAudioEndpointVolume)o;
  }
  public static float GetVol() { float v; Marshal.ThrowExceptionForHR(Vol().GetMasterVolumeLevelScalar(out v)); return v; }
  public static void SetVol(float v) { Guid c = Guid.Empty; Marshal.ThrowExceptionForHR(Vol().SetMasterVolumeLevelScalar(v, ref c)); }
  public static bool GetMute() { bool m; Marshal.ThrowExceptionForHR(Vol().GetMute(out m)); return m; }
  public static void SetMute(bool m) { Guid c = Guid.Empty; Marshal.ThrowExceptionForHR(Vol().SetMute(m, ref c)); }
  public static void Key(byte vk) { keybd_event(vk, 0, 0, UIntPtr.Zero); keybd_event(vk, 0, 2, UIntPtr.Zero); }
}
'@
Add-Type -TypeDefinition $src
$script:est = -1
$script:muteEst = 0
function Fallback-Set([int]$t) {
  if ($script:est -lt 0) { for ($i = 0; $i -lt 50; $i++) { [AudioCtl]::Key(174) }; $script:est = 0 }
  $d = [int][Math]::Round(($t - $script:est) / 2.0)
  for ($i = 0; $i -lt [Math]::Abs($d); $i++) { if ($d -gt 0) { [AudioCtl]::Key(175) } else { [AudioCtl]::Key(174) } }
  $script:est = [Math]::Max(0, [Math]::Min(100, $script:est + 2 * $d))
}
[Console]::Out.WriteLine('ready')
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($null -eq $line) { break }
  try {
    $p = $line.Trim().Split(' ')
    switch ($p[0]) {
      'set'  { try { [AudioCtl]::SetVol([single]([double]$p[1] / 100.0)) } catch { Fallback-Set ([int]$p[1]) } }
      'mute' { try { [AudioCtl]::SetMute(-not [AudioCtl]::GetMute()) } catch { [AudioCtl]::Key(173); $script:muteEst = 1 - $script:muteEst } }
      'key'  { [AudioCtl]::Key([byte][int]$p[1]) }
    }
    try {
      $v = [int][Math]::Round([AudioCtl]::GetVol() * 100)
      $m = [int][AudioCtl]::GetMute()
    } catch {
      if ($script:est -lt 0) { throw }
      $v = $script:est; $m = $script:muteEst
    }
    [Console]::Out.WriteLine("$v $m")
  } catch {
    [Console]::Out.WriteLine('err ' + ($_.Exception.Message -replace '\s+', ' '))
  }
}

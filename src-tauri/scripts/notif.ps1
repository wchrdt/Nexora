$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
# Постоянный процесс: читает системные уведомления Windows (Discord, Telegram) через UserNotificationListener.
# Команды (по строке в stdin):
#   poll            -> JSON {"st":"allowed|denied|unsupported|error","err":"","new":[{id,app,title,body,at}],"gone":[id,...]}
#   discord accept  -> открыть Discord и нажать Ctrl+Enter (принять звонок)
#   discord decline -> открыть Discord и нажать Esc (отклонить звонок), затем вернуть прежнее окно
Add-Type -AssemblyName System.Runtime.WindowsRuntime
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class NxWin {
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
}
'@
$script:ok = $true
$script:err = ''
try {
  $null = [Windows.UI.Notifications.Management.UserNotificationListener, Windows.UI.Notifications, ContentType = WindowsRuntime]
  $null = [Windows.UI.Notifications.UserNotification, Windows.UI.Notifications, ContentType = WindowsRuntime]
  $null = [Windows.UI.Notifications.NotificationKinds, Windows.UI.Notifications, ContentType = WindowsRuntime]
  $null = [Windows.UI.Notifications.KnownNotificationBindings, Windows.UI.Notifications, ContentType = WindowsRuntime]
} catch { $script:ok = $false; $script:err = [string]$_.Exception.Message }
$asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
  $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetGenericArguments().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -like 'IAsyncOperation*'
})[0]
function Await($op, $type) {
  $t = $asTask.MakeGenericMethod($type).Invoke($null, @($op))
  if (-not $t.Wait(5000)) { return $null }
  $t.Result
}
$script:listener = $null
$script:access = 'unknown'
$script:seen = @{}       # id -> true (уже отдавали)
$script:alive = @{}      # id на прошлом опросе
$script:first = $true
function Init-Listener {
  if (-not $script:ok) { return }
  try {
    $script:listener = [Windows.UI.Notifications.Management.UserNotificationListener]::Current
    $st = Await ($script:listener.RequestAccessAsync()) ([Windows.UI.Notifications.Management.UserNotificationListenerAccessStatus])
    $script:access = if ($null -eq $st) { 'error' } else { ([string]$st).ToLower() }
  } catch { $script:access = 'error'; $script:err = [string]$_.Exception.Message }
}
function Get-Poll {
  if ($null -eq $script:listener -or $script:access -ne 'allowed') {
    if ($script:ok) { Init-Listener }
  }
  if ($script:access -ne 'allowed') {
    return (@{ st = $script:access; err = $script:err; new = @(); gone = @() } | ConvertTo-Json -Compress -Depth 5)
  }
  $list = Await ($script:listener.GetNotificationsAsync([Windows.UI.Notifications.NotificationKinds]::Toast)) ([System.Collections.Generic.IReadOnlyList[Windows.UI.Notifications.UserNotification]])
  if ($null -eq $list) {
    return (@{ st = 'error'; err = 'нет ответа от системы уведомлений'; new = @(); gone = @() } | ConvertTo-Json -Compress -Depth 5)
  }
  $new = New-Object System.Collections.ArrayList
  $now = @{}
  foreach ($n in $list) {
    $id = [string]$n.Id
    $now[$id] = $true
    if ($script:seen.ContainsKey($id)) { continue }
    $script:seen[$id] = $true
    # уведомления, которые уже висели в центре действий при запуске, не показываем
    if ($script:first) { continue }
    try {
      $app = ''
      try { $app = [string]$n.AppInfo.DisplayInfo.DisplayName } catch {}
      $aumid = ''
      try { $aumid = [string]$n.AppInfo.AppUserModelId } catch {}
      $texts = @()
      try {
        $b = $n.Notification.Visual.GetBinding([Windows.UI.Notifications.KnownNotificationBindings]::ToastGeneric)
        if ($null -ne $b) { foreach ($t in $b.GetTextElements()) { $texts += [string]$t.Text } }
      } catch {}
      $hay = ($app + ' ' + $aumid).ToLower()
      $kind = ''
      if ($hay -match 'discord') { $kind = 'discord' }
      elseif ($hay -match 'telegram') { $kind = 'telegram' }
      if (-not $kind) { continue }
      $title = if ($texts.Count -gt 0) { $texts[0] } else { '' }
      $body = if ($texts.Count -gt 1) { ($texts[1..($texts.Count - 1)] -join "`n") } else { '' }
      [void]$new.Add(@{ id = $id; kind = $kind; app = $app; title = $title; body = $body; at = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() })
    } catch {}
  }
  $script:first = $false
  $gone = @()
  foreach ($k in @($script:alive.Keys)) { if (-not $now.ContainsKey($k)) { $gone += $k } }
  $script:alive = $now
  # не даём словарю расти бесконечно
  if ($script:seen.Count -gt 500) { $script:seen = @{}; foreach ($k in $now.Keys) { $script:seen[$k] = $true } }
  return (@{ st = 'allowed'; err = ''; new = @($new); gone = @($gone) } | ConvertTo-Json -Compress -Depth 6)
}
function Find-Discord {
  $p = Get-Process -Name 'Discord', 'DiscordPTB', 'DiscordCanary' -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne [IntPtr]::Zero } | Select-Object -First 1
  return $p
}
function Discord-Act([string]$what) {
  $prev = [NxWin]::GetForegroundWindow()
  # discord:// будит уже запущенный Discord и выводит его окно из трея
  try { Start-Process 'discord://-/channels/@me' } catch {}
  $p = $null
  for ($i = 0; $i -lt 20; $i++) {
    Start-Sleep -Milliseconds 150
    $p = Find-Discord
    if ($null -ne $p) { break }
  }
  if ($null -eq $p) { return 'нет окна Discord' }
  $h = $p.MainWindowHandle
  if ([NxWin]::IsIconic($h)) { [void][NxWin]::ShowWindow($h, 9) }
  [void][NxWin]::SetForegroundWindow($h)
  try { (New-Object -ComObject WScript.Shell).AppActivate([int]$p.Id) | Out-Null } catch {}
  Start-Sleep -Milliseconds 350
  if ($what -eq 'accept') { [System.Windows.Forms.SendKeys]::SendWait('^{ENTER}') }
  else {
    [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
    Start-Sleep -Milliseconds 250
    if ($prev -ne [IntPtr]::Zero -and $prev -ne $h) { [void][NxWin]::SetForegroundWindow($prev) }
  }
  return 'ok'
}
[Console]::Out.WriteLine('ready')
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($null -eq $line) { break }
  try {
    $p = $line.Trim().Split(' ')
    if ($p[0] -eq 'discord' -and $p.Length -gt 1) {
      $r = Discord-Act $p[1]
      [Console]::Out.WriteLine('discord ' + $r)
      continue
    }
    [Console]::Out.WriteLine((Get-Poll))
  } catch {
    [Console]::Out.WriteLine((@{ st = 'error'; err = [string]$_.Exception.Message; new = @(); gone = @() } | ConvertTo-Json -Compress))
  }
}

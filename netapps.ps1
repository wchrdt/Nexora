# Nexora: какие приложения используют интернет (list) и закрытие приложения (kill).
# NX_MODE = list | kill ; для kill: NX_PIDS = "123,456" (корневые процессы приложения из list), NX_NAME = имя exe
$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$mode = $env:NX_MODE

# --- все процессы ---
$parent = @{}; $pname = @{}; $ppath = @{}; $pcreate = @{}
Get-CimInstance Win32_Process | ForEach-Object {
  $i = [int]$_.ProcessId
  $parent[$i] = [int]$_.ParentProcessId
  $pname[$i] = [string]$_.Name
  $ppath[$i] = [string]$_.ExecutablePath
  $pcreate[$i] = $_.CreationDate
}
$children = @{}
foreach ($k in $parent.Keys) {
  $pp = [int]$parent[$k]
  if (-not $children.ContainsKey($pp)) { $children[$pp] = New-Object System.Collections.ArrayList }
  [void]$children[$pp].Add([int]$k)
}

function Name-Key([string]$n) { ($n -replace '\.exe$', '').ToLower() }

# --- «свои» процессы Nexora: сама Nexora, её служебные (WebView2, PowerShell, xray, zapret, tg-прокси) ---
# приложения, которые пользователь запустил из острова, сюда НЕ входят: они показываются как обычные
$nexora = [int]$parent[[int]$PID]
$exeDir = ''
if ($ppath[$nexora]) { $exeDir = [System.IO.Path]::GetDirectoryName($ppath[$nexora]) }
$dataDir = Join-Path $env:APPDATA 'Nexora'
$infraNames = @('msedgewebview2','powershell','pwsh','conhost','reg','openconsole')
function Is-Infra([int]$id) {
  $n = Name-Key $pname[$id]
  if ($infraNames -contains $n) { return $true }
  $p = $ppath[$id]
  if ($p) {
    if ($exeDir -and $p.StartsWith($exeDir, [StringComparison]::OrdinalIgnoreCase)) { return $true }
    if ($p.StartsWith($dataDir, [StringComparison]::OrdinalIgnoreCase)) { return $true }
  }
  return $false
}
$mine = @{}
$mine[$nexora] = $true; $mine[[int]$PID] = $true
$queue = New-Object System.Collections.Queue
$queue.Enqueue($nexora)
while ($queue.Count -gt 0) {
  $cur = [int]$queue.Dequeue()
  if ($children.ContainsKey($cur)) {
    foreach ($c in $children[$cur]) {
      if (-not $mine.ContainsKey($c) -and (Is-Infra $c)) { $mine[$c] = $true; $queue.Enqueue($c) }
    }
  }
}

# системные процессы и оболочки: показываем, но закрыть из острова нельзя / выше них не поднимаемся
$protected = @('system','idle','registry','smss','csrss','wininit','services','lsass','winlogon','svchost','dwm','fontdrvhost',
  'explorer','spoolsv','msmpeng','nissrv','securityhealthservice','sihost','taskhostw','ctfmon','runtimebroker','searchhost',
  'startmenuexperiencehost','shellexperiencehost','wudfhost','audiodg','lsm','conhost','memory compression','wlanext','dashost',
  'cmd','powershell','pwsh','windowsterminal','wt','openconsole','userinit','applicationframehost')

# корень приложения: у Chrome/Discord/Telegram/Steam сокеты держит служебный дочерний процесс (сетевая служба, helper),
# а закрывать нужно главный процесс. Поднимаемся к родителю, если он того же приложения (то же имя) или это helper/webview/crashpad.
function Get-Root([int]$id) {
  $guard = 0
  while ($guard -lt 12) {
    $guard++
    $par = [int]$parent[$id]
    if ($par -le 4 -or -not $pname.ContainsKey($par) -or $par -eq $id) { break }
    if ($mine.ContainsKey($par)) { break }
    if ($pcreate[$par] -and $pcreate[$id] -and $pcreate[$par] -gt $pcreate[$id]) { break }   # PID родителя переиспользован
    $pk = Name-Key $pname[$par]
    if ($protected -contains $pk) { break }
    $ck = Name-Key $pname[$id]
    if ($pk -eq $ck -or $ck -match 'helper|webview|crashpad') { $id = $par } else { break }
  }
  return $id
}

if ($mode -eq 'kill') {
  $want = Name-Key $env:NX_NAME
  $killed = 0; $skipped = 0; $err = ''
  foreach ($s in ($env:NX_PIDS -split ',')) {
    $id = 0
    if (-not [int]::TryParse($s.Trim(), [ref]$id)) { continue }
    if ($id -le 4 -or $mine.ContainsKey($id)) { $skipped++; continue }
    $cur = Name-Key $pname[$id]
    if (-not $cur -or $cur -ne $want -or ($protected -contains $cur)) { $skipped++; continue }
    # корень + всё его поддерево (дети, helper'ы)
    $tree = New-Object System.Collections.ArrayList
    $q = New-Object System.Collections.Queue
    $q.Enqueue($id); [void]$tree.Add($id)
    while ($q.Count -gt 0) {
      $x = [int]$q.Dequeue()
      if ($children.ContainsKey($x)) {
        foreach ($c in $children[$x]) {
          if ($tree.Contains($c) -or $mine.ContainsKey($c)) { continue }
          if ($pcreate[$c] -and $pcreate[$x] -and $pcreate[$c] -lt $pcreate[$x]) { continue }
          if ($protected -contains (Name-Key $pname[$c])) { continue }
          [void]$tree.Add($c); $q.Enqueue($c)
        }
      }
    }
    # сначала дети, потом корень
    for ($i = $tree.Count - 1; $i -ge 0; $i--) {
      $t = [int]$tree[$i]
      try { Stop-Process -Id $t -Force -ErrorAction Stop; $killed++ }
      catch {
        # запасной путь: taskkill
        $o = & taskkill.exe /F /PID $t 2>&1 | Out-String
        if ($LASTEXITCODE -eq 0) { $killed++ } else { $skipped++; if (-not $err) { $err = ($o.Trim() -replace '[\r\n"\\]+', ' ') } }
      }
    }
  }
  Write-Output ('{"ok":true,"killed":' + $killed + ',"skipped":' + $skipped + ',"err":"' + $err + '"}')
  exit 0
}

function Is-Public([string]$ip) {
  if (-not $ip) { return $false }
  $ip = $ip -replace '^::ffff:', ''
  if ($ip -match ':') {
    $l = $ip.ToLower()
    if ($l -eq '::' -or $l -eq '::1' -or $l.StartsWith('fe80') -or $l.StartsWith('fc') -or $l.StartsWith('fd') -or $l.StartsWith('ff')) { return $false }
    return $true
  }
  if ($ip -match '^(127\.|10\.|0\.|192\.168\.|169\.254\.|22[4-9]\.|23\d\.|255\.)') { return $false }
  if ($ip -match '^172\.(1[6-9]|2\d|3[01])\.') { return $false }
  if ($ip -match '^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.') { return $false }
  return $true
}
function Is-Loop([string]$ip) { $ip = $ip -replace '^::ffff:', ''; return ($ip -eq '::1' -or $ip.StartsWith('127.')) }

# порты, которые слушает сам Nexora (локальный прокси VPN): трафик приложений через них тоже идёт в интернет
$listen = @{}
Get-NetTCPConnection -State Listen | ForEach-Object { $listen[[int]$_.LocalPort] = [int]$_.OwningProcess }

# соединения по корневому процессу приложения
$byRoot = @{}
foreach ($c in (Get-NetTCPConnection -State Established)) {
  $p = [int]$c.OwningProcess
  if ($p -le 4 -or $mine.ContainsKey($p) -or -not $pname.ContainsKey($p)) { continue }
  $ra = [string]$c.RemoteAddress
  $remote = $null
  if (Is-Public $ra) { $remote = $ra }
  elseif ((Is-Loop $ra) -and $listen.ContainsKey([int]$c.RemotePort) -and $mine.ContainsKey([int]$listen[[int]$c.RemotePort])) { $remote = 'proxy' }
  if (-not $remote) { continue }
  $root = Get-Root $p
  if ($mine.ContainsKey($root)) { continue }
  if (-not $byRoot.ContainsKey($root)) { $byRoot[$root] = @{ n = 0; r = @{} } }
  $byRoot[$root].n++
  $byRoot[$root].r[$remote] = $true
}

# группировка корней по имени приложения
$apps = @{}
foreach ($root in $byRoot.Keys) {
  $nm = [string]$pname[[int]$root]
  if (-not $nm) { continue }
  $key = Name-Key $nm
  if (-not $apps.ContainsKey($key)) {
    $apps[$key] = @{ key = $key; exe = $nm; pids = @(); n = 0; hosts = @{}; path = $ppath[[int]$root] }
  }
  $a = $apps[$key]
  $a.pids += [int]$root
  $a.n += $byRoot[$root].n
  foreach ($h in $byRoot[$root].r.Keys) { $a.hosts[$h] = $true }
  if (-not $a.path) { $a.path = $ppath[[int]$root] }
}

$items = @()
foreach ($a in $apps.Values) {
  $title = ''
  if ($a.path) { try { $title = [string][System.Diagnostics.FileVersionInfo]::GetVersionInfo($a.path).FileDescription } catch {} }
  if (-not $title) { $title = ($a.exe -replace '\.exe$', '') }
  $items += [pscustomobject]@{
    name = $title.Trim()
    exe = [string]$a.exe
    pids = @($a.pids)
    conns = [int]$a.n
    hosts = [int]$a.hosts.Count
    prot = [bool]($protected -contains $a.key)
  }
}
$items = $items | Sort-Object @{ Expression = { $_.prot }; Ascending = $true }, @{ Expression = { $_.conns }; Descending = $true } | Select-Object -First 60
$json = '[' + ((@($items) | ForEach-Object { $_ | ConvertTo-Json -Compress -Depth 3 }) -join ',') + ']'
Write-Output $json

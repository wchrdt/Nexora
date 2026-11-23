$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$op = $env:ISL_OP
$soundDir = Join-Path $env:APPDATA 'Nexora\sounds'

function New-Owner {
  Add-Type -AssemblyName System.Windows.Forms
  Add-Type -AssemblyName System.Drawing
  $o = New-Object System.Windows.Forms.Form
  $o.TopMost = $true
  $o.ShowInTaskbar = $false
  $o.StartPosition = 'Manual'
  $o.Location = New-Object System.Drawing.Point(-2000, -2000)
  $o.Size = New-Object System.Drawing.Size(1, 1)
  $o.Show()
  $o.Activate()
  return $o
}

if ($op -eq 'exe') {
  $owner = New-Owner
  $d = New-Object System.Windows.Forms.OpenFileDialog
  $d.Title = 'Выбери программу'
  $d.Filter = 'Программы и ярлыки|*.exe;*.lnk;*.bat;*.cmd|Все файлы|*.*'
  if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.FileName) }
  $owner.Dispose()
}
elseif ($op -eq 'sound') {
  $owner = New-Owner
  $d = New-Object System.Windows.Forms.OpenFileDialog
  $d.Title = 'Выбери звук для открытия острова'
  $d.Filter = 'Аудио|*.mp3;*.wav;*.ogg;*.m4a;*.aac;*.flac'
  if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) {
    $src = Get-Item -LiteralPath $d.FileName
    if ($src.Length -gt 8MB) {
      @{ ok = $false; err = 'Файл больше 8 МБ, выбери покороче' } | ConvertTo-Json -Compress | ForEach-Object { [Console]::Out.Write($_) }
    } else {
      New-Item -ItemType Directory -Force -Path $soundDir | Out-Null
      Get-ChildItem -Path $soundDir -Filter 'open.*' -ErrorAction SilentlyContinue | Remove-Item -Force
      Copy-Item -LiteralPath $src.FullName -Destination (Join-Path $soundDir ('open' + $src.Extension.ToLower())) -Force
      @{ ok = $true; name = $src.Name } | ConvertTo-Json -Compress | ForEach-Object { [Console]::Out.Write($_) }
    }
  }
  $owner.Dispose()
}
elseif ($op -eq 'soundread') {
  $f = Get-ChildItem -Path $soundDir -Filter 'open.*' -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($f) {
    $mime = switch ($f.Extension.ToLower()) {
      '.mp3' { 'audio/mpeg' } '.wav' { 'audio/wav' } '.ogg' { 'audio/ogg' }
      '.m4a' { 'audio/mp4' } '.aac' { 'audio/aac' } '.flac' { 'audio/flac' } default { 'audio/mpeg' }
    }
    [Console]::Out.Write('data:' + $mime + ';base64,' + [Convert]::ToBase64String([IO.File]::ReadAllBytes($f.FullName)))
  }
}
elseif ($op -eq 'soundclear') {
  Get-ChildItem -Path $soundDir -Filter 'open.*' -ErrorAction SilentlyContinue | Remove-Item -Force
  [Console]::Out.Write('ok')
}
elseif ($op -eq 'save') {
  $data = [Console]::In.ReadToEnd()
  $owner = New-Owner
  $d = New-Object System.Windows.Forms.SaveFileDialog
  $d.Title = 'Сохранить скриншот'
  $d.Filter = 'PNG|*.png'
  $d.InitialDirectory = [Environment]::GetFolderPath('MyPictures')
  $d.FileName = 'island-' + [DateTimeOffset]::Now.ToUnixTimeSeconds() + '.png'
  if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) {
    $b64 = $data.Substring($data.IndexOf(',') + 1).Trim()
    [IO.File]::WriteAllBytes($d.FileName, [Convert]::FromBase64String($b64))
    [Console]::Out.Write($d.FileName)
  }
  $owner.Dispose()
}

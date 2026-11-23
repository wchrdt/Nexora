$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$null = [Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager, Windows.Media.Control, ContentType = WindowsRuntime]
$null = [Windows.Media.Control.GlobalSystemMediaTransportControlsSessionMediaProperties, Windows.Media.Control, ContentType = WindowsRuntime]
$null = [Windows.Storage.Streams.IRandomAccessStreamWithContentType, Windows.Storage.Streams, ContentType = WindowsRuntime]
$null = [Windows.Storage.Streams.DataReader, Windows.Storage.Streams, ContentType = WindowsRuntime]
$asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
  $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetGenericArguments().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -like 'IAsyncOperation*'
})[0]
function Await($op, $type) {
  $t = $asTask.MakeGenericMethod($type).Invoke($null, @($op))
  # по истечении времени возвращаем $null, а не ждём дальше через .Result
  if (-not $t.Wait(4000)) { return $null }
  $t.Result
}
$mgrT = [Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager]
$propsT = [Windows.Media.Control.GlobalSystemMediaTransportControlsSessionMediaProperties]
$streamT = [Windows.Storage.Streams.IRandomAccessStreamWithContentType]
$script:mgr = $null
$script:mgrAt = [DateTime]::MinValue
$script:tk = ''
$script:tb = ''
$script:tt = ''
$script:tries = 0
function Get-Cur {
  # менеджер сессий со временем «протухает» (после сна/долгой паузы) — пересоздаём раз в минуту и когда сессий нет
  if ($null -eq $script:mgr -or ([DateTime]::UtcNow - $script:mgrAt).TotalSeconds -gt 60) {
    $script:mgr = Await ($mgrT::RequestAsync()) $mgrT
    $script:mgrAt = [DateTime]::UtcNow
  }
  $cur = $null
  foreach ($s in @($script:mgr.GetSessions())) {
    if ([string]$s.GetPlaybackInfo().PlaybackStatus -eq 'Playing') { $cur = $s; break }
  }
  if ($null -eq $cur) { $cur = $script:mgr.GetCurrentSession() }
  if ($null -eq $cur) { $script:mgr = $null }
  return $cur
}
function Get-State {
  $cur = Get-Cur
  if ($null -eq $cur) { return '{}' }
  $props = Await ($cur.TryGetMediaPropertiesAsync()) $propsT
  $pb = $cur.GetPlaybackInfo()
  $tl = $cur.GetTimelineProperties()
  $status = [string]$pb.PlaybackStatus
  $dur = [double]$tl.EndTime.TotalSeconds - [double]$tl.StartTime.TotalSeconds
  $pos = [double]$tl.Position.TotalSeconds
  if ($status -eq 'Playing') {
    try { $pos += ([DateTimeOffset]::UtcNow - $tl.LastUpdatedTime).TotalSeconds } catch {}
  }
  if ($dur -gt 0 -and $pos -gt $dur) { $pos = $dur }
  if ($pos -lt 0) { $pos = 0 }
  $title = [string]$props.Title
  $artist = [string]$props.Artist
  $tkey = [string]$cur.SourceAppUserModelId + '|' + $title + '|' + $artist
  if ($tkey -ne $script:tk) { $script:tk = $tkey; $script:tb = ''; $script:tt = ''; $script:tries = 0 }
  $thumb = ''
  $ttype = ''
  if ([string]::IsNullOrEmpty($script:tb) -and $script:tries -lt 40) {
    $script:tries++
    if ($null -ne $props.Thumbnail) {
      $bytes = $null
      $ct = ''
      try {
        $st = Await ($props.Thumbnail.OpenReadAsync()) $streamT
        if ($null -ne $st) {
          $ct = [string]$st.ContentType
          $size = [uint32]$st.Size
          if ($size -gt 0 -and $size -lt 8000000) {
            # способ 1: .NET-обёртка над WinRT-потоком
            try {
              $net = [System.IO.WindowsRuntimeStreamExtensions]::AsStreamForRead($st)
              $ms = New-Object System.IO.MemoryStream
              $net.CopyTo($ms)
              if ($ms.Length -gt 0) { $bytes = $ms.ToArray() }
            } catch {}
            # способ 2: DataReader (на случай, если AsStreamForRead не подошёл по перегрузке)
            if ($null -eq $bytes) {
              try {
                $rdr = New-Object Windows.Storage.Streams.DataReader($st.GetInputStreamAt(0))
                $null = Await ($rdr.LoadAsync($size)) ([uint32])
                $buf = New-Object byte[] $size
                $rdr.ReadBytes($buf)
                $bytes = $buf
              } catch {}
            }
          }
        }
      } catch {}
      if ($null -ne $bytes -and $bytes.Length -gt 0) {
        $script:tb = [Convert]::ToBase64String($bytes)
        $script:tt = $ct
        $thumb = $script:tb
        $ttype = $script:tt
      }
    }
  }
  $o = [ordered]@{
    source = [string]$cur.SourceAppUserModelId
    title = $title
    artist = $artist
    album = [string]$props.AlbumTitle
    status = $status
    position = [Math]::Round($pos, 2)
    duration = [Math]::Round($dur, 2)
    canSeek = [bool]$pb.Controls.IsPlaybackPositionEnabled
    thumbKey = $tkey
    thumb = $thumb
    thumbType = $ttype
  }
  return ($o | ConvertTo-Json -Compress)
}
[Console]::Out.WriteLine('ready')
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($null -eq $line) { break }
  try {
    $p = $line.Trim().Split(' ')
    if ($p[0] -eq 'ctl' -and $p.Length -gt 1) {
      # управление той же системной медиа-сессией, из которой читаем состояние (Яндекс Музыка в WebView2 и др.)
      $res = '0'
      try {
        $cur = Get-Cur
        if ($null -ne $cur) {
          $op = $null
          switch ($p[1]) {
            'playpause' { $op = $cur.TryTogglePlayPauseAsync() }
            'next'      { $op = $cur.TrySkipNextAsync() }
            'prev'      { $op = $cur.TrySkipPreviousAsync() }
          }
          if ($null -ne $op -and [bool](Await $op ([bool]))) { $res = '1' }
        }
      } catch {}
      [Console]::Out.WriteLine('ctl ' + $res)
      continue
    }
    if ($p[0] -eq 'seek' -and $p.Length -gt 1) {
      $res = '0'
      try {
        $cur = Get-Cur
        if ($null -ne $cur) {
          $ticks = [long]([double]$p[1] * 10000000)
          if ([bool](Await ($cur.TryChangePlaybackPositionAsync($ticks)) ([bool]))) { $res = '1' }
        }
      } catch {}
      [Console]::Out.WriteLine('seek ' + $res)
      continue
    }
    [Console]::Out.WriteLine((Get-State))
  }
  catch { $script:mgr = $null; [Console]::Out.WriteLine('{}') }
}

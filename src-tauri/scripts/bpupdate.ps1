$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch {}

# ISL_COMP: zapret | tg ; ISL_FORCE=1 скачать даже если версия та же ; ISL_CHECK=1 только проверить
$comp = [string]$env:ISL_COMP
$force = ($env:ISL_FORCE -eq '1')
$checkOnly = ($env:ISL_CHECK -eq '1')

function Out-Json($o) { $o | ConvertTo-Json -Compress -Depth 4 }

$tmp = $null
try {
  if ($comp -eq 'zapret') { $repo = 'Flowseal/zapret-discord-youtube'; $sub = 'zapret' }
  elseif ($comp -eq 'tg') { $repo = 'Flowseal/tg-ws-proxy'; $sub = 'tgproxy' }
  else { throw 'неизвестный компонент' }

  $root = Join-Path $env:APPDATA ('Nexora\' + $sub)
  $verFile = Join-Path $root 'version.txt'
  $hdr = @{ 'User-Agent' = 'Nexora'; 'Accept' = 'application/vnd.github+json' }

  $rel = Invoke-RestMethod -Uri ("https://api.github.com/repos/$repo/releases/latest") -Headers $hdr -TimeoutSec 25
  $tag = [string]$rel.tag_name
  $cur = ''
  if (Test-Path $verFile) { $cur = ([string](Get-Content $verFile -Raw)).Trim() }

  if (-not $force -and $cur -eq $tag) {
    Out-Json @{ ok = $true; updated = $false; version = $tag; current = $cur }
    exit 0
  }
  if ($checkOnly) {
    Out-Json @{ ok = $true; updated = $false; available = ($cur -ne $tag); version = $tag; current = $cur }
    exit 0
  }

  if ($comp -eq 'zapret') {
    $asset = $rel.assets | Where-Object { $_.name -like '*.zip' } | Select-Object -First 1
  } else {
    $asset = $rel.assets | Where-Object { $_.name -eq 'TgWsProxy_windows.exe' } | Select-Object -First 1
  }
  if ($null -eq $asset) { throw 'в релизе нет нужного файла' }

  $tmp = Join-Path $env:TEMP ('nexora-' + $comp + '-' + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $tmp -Force | Out-Null
  $file = Join-Path $tmp ([string]$asset.name)
  Invoke-WebRequest -Uri ([string]$asset.browser_download_url) -OutFile $file -UseBasicParsing -TimeoutSec 300 -Headers @{ 'User-Agent' = 'Nexora' }

  # проверка SHA-256, если GitHub отдаёт хэш файла
  $verified = $false
  $digest = [string]$asset.digest
  if ($digest -match '^sha256:(.+)$') {
    $want = $Matches[1].Trim()
    $have = (Get-FileHash -Path $file -Algorithm SHA256).Hash
    if ($have -ne $want) { throw 'контрольная сумма не совпала, файл не установлен' }
    $verified = $true
  }

  if ($comp -eq 'zapret') {
    # остановить winws из нашей папки (чужой отдельный zapret не трогаем) и драйвер, чтобы файлы не были заняты
    Get-Process -Name winws -ErrorAction SilentlyContinue |
      Where-Object { $_.Path -and $_.Path.StartsWith($root, [StringComparison]::OrdinalIgnoreCase) } |
      Stop-Process -Force -ErrorAction SilentlyContinue
    # драйвер WinDivert общий для системы: останавливаем его, только если нет чужого winws (или его путь не удалось определить)
    $foreign = @(Get-Process -Name winws -ErrorAction SilentlyContinue |
      Where-Object { -not $_.Path -or -not $_.Path.StartsWith($root, [StringComparison]::OrdinalIgnoreCase) })
    if ($foreign.Count -eq 0) { & sc.exe stop WinDivert 2>$null | Out-Null }
    Start-Sleep -Milliseconds 400

    $x = Join-Path $tmp 'x'
    Expand-Archive -Path $file -DestinationPath $x -Force
    $w = Get-ChildItem -Path $x -Recurse -Filter winws.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -eq $w) { throw 'в архиве нет bin\winws.exe' }
    $src = Split-Path -Parent (Split-Path -Parent $w.FullName)

    # новый набор собираем рядом со старым; старый удаляем только после успешной замены
    $new = $root + '.new'
    $old = $root + '.old'
    if (Test-Path $new) { Remove-Item -Path $new -Recurse -Force -ErrorAction SilentlyContinue }
    if (Test-Path $old) { Remove-Item -Path $old -Recurse -Force -ErrorAction SilentlyContinue }
    New-Item -ItemType Directory -Path $new -Force | Out-Null
    Copy-Item -Path (Join-Path $src '*') -Destination $new -Recurse -Force
    if (-not (Test-Path (Join-Path $new 'bin\winws.exe'))) { throw 'новый набор неполный (нет bin\winws.exe), старый оставлен без изменений' }

    # свои списки (*user*.txt) переживают обновление
    $newLists = Join-Path $new 'lists'
    if (-not (Test-Path $newLists)) { New-Item -ItemType Directory -Path $newLists -Force | Out-Null }
    $oldLists = Join-Path $root 'lists'
    if (Test-Path $oldLists) {
      Get-ChildItem -Path $oldLists -Filter '*user*.txt' -ErrorAction SilentlyContinue | Copy-Item -Destination $newLists -Force
    }

    # замена: старое переименовываем, новое ставим на место; если не вышло, возвращаем старое
    if (Test-Path $root) { Rename-Item -Path $root -NewName (Split-Path $old -Leaf) -ErrorAction Stop }
    try {
      Rename-Item -Path $new -NewName (Split-Path $root -Leaf) -ErrorAction Stop
    } catch {
      if ((Test-Path $old) -and -not (Test-Path $root)) { Rename-Item -Path $old -NewName (Split-Path $root -Leaf) -ErrorAction SilentlyContinue }
      throw
    }
    Remove-Item -Path $old -Recurse -Force -ErrorAction SilentlyContinue
  } else {
    # только наш процесс (из папки Nexora); чужие копии с таким же именем не трогаем
    Get-Process -Name TgWsProxy_windows -ErrorAction SilentlyContinue |
      Where-Object { $_.Path -and $_.Path.StartsWith($root, [StringComparison]::OrdinalIgnoreCase) } |
      Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 300
    New-Item -ItemType Directory -Path $root -Force | Out-Null
    Copy-Item -Path $file -Destination (Join-Path $root 'TgWsProxy_windows.exe') -Force
  }

  Set-Content -Path $verFile -Value $tag -Encoding ASCII
  Out-Json @{ ok = $true; updated = $true; version = $tag; previous = $cur; verified = $verified }
} catch {
  Out-Json @{ ok = $false; err = [string]$_.Exception.Message }
} finally {
  if ($tmp -and (Test-Path $tmp)) { Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue }
  if ($new -and (Test-Path $new)) { Remove-Item -Path $new -Recurse -Force -ErrorAction SilentlyContinue }
}

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
# Включение/выключение радиомодуля Wi-Fi через Windows.Devices.Radios (как переключатель в системной шторке, права администратора не нужны).
# ISL_OP = get | set ; ISL_STATE = on | off
function Out-Json($ok, $on, $err) {
  $o = @{ ok = [bool]$ok; on = $on; err = [string]$err }
  [Console]::Out.Write(($o | ConvertTo-Json -Compress))
}
try {
  Add-Type -AssemblyName System.Runtime.WindowsRuntime
  $null = [Windows.Devices.Radios.Radio, Windows.System.Devices, ContentType = WindowsRuntime]
  $null = [Windows.Devices.Radios.RadioAccessStatus, Windows.System.Devices, ContentType = WindowsRuntime]
  $null = [Windows.Devices.Radios.RadioState, Windows.System.Devices, ContentType = WindowsRuntime]
  $asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetGenericArguments().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -like 'IAsyncOperation*'
  })[0]
  function Await($op, $type) {
    $t = $asTask.MakeGenericMethod($type).Invoke($null, @($op))
    if (-not $t.Wait(8000)) { throw 'таймаут ответа Windows' }
    $t.Result
  }
  $acc = Await ([Windows.Devices.Radios.Radio]::RequestAccessAsync()) ([Windows.Devices.Radios.RadioAccessStatus])
  if ([string]$acc -ne 'Allowed') { Out-Json $false $null ('Windows не дала доступ к радиомодулям: ' + $acc); exit 0 }
  $radios = Await ([Windows.Devices.Radios.Radio]::GetRadiosAsync()) ([System.Collections.Generic.IReadOnlyList[Windows.Devices.Radios.Radio]])
  $wifi = @($radios | Where-Object { [string]$_.Kind -eq 'WiFi' })
  if ($wifi.Count -eq 0) { Out-Json $false $null 'Wi-Fi адаптер не найден'; exit 0 }
  if ($env:ISL_OP -eq 'set') {
    $want = if ($env:ISL_STATE -eq 'on') { [Windows.Devices.Radios.RadioState]::On } else { [Windows.Devices.Radios.RadioState]::Off }
    foreach ($r in $wifi) {
      $res = Await ($r.SetStateAsync($want)) ([Windows.Devices.Radios.RadioAccessStatus])
      if ([string]$res -ne 'Allowed') { Out-Json $false $null ('Не удалось переключить: ' + $res); exit 0 }
    }
    Start-Sleep -Milliseconds 400
    $radios = Await ([Windows.Devices.Radios.Radio]::GetRadiosAsync()) ([System.Collections.Generic.IReadOnlyList[Windows.Devices.Radios.Radio]])
    $wifi = @($radios | Where-Object { [string]$_.Kind -eq 'WiFi' })
  }
  $on = $false
  foreach ($r in $wifi) { if ([string]$r.State -eq 'On') { $on = $true } }
  Out-Json $true $on ''
} catch {
  Out-Json $false $null ([string]$_.Exception.Message)
}

# Описание трека SoundCloud (текст песни лежит там): страница трека отдаёт данные в window.__sc_hydration
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch {}
function Out-Json($ok, $desc, $err) {
  $o = @{ ok = $ok; desc = [string]$desc; err = [string]$err }
  Write-Output ($o | ConvertTo-Json -Compress)
}
try {
  $to = 12
  $r = Invoke-WebRequest -Uri $env:ISL_URL -UserAgent $env:ISL_UA -UseBasicParsing -TimeoutSec $to -MaximumRedirection 5
  $c = [string]$r.Content
  $desc = ''
  $i = $c.IndexOf('window.__sc_hydration')
  if ($i -ge 0) {
    $a = $c.IndexOf('[', $i)
    $e = $c.IndexOf('</script>', $a)
    if ($a -ge 0 -and $e -gt $a) {
      $js = $c.Substring($a, $e - $a).Trim().TrimEnd(';')
      Add-Type -AssemblyName System.Web.Extensions
      $ser = New-Object System.Web.Script.Serialization.JavaScriptSerializer
      $ser.MaxJsonLength = [int]::MaxValue
      $ser.RecursionLimit = 500
      $arr = $ser.DeserializeObject($js)
      foreach ($it in $arr) {
        if ($it -is [System.Collections.IDictionary] -and $it.Contains('hydratable') -and $it['hydratable'] -eq 'sound') {
          $d = $it['data']
          if ($d -is [System.Collections.IDictionary] -and $d.Contains('description') -and $d['description']) { $desc = [string]$d['description'] }
          break
        }
      }
    }
  }
  Out-Json $true $desc ''
} catch {
  Out-Json $false '' ($_.Exception.Message)
}

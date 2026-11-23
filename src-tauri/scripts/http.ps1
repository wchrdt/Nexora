$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls11 } catch {}
$h = @{}
if ($env:ISL_HEADERS) {
  $o = $env:ISL_HEADERS | ConvertFrom-Json
  foreach ($p in $o.PSObject.Properties) { $h[$p.Name] = [string]$p.Value }
}
$to = 0
[int]::TryParse([string]$env:ISL_TIMEOUT, [ref]$to) | Out-Null
if ($to -le 0) { $to = 12 }
$status = 0; $body = ''; $err = ''
$hdr = @{}

function Read-Bytes($resp) {
  $s = $resp.GetResponseStream()
  $m = New-Object System.IO.MemoryStream
  $s.CopyTo($m)
  return $m.ToArray()
}
function Pack($bytes) {
  if ($env:ISL_MODE -eq 'b64') { return [Convert]::ToBase64String($bytes) }
  return [System.Text.Encoding]::UTF8.GetString($bytes)
}

$method = 'GET'
if ($env:ISL_METHOD) { $method = [string]$env:ISL_METHOD }
$bodyBytes = $null
if ($method -ne 'GET') {
  try {
    $b64 = ([Console]::In.ReadToEnd()).Trim()
    if ($b64) { $bodyBytes = [Convert]::FromBase64String($b64) }
  } catch {}
}

try {
  $iw = @{ Uri = $env:ISL_URL; UserAgent = $env:ISL_UA; Headers = $h; TimeoutSec = $to; UseBasicParsing = $true; MaximumRedirection = 5 }
  if ($method -ne 'GET') {
    $iw.Method = $method
    $iw.ContentType = 'application/json; charset=utf-8'
    if ($null -ne $bodyBytes) { $iw.Body = $bodyBytes }
  }
  $r = Invoke-WebRequest @iw
  $status = [int]$r.StatusCode
  foreach ($k in $r.Headers.Keys) { $hdr[([string]$k).ToLower()] = [string]$r.Headers[$k] }
  $ms = $r.RawContentStream
  $body = Pack ($ms.ToArray())
} catch {
  $err = $_.Exception.Message
  $resp = $_.Exception.Response
  if ($null -ne $resp) {
    try { $status = [int]$resp.StatusCode } catch {}
    try { foreach ($k in $resp.Headers.AllKeys) { $hdr[([string]$k).ToLower()] = [string]$resp.Headers[$k] } } catch {}
    try { $body = Pack (Read-Bytes $resp) } catch {}
  }
}
@{ status = $status; headers = $hdr; body = $body; err = $err } | ConvertTo-Json -Compress -Depth 4

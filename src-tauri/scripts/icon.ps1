$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class IslIcon {
  [DllImport("user32.dll", CharSet = CharSet.Unicode)]
  public static extern uint PrivateExtractIcons(string file, int index, int cx, int cy, IntPtr[] phicon, uint[] piconid, uint nIcons, uint flags);
  [DllImport("user32.dll")]
  public static extern bool DestroyIcon(IntPtr h);
}
'@
function Expand([string]$s) { [Environment]::ExpandEnvironmentVariables(($s -replace '^"+|"+$', '').Trim()) }
$path = Expand $env:ISL_PATH
if (-not $path) { exit 0 }

$files = New-Object System.Collections.ArrayList
$idxs = New-Object System.Collections.ArrayList
if ($path -match '\.lnk$') {
  try {
    $sc = (New-Object -ComObject WScript.Shell).CreateShortcut($path)
    $loc = Expand $sc.IconLocation
    $target = Expand $sc.TargetPath
    if ($loc) {
      $i = $loc.LastIndexOf(',')
      $f = $loc; $n = 0
      if ($i -ge 0) { $f = $loc.Substring(0, $i); $n = 0; [int]::TryParse($loc.Substring($i + 1), [ref]$n) | Out-Null }
      if ($f -and (Test-Path -LiteralPath $f)) { [void]$files.Add($f); [void]$idxs.Add($n) }
    }
    if ($target -and (Test-Path -LiteralPath $target) -and ($target -notmatch '[\\/](explorer|cmd|powershell|pwsh|wscript|cscript|conhost)\.exe$')) {
      [void]$files.Add($target); [void]$idxs.Add(0)
    }
  } catch {}
} else {
  [void]$files.Add($path); [void]$idxs.Add(0)
}

$bmp = $null
for ($k = 0; $k -lt $files.Count -and $null -eq $bmp; $k++) {
  $file = [string]$files[$k]
  if ($file -notmatch '\.(exe|dll|ico|ocx|scr|icl|cpl)$') { continue }
  foreach ($sz in 128, 64, 48, 32) {
    try {
      $h = New-Object 'System.IntPtr[]' 1
      $id = New-Object 'System.UInt32[]' 1
      $n = [IslIcon]::PrivateExtractIcons($file, [int]$idxs[$k], $sz, $sz, $h, $id, 1, 0)
      if ($n -ge 1 -and $n -lt 1000 -and $h[0] -ne [IntPtr]::Zero) {
        $ic = [System.Drawing.Icon]::FromHandle($h[0])
        $bmp = New-Object System.Drawing.Bitmap($ic.ToBitmap())
        [void][IslIcon]::DestroyIcon($h[0])
        break
      }
    } catch {}
  }
}
if ($null -eq $bmp) {
  try {
    $ic = [System.Drawing.Icon]::ExtractAssociatedIcon($path)
    if ($null -ne $ic) { $bmp = New-Object System.Drawing.Bitmap($ic.ToBitmap()) }
  } catch {}
}
if ($null -ne $bmp) {
  $ms = New-Object System.IO.MemoryStream
  $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  [Console]::Out.Write([Convert]::ToBase64String($ms.ToArray()))
}

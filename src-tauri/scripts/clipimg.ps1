$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$data = [Console]::In.ReadToEnd()
$b64 = $data.Substring($data.IndexOf(',') + 1).Trim()
$ms = New-Object System.IO.MemoryStream(, [Convert]::FromBase64String($b64))
$img = [System.Drawing.Image]::FromStream($ms)
[System.Windows.Forms.Clipboard]::SetImage($img)
[Console]::Out.Write('ok')

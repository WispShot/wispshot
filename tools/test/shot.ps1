param([string]$out,[int]$x=0,[int]$y=0,[int]$w=1200,[int]$h=900)
Add-Type -AssemblyName System.Drawing
Add-Type 'using System.Runtime.InteropServices; public static class D { [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }'
[D]::SetProcessDPIAware() | Out-Null
$b = New-Object System.Drawing.Bitmap $w,$h; $g=[System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($x,$y,0,0,$b.Size); $b.Save($out)

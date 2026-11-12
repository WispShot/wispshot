# Send a key chord in one process, e.g. "17,85" for Ctrl+U.
param([string]$k) $vks = [int[]]($k -split ",")
Add-Type 'using System;using System.Runtime.InteropServices;public static class C{[DllImport("user32.dll")]public static extern void keybd_event(byte k,byte s,uint f,IntPtr e);}'
foreach($v in $vks){ [C]::keybd_event([byte]$v,0,0,[IntPtr]::Zero); Start-Sleep -m 40 }
[array]::Reverse($vks); foreach($v in $vks){ [C]::keybd_event([byte]$v,0,2,[IntPtr]::Zero); Start-Sleep -m 40 }

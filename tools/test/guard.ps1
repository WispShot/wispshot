# Idle seconds and the foreground window: check before every injected input (never into a game).
Add-Type @"
using System; using System.Runtime.InteropServices; using System.Text;
public static class G {
 [StructLayout(LayoutKind.Sequential)] public struct LII { public uint cb; public uint t; }
 [DllImport("user32.dll")] public static extern bool GetLastInputInfo(ref LII l);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h,StringBuilder s,int n);
}
"@
$l = New-Object G+LII; $l.cb = 8; [G]::GetLastInputInfo([ref]$l) | Out-Null
$idle = ([Environment]::TickCount - $l.t) / 1000
$h = [G]::GetForegroundWindow(); $p = 0; [G]::GetWindowThreadProcessId($h,[ref]$p) | Out-Null
$c = New-Object Text.StringBuilder 64; [G]::GetClassName($h,$c,64) | Out-Null
$n = (Get-Process -Id $p -ErrorAction SilentlyContinue).ProcessName
"idle=$([int]$idle) fg=$c proc=$n"

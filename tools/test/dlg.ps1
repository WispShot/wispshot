# Answer a dialog of the test build by button id (6 = Yes, 7 = No) without injecting input.
param([int]$btn=0)
Add-Type @"
using System; using System.Runtime.InteropServices; using System.Text;
public static class D {
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c,string n);
 [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr h,int id);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h,uint m,IntPtr w,IntPtr l);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);
 [StructLayout(LayoutKind.Sequential)] public struct R { public int l,t,r,b; }
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out R r);
}
"@
$h = [D]::FindWindow("#32770", [NullString]::Value)
if ($h -eq [IntPtr]::Zero) { "nodialog"; exit }
$p=0; [D]::GetWindowThreadProcessId($h,[ref]$p) | Out-Null
$n=(Get-Process -Id $p).ProcessName
$r = New-Object D+R; [D]::GetWindowRect($h,[ref]$r) | Out-Null
"dialog proc=$n rect=($($r.l),$($r.t),$($r.r),$($r.b))"
if ($btn -gt 0 -and $n -eq "wispshot") { $b=[D]::GetDlgItem($h,$btn); [D]::PostMessage($b,0x00F5,[IntPtr]::Zero,[IntPtr]::Zero) | Out-Null; "clicked $btn" }

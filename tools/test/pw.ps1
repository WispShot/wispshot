param([string]$cls,[string]$out)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class PW {
 [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c,string n);
 [StructLayout(LayoutKind.Sequential)] public struct R { public int l,t,r,b; }
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out R r);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h,IntPtr dc,uint f);
}
"@
[PW]::SetProcessDPIAware() | Out-Null
$h=[PW]::FindWindow($cls,[NullString]::Value); $r=New-Object PW+R; [PW]::GetWindowRect($h,[ref]$r) | Out-Null
$b=New-Object System.Drawing.Bitmap ($r.r-$r.l),($r.b-$r.t); $g=[System.Drawing.Graphics]::FromImage($b); $dc=$g.GetHdc(); [PW]::PrintWindow($h,$dc,2) | Out-Null; $g.ReleaseHdc($dc); $b.Save($out)

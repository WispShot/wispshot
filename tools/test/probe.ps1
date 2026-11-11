param([string]$op, [int]$x=0, [int]$y=0)
Add-Type @"
using System; using System.Runtime.InteropServices; using System.Text;
public static class P {
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint f,int x,int y,uint d,IntPtr e);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c,string n);
 [DllImport("user32.dll")] public static extern void keybd_event(byte k,byte s,uint f,IntPtr e);
 [DllImport("user32.dll")] public static extern bool IsHungAppWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern IntPtr SendMessageTimeout(IntPtr h,uint m,IntPtr w,IntPtr l,uint f,uint t,out IntPtr r);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h,StringBuilder s,int n);
 [DllImport("user32.dll")] public static extern IntPtr GetCapture();
 [StructLayout(LayoutKind.Sequential)] public struct GTI { public int cb; public int flags; public IntPtr act, foc, cap, menu, move, caret; public int l,t,r,b; }
 [DllImport("user32.dll")] public static extern bool GetGUIThreadInfo(uint tid, ref GTI g);
 public static string Cls(IntPtr h){ var s=new StringBuilder(64); GetClassName(h,s,64); return s.ToString(); }
}
"@
switch ($op) {
 "move" { [P]::SetCursorPos($x,$y) | Out-Null }
 "wheel" { for($i=0;$i -lt $y;$i++){ [P]::mouse_event(0x800,0,0,[BitConverter]::ToUInt32([BitConverter]::GetBytes([int]$x),0),[IntPtr]::Zero); Start-Sleep -m 30 } }
 "kdown" { [P]::keybd_event([byte]$x,0,0,[IntPtr]::Zero) }
 "kup" { [P]::keybd_event([byte]$x,0,2,[IntPtr]::Zero) }
 "key" { [P]::keybd_event([byte]$x,0,0,[IntPtr]::Zero); Start-Sleep -m 30; [P]::keybd_event([byte]$x,0,2,[IntPtr]::Zero) }
 "down" { [P]::SetCursorPos($x,$y) | Out-Null; [P]::mouse_event(2,0,0,0,[IntPtr]::Zero) }
 "up" { [P]::SetCursorPos($x,$y) | Out-Null; [P]::mouse_event(4,0,0,0,[IntPtr]::Zero) }
 "click" { [P]::SetCursorPos($x,$y) | Out-Null; [P]::mouse_event(2,0,0,0,[IntPtr]::Zero); Start-Sleep -m 40; [P]::mouse_event(4,0,0,0,[IntPtr]::Zero) }
 "state" {
   $o = [P]::FindWindow("WispOverlay","Wisp"); $r=[IntPtr]::Zero
   $ok = [P]::SendMessageTimeout($o,0,[IntPtr]::Zero,[IntPtr]::Zero,2,1000,[ref]$r)
   $g = New-Object P+GTI; $g.cb = [Runtime.InteropServices.Marshal]::SizeOf($g); [P]::GetGUIThreadInfo(0,[ref]$g) | Out-Null
   "overlay=$o responsive=$($ok -ne [IntPtr]::Zero) fg=$([P]::Cls([P]::GetForegroundWindow())) guiflags=$($g.flags) cap=$([P]::Cls($g.cap)) menu=$($g.menu)"
 }
}

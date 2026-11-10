Add-Type @"
using System; using System.Runtime.InteropServices; using System.Text;
public static class Q {
 public delegate bool EP(IntPtr h, IntPtr l);
 [DllImport("user32.dll")] public static extern bool EnumWindows(EP f, IntPtr l);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h,StringBuilder s,int n);
 [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h,int i);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);
 [StructLayout(LayoutKind.Sequential)] public struct R { public int l,t,r,b; }
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out R r);
 [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h,int a,out int v,int c);
 [DllImport("user32.dll")] public static extern bool GetLayeredWindowAttributes(IntPtr h,out uint k,out byte a,out uint f);
 public static string Dump(){ var sb=new StringBuilder(); int n=0; EnumWindows((h,l)=>{ if(!IsWindowVisible(h)) return true; var c=new StringBuilder(64); GetClassName(h,c,64); R r; GetWindowRect(h,out r); int cl; DwmGetWindowAttribute(h,14,out cl,4); uint p; GetWindowThreadProcessId(h,out p); uint k; byte a=255; uint f=0; GetLayeredWindowAttributes(h,out k,out a,out f); sb.AppendFormat("{0} {1} pid={2} ex={3:X} rect=({4},{5},{6},{7}) cloak={8} alpha={9}/{10}\n", h, c, p, GetWindowLong(h,-20), r.l,r.t,r.r,r.b, cl, a, f); return ++n<25; }, IntPtr.Zero); return sb.ToString(); }
}
"@
[Q]::Dump()

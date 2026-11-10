param([string]$cls,[int]$msg)
Add-Type 'using System;using System.Runtime.InteropServices;public static class M{[DllImport("user32.dll",CharSet=CharSet.Unicode)]public static extern IntPtr FindWindow(string c,string n);[DllImport("user32.dll")]public static extern bool PostMessage(IntPtr h,uint m,IntPtr w,IntPtr l);}'
$h=[M]::FindWindow($cls,[NullString]::Value); "hwnd=$h"; if($h -ne [IntPtr]::Zero){ [M]::PostMessage($h,[uint32]$msg,[IntPtr]::Zero,[IntPtr]::Zero) }

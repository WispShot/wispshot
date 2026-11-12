# "taken" when another program (WispShot) holds the hotkey vk, "free" otherwise.
param([int]$vk)
Add-Type 'using System;using System.Runtime.InteropServices;public static class H{[DllImport("user32.dll")]public static extern bool RegisterHotKey(IntPtr h,int id,uint m,uint v);[DllImport("user32.dll")]public static extern bool UnregisterHotKey(IntPtr h,int id);}'
if([H]::RegisterHotKey([IntPtr]::Zero,77,0x4000,$vk)){ [H]::UnregisterHotKey([IntPtr]::Zero,77)|Out-Null; "free" } else { "taken" }

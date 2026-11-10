# Rough, reproducible comparison of screenshot tools on this machine.
#   idle    -Root <root pid> [-Seconds 30]  process tree: count, private bytes, working set, CPU time over the window
#   latency -Vk <vk> [-Mods shift|ctrl|alt] [-Runs 10] [-Close esc|rclick|clickesc] [-Park other|primary] [-Window <class>]
#           presses the shortcut and times until the screen visibly changes (the dimmed capture overlay),
#           then presses Esc. Samples two horizontal lines across the primary monitor with BitBlt.
#           Run `white` first in another process (a plain white maximized window on the primary monitor)
#           and park the cursor on another monitor, so the dimming is easy to see.
#   white [-Image <png>]   shows that window until closed (optionally with a picture, for README screenshots)
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools/bench/bench.ps1 idle -Root 1234
param([string]$op, [int]$Root = 0, [int]$Seconds = 30, [int]$Vk = 0x70, [string]$Mods = "", [int]$Runs = 10,
      [int]$Settle = 1500, [string]$Close = "esc", [string]$Park = "other", [string]$Window = "", [string]$Image = "")

Add-Type @"
using System; using System.Diagnostics; using System.Runtime.InteropServices; using System.Threading;
public static class B {
  [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("user32.dll")] static extern int GetSystemMetrics(int i);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void keybd_event(byte k, byte s, uint f, IntPtr e);
  [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr dc);
  [DllImport("gdi32.dll")] static extern IntPtr CreateDIBSection(IntPtr dc, ref BMI bmi, uint u, out IntPtr bits, IntPtr s, uint o);
  [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
  [DllImport("gdi32.dll")] static extern bool BitBlt(IntPtr d, int x, int y, int w, int h, IntPtr s, int sx, int sy, uint rop);
  [StructLayout(LayoutKind.Sequential)] struct BMI { public int size, w, h; public short planes, bits; public int comp, img, xp, yp, used, imp; }
  public static void Dpi() { SetProcessDpiAwarenessContext(new IntPtr(-4)); }

  static IntPtr scr, mem, bits; static int W, H; public static double LastBest;
  static void Init() {
    W = GetSystemMetrics(0); H = GetSystemMetrics(1);
    scr = GetDC(IntPtr.Zero); mem = CreateCompatibleDC(scr);
    var b = new BMI(); b.size = 40; b.w = W; b.h = -2; b.planes = 1; b.bits = 32;
    SelectObject(mem, CreateDIBSection(scr, ref b, 0, out bits, IntPtr.Zero, 0));
  }
  static int[] Grab() {
    BitBlt(mem, 0, 0, W, 1, scr, 0, H / 3, 0x00CC0020);
    BitBlt(mem, 0, 1, W, 1, scr, 0, 2 * H / 3, 0x00CC0020);
    var a = new int[W * 2]; Marshal.Copy(bits, a, 0, a.Length); return a;
  }
  static double Diff(int[] a, int[] b) {
    long s = 0;
    for (int i = 0; i < a.Length; i++) {
      int x = a[i], y = b[i];
      s += Math.Abs((x & 255) - (y & 255)) + Math.Abs(((x >> 8) & 255) - ((y >> 8) & 255)) + Math.Abs(((x >> 16) & 255) - ((y >> 16) & 255));
    }
    return s / (3.0 * a.Length);
  }
  static void Key(byte vk, bool down) { keybd_event(vk, 0, down ? 0u : 2u, IntPtr.Zero); }

  // Returns ms until the sampled lines differ by more than `thr` on average, or -1 after 3 s.
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowW(string c, string n);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  public static double LastWin;
  // Also records in LastWin when a window of class `cls` (if given) first becomes visible.
  public static double Once(byte vk, byte mod, double thr, string cls) {
    LastWin = -1;
    if (scr == IntPtr.Zero) Init();
    var base0 = Grab();
    var sw = Stopwatch.StartNew();
    if (mod != 0) Key(mod, true);
    Key(vk, true); Key(vk, false);
    if (mod != 0) Key(mod, false);
    double best = 0;
    while (sw.ElapsedMilliseconds < 3000) {
      if (cls != null && LastWin < 0) { var w = FindWindowW(cls, null); if (w != IntPtr.Zero && IsWindowVisible(w)) LastWin = sw.Elapsed.TotalMilliseconds; else continue; }
      var now = Grab();
      double d = Diff(base0, now);
      if (d > thr) return sw.Elapsed.TotalMilliseconds;
      if (d > best) best = d;
    }
    LastBest = best;
    return -1;
  }
  [DllImport("user32.dll")] static extern void mouse_event(uint f, int x, int y, uint d, IntPtr e);
  public static void LClick() { mouse_event(2, 0, 0, 0, IntPtr.Zero); Thread.Sleep(30); mouse_event(4, 0, 0, 0, IntPtr.Zero); }
  public static void RClick() { mouse_event(8, 0, 0, 0, IntPtr.Zero); Thread.Sleep(30); mouse_event(16, 0, 0, 0, IntPtr.Zero); }
  public static void Esc() { Key(0x1B, true); Thread.Sleep(20); Key(0x1B, false); }
}
"@
[B]::Dpi()
Add-Type -AssemblyName System.Windows.Forms

function Tree([int]$root) {
  $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name
  $ids = New-Object System.Collections.Generic.List[int]; $ids.Add($root)
  for ($i = 0; $i -lt $ids.Count; $i++) {
    foreach ($p in $all) { if ($p.ParentProcessId -eq $ids[$i] -and -not $ids.Contains([int]$p.ProcessId)) { $ids.Add([int]$p.ProcessId) } }
  }
  $ids | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }
}

switch ($op) {
  "white" {
    $f = New-Object System.Windows.Forms.Form
    $f.Text = "bench"; $f.BackColor = [System.Drawing.Color]::White; $f.StartPosition = "Manual"
    $f.Location = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds.Location; $f.WindowState = "Maximized"
    if ($Image) { $f.BackgroundImage = [System.Drawing.Image]::FromFile($Image); $f.BackgroundImageLayout = "Stretch" }
    [System.Windows.Forms.Application]::Run($f)
  }
  "idle" {
    $t0 = Tree $Root
    $cpu0 = ($t0 | ForEach-Object { $_.TotalProcessorTime.TotalSeconds } | Measure-Object -Sum).Sum
    Start-Sleep -Seconds $Seconds
    $t1 = Tree $Root
    $cpu1 = ($t1 | ForEach-Object { $_.TotalProcessorTime.TotalSeconds } | Measure-Object -Sum).Sum
    $priv = ($t1 | Measure-Object PrivateMemorySize64 -Sum).Sum / 1MB
    $ws = ($t1 | Measure-Object WorkingSet64 -Sum).Sum / 1MB
    "processes={0} private_mb={1:N1} workingset_mb={2:N1} cpu_s={3:N3} over {4}s  [{5}]" -f `
      $t1.Count, $priv, $ws, ($cpu1 - $cpu0), $Seconds, (($t1 | ForEach-Object { $_.Name }) -join ",")
  }
  "latency" {
    $mod = switch ($Mods) { "shift" { 0x10 } "ctrl" { 0x11 } "alt" { 0x12 } default { 0 } }
    # park the cursor on the left edge of the virtual screen (another monitor), so no tool highlights the white window
    # (-Park primary for tools without window detection that only cover the cursor's monitor, like Flameshot 14)
    if ($Park -eq "primary") { [B]::SetCursorPos(960, 700) | Out-Null }
    else { [B]::SetCursorPos([System.Windows.Forms.SystemInformation]::VirtualScreen.Left + 200, 300) | Out-Null }
    $r = @(); $wr = @()
    for ($i = 0; $i -lt $Runs; $i++) {
      $ms = [B]::Once([byte]$Vk, [byte]$mod, 12.0, $(if ($Window) { $Window } else { [NullString]::Value }))
      $r += $ms
      if ($Window) { $wr += [B]::LastWin }
      if ($ms -lt 0) { "  no change seen (max diff {0:N1})" -f [B]::LastBest }
      Start-Sleep -Milliseconds 400
      # clickesc: a click gives the overlay focus first (Flameshot doesn't take it on its own)
      switch ($Close) { "rclick" { [B]::RClick() } "clickesc" { [B]::LClick(); Start-Sleep -Milliseconds 300; [B]::Esc() } default { [B]::Esc() } }
      Start-Sleep -Milliseconds $Settle
    }
    $ok = $r | Where-Object { $_ -ge 0 } | Sort-Object
    "runs: " + (($r | ForEach-Object { "{0:N0}" -f $_ }) -join " ")
    if ($Window) { "$Window visible: " + (($wr | ForEach-Object { "{0:N1}" -f $_ }) -join " ") }
    if ($ok) { "first={0:N0} median={1:N0} min={2:N0} max={3:N0} ms" -f $r[0], $ok[[int][math]::Floor($ok.Count / 2)], $ok[0], $ok[-1] }
  }
}

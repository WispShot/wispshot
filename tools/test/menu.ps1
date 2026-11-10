Add-Type -AssemblyName System.Windows.Forms
$f = New-Object System.Windows.Forms.Form
$f.Text = "MenuHost"; $f.StartPosition = "Manual"; $f.Location = New-Object System.Drawing.Point(300,300); $f.Size = New-Object System.Drawing.Size(500,400); $f.TopMost = $true
$cm = New-Object System.Windows.Forms.ContextMenu
1..8 | % { [void]$cm.MenuItems.Add("Item $_") }
$f.Add_Shown({ $f.Activate(); $script:t = New-Object System.Windows.Forms.Timer; $script:t.Interval = 300; $script:t.Add_Tick({ $script:t.Stop(); $script:cm.Show($script:f, (New-Object System.Drawing.Point(50,50))) }); $script:t.Start() })
[System.Windows.Forms.Application]::Run($f)

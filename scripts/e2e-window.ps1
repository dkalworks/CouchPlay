# A window with a known title for `E2E_WINDOW=1 npm run e2e:native`.
# It resizes itself after a few seconds (to test mid-stream resize) and exits on its own.
param([string]$Title = 'CouchPlay E2E Window', [int]$ResizeAfterMs = 9000, [int]$LifetimeMs = 60000)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$form = New-Object System.Windows.Forms.Form
$form.Text = $Title
$form.StartPosition = 'Manual'
$form.Location = New-Object System.Drawing.Point(60, 60)
$form.ClientSize = New-Object System.Drawing.Size(1280, 720)
$form.BackColor = [System.Drawing.Color]::FromArgb(200, 50, 50)

$label = New-Object System.Windows.Forms.Label
$label.Text = 'WINDOW TEST'
$label.ForeColor = [System.Drawing.Color]::White
$label.Font = New-Object System.Drawing.Font('Segoe UI', 60, [System.Drawing.FontStyle]::Bold)
$label.AutoSize = $true
$label.Location = New-Object System.Drawing.Point(20, 20)
$form.Controls.Add($label)

$resize = New-Object System.Windows.Forms.Timer
$resize.Interval = $ResizeAfterMs
$resize.Add_Tick({
    $resize.Stop()
    $form.ClientSize = New-Object System.Drawing.Size(600, 900)
    $form.BackColor = [System.Drawing.Color]::FromArgb(50, 100, 200)
})
$resize.Start()

$quit = New-Object System.Windows.Forms.Timer
$quit.Interval = $LifetimeMs
$quit.Add_Tick({ $form.Close() })
$quit.Start()

[void]$form.ShowDialog()

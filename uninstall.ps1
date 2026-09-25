param(
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA "rh"),
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"

function Invoke-Step([string]$Label, [scriptblock]$Action) {
    if ($DryRun) { Write-Host "  + $Label" }
    else { Write-Host "  $Label"; & $Action }
}

$binDir = Join-Path $Prefix "bin"
foreach ($exe in @("rh.exe", "cloudflared.exe")) {
    $p = Join-Path $binDir $exe
    if (Test-Path $p) {
        Invoke-Step "remove $p" { Remove-Item $p -Force }
    } else {
        Write-Host "  no $exe at $p"
    }
}

Invoke-Step "remove $binDir if empty" {
    if ((Test-Path $binDir) -and @(Get-ChildItem $binDir -Force).Count -eq 0) {
        Remove-Item $binDir -Force
    }
}
Invoke-Step "remove $Prefix if empty" {
    if ((Test-Path $Prefix) -and @(Get-ChildItem $Prefix -Force).Count -eq 0) {
        Remove-Item $Prefix -Force
    }
}

Write-Host "  note: $binDir stays on your user PATH (harmless empty entry); remove manually if desired"
Write-Host "  done."

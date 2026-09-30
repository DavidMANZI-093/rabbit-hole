# ------ Command parameters ------
param(
    [string]$Version = "",
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA "rh"),
    [switch]$NoCloudflared,
    [switch]$DryRun
)

$Repo = "DavidMANZI-093/rabbit-hole"

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

function Invoke-Step([string]$Label, [scriptblock]$Action) {
    if ($DryRun) { Write-Host "  + $Label" }
    else { Write-Host "  $Label"; & $Action }
}

# ------ Platform detection ------

if (-not [Environment]::Is64BitOperatingSystem) {
    throw "only 64-bit Windows is supported (no x86 rh builds published)"
}
$Target = "x86_64-pc-windows-gnu"
$CfAsset = "cloudflared-windows-amd64.exe"

# ------ Version resolution ------

if ([string]::IsNullOrEmpty($Version)) {
    Write-Host "  resolving latest rh release..."
    $latest = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    $Version = $latest.tag_name
    if ([string]::IsNullOrEmpty($Version)) { throw "could not resolve latest release (pass -Version vX.Y.Z)" }
}
Write-Host "  rh $Version for $Target"

# ------ Temporary working directory ------

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("rh-install-" + [Guid]::NewGuid().ToString("N"))

# ------ Installation (rh & cloudflared) ------
Invoke-Step "download rh $Version" { New-Item -ItemType Directory -Path $tmp | Out-Null }
try {
    $binDir = Join-Path $Prefix "bin"
    # rh binary (skipped when the installed one already matches the target)
    $installedRh = ""
    $rhBin = Join-Path $binDir "rh.exe"
    if (Test-Path $rhBin) {
        try { $installedRh = ((& $rhBin --version 2>$null | Select-Object -First 1) -split '\s+')[1] } catch { $installedRh = "" }
    }
    if (-not [string]::IsNullOrEmpty($installedRh) -and ("v$installedRh" -eq $Version)) {
        Write-Host "  rh $Version already installed — skipping"
    } else {
        $zipUrl = "https://github.com/$Repo/releases/download/$Version/rh-$Version-$Target.zip"
        $zipPath = Join-Path $tmp "rh.zip"
        Invoke-Step "download $zipUrl" { Invoke-WebRequest -Uri $zipUrl -OutFile $zipPath }
        Invoke-Step "install $binDir\rh.exe" {
            New-Item -ItemType Directory -Path $binDir -Force | Out-Null
            Expand-Archive -Path $zipPath -DestinationPath $tmp -Force
            Copy-Item (Join-Path $tmp "rh.exe") (Join-Path $binDir "rh.exe") -Force
        }
    }

    # Pinned cloudflared (versions/hashes from the pin file)
    if (-not $NoCloudflared) {
        $pinUrl = "https://raw.githubusercontent.com/$Repo/$Version/third-party/cloudflared.pin"
        if ($DryRun) {
            Write-Host "  + download pin file $pinUrl"
            $cfVersion = "<from pin>"; $cfHash = "<from pin>"
        } else {
            $pin = (Invoke-WebRequest -Uri $pinUrl -UseBasicParsing).Content
            $cfVersion = (($pin -split "`n") -match '^PINNED=' | Select-Object -First 1) -replace '^PINNED=', ''
            $cfVersion = $cfVersion.Trim()
            $hashLine = (($pin -split "`n") -match "^$CfAsset\s" | Select-Object -First 1)
            $cfHash = ($hashLine -split '\s+')[1]
            if ([string]::IsNullOrEmpty($cfVersion) -or [string]::IsNullOrEmpty($cfHash)) {
                throw "pin file lacks PINNED= or hash for $CfAsset"
            }
        }
        Write-Host "  downloading cloudflared $cfVersion ($CfAsset)..."
        $installedCf = ""
        $cfBin = Join-Path $binDir "cloudflared.exe"
        if (Test-Path $cfBin) {
            try {
                $firstLine = @(& $cfBin --version 2>$null)[0]
                if ($firstLine -match '(\d{4}\.\d+\.\d+)') { $installedCf = $Matches[1] }
            } catch { $installedCf = "" }
        }
        if (-not [string]::IsNullOrEmpty($installedCf) -and ($installedCf -eq $cfVersion)) {
            Write-Host "  cloudflared $cfVersion already installed — skipping"
        } else {
            $cfUrl = "https://github.com/cloudflare/cloudflared/releases/download/$cfVersion/$CfAsset"
            $cfPath = Join-Path $tmp $CfAsset
            Invoke-Step "download $cfUrl" { Invoke-WebRequest -Uri $cfUrl -OutFile $cfPath }
            if ($DryRun) {
                Write-Host "  + verify sha256 $cfHash"
            } else {
                $actual = (Get-FileHash -Path $cfPath -Algorithm SHA256).Hash
                if ($actual.ToLower() -ne $cfHash.ToLower()) { throw "cloudflared checksum mismatch" }
            }
            Invoke-Step "install $binDir\cloudflared.exe" {
                Copy-Item $cfPath (Join-Path $binDir "cloudflared.exe") -Force
            }
        }
    } else {
        Write-Host "  skipping cloudflared (-NoCloudflared); rh will use PATH or LAN-only"
    }

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -split ";" -notcontains $binDir) {
        Invoke-Step "add $binDir to user PATH" {
            [Environment]::SetEnvironmentVariable("Path", "$userPath;$binDir", "User")
        }
    }
    Write-Host "  to uninstall later: irm https://raw.githubusercontent.com/$Repo/$Version/uninstall.ps1 | iex"
    Write-Host "  done. Run:  rh check"
} finally {
    if (-not $DryRun) { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
}

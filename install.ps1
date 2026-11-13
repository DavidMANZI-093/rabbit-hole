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

# Primary: the github.com "latest release" redirect (no API, no rate limit).
# Fallback: the REST API. Either way the result must look like a tag.
function Resolve-LatestVersion {
    try {
        $req = [Net.HttpWebRequest]::Create("https://github.com/$Repo/releases/latest")
        $req.AllowAutoRedirect = $false
        $req.Method = "HEAD"
        $req.Timeout = 15000
        $resp = $req.GetResponse()
        try { $location = $resp.Headers["Location"] } finally { $resp.Close() }
        if ($location -match '/releases/tag/(v[0-9][^/]*)$') { return $Matches[1] }
    } catch { }
    try {
        $tag = (Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -TimeoutSec 15).tag_name
        if ($tag -match '^v[0-9]') { return $tag }
    } catch { }
    return ""
}

if ([string]::IsNullOrEmpty($Version)) {
    Write-Host "  resolving latest rh release..."
    $Version = Resolve-LatestVersion
    if ([string]::IsNullOrEmpty($Version)) {
        throw "could not resolve latest release from github.com (check connectivity, or pass -Version vX.Y.Z)"
    }
}
# accept "1.2.3" as well as "v1.2.3"
if (-not $Version.StartsWith("v")) { $Version = "v$Version" }
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
        Invoke-Step "download $zipUrl" {
            try { Invoke-WebRequest -Uri $zipUrl -OutFile $zipPath -UseBasicParsing }
            catch { throw "could not download rh $Version for $Target from $zipUrl (does the release exist?): $($_.Exception.Message)" }
        }
        # Releases from v1.0.1 publish SHA256SUMS; older ones are installed unverified.
        $zipName = Split-Path $zipUrl -Leaf
        if ($DryRun) {
            Write-Host "  + verify sha256 from SHA256SUMS (when published)"
        } else {
            $sums = $null
            try { $sums = (Invoke-WebRequest -Uri "https://github.com/$Repo/releases/download/$Version/SHA256SUMS" -UseBasicParsing).Content } catch { }
            if ($sums -is [byte[]]) { $sums = [Text.Encoding]::UTF8.GetString($sums) }
            if ([string]::IsNullOrEmpty($sums)) {
                Write-Host "  warning: $Version publishes no SHA256SUMS; skipping rh checksum"
            } else {
                $line = ($sums -split "`n") | Where-Object { ($_ -split '\s+')[1] -in @($zipName, "*$zipName") } | Select-Object -First 1
                if (-not $line) { throw "SHA256SUMS has no entry for $zipName" }
                $expected = ($line -split '\s+')[0]
                $actual = (Get-FileHash -Path $zipPath -Algorithm SHA256).Hash
                if ($actual.ToLower() -ne $expected.ToLower()) { throw "rh checksum mismatch" }
            }
        }
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

<#
.SYNOPSIS
  Packages the Lowtide desktop app as an MSIX.

.DESCRIPTION
  Produces Lowtide-<version>-x64.msix (unsigned, for Microsoft Store upload; the Store signs it).
  With -TestSign it also produces Lowtide-<version>-x64-test.msix signed with a throwaway
  self-signed certificate, plus Lowtide-test.cer, so the package can be sideloaded and tested.

  Requires the Windows 10/11 SDK (makeappx.exe, signtool.exe). Works in Windows PowerShell 5.1.

.EXAMPLE
  cargo build --release -p lowtide-app   # or: cd crates/gui; npx tauri build
  ./crates/gui/msix/build-msix.ps1 -Exe target/release/lowtide-app.exe -Version 0.2.0 -TestSign
#>
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$OutDir = "dist",
    [string]$IdentityName = $(if ($env:MSIX_IDENTITY_NAME) { $env:MSIX_IDENTITY_NAME } else { "Lowtide" }),
    [string]$Publisher = $(if ($env:MSIX_PUBLISHER) { $env:MSIX_PUBLISHER } else { "CN=Lowtide Test" }),
    [string]$PublisherDisplayName = $(if ($env:MSIX_PUBLISHER_DISPLAY_NAME) { $env:MSIX_PUBLISHER_DISPLAY_NAME } else { "Lowtide" }),
    [switch]$TestSign
)

$ErrorActionPreference = "Stop"
$gui = Split-Path $PSScriptRoot -Parent

function Find-SdkTool([string]$Name) {
    $root = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $tool = Get-ChildItem $root -Recurse -Filter $Name -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq "x64" -and $_.Directory.Parent.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $tool) { throw "$Name not found under $root. Install the Windows 10/11 SDK." }
    $tool.FullName
}

function Invoke-Tool([string]$Path, [string[]]$Arguments) {
    & $Path @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$(Split-Path $Path -Leaf) failed with exit code $LASTEXITCODE" }
}

if (-not (Test-Path $Exe)) { throw "App executable not found: $Exe (build the desktop app first)" }

# MSIX wants a four-part version; the Store requires the last part to be 0.
$core = ($Version.TrimStart("v") -split '[-+]')[0]
$parts = @($core.Split("."))
while ($parts.Count -lt 3) { $parts += "0" }
$msixVersion = "$($parts[0]).$($parts[1]).$($parts[2]).0"

New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$layout = Join-Path $OutDir "msix-layout"
if (Test-Path $layout) { Remove-Item $layout -Recurse -Force }
New-Item -ItemType Directory -Force (Join-Path $layout "Assets") | Out-Null

Copy-Item $Exe (Join-Path $layout "Lowtide.exe")
foreach ($icon in "StoreLogo.png", "Square44x44Logo.png", "Square150x150Logo.png") {
    Copy-Item (Join-Path $gui "icons\$icon") (Join-Path $layout "Assets\$icon")
}

$esc = { param($s) [System.Security.SecurityElement]::Escape($s) }
$manifest = Get-Content (Join-Path $PSScriptRoot "AppxManifest.xml") -Raw
$manifest = $manifest.Replace("{{IDENTITY_NAME}}", (& $esc $IdentityName))
$manifest = $manifest.Replace("{{PUBLISHER_DISPLAY_NAME}}", (& $esc $PublisherDisplayName))
$manifest = $manifest.Replace("{{PUBLISHER}}", (& $esc $Publisher))
$manifest = $manifest.Replace("{{VERSION}}", $msixVersion)
[System.IO.File]::WriteAllText((Join-Path $layout "AppxManifest.xml"), $manifest, (New-Object System.Text.UTF8Encoding $false))

$makeappx = Find-SdkTool "makeappx.exe"
$msix = Join-Path $OutDir "Lowtide-$core-x64.msix"
Invoke-Tool $makeappx @("pack", "/d", $layout, "/p", $msix, "/o")
Write-Host "Store package: $msix (version $msixVersion, publisher $Publisher)"

if ($TestSign) {
    $signtool = Find-SdkTool "signtool.exe"
    $cert = New-SelfSignedCertificate -Type Custom -Subject $Publisher -KeyUsage DigitalSignature `
        -FriendlyName "Lowtide MSIX test" -CertStoreLocation "Cert:\CurrentUser\My" `
        -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}") `
        -NotAfter (Get-Date).AddMonths(6)
    $pfx = Join-Path $env:TEMP "lowtide-msix-test.pfx"
    $plain = [guid]::NewGuid().ToString()
    try {
        Export-PfxCertificate -Cert $cert -FilePath $pfx -Password (ConvertTo-SecureString $plain -AsPlainText -Force) | Out-Null
        Export-Certificate -Cert $cert -FilePath (Join-Path $OutDir "Lowtide-test.cer") | Out-Null
        $testMsix = Join-Path $OutDir "Lowtide-$core-x64-test.msix"
        Copy-Item $msix $testMsix -Force
        Invoke-Tool $signtool @("sign", "/fd", "SHA256", "/f", $pfx, "/p", $plain, $testMsix)
        Write-Host "Test package: $testMsix (trust Lowtide-test.cer before installing)"
    }
    finally {
        Remove-Item $pfx -ErrorAction SilentlyContinue
        Remove-Item "Cert:\CurrentUser\My\$($cert.Thumbprint)" -ErrorAction SilentlyContinue
    }
}

Remove-Item $layout -Recurse -Force

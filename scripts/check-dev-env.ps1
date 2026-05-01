$ErrorActionPreference = "Stop"
$Failures = 0
$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path

function Write-Section($title) {
  Write-Host ""
  Write-Host "== $title =="
}

function Invoke-Check($label, [scriptblock]$command) {
  Write-Host "[check] $label"
  try {
    & $command
    if ($LASTEXITCODE -ne 0) {
      throw "exit code $LASTEXITCODE"
    }
  } catch {
    $script:Failures += 1
    Write-Host "  missing or failed: $label"
    Write-Host "  $($_.Exception.Message)"
  }
}

Write-Section "Versions"
Invoke-Check "Node.js" { node --version }
Invoke-Check "npm" { npm.cmd --version }
Invoke-Check "rustc" { rustc --version }
Invoke-Check "cargo" { cargo --version }
Invoke-Check "local Tauri CLI" { npm.cmd exec tauri -- --version }

Write-Section "Visual Studio Build Tools"
$vswhere = "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"
if (Test-Path $vswhere) {
  Invoke-Check "Visual Studio C++ build tools" {
    & $vswhere -latest -products * -requires Microsoft.VisualStudio.Workload.VCTools -property installationPath
  }
} else {
  $Failures += 1
  Write-Host "vswhere.exe not found"
  Write-Host "Install Visual Studio 2022 Build Tools with Desktop development with C++."
}

Write-Section "WebView2 Runtime"
$webView2 = "C:\Program Files (x86)\Microsoft\EdgeWebView\Application"
if (Test-Path $webView2) {
  Write-Host $webView2
} else {
  $Failures += 1
  Write-Host "WebView2 runtime not found"
  Write-Host "Install Microsoft Edge WebView2 Runtime."
}

Write-Section "Smart App Control"
try {
  $ciPolicy = Get-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy"
  if ($null -ne $ciPolicy) {
    Write-Host "VerifiedAndReputablePolicyState:" $ciPolicy.VerifiedAndReputablePolicyState
    Write-Host "SAC_EnforcementReason:" $ciPolicy.SAC_EnforcementReason
  } else {
    Write-Host "Code Integrity policy not found"
  }
} catch {
  Write-Host "Could not read Code Integrity policy: $($_.Exception.Message)"
}

Write-Section "Recent Code Integrity Events"
try {
  Get-WinEvent -LogName "Microsoft-Windows-CodeIntegrity/Operational" -MaxEvents 5 |
    Select-Object TimeCreated, Id, LevelDisplayName, Message |
    Format-List
} catch {
  Write-Host "Could not read recent Code Integrity events: $($_.Exception.Message)"
}

Write-Section "Hint"
Write-Host "If Smart App Control is ON, open: Windows Security > App & browser control > Smart App Control settings"
Write-Host "Then run: npm run tauri:dev"

if ($Failures -gt 0) {
  Write-Host ""
  Write-Host "Environment is not ready: $Failures check(s) failed."
  exit 1
}

Write-Host ""
Write-Host "Environment looks ready for the current mykvm desktop prototype."

$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path
$env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\CargoTarget\mykvm"

Write-Host "Starting mykvm Tauri dev environment..."
Write-Host "CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR"

npm.cmd run tauri:dev
exit $LASTEXITCODE

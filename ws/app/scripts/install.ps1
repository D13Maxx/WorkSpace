# Build F.R.I.D.A.Y and put a shortcut to it on the desktop.
#
# Run again after changing the app to update the copy the shortcut opens:
#   powershell -ExecutionPolicy Bypass -File ws\app\scripts\install.ps1
#
# It installs a copy rather than pointing at the build output, so `cargo clean`
# cannot break the shortcut and a rebuild never fights the running app.

$ErrorActionPreference = 'Stop'
$app = Split-Path $PSScriptRoot -Parent
$dest = Join-Path $env:LOCALAPPDATA 'Programs\F.R.I.D.A.Y'
$installed = Join-Path $dest 'FRIDAY.exe'

if (Get-Process FRIDAY -ErrorAction SilentlyContinue) {
    throw 'F.R.I.D.A.Y is running. Close it, then run this again.'
}

Write-Host 'Building a release copy (several minutes the first time)...'
Push-Location $app
try {
    # --no-bundle: only the app itself. Installers would download WiX/NSIS.
    npm run tauri build -- --no-bundle
    if ($LASTEXITCODE -ne 0) { throw "the build failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}

$built = Join-Path $app '..\target\release\ws-app.exe'
if (-not (Test-Path $built)) { throw "the build finished but $built is missing" }

New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item $built $installed -Force

# Understand-Anything's /understand pipeline, which the app finds beside it.
$pipeline = Join-Path $app 'src-tauri\ua-pipeline'
if (-not (Test-Path (Join-Path $pipeline 'agents'))) {
    Write-Host 'Building the Understand-Anything pipeline (downloads UA once)...'
    node (Join-Path $PSScriptRoot 'build-ua-pipeline.mjs')
    if ($LASTEXITCODE -ne 0) { throw "the pipeline build failed (exit $LASTEXITCODE)" }
}
$installedPipeline = Join-Path $dest 'ua-pipeline'
if (Test-Path $installedPipeline) { Remove-Item $installedPipeline -Recurse -Force }
Copy-Item $pipeline $installedPipeline -Recurse

$link = Join-Path ([Environment]::GetFolderPath('Desktop')) 'F.R.I.D.A.Y.lnk'
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($link)
$shortcut.TargetPath = $installed
$shortcut.WorkingDirectory = $dest
$shortcut.IconLocation = "$installed,0"
$shortcut.Description = 'F.R.I.D.A.Y'
$shortcut.Save()

# Double-clicking a .blueprint opens it in FRIDAY (this user only; the .NET
# calls add or update values and never remove what Windows keeps there).
$classes = 'HKEY_CURRENT_USER\Software\Classes'
[Microsoft.Win32.Registry]::SetValue("$classes\.blueprint", '', 'FRIDAY.Blueprint')
[Microsoft.Win32.Registry]::SetValue("$classes\.blueprint\OpenWithProgids", 'FRIDAY.Blueprint', '')
[Microsoft.Win32.Registry]::SetValue("$classes\FRIDAY.Blueprint", '', 'F.R.I.D.A.Y blueprint')
[Microsoft.Win32.Registry]::SetValue("$classes\FRIDAY.Blueprint\DefaultIcon", '', "`"$installed`",0")
[Microsoft.Win32.Registry]::SetValue("$classes\FRIDAY.Blueprint\shell\open\command", '', "`"$installed`" `"%1`"")

Write-Host "Installed $installed"
Write-Host "Shortcut  $link"

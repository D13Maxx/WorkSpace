# Build Workspace and put a shortcut to it on the desktop.
#
# Run again after changing the app to update the copy the shortcut opens:
#   powershell -ExecutionPolicy Bypass -File ws\app\scripts\install.ps1
#
# It installs a copy rather than pointing at the build output, so `cargo clean`
# cannot break the shortcut and a rebuild never fights the running app.

$ErrorActionPreference = 'Stop'
$app = Split-Path $PSScriptRoot -Parent
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Workspace'
$installed = Join-Path $dest 'Workspace.exe'

if (Get-Process Workspace, FRIDAY -ErrorAction SilentlyContinue) {
    throw 'Workspace is running. Close it, then run this again.'
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

$link = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Workspace.lnk'
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($link)
$shortcut.TargetPath = $installed
$shortcut.WorkingDirectory = $dest
$shortcut.IconLocation = "$installed,0"
$shortcut.Description = 'Workspace'
$shortcut.Save()

# Double-clicking a .blueprint opens it in Workspace (this user only; the .NET
# calls add or update values and never remove what Windows keeps there).
$classes = 'HKEY_CURRENT_USER\Software\Classes'
[Microsoft.Win32.Registry]::SetValue("$classes\.blueprint", '', 'Workspace.Blueprint')
[Microsoft.Win32.Registry]::SetValue("$classes\.blueprint\OpenWithProgids", 'Workspace.Blueprint', '')
[Microsoft.Win32.Registry]::SetValue("$classes\Workspace.Blueprint", '', 'Workspace blueprint')
[Microsoft.Win32.Registry]::SetValue("$classes\Workspace.Blueprint\DefaultIcon", '', "`"$installed`",0")
[Microsoft.Win32.Registry]::SetValue("$classes\Workspace.Blueprint\shell\open\command", '', "`"$installed`" `"%1`"")

# The app used to be called F.R.I.D.A.Y: remove that copy of it (the app only;
# chats, keys and the inventory live in the app data folder, which is unchanged).
$old = Join-Path $env:LOCALAPPDATA 'Programs\F.R.I.D.A.Y'
if (Test-Path $old) { Remove-Item $old -Recurse -Force }
$oldLink = Join-Path ([Environment]::GetFolderPath('Desktop')) 'F.R.I.D.A.Y.lnk'
if (Test-Path $oldLink) { Remove-Item $oldLink -Force }
[Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree('Software\Classes\FRIDAY.Blueprint', $false)
$progids = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Classes\.blueprint\OpenWithProgids', $true)
if ($progids) { $progids.DeleteValue('FRIDAY.Blueprint', $false); $progids.Close() }

Write-Host "Installed $installed"
Write-Host "Shortcut  $link"

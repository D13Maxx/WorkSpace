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

# Windows caches icons by file path, so a new icon inside the same exe never
# shows. The shortcut and .blueprint files use a copy named by its contents.
Get-ChildItem $dest -Filter 'icon-*.ico' | Remove-Item -Force
$ico = Join-Path $app 'src-tauri\icons\icon.ico'
$iconFile = Join-Path $dest ("icon-{0}.ico" -f (Get-FileHash $ico -Algorithm SHA256).Hash.Substring(0, 8))
Copy-Item $ico $iconFile

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
$shortcut.IconLocation = "$iconFile,0"
$shortcut.Description = 'Workspace'
$shortcut.Save()

# Double-clicking a .blueprint opens it in Workspace (this user only; the .NET
# calls add or update values and never remove what Windows keeps there).
$classes = 'HKEY_CURRENT_USER\Software\Classes'
[Microsoft.Win32.Registry]::SetValue("$classes\.blueprint", '', 'Workspace.Blueprint')
[Microsoft.Win32.Registry]::SetValue("$classes\.blueprint\OpenWithProgids", 'Workspace.Blueprint', '')
[Microsoft.Win32.Registry]::SetValue("$classes\Workspace.Blueprint", '', 'Workspace blueprint')
[Microsoft.Win32.Registry]::SetValue("$classes\Workspace.Blueprint\DefaultIcon", '', "`"$iconFile`",0")
[Microsoft.Win32.Registry]::SetValue("$classes\Workspace.Blueprint\shell\open\command", '', "`"$installed`" `"%1`"")
# Without this, Windows sees a "new app" for .blueprint and asks on every open.
[Microsoft.Win32.Registry]::SetValue('HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\ApplicationAssociationToasts', 'Workspace.Blueprint_.blueprint', 0)

# The app used to be called F.R.I.D.A.Y: remove that copy of it (the app only;
# chats, keys and the inventory live in the app data folder, which is unchanged).
$old = Join-Path $env:LOCALAPPDATA 'Programs\F.R.I.D.A.Y'
if (Test-Path $old) { Remove-Item $old -Recurse -Force }
$oldLink = Join-Path ([Environment]::GetFolderPath('Desktop')) 'F.R.I.D.A.Y.lnk'
if (Test-Path $oldLink) { Remove-Item $oldLink -Force }
[Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree('Software\Classes\FRIDAY.Blueprint', $false)
[Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree('Software\Classes\Applications\FRIDAY.exe', $false)
foreach ($k in 'Software\Classes\.blueprint\OpenWithProgids', 'Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.blueprint\OpenWithProgids') {
    $progids = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($k, $true)
    if ($progids) { $progids.DeleteValue('FRIDAY.Blueprint', $false); $progids.Close() }
}
# Picking FRIDAY.exe through "Open with" made Windows its own entry for it,
# which stays the remembered default: point it here, or every double-click
# asks which app to use.
$auto = "$classes\blueprint_auto_file\shell\open\command"
if ([string][Microsoft.Win32.Registry]::GetValue($auto, '', '') -match 'FRIDAY\.exe') {
    [Microsoft.Win32.Registry]::SetValue($auto, '', "`"$installed`" `"%1`"")
}

# Tell Explorer file types and icons changed, so it redraws them now.
Add-Type -Namespace Win -Name Shell -MemberDefinition '[DllImport("shell32.dll")] public static extern void SHChangeNotify(int e, uint f, IntPtr a, IntPtr b);'
[Win.Shell]::SHChangeNotify(0x08000000, 0, [IntPtr]::Zero, [IntPtr]::Zero)

Write-Host "Installed $installed"
Write-Host "Shortcut  $link"

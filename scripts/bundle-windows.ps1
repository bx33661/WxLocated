[CmdletBinding()]
param(
    [ValidateSet('x86_64-pc-windows-msvc')]
    [string]$Target = 'x86_64-pc-windows-msvc',
    [string]$OutputDir = '',
    [switch]$DebugBuild
)

$ErrorActionPreference = 'Stop'
$ProjectRoot = Split-Path $PSScriptRoot -Parent
if ($env:OS -ne 'Windows_NT') { throw 'Run this script on Windows with the MSVC toolchain.' }
if (-not $OutputDir) { $OutputDir = Join-Path $ProjectRoot 'target/bundle/windows' }
$OutputDir = [IO.Path]::GetFullPath($OutputDir)

function Invoke-Cargo {
    param([string[]]$CargoArgs)
    & cargo @CargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo failed ($LASTEXITCODE): $CargoArgs" }
}

Push-Location $ProjectRoot
try {
    & rustup target add $Target
    if ($LASTEXITCODE -ne 0) { throw 'Unable to install the Windows Rust target.' }
    # GPUI compiles DirectX shaders in release builds. Its build script also
    # discovers fxc.exe from the Windows SDK, or accepts GPUI_FXC_PATH.
    $BuildArgs = @('build', '--locked', '--target', $Target)
    $ProfileDir = 'debug'
    if (-not $DebugBuild) { $BuildArgs += '--release'; $ProfileDir = 'release' }
    # Build separately to preserve the CLI's own dependency feature set.
    Invoke-Cargo -CargoArgs ($BuildArgs + @('-p', 'wxlocated-cli'))
    Invoke-Cargo -CargoArgs ($BuildArgs + @('-p', 'wxlocated-app'))
    $MetadataJson = & cargo metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) { throw 'Unable to read Cargo metadata.' }
    $Metadata = $MetadataJson | ConvertFrom-Json
    $Version = ($Metadata.packages | Where-Object name -eq 'wxlocated-app').version
    $BinaryDir = Join-Path $Metadata.target_directory "$Target/$ProfileDir"
    $PackageName = "WxLocated-$Version-windows-x64"
    if ($DebugBuild) { $PackageName += '-debug' }
    $PackageDir = Join-Path $OutputDir $PackageName
    New-Item -ItemType Directory -Force -Path $PackageDir | Out-Null
    Copy-Item (Join-Path $BinaryDir 'wxlocated-app.exe') $PackageDir -Force
    Copy-Item (Join-Path $BinaryDir 'wxlocated.exe') $PackageDir -Force
    Copy-Item (Join-Path $ProjectRoot 'README.md') $PackageDir -Force
    Copy-Item (Join-Path $ProjectRoot 'TODO-windows.md') $PackageDir -Force
    Copy-Item (Join-Path $PSScriptRoot 'install-windows.ps1') $PackageDir -Force
    @'
WxLocated for Windows x64

Launch wxlocated-app.exe. Keep wxlocated.exe beside it for the Agent CLI.
Requires Windows 10/11 x64 and the Microsoft Visual C++ 2015-2022 x64 runtime.
Use Settings > Agent to install the CLI for your user; reopen terminals afterwards.
Optionally run install-windows.ps1 to create a Start menu shortcut and register
.wxapkg under Open With. It does not change your default application.
Run install-windows.ps1 -Uninstall before moving or removing this folder.
'@ | Set-Content (Join-Path $PackageDir 'Readme-Windows.txt') -Encoding UTF8
    $Archive = Join-Path $OutputDir "$PackageName.zip"
    Compress-Archive -Path $PackageDir -DestinationPath $Archive -Force
    $Hash = (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLowerInvariant()
    "$Hash  $PackageName.zip" | Set-Content "$Archive.sha256" -Encoding ASCII
    Write-Output $Archive
}
finally {
    Pop-Location
}

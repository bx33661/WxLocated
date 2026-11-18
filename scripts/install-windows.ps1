[CmdletBinding()]
param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$Executable = Join-Path $PSScriptRoot 'wxlocated-app.exe'
$ClassName = 'WxLocated.wxapkg'
$ClassKey = "HKCU:\Software\Classes\$ClassName"
$OpenWithKey = 'HKCU:\Software\Classes\.wxapkg\OpenWithProgids'
$Programs = [Environment]::GetFolderPath('Programs')
$ShortcutPath = Join-Path $Programs 'WxLocated.lnk'
$Shell = New-Object -ComObject WScript.Shell

if ($Uninstall) {
    # Remove only registrations still pointing to this portable installation.
    $CommandKey = Join-Path $ClassKey 'shell\open\command'
    if ((Test-Path $CommandKey) -and ((Get-Item $CommandKey).GetValue('') -eq "`"$Executable`" `"%1`"")) {
        Remove-Item $ClassKey -Recurse -Force
        if (Test-Path $OpenWithKey) {
            Remove-ItemProperty $OpenWithKey -Name $ClassName -ErrorAction SilentlyContinue
        }
    }
    if ((Test-Path $ShortcutPath) -and ($Shell.CreateShortcut($ShortcutPath).TargetPath -eq $Executable)) {
        Remove-Item $ShortcutPath -Force
    }
    Write-Output 'Removed the Start menu shortcut and Open With registration for this folder.'
    return
}

if (-not (Test-Path $Executable)) { throw 'Keep this script beside wxlocated-app.exe in the extracted package.' }
New-Item -Path "$ClassKey\shell\open\command" -Force | Out-Null
Set-Item $ClassKey -Value 'WeChat mini-program package (WxLocated)'
Set-Item "$ClassKey\shell\open\command" -Value "`"$Executable`" `"%1`""
New-Item "$ClassKey\DefaultIcon" -Force | Out-Null
Set-Item "$ClassKey\DefaultIcon" -Value "`"$Executable`",0"
New-Item $OpenWithKey -Force | Out-Null
New-ItemProperty $OpenWithKey -Name $ClassName -PropertyType String -Value '' -Force | Out-Null
$Shortcut = $Shell.CreateShortcut($ShortcutPath)
$Shortcut.TargetPath = $Executable
$Shortcut.WorkingDirectory = $PSScriptRoot
$Shortcut.IconLocation = "$Executable,0"
$Shortcut.Save()
Write-Output 'Added WxLocated to the Start menu and .wxapkg Open With. The default application is unchanged.'

<#!
.SYNOPSIS
清理米家空调产生的本机凭据、设置、缓存与日志。

.DESCRIPTION
删除当前 Windows 用户在 %APPDATA%\米家空调 和 %LOCALAPPDATA%\米家空调
下的应用数据；同时清理项目根目录及 dist 目录中兼容旧版便携包的凭据文件。

此操作会移除 device.json、cloud-session.json、thermometer.json、settings.json
及其临时文件。下次启动应用需要重新扫码登录并重新设置偏好。

如果曾设置 MIAC_TRACE 把诊断日志写到其他目录，请通过 -TraceLogPath 指定该文件。

.EXAMPLE
.\scripts\clear-miac-data.ps1 -WhatIf

.EXAMPLE
.\scripts\clear-miac-data.ps1 -Confirm:$false

.EXAMPLE
.\scripts\clear-miac-data.ps1 -TraceLogPath 'D:\Logs\miac-trace.log' -Confirm:$false
#>
[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param(
    [string[]]$TraceLogPath = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-NormalizedPath([string]$Path) {
    return [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
}

function Stop-MiacProcess {
    foreach ($name in @('miac-app', 'miac-cli')) {
        Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object {
            if ($PSCmdlet.ShouldProcess("$($_.ProcessName) (PID $($_.Id))", '停止进程以释放应用数据文件')) {
                Stop-Process -Id $_.Id -Force
                Write-Host "已停止 $($_.ProcessName)（PID $($_.Id)）。"
            }
        }
    }
}

function Remove-OwnedDirectory([string]$Path) {
    $fullPath = Get-NormalizedPath $Path
    if (-not (Test-Path -LiteralPath $fullPath -PathType Container)) {
        Write-Host "未找到：$fullPath"
        return
    }

    if ($PSCmdlet.ShouldProcess($fullPath, '删除米家空调应用数据目录')) {
        Remove-Item -LiteralPath $fullPath -Recurse -Force
        Write-Host "已删除：$fullPath"
    }
}

function Remove-PortableArtifacts([string]$Directory) {
    $fullDirectory = Get-NormalizedPath $Directory
    if (-not (Test-Path -LiteralPath $fullDirectory -PathType Container)) {
        return
    }

    $fileNames = @(
        'device.json',
        'cloud-session.json',
        'thermometer.json',
        'settings.json',
        'credentials.json'
    )
    $tempPatterns = @(
        '.device.json.*.tmp',
        '.cloud-session.json.*.tmp',
        '.thermometer.json.*.tmp',
        '.settings.json.*.tmp',
        'miac*.log',
        '*miac*.log'
    )

    $targets = foreach ($name in $fileNames) {
        Join-Path $fullDirectory $name
    }
    foreach ($pattern in $tempPatterns) {
        Get-ChildItem -LiteralPath $fullDirectory -File -Filter $pattern -ErrorAction SilentlyContinue |
            ForEach-Object { $_.FullName }
    }

    foreach ($target in $targets | Select-Object -Unique) {
        if ((Test-Path -LiteralPath $target -PathType Leaf) -and
            $PSCmdlet.ShouldProcess($target, '删除米家空调凭据、设置或日志文件')) {
            Remove-Item -LiteralPath $target -Force
            Write-Host "已删除：$target"
        }
    }
}

function Remove-TraceLog([string]$Path) {
    $fullPath = Get-NormalizedPath $Path
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        Write-Host "未找到追踪日志：$fullPath"
        return
    }

    if ($PSCmdlet.ShouldProcess($fullPath, '删除 MIAC_TRACE 追踪日志')) {
        Remove-Item -LiteralPath $fullPath -Force
        Write-Host "已删除追踪日志：$fullPath"
    }
}

$projectRoot = Split-Path -Parent $PSScriptRoot
$appDataDirectory = Join-Path ([Environment]::GetFolderPath('ApplicationData')) '米家空调'
$localAppDataDirectory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) '米家空调'

Stop-MiacProcess
Remove-OwnedDirectory $appDataDirectory
Remove-OwnedDirectory $localAppDataDirectory
Remove-PortableArtifacts $projectRoot
Remove-PortableArtifacts (Join-Path $projectRoot 'dist')

$traceTargets = @($TraceLogPath)
if ($env:MIAC_TRACE) {
    $traceTargets += $env:MIAC_TRACE
}
foreach ($tracePath in $traceTargets | Where-Object { $_ } | Select-Object -Unique) {
    Remove-TraceLog $tracePath
}

if ($WhatIfPreference) {
    Write-Host '预览完成：未删除任何文件。'
} else {
    Write-Host '米家空调的本机凭据、设置、缓存和指定日志已处理。'
}

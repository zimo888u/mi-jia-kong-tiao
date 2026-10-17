@echo off
chcp 65001 >nul
setlocal EnableExtensions DisableDelayedExpansion
if not "%~1"=="" exit /b 2
rem Deletion is handled only by the native helper with directory handle locks.
set "MIAC_CLEANUP_LAUNCHER=%~dp0"
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "$ErrorActionPreference='Stop'; $base=$env:MIAC_CLEANUP_LAUNCHER; $names=@('target\debug\miac-app.exe','target\release\miac-app.exe','dist\miac-app.exe','dist\米家空调.exe','miac-app.exe','米家空调.exe'); $exe=$names | ForEach-Object { Join-Path $base $_ } | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1; if (-not $exe) { Write-Error 'Build the updated app first, or use Cleanup in app settings.'; exit 1 }; Start-Process -FilePath $exe -ArgumentList @('--cleanup-data-v3',[string]$PID) -WindowStyle Hidden"
if errorlevel 1 pause
exit /b %errorlevel%

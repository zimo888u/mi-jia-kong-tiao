@echo off
@chcp 65001 >nul
setlocal

rem 双击运行：结束米家空调并清理凭据、设置、缓存和日志。
rem 删除后需重新扫码登录；此脚本不删除程序文件或项目源码。

taskkill /f /im miac-app.exe >nul 2>&1
taskkill /f /im miac-cli.exe >nul 2>&1

rem 默认应用数据（当前 Rust 版与旧版 Electron 客户端）。
rd /s /q "%APPDATA%\米家空调" >nul 2>&1
rd /s /q "%LOCALAPPDATA%\米家空调" >nul 2>&1

rem 兼容旧版便携包：清理脚本所在项目目录及 dist 目录旁的凭据和日志。
for %%D in ("%~dp0" "%~dp0dist") do (
    del /f /q "%%~fD\device.json" >nul 2>&1
    del /f /q "%%~fD\cloud-session.json" >nul 2>&1
    del /f /q "%%~fD\thermometer.json" >nul 2>&1
    del /f /q "%%~fD\settings.json" >nul 2>&1
    del /f /q "%%~fD\credentials.json" >nul 2>&1
    del /f /q "%%~fD\.device.json.*.tmp" >nul 2>&1
    del /f /q "%%~fD\.cloud-session.json.*.tmp" >nul 2>&1
    del /f /q "%%~fD\.thermometer.json.*.tmp" >nul 2>&1
    del /f /q "%%~fD\.settings.json.*.tmp" >nul 2>&1
    del /f /q "%%~fD\*.log" >nul 2>&1
)

rem 若启动时通过 MIAC_TRACE 指定了外部追踪日志，也一并删除。
if not "%MIAC_TRACE%"=="" del /f /q "%MIAC_TRACE%" >nul 2>&1

echo 米家空调的日志、缓存、设置和凭据已清理。
endlocal

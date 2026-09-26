@echo off
chcp 65001 >nul
setlocal EnableExtensions DisableDelayedExpansion
set "failed=0"
set "testmode=0"
if /i "%~1"=="--test" goto testmode
goto start

:testmode
rem A copied script and marker are required for isolated tests.
if not exist "%~dp0.cleanup-test-root" exit /b 2
set "testmode=1"
set "APPDATA=%~dp0roaming"
set "LOCALAPPDATA=%~dp0local"

:start
echo 仅清理米家空调数据；保留 EXE、源码和其他文件。
echo 删除的凭据不可恢复，完成后需重新扫码登录。
if "%testmode%"=="1" goto clean
call :stop "miac-app.exe"
call :stop "miac-cli.exe"
call :stop "米家空调.exe"
if "%failed%"=="1" goto finish

:clean
if not defined APPDATA goto invalid
if not defined LOCALAPPDATA goto invalid
rem Delete only the exact named application subdirectories.
for %%P in ("%APPDATA%" "%LOCALAPPDATA%") do if not "%%~fP"=="%%~dP\" call :remove_app_dir "%%~fP\米家空调"
call :portable "%~dp0."
call :portable "%~dp0dist"
call :portable "%~dp0target\debug"
call :portable "%~dp0target\release"
goto finish

:invalid
echo [失败] 无法确定当前用户的数据目录，已停止。
set "failed=1"
goto finish

:stop
tasklist /fi "IMAGENAME eq %~1" /nh 2>nul | find /i "%~1" >nul
if errorlevel 1 exit /b
taskkill /f /t /im "%~1"
if errorlevel 1 set "failed=1"
exit /b

:remove_app_dir
if not exist "%~1" exit /b
echo [清理目录] "%~1"
rd /s /q "%~1"
if exist "%~1" (
    echo [失败] 目录仍然存在："%~1"
    set "failed=1"
)
exit /b

:portable
if not exist "%~1\" exit /b
rem Exact filenames only; never remove EXEs or the project directory.
for %%F in (device.json cloud-session.json thermometer.json settings.json credentials.json) do call :remove_file "%~1\%%F"
for %%F in ("%~1\.device.json.*.tmp" "%~1\.cloud-session.json.*.tmp" "%~1\.thermometer.json.*.tmp" "%~1\.settings.json.*.tmp" "%~1\*.log") do call :remove_file "%%~fF"
exit /b

:remove_file
if not exist "%~1" exit /b
if exist "%~1\" exit /b
echo [清理文件] "%~1"
del /f /q /a "%~1"
if exist "%~1" (
    echo [失败] 文件仍然存在："%~1"
    set "failed=1"
)
exit /b

:finish
echo.
if "%failed%"=="0" (echo [成功] 已检查清理目标，EXE 和源码未删除。) else (echo [未完成] 上方有未删除项目，请关闭占用程序；若提示拒绝访问，可右键以管理员身份运行。)
if "%testmode%"=="0" pause
exit /b %failed%

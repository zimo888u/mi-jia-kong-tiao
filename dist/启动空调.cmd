@echo off
rem 米家空调 v2（Rust + Slint 原生版）启动脚本
rem 单进程原生应用，不需要 Electron / Chromium / Node.js。
rem 进程名保持 miac-app，便于在任务管理器里与新/旧版本区分。
cd /d "%~dp0"
start "" "miac-app.exe"

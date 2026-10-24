@echo off
rem Start the PC-Monitor daemon detached, appending to daemon.log.
rem Usage: start-daemon.bat [extra args...]
setlocal
cd /d "%~dp0"
set "PY=C:\Users\Administrator\AppData\Local\Programs\Python\Python312\python.exe"
if not exist "%PY%" set "PY=python"
start "" /b "%PY%" fake_daemon.py --source lhm --lenient %* >> daemon.log 2>&1
exit /b 0

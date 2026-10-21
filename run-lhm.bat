@echo off
rem =====================================================================
rem  PC-Monitor, real-sensor stack (M5).
rem  Starts the daemon on the LibreHardwareMonitor source and a terminal
rem  attached to it. Pass a port to override 7802; pass "check" to see
rem  what it would do without starting anything.
rem
rem    run-lhm.bat            daemon + terminal on 7802, real sensors
rem    run-lhm.bat 7803       same, on 7803
rem    run-lhm.bat check      dry run: only report, start nothing
rem =====================================================================
setlocal EnableExtensions
cd /d "%~dp0"

rem ---- arguments: port and/or the word "check" ----
set "PORT=7802"
set "DRYRUN="
for %%a in (%*) do (
  if /i "%%~a"=="check" (
    set "DRYRUN=1"
  ) else (
    set "PORT=%%~a"
  )
)

rem ---- locate Python 3.12 first, then fall back to PATH python ----
set "PY=C:\Users\Administrator\AppData\Local\Programs\Python\Python312\python.exe"
if not exist "%PY%" set "PY=python"
%PY% -c "import sys" >nul 2>nul
if errorlevel 1 set "PY=python"
%PY% -c "import sys" >nul 2>nul
if errorlevel 1 (
  echo.
  echo [ERROR] Python 3 not found. Install it or edit the PY line here.
  echo.
  if not defined DRYRUN pause
  exit /b 1
)

set "BRIDGE=H:\tools\lhm\lhm_bridge.exe"
if not exist "%BRIDGE%" (
  echo.
  echo [ERROR] sensor bridge not found: %BRIDGE%
  echo         Build it first:  powershell -File tools\lhm_bridge\build.ps1
  echo.
  if not defined DRYRUN pause
  exit /b 1
)

rem ---- do NOT pass --bridge "%BRIDGE%" on the start line below: nested
rem      quotes inside cmd /k mangle the argument, argparse refuses it and the
rem      daemon dies before it binds. The default inside the daemon is this path.

rem ---- refuse to start on a port that already has a listener ----
set "BUSY="
for /f "delims=" %%p in ('netstat -ano ^| findstr /c:"LISTENING" ^| findstr /c:":%PORT% "') do set "BUSY=%%p"
if defined BUSY (
  echo.
  echo [ERROR] port %PORT% is already in use:
  echo         %BUSY%
  echo.
  echo         Close that window, or pick another port:
  echo         run-lhm.bat 7803
  echo.
  if not defined DRYRUN pause
  exit /b 1
)

echo.
echo   python   : %PY%
echo   daemon   : port %PORT%, real LibreHardwareMonitor sensors
echo   bridge   : %BRIDGE%
echo   terminal : keys n/p page, arrows focus, Enter select
echo   stop     : close the two windows, or taskkill /F /IM lhm_bridge.exe
echo.

if defined DRYRUN (
  echo [dry run] everything looks fine; nothing was started.
  echo.
  exit /b 0
)

start "PC-Monitor Daemon (lhm)" cmd /k "%PY%" fake_daemon.py --source lhm --port %PORT% >nul 2>nul
rem give the daemon time to bind and write daemon.token before the terminal reads it
ping -n 3 127.0.0.1 >nul
start "PC-Monitor Terminal (lhm)" cmd /k "%PY%" term_tk.py --port %PORT% >nul 2>nul

exit /b 0

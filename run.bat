@echo off
setlocal
cd /d "%~dp0"

rem ---- locate Python 3.12 first, then fall back to PATH python ----
set "PY=C:\Users\Administrator\AppData\Local\Programs\Python\Python312\python.exe"
if not exist "%PY%" set "PY=python"
%PY% -c "import sys" >nul 2>nul
if errorlevel 1 set "PY=python"
%PY% -c "import sys" >nul 2>nul
if errorlevel 1 (
  echo.
  echo [ERROR] Python 3 not found.
  echo Install Python 3.12 or edit the PY line in this file.
  echo.
  pause
  exit /b 1
)

for /f "delims=" %%v in ('%PY% -c "import sys;print(sys.version.split()[0])"') do set "PYVER=%%v"
echo Using Python %PYVER% at %PY%
echo.
echo   Window 1 = fake daemon (fake data, lenient token)
echo   Window 2 = terminal  (keys: n/p page, arrows focus, Enter select)
echo.

start "PC-Monitor Daemon" cmd /k "%PY%" fake_daemon.py
ping -n 3 127.0.0.1 >nul
start "PC-Monitor Terminal" cmd /k "%PY%" term_tk.py

exit /b 0

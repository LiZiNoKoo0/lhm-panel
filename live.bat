@echo off
REM Push live CPU load / temperature to the ST7735 panel over USB serial.
REM Leave this window open: closing it freezes the panel on the last frame.
REM   live.bat                 COM10, 1 Hz, real sensors
REM   live.bat --source demo   sine wave, no LibreHardwareMonitor needed
REM   live.bat --port COM7 --interval 0.5
cd /d "%~dp0"
python live_serial.py %*

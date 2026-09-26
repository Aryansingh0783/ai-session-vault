@echo off
cd /d "%~dp0"
where py >nul 2>nul && (py -3 vault.py %*) || (python vault.py %*)
if errorlevel 9009 echo Python 3 is not installed. Get it from https://www.python.org/downloads/ & pause

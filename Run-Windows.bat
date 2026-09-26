@echo off
cd /d "%~dp0"
where py >nul 2>nul
if %errorlevel%==0 (
    py -3 vault.py %*
) else (
    python vault.py %*
)
if errorlevel 9009 echo Python 3 is not installed. Get it from https://www.python.org/downloads/ and tick "Add python.exe to PATH".
if errorlevel 1 pause

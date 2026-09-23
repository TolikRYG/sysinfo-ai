@echo off
setlocal
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0build.ps1" %*
if errorlevel 1 (
    echo.
    echo Build failed. Read the error above.
    pause
    exit /b 1
)
echo.
echo Build completed. Executable: dist\sysinfo-ai.exe
pause

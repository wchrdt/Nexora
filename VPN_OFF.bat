@echo off
chcp 65001 >nul
taskkill /F /IM xray.exe >nul 2>nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings" /v ProxyEnable /t REG_DWORD /d 0 /f >nul
echo Xray остановлен, системный прокси выключен.
pause

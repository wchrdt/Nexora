@echo off
chcp 65001 >nul
cd /d "%~dp0"
rem --- Xray (VPN) должен лежать в папке xray проекта: он попадёт в установщик ---
if not exist "xray" mkdir "xray"
if not exist "xray\xray.exe" if exist "%APPDATA%\Nexora\xray\xray.exe" (
  echo Копирую xray из %APPDATA%\Nexora\xray в папку проекта...
  xcopy /y /q "%APPDATA%\Nexora\xray\*" "xray\" >nul
)
if not exist "xray\xray.exe" (
  echo.
  echo ВНИМАНИЕ: нет xray\xray.exe. Без него VPN у друзей не заработает.
  echo Положи xray.exe, geoip.dat и geosite.dat в папку xray рядом с этим файлом.
  choice /m "Всё равно собрать без VPN"
  if errorlevel 2 exit /b
)
if not exist "zapret\bin\winws.exe" (
  echo.
  echo Заметка: папка zapret пуста, набор для обхода блокировок Nexora скачает сама при первом включении.
)
cd /d "%~dp0src-tauri"
cargo tauri --version >nul 2>nul || cargo install tauri-cli --version "^2" --locked
echo Идёт сборка exe, это может занять 10-20 минут. Строки ниже - это нормально.
powershell -NoProfile -Command "$env:CARGO_TERM_COLOR='never'; cmd /c 'cargo tauri build 2>&1' | Tee-Object -FilePath ..\build_log.txt"
echo.
echo Готово или остановилось. Лог сохранён в build_log.txt
pause

@echo off
chcp 65001 >nul
net session >nul 2>&1
if %errorlevel% neq 0 (
  echo Нужны права администратора, перезапускаю...
  powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
  exit /b
)
cd /d "%~dp0src-tauri"
cargo tauri --version >nul 2>nul || cargo install tauri-cli --version "^2" --locked
echo Идёт сборка, это может занять 10-20 минут. Строки ниже - это нормально.
rem закрываем прошлый запуск Nexora и освобождаем лог (если его держит старое окно сборки - пишем в лог с другим именем)
taskkill /f /im nexora.exe >nul 2>&1
set "LOG=build_log.txt"
del /f /q "%~dp0build_log.txt" >nul 2>&1
if exist "%~dp0build_log.txt" set "LOG=build_log_%RANDOM%.txt"
powershell -NoProfile -Command "$env:CARGO_TERM_COLOR='never'; cmd /c 'cargo tauri dev 2>&1' | Tee-Object -FilePath ..\%LOG%"
echo.
echo Готово или остановилось. Лог сохранён в %LOG%
pause

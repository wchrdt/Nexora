@echo off
chcp 65001 >nul
echo Останавливаю зависшие процессы сборки...
taskkill /F /IM mspdbsrv.exe >nul 2>nul
taskkill /F /IM cargo.exe >nul 2>nul
taskkill /F /IM rustc.exe >nul 2>nul
taskkill /F /IM link.exe >nul 2>nul
taskkill /F /IM nexora.exe >nul 2>nul
echo Удаляю старые файлы сборки: src-tauri\target
if exist "%~dp0src-tauri\target" rmdir /s /q "%~dp0src-tauri\target"
if exist "%~dp0src-tauri\target" (
  echo Не удалось удалить папку target: что-то держит файлы. Закрой окна проводника с этой папкой, перезагрузи ПК и запусти файл снова.
) else (
  echo Готово. Теперь запускай BUILD_TAURI.bat
)
pause

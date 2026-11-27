#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, RunEvent, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

mod extras;
mod music;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const AUDIO_PS: &str = include_str!("../scripts/audio.ps1");
const MEDIA_PS: &str = include_str!("../scripts/media.ps1");
const LEVELS_PS: &str = include_str!("../scripts/levels.ps1");
const NOTIF_PS: &str = include_str!("../scripts/notif.ps1");
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const THEME_KEY: &str = r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Themes\Personalize";

fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/* ---------- постоянный PowerShell-процесс (громкость / медиа) ---------- */
struct PsProc {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<String>,
}

struct PsWorker {
    name: &'static str,
    script: &'static str,
    proc: Mutex<Option<PsProc>>,
    dead_at: Mutex<Option<Instant>>,
}

impl PsWorker {
    fn new(name: &'static str, script: &'static str) -> Self {
        PsWorker { name, script, proc: Mutex::new(None), dead_at: Mutex::new(None) }
    }

    fn spawn(&self) -> Option<PsProc> {
        if let Some(t) = *self.dead_at.lock().unwrap() {
            if t.elapsed() < Duration::from_secs(8) {
                return None;
            }
        }
        let path = std::env::temp_dir().join(format!("island-{}.ps1", self.name));
        let mut data: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
        data.extend_from_slice(self.script.as_bytes());
        std::fs::write(&path, &data).ok()?;

        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        quiet(&mut cmd);
        let mut child = cmd.spawn().ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;

        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            let mut buf: Vec<u8> = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let s = String::from_utf8_lossy(&buf).trim().to_string();
                        if !s.is_empty() && tx.send(s).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        /* ждём сигнал ready (Add-Type на медленной машине может компилироваться долго) */
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(l) if l == "ready" => break,
                Ok(_) => continue,
                Err(_) => {
                    let _ = child.kill();
                    *self.dead_at.lock().unwrap() = Some(Instant::now());
                    return None;
                }
            }
        }
        Some(PsProc { child, stdin, rx })
    }

    fn request(&self, cmd: &str, timeout: Duration) -> Option<String> {
        let mut g = self.proc.lock().unwrap();
        if g.is_none() {
            *g = self.spawn();
        }
        let ok = {
            let p = g.as_mut()?;
            while p.rx.try_recv().is_ok() {}
            writeln!(p.stdin, "{}", cmd).and_then(|_| p.stdin.flush()).is_ok()
        };
        if !ok {
            if let Some(mut p) = g.take() {
                let _ = p.child.kill();
            }
            return None;
        }
        let res = g.as_mut()?.rx.recv_timeout(timeout);
        match res {
            Ok(l) => Some(l),
            Err(_) => {
                if let Some(mut p) = g.take() {
                    let _ = p.child.kill();
                }
                None
            }
        }
    }

    fn stop(&self) {
        if let Ok(mut g) = self.proc.lock() {
            if let Some(mut p) = g.take() {
                let _ = p.child.kill();
            }
        }
    }
}

/* ---------- общее состояние ---------- */
struct AppState {
    audio: PsWorker,
    media: PsWorker,
    notif: PsWorker, /* уведомления Windows (Discord-звонки, Telegram): читает notif.ps1 */
    mctl: PsWorker, /* отдельный процесс для команд play/next/prev: опрос состояния не задерживает кнопки */
    ui_active: AtomicBool,
    sticky: AtomicBool, /* остров «залип» раскрытым (список приложений с интернетом): снимается кликом в другом месте */
    capture: AtomicBool,
    levels_on: AtomicBool,
    levels_info: Mutex<String>,
    rect: Mutex<[f64; 4]>,
    cover: Mutex<(String, String)>, /* (key, data-url) */
    cover_pushed: Mutex<String>,
    last_media: Mutex<Option<Value>>,
    media_fails: AtomicU32,
    sys: Mutex<sysinfo::System>,
    hk: Mutex<HashMap<String, String>>,
    clip_last: Mutex<String>,
    mouse_binds: Mutex<Vec<MouseBind>>, /* бинды на кнопки мыши (Mouse3/4/5) */
    mouse_paused: AtomicBool,           /* пока в настройках записывается новый бинд, хук мыши пропускает всё */
    user_hidden: AtomicBool,            /* остров скрыт самим пользователем */
    game_hidden: AtomicBool,            /* остров скрыт автоматически в игре */
    auto_hide: AtomicBool,              /* скрывать в играх и полноэкранных приложениях */
    in_fs: AtomicBool,                  /* сейчас на экране полноэкранное приложение */
    fs_override: AtomicBool,            /* пользователь сам показал остров поверх игры */
}

#[derive(Clone)]
struct MouseBind {
    ctrl: bool,
    alt: bool,
    shift: bool,
    btn: u8, /* 3 = средняя, 4 и 5 = боковые */
    act: String,
}

type St<'a> = State<'a, Arc<AppState>>;

fn hk_default() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("toggle".to_string(), "CommandOrControl+Alt+I".to_string());
    m.insert("palette".to_string(), "Alt+Space".to_string());
    m.insert("shot".to_string(), "CommandOrControl+Alt+S".to_string());
    m.insert("expand".to_string(), String::new());
    for k in ["play", "next", "prev", "mute"] {
        m.insert(k.to_string(), String::new());
    }
    m
}

fn vk_code(key: &str) -> Option<u32> {
    Some(match key {
        "playpause" => 0xB3,
        "next" => 0xB0,
        "prev" => 0xB1,
        "stop" => 0xB2,
        "volup" => 0xAF,
        "voldown" => 0xAE,
        "mute" => 0xAD,
        _ => return None,
    })
}

/* ---------- система ---------- */
fn sys_snapshot(s: &AppState) -> Value {
    let mut sys = s.sys.lock().unwrap();
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    let cpu = sys.global_cpu_usage().round() as u32;
    let total = sys.total_memory() as f64;
    let avail = sys.available_memory() as f64;
    let ram = if total > 0.0 { ((1.0 - avail / total) * 100.0).round() } else { 0.0 };
    let gb = 1_073_741_824.0;
    json!({
        "cpu": cpu,
        "ram": ram,
        "ramUsed": (((total - avail) / gb) * 10.0).round() / 10.0,
        "ramTotal": ((total / gb) * 10.0).round() / 10.0,
        "uptime": sysinfo::System::uptime(),
        "cores": sys.cpus().len(),
        "host": sysinfo::System::host_name().unwrap_or_default()
    })
}

/* ---------- громкость ---------- */
fn vol_call(s: &AppState, cmd: &str) -> Value {
    let line = s.audio.request(cmd, Duration::from_secs(15));
    if let Some(l) = &line {
        let parts: Vec<&str> = l.split_whitespace().collect();
        if parts.len() == 2 {
            if let (Ok(v), true) = (parts[0].parse::<u32>(), parts[1] == "0" || parts[1] == "1") {
                return json!({ "v": v, "m": parts[1] == "1" });
            }
        }
        if let Some(e) = l.strip_prefix("err ") {
            return json!({ "err": e });
        }
    }
    json!({ "err": "PowerShell не ответил" })
}

/* Медиа-клавиши шлём напрямую через SendInput (с флагом extended-key), без PowerShell:
   раньше они шли через воркер громкости и не срабатывали, если тот завис или не поднялся. */
#[cfg(windows)]
fn send_vk(vk: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    };
    let mk = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk as u16),
                wScan: 0,
                dwFlags: if up { KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP } else { KEYEVENTF_EXTENDEDKEY },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [mk(false), mk(true)];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) == 2 }
}

#[cfg(not(windows))]
fn send_vk(_vk: u32) -> bool {
    false
}

fn press_key(s: &AppState, key: &str) -> bool {
    let Some(vk) = vk_code(key) else { return false };
    /* сначала команда напрямую системной медиа-сессии (GSMTC); если она не приняла, шлём медиа-клавишу */
    if matches!(key, "playpause" | "next" | "prev") {
        if let Some(r) = s.mctl.request(&format!("ctl {}", key), Duration::from_secs(6)) {
            if r.trim() == "ctl 1" {
                return true;
            }
        }
    }
    if send_vk(vk) {
        return true;
    }
    /* последний запасной путь: клавиша через PowerShell-воркер; успех только если он ответил без ошибки */
    match s.audio.request(&format!("key {}", vk), Duration::from_secs(9)) {
        Some(l) => !l.starts_with("err"),
        None => false,
    }
}

/* ---------- медиа Windows (GSMTC) ---------- */
fn value_key(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(o) => o.to_string(),
    }
}

fn media_state_blocking(s: &AppState) -> Option<Value> {
    match s.media.request("state", Duration::from_secs(12)) {
        None => {
            /* воркер не ответил: пару циклов держим прошлое состояние, дальше считаем, что ничего не играет */
            if s.media_fails.fetch_add(1, Ordering::SeqCst) + 1 >= 3 {
                *s.last_media.lock().unwrap() = None;
            }
        }
        Some(line) => {
            s.media_fails.store(0, Ordering::SeqCst);
            let mut v: Value = serde_json::from_str(&line).unwrap_or_else(|_| json!({}));
            let thumb_key = value_key(v.get("thumbKey"));
            if let Some(t) = v.get("thumb").and_then(|x| x.as_str()).filter(|t| !t.is_empty()) {
                let ty = v.get("thumbType").and_then(|x| x.as_str()).unwrap_or("image/png");
                let url = format!("data:{};base64,{}", ty, t);
                *s.cover.lock().unwrap() = (thumb_key.clone(), url);
            }
            if let Some(o) = v.as_object_mut() {
                o.remove("thumb");
                o.remove("thumbType");
                let c = s.cover.lock().unwrap();
                let key = if !c.1.is_empty() && c.0 == thumb_key { c.0.clone() } else { String::new() };
                o.insert("coverKey".to_string(), Value::String(key));
            }
            let has = v.get("title").is_some() || v.get("artist").is_some();
            *s.last_media.lock().unwrap() = if has { Some(v) } else { None };
        }
    }
    s.last_media.lock().unwrap().clone()
}

/* ---------- команды ---------- */
#[tauri::command]
async fn sys(state: St<'_>) -> Result<Value, String> {
    let s = state.inner().clone();
    Ok(sys_snapshot(&s))
}

#[tauri::command]
fn ui_active(state: St<'_>, v: bool) {
    state.ui_active.store(v, Ordering::Relaxed);
}

#[tauri::command]
fn ui_sticky(state: St<'_>, v: bool) {
    state.sticky.store(v, Ordering::Relaxed);
}

#[tauri::command]
fn hit(state: St<'_>, inside: bool) {
    state.capture.store(inside, Ordering::Relaxed);
}

/* живой микшер: интерфейс сообщает, играет ли музыка; поток уровней работает только пока играет */
#[tauri::command]
fn levels_diag(state: St<'_>) -> String {
    state.levels_info.lock().map(|g| g.clone()).unwrap_or_default()
}

#[tauri::command]
fn levels_active(state: St<'_>, v: bool) {
    state.levels_on.store(v, Ordering::Relaxed);
}

#[tauri::command]
fn set_rect(state: St<'_>, x: f64, y: f64, w: f64, h: f64) {
    *state.rect.lock().unwrap() = [x, y, w, h];
}

#[tauri::command]
async fn media_key(state: St<'_>, key: String) -> Result<bool, String> {
    let s = state.inner().clone();
    Ok(tauri::async_runtime::spawn_blocking(move || press_key(&s, &key)).await.unwrap_or(false))
}

#[tauri::command]
async fn media_state(state: St<'_>) -> Result<Value, String> {
    let s = state.inner().clone();
    let v = tauri::async_runtime::spawn_blocking(move || media_state_blocking(&s)).await.unwrap_or(None);
    Ok(v.unwrap_or(Value::Null))
}

#[tauri::command]
async fn media_seek(state: St<'_>, sec: f64) -> Result<bool, String> {
    if !sec.is_finite() || sec < 0.0 {
        return Ok(false);
    }
    let s = state.inner().clone();
    Ok(tauri::async_runtime::spawn_blocking(move || {
        /* скрипт отвечает «seek 1» только если Windows реально приняла перемотку */
        s.media
            .request(&format!("seek {:.2}", sec), Duration::from_secs(12))
            .map(|r| r.trim() == "seek 1")
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false))
}

#[tauri::command]
fn media_cover(state: St<'_>) -> Value {
    let c = state.cover.lock().unwrap();
    json!({ "key": c.0, "url": c.1 })
}

#[tauri::command]
async fn vol_get(state: St<'_>) -> Result<Value, String> {
    let s = state.inner().clone();
    Ok(tauri::async_runtime::spawn_blocking(move || vol_call(&s, "get")).await.unwrap_or(json!({"err":"ошибка"})))
}

#[tauri::command]
async fn vol_set(state: St<'_>, v: f64) -> Result<Value, String> {
    let s = state.inner().clone();
    let n = v.round().clamp(0.0, 100.0) as u32;
    Ok(tauri::async_runtime::spawn_blocking(move || vol_call(&s, &format!("set {}", n)))
        .await
        .unwrap_or(json!({"err":"ошибка"})))
}

#[tauri::command]
async fn vol_mute(state: St<'_>) -> Result<Value, String> {
    let s = state.inner().clone();
    Ok(tauri::async_runtime::spawn_blocking(move || vol_call(&s, "mute")).await.unwrap_or(json!({"err":"ошибка"})))
}

#[tauri::command]
fn clip_read() -> String {
    arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default()
}

#[tauri::command]
fn clip_write(state: St<'_>, text: String) -> bool {
    *state.clip_last.lock().unwrap() = text.clone();
    arboard::Clipboard::new().and_then(|mut c| c.set_text(text)).is_ok()
}

#[tauri::command]
fn open(target: String) -> bool {
    let mut cmd;
    if target.starts_with("http://") || target.starts_with("https://") {
        /* ссылку открываем штатным обработчиком (браузер по умолчанию), explorer.exe с URL бывает капризным */
        cmd = Command::new("rundll32.exe");
        cmd.args(["url.dll,FileProtocolHandler"]).arg(&target);
    } else {
        cmd = Command::new("explorer.exe");
        cmd.arg(&target);
    }
    quiet(&mut cmd);
    cmd.spawn().is_ok()
}

#[tauri::command]
fn power(action: String) -> bool {
    let mut cmd;
    match action.as_str() {
        "lock" => {
            cmd = Command::new("rundll32.exe");
            cmd.args(["user32.dll,LockWorkStation"]);
        }
        "sleep" => {
            cmd = Command::new("powershell.exe");
            cmd.args([
                "-NoProfile",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.Application]::SetSuspendState([System.Windows.Forms.PowerState]::Suspend,$false,$false)",
            ]);
        }
        "shutdown" => {
            cmd = Command::new("shutdown");
            cmd.args(["/s", "/t", "0"]);
        }
        "restart" => {
            cmd = Command::new("shutdown");
            cmd.args(["/r", "/t", "0"]);
        }
        _ => return false,
    }
    quiet(&mut cmd);
    cmd.spawn().is_ok()
}

/* Свернуть все окна одной кнопкой: команда «Свернуть все» панели задач (то же, что Win+M), без запуска PowerShell */
#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn FindWindowW(class: *const u16, title: *const u16) -> isize;
    fn SendMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
}

#[tauri::command]
fn minimize_all(app: AppHandle) -> bool {
    #[cfg(windows)]
    {
        let class: Vec<u16> = "Shell_TrayWnd".encode_utf16().chain(std::iter::once(0)).collect();
        let tray = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
        if tray == 0 {
            return false;
        }
        /* WM_COMMAND, 419 = MIN_ALL */
        unsafe { SendMessageW(tray, 0x0111, 419, 0) };
        /* остров должен остаться на месте и поверх остальных */
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.show();
            let _ = win.set_always_on_top(true);
        }
        true
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        false
    }
}

#[tauri::command]
fn theme_windows(light: bool) -> bool {
    let v = if light { "1" } else { "0" };
    let mut ok = true;
    for name in ["AppsUseLightTheme", "SystemUsesLightTheme"] {
        let mut cmd = Command::new("reg");
        cmd.args(["add", THEME_KEY, "/v", name, "/t", "REG_DWORD", "/d", v, "/f"]);
        quiet(&mut cmd);
        ok &= cmd.output().map(|o| o.status.success()).unwrap_or(false);
    }
    ok
}

/* Автозапуск от администратора: запись в Run-ключе для таких программ Windows молча игнорирует,
   поэтому используем планировщик заданий (вход в систему, наивысшие права, без запроса UAC). */
fn run_ps_hidden(script: &str) -> bool {
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]);
    quiet(&mut cmd);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn autostart_old_key(remove: bool) -> bool {
    let mut cmd = Command::new("reg");
    if remove {
        cmd.args(["delete", RUN_KEY, "/v", "Nexora", "/f"]);
    } else {
        cmd.args(["query", RUN_KEY, "/v", "Nexora"]);
    }
    quiet(&mut cmd);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

#[tauri::command]
fn autostart_get() -> bool {
    let mut cmd = Command::new("schtasks");
    cmd.args(["/Query", "/TN", "Nexora"]);
    quiet(&mut cmd);
    cmd.output().map(|o| o.status.success()).unwrap_or(false) || autostart_old_key(false)
}

#[tauri::command]
fn autostart_set(v: bool) -> bool {
    if v {
        let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default().replace('\'', "''");
        let script = format!(
            "$ErrorActionPreference='Stop';\
             $a=New-ScheduledTaskAction -Execute '{exe}';\
             $t=New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME;\
             $p=New-ScheduledTaskPrincipal -UserId $env:USERNAME -RunLevel Highest -LogonType Interactive;\
             $s=New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero);\
             Register-ScheduledTask -TaskName 'Nexora' -Action $a -Trigger $t -Principal $p -Settings $s -Force | Out-Null",
            exe = exe
        );
        let ok = run_ps_hidden(&script);
        if ok {
            autostart_old_key(true); /* старая запись больше не нужна */
        }
        ok
    } else {
        let mut cmd = Command::new("schtasks");
        cmd.args(["/Delete", "/TN", "Nexora", "/F"]);
        quiet(&mut cmd);
        let _ = cmd.output();
        autostart_old_key(true);
        /* успех, только если автозапуска действительно не осталось */
        !autostart_get()
    }
}


/* ---------- уведомления Windows: звонки Discord и сообщения Telegram на острове ---------- */
#[tauri::command]
async fn notif_poll(state: St<'_>) -> Result<Value, String> {
    let s = state.inner().clone();
    let r = tauri::async_runtime::spawn_blocking(move || s.notif.request("poll", Duration::from_secs(12)))
        .await
        .unwrap_or(None);
    Ok(match r {
        Some(l) => serde_json::from_str::<Value>(&l)
            .unwrap_or_else(|_| json!({ "st": "error", "err": "непонятный ответ службы уведомлений", "new": [], "gone": [] })),
        None => json!({ "st": "error", "err": "служба уведомлений не ответила", "new": [], "gone": [] }),
    })
}

/* what: "accept" (принять звонок) или "decline" (сбросить) */
#[tauri::command]
async fn notif_discord(state: St<'_>, what: String) -> Result<bool, String> {
    let w = if what == "accept" { "accept" } else { "decline" };
    let s = state.inner().clone();
    let cmd = format!("discord {}", w);
    let r = tauri::async_runtime::spawn_blocking(move || s.notif.request(&cmd, Duration::from_secs(15)))
        .await
        .unwrap_or(None);
    Ok(r.map(|l| l.trim().eq_ignore_ascii_case("discord ok")).unwrap_or(false))
}

/* ---------- резервная копия заметок в файл (%APPDATA%\Nexora\notes.json) ----------
   localStorage у WebView2 пишется на диск с задержкой и теряется, если приложение закрыли принудительно или выключили ПК,
   поэтому заметки дублируются в обычный файл (запись через временный файл, чтобы не получить обрезанный JSON). */
#[tauri::command]
fn state_save(json: String) -> bool {
    let dir = extras::data_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let tmp = dir.join("notes.json.tmp");
    let dst = dir.join("notes.json");
    if std::fs::write(&tmp, json.as_bytes()).is_err() {
        return false;
    }
    std::fs::rename(&tmp, &dst).is_ok()
}

#[tauri::command]
fn state_load() -> String {
    std::fs::read_to_string(extras::data_dir().join("notes.json")).unwrap_or_default()
}

/* ---------- удаление Nexora: запускаем штатный деинсталлятор NSIS и закрываемся ---------- */
#[tauri::command]
fn uninstall_app(app: AppHandle) -> Result<bool, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("не нашёл папку приложения")?.to_path_buf();
    let un = ["uninstall.exe", "Uninstall.exe", "unins000.exe"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())
        .ok_or("Деинсталлятор не найден: приложение запущено не из установленной копии. Удали Nexora через «Параметры → Приложения».")?;
    /* убираем автозапуск и всё, что мы запускали (zapret, Xray, прокси Telegram, системный прокси) */
    let mut cmd = Command::new("schtasks");
    cmd.args(["/Delete", "/TN", "Nexora", "/F"]);
    quiet(&mut cmd);
    let _ = cmd.output();
    autostart_old_key(true);
    extras::shutdown();
    let mut u = Command::new(&un);
    u.current_dir(&dir);
    u.spawn().map_err(|e| format!("не удалось запустить деинсталлятор: {}", e))?;
    /* даём деинсталлятору появиться и закрываемся, чтобы он смог удалить файлы */
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(900));
        app.exit(0);
    });
    Ok(true)
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn hide_window(app: AppHandle) {
    if let Some(st) = app.try_state::<Arc<AppState>>() {
        st.user_hidden.store(true, Ordering::Relaxed);
        st.game_hidden.store(false, Ordering::Relaxed);
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

#[tauri::command]
fn auto_hide_set(app: AppHandle, state: St<'_>, v: bool) {
    state.auto_hide.store(v, Ordering::Relaxed);
    /* выключили автоскрытие, пока остров спрятан из-за игры — возвращаем его */
    if !v && state.game_hidden.swap(false, Ordering::Relaxed) && !state.user_hidden.load(Ordering::Relaxed) {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
        }
    }
}

#[tauri::command]
fn mouse_pause(state: St<'_>, v: bool) {
    state.mouse_paused.store(v, Ordering::Relaxed);
}

#[tauri::command]
fn clip_mark_image(state: St<'_>) {
    /* мы сами положили картинку в буфер: чтобы наблюдатель буфера не добавил её второй раз */
    *state.clip_last.lock().unwrap() = "img:self".to_string();
}

#[tauri::command]
fn shot_layout(app: AppHandle, state: St<'_>, full: bool) -> bool {
    let Some(win) = app.get_webview_window("main") else { return false };
    if full {
        let mon = win.primary_monitor().ok().flatten().or_else(|| win.current_monitor().ok().flatten());
        let Some(mon) = mon else { return false };
        let pos = mon.position();
        let size = mon.size();
        /* критичные операции проверяем: при отказе Windows возвращаем окно на место и сообщаем неудачу */
        let ok = win.set_position(PhysicalPosition::new(pos.x, pos.y)).is_ok()
            && win.set_size(PhysicalSize::new(size.width, size.height)).is_ok()
            && win.set_ignore_cursor_events(false).is_ok()
            && win.show().is_ok();
        if !ok {
            state.capture.store(false, Ordering::Relaxed);
            place_window(&app);
            return false;
        }
        state.capture.store(true, Ordering::Relaxed);
        let _ = win.set_focus(); /* фокус может не дойти до окна, на снимок это не влияет */
    } else {
        state.capture.store(false, Ordering::Relaxed);
        place_window(&app);
    }
    true
}

#[tauri::command]
fn hk_set(app: AppHandle, state: St<'_>, map: HashMap<String, String>) -> Value {
    let mut nm = hk_default();
    for (k, def) in hk_default() {
        nm.insert(k.clone(), map.get(&k).cloned().unwrap_or(def));
    }
    *state.hk.lock().unwrap() = nm;
    apply_hotkeys(&app, state.inner())
}

/* ---------- горячие клавиши ---------- */
fn show_window(app: &AppHandle) {
    /* явный показ от пользователя: отменяет и ручное скрытие, и автоскрытие в игре (пока игра не закроется) */
    if let Some(st) = app.try_state::<Arc<AppState>>() {
        st.user_hidden.store(false, Ordering::Relaxed);
        st.game_hidden.store(false, Ordering::Relaxed);
        let fs = st.in_fs.load(Ordering::Relaxed);
        st.fs_override.store(fs, Ordering::Relaxed);
    }
    if let Some(w) = app.get_webview_window("main") {
        if !w.is_visible().unwrap_or(true) {
            let _ = w.show();
        }
    }
}

fn run_action(app: &AppHandle, st: &Arc<AppState>, act: &str) {
    match act {
        "toggle" => {
            /* показать / скрыть остров целиком */
            if let Some(w) = app.get_webview_window("main") {
                if w.is_visible().unwrap_or(true) {
                    st.user_hidden.store(true, Ordering::Relaxed);
                    st.game_hidden.store(false, Ordering::Relaxed);
                    let _ = w.hide();
                } else {
                    show_window(app);
                }
            }
        }
        "expand" => {
            /* раскрыть / свернуть остров */
            show_window(app);
            let _ = app.emit("island:toggle", ());
        }
        "palette" => {
            show_window(app);
            let _ = app.emit("palette", ());
        }
        "shot" => {
            let _ = app.emit("shot", ());
        }
        "play" | "next" | "prev" => {
            let (st, key) = (st.clone(), if act == "play" { "playpause".to_string() } else { act.to_string() });
            thread::spawn(move || {
                press_key(&st, &key);
            });
        }
        "mute" => {
            let st = st.clone();
            thread::spawn(move || {
                vol_call(&st, "mute");
            });
        }
        _ => {}
    }
}

fn apply_hotkeys(app: &AppHandle, st: &Arc<AppState>) -> Value {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let map = st.hk.lock().unwrap().clone();
    let mut res = serde_json::Map::new();
    let mut mouse: Vec<MouseBind> = Vec::new();
    for (k, acc) in map {
        if acc.is_empty() {
            res.insert(k, Value::Bool(true));
            continue;
        }
        if acc.to_lowercase().contains("mouse") {
            match parse_mouse(&acc) {
                Some((ctrl, alt, shift, btn)) => {
                    mouse.push(MouseBind { ctrl, alt, shift, btn, act: k.clone() });
                    res.insert(k, Value::Bool(cfg!(windows)));
                }
                None => {
                    res.insert(k, Value::Bool(false));
                }
            }
            continue;
        }
        let (a, s2, act) = (app.clone(), st.clone(), k.clone());
        let ok = gs
            .on_shortcut(acc.as_str(), move |_app, _sc, ev| {
                if ev.state() == ShortcutState::Pressed {
                    run_action(&a, &s2, &act);
                }
            })
            .is_ok();
        res.insert(k, Value::Bool(ok));
    }
    *st.mouse_binds.lock().unwrap() = mouse;
    Value::Object(res)
}

/* "Ctrl+Mouse4" -> (ctrl, alt, shift, кнопка) */
fn parse_mouse(acc: &str) -> Option<(bool, bool, bool, u8)> {
    let (mut ctrl, mut alt, mut shift, mut btn) = (false, false, false, 0u8);
    for t in acc.split('+') {
        match t.trim().to_lowercase().as_str() {
            "commandorcontrol" | "ctrl" | "control" | "cmd" | "command" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "mouse3" => btn = 3,
            "mouse4" => btn = 4,
            "mouse5" => btn = 5,
            _ => return None,
        }
    }
    if btn == 0 {
        None
    } else {
        Some((ctrl, alt, shift, btn))
    }
}

/* Глобальный хук мыши: боковые кнопки и колесо-кнопка как горячие клавиши.
   Привязанное нажатие «съедается» (в браузере не сработает «назад»), остальные кнопки не трогаем. */
#[cfg(windows)]
mod winmouse {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::OnceLock;

    static CTX: OnceLock<(AppHandle, Arc<AppState>)> = OnceLock::new();
    static SWALLOW: AtomicU32 = AtomicU32::new(0);

    #[repr(C)]
    struct Pt {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    struct MsLl {
        pt: Pt,
        mouse_data: u32,
        flags: u32,
        time: u32,
        extra: usize,
    }
    #[repr(C)]
    struct Msg {
        hwnd: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt: Pt,
        private: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn SetWindowsHookExW(id: i32, f: Option<unsafe extern "system" fn(i32, usize, isize) -> isize>, hmod: isize, tid: u32) -> isize;
        fn CallNextHookEx(h: isize, code: i32, w: usize, l: isize) -> isize;
        fn GetMessageW(m: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
        fn GetAsyncKeyState(vk: i32) -> i16;
    }

    unsafe extern "system" fn hook_proc(code: i32, w: usize, l: isize) -> isize {
        if code >= 0 {
            if let Some((app, st)) = CTX.get() {
                let msg = w as u32;
                let info = &*(l as *const MsLl);
                /* пока остров «залип» раскрытым, любой клик мышью (левая/правая/средняя) сообщаем интерфейсу:
                   он сам решит по положению курсора, был ли клик вне острова */
                if matches!(msg, 0x0201 | 0x0204 | 0x0207) && st.sticky.load(Ordering::Relaxed) {
                    let a = app.clone();
                    std::thread::spawn(move || {
                        let _ = a.emit("mouse:down", true);
                    });
                }
                let btn: u8 = match msg {
                    0x0207 | 0x0208 => 3,
                    0x020B | 0x020C => {
                        if (info.mouse_data >> 16) & 0xFFFF == 1 {
                            4
                        } else {
                            5
                        }
                    }
                    _ => 0,
                };
                if btn != 0 && !st.mouse_paused.load(Ordering::Relaxed) {
                    let bit = 1u32 << btn;
                    if msg == 0x0207 || msg == 0x020B {
                        let ctrl = GetAsyncKeyState(0x11) < 0;
                        let alt = GetAsyncKeyState(0x12) < 0;
                        let shift = GetAsyncKeyState(0x10) < 0;
                        let hit = st.mouse_binds.lock().ok().and_then(|g| {
                            g.iter().find(|b| b.btn == btn && b.ctrl == ctrl && b.alt == alt && b.shift == shift).map(|b| b.act.clone())
                        });
                        if let Some(act) = hit {
                            SWALLOW.fetch_or(bit, Ordering::Relaxed);
                            let (a, s2) = (app.clone(), st.clone());
                            std::thread::spawn(move || run_action(&a, &s2, &act));
                            return 1;
                        }
                    } else if SWALLOW.fetch_and(!bit, Ordering::Relaxed) & bit != 0 {
                        return 1; /* отпускание кнопки, нажатие которой мы съели */
                    }
                }
            }
        }
        CallNextHookEx(0, code, w, l)
    }

    pub fn start(app: AppHandle, st: Arc<AppState>) {
        if CTX.set((app, st)).is_err() {
            return;
        }
        std::thread::spawn(|| unsafe {
            let h = SetWindowsHookExW(14, Some(hook_proc), 0, 0); /* WH_MOUSE_LL */
            if h == 0 {
                return;
            }
            let mut m: Msg = std::mem::zeroed();
            while GetMessageW(&mut m, 0, 0, 0) > 0 {}
        });
    }
}
#[cfg(not(windows))]
mod winmouse {
    use super::*;
    pub fn start(_app: AppHandle, _st: Arc<AppState>) {}
}

/* полноэкранное приложение или игра на экране (Windows сама знает: QUNS_BUSY / D3D fullscreen / презентация) */
#[cfg(windows)]
fn fullscreen_busy() -> bool {
    #[link(name = "shell32")]
    extern "system" {
        fn SHQueryUserNotificationState(pquns: *mut i32) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> isize;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    }
    /* Окно острова растянуто на всю ширину экрана, а на небольшом экране или при масштабе 125-150% ещё и на всю высоту.
       Когда оно на переднем плане, Windows считает его «полноэкранным приложением» (QUNS_BUSY), остров прятал сам себя,
       потом показывал, и так по кругу: постоянное мигание. Если на переднем плане наше окно, это не игра. */
    unsafe {
        let fg = GetForegroundWindow();
        if fg != 0 {
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(fg, &mut pid);
            if pid == std::process::id() {
                return false;
            }
        }
    }
    let mut s: i32 = 5;
    let hr = unsafe { SHQueryUserNotificationState(&mut s) };
    hr == 0 && (s == 2 || s == 3 || s == 4)
}
#[cfg(not(windows))]
fn fullscreen_busy() -> bool {
    false
}

/* ---------- фоновые потоки ---------- */
fn start_threads(app: AppHandle, st: Arc<AppState>) {
    /* хук мыши для биндов на боковые кнопки */
    winmouse::start(app.clone(), st.clone());
    /* остров прячется, пока на экране игра или другое полноэкранное приложение, и возвращается после */
    {
        let (app, st) = (app.clone(), st.clone());
        thread::spawn(move || {
            /* прячем остров, только если полноэкранность держится минимум два опроса подряд (~1,4 с): короткие скачки не мигают */
            let mut fs_ticks: u32 = 0;
            loop {
                thread::sleep(Duration::from_millis(700));
                let fs = !st.capture.load(Ordering::Relaxed) && fullscreen_busy();
                fs_ticks = if fs { fs_ticks.saturating_add(1) } else { 0 };
                st.in_fs.store(fs, Ordering::Relaxed);
                let Some(w) = app.get_webview_window("main") else { continue };
                if !fs {
                    st.fs_override.store(false, Ordering::Relaxed);
                    if st.game_hidden.swap(false, Ordering::Relaxed) && !st.user_hidden.load(Ordering::Relaxed) {
                        let _ = w.show();
                    }
                } else if fs_ticks >= 2 && st.auto_hide.load(Ordering::Relaxed) && !st.fs_override.load(Ordering::Relaxed) && w.is_visible().unwrap_or(false) {
                    st.game_hidden.store(true, Ordering::Relaxed);
                    let _ = w.hide();
                }
            }
        });
    }
    /* CPU/RAM — только пока остров раскрыт */
    {
        let (app, st) = (app.clone(), st.clone());
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(1000));
            if st.ui_active.load(Ordering::Relaxed) {
                let _ = app.emit("sys", sys_snapshot(&st));
            }
        });
    }
    /* системное медиа */
    {
        let (app, st) = (app.clone(), st.clone());
        thread::spawn(move || {
            let mut last_json = String::new();
            loop {
                thread::sleep(Duration::from_millis(2000));
                let m = media_state_blocking(&st);
                let mut out = m.unwrap_or_else(|| json!({}));
                let ck = value_key(out.get("coverKey"));
                if !ck.is_empty() {
                    let mut pushed = st.cover_pushed.lock().unwrap();
                    if *pushed != ck {
                        out["cover"] = Value::String(st.cover.lock().unwrap().1.clone());
                        *pushed = ck;
                    }
                }
                let j = out.to_string();
                if j != last_json {
                    last_json = j;
                    let _ = app.emit("media:state", out);
                }
            }
        });
    }
    /* буфер обмена (текст) */
    {
        let (app, st) = (app.clone(), st.clone());
        thread::spawn(move || {
            let mut cb = arboard::Clipboard::new().ok();
            loop {
                thread::sleep(Duration::from_millis(1500));
                if cb.is_none() {
                    cb = arboard::Clipboard::new().ok();
                }
                if let Some(c) = cb.as_mut() {
                    if let Ok(t) = c.get_text() {
                        let mut last = st.clip_last.lock().unwrap();
                        if !t.is_empty() && t != *last && t.len() < 20000 {
                            *last = t.clone();
                            let _ = app.emit("clipboard", json!({ "text": t, "at": now_ms() }));
                        }
                    } else if !st.clip_last.lock().unwrap().starts_with("img:") {
                        /* в буфере не текст: пробуем картинку (как в Electron — одна за раз, пока не сменится текст) */
                        if let Ok(img) = c.get_image() {
                            let url = extras::rgba_to_bmp_data_url(img.width, img.height, &img.bytes);
                            if !url.is_empty() {
                                *st.clip_last.lock().unwrap() = format!("img:{}x{}:{}", img.width, img.height, url.len());
                                let _ = app.emit(
                                    "clipboard:image",
                                    json!({ "dataUrl": url, "w": img.width, "h": img.height, "at": now_ms() }),
                                );
                            }
                        }
                    }
                }
            }
        });
    }
    /* поток уровней системного звука (для живой волны острова) */
    {
        let (app, st) = (app.clone(), st.clone());
        thread::spawn(move || {
            let mut spawns: u32 = 0;
            let set_info = |st: &Arc<AppState>, s: String| {
                if let Ok(mut g) = st.levels_info.lock() {
                    *g = s;
                }
            };
            loop {
                if !st.levels_on.load(Ordering::Relaxed) {
                    set_info(&st, "выключен (levels_on=false)".to_string());
                    thread::sleep(Duration::from_millis(300));
                    continue;
                }
                let path = std::env::temp_dir().join("island-levels.ps1");
                let errp = std::env::temp_dir().join("island-levels.err.txt");
                let mut data: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
                data.extend_from_slice(LEVELS_PS.as_bytes());
                if std::fs::write(&path, &data).is_err() {
                    set_info(&st, "не удалось записать скрипт в TEMP".to_string());
                    thread::sleep(Duration::from_secs(5));
                    continue;
                }
                let mut cmd = Command::new("powershell.exe");
                cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
                    .arg(&path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped());
                match std::fs::File::create(&errp) {
                    Ok(f) => {
                        cmd.stderr(Stdio::from(f));
                    }
                    Err(_) => {
                        cmd.stderr(Stdio::null());
                    }
                }
                quiet(&mut cmd);
                spawns += 1;
                let Ok(mut child) = cmd.spawn() else {
                    set_info(&st, format!("запуск #{}: powershell.exe не запустился", spawns));
                    thread::sleep(Duration::from_secs(5));
                    continue;
                };
                let Some(out) = child.stdout.take() else {
                    let _ = child.kill();
                    thread::sleep(Duration::from_secs(5));
                    continue;
                };
                /* читаем трубу в отдельном потоке, чтобы зависший PowerShell не вешал этот цикл навсегда:
                   он шлёт строку каждые ~40 мс, поэтому долгая тишина = процесс завис, перезапускаем */
                let (tx, rx) = mpsc::channel::<String>();
                thread::spawn(move || {
                    let mut rd = BufReader::new(out);
                    let mut l = String::new();
                    loop {
                        l.clear();
                        match rd.read_line(&mut l) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                if tx.send(l.clone()).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
                let started = Instant::now();
                let mut lines: u64 = 0;
                let mut emitted: u64 = 0;
                let mut first = true;
                let mut off_since: Option<Instant> = None;
                let mut last_silent = false;
                let mut last_line = String::new();
                let reason: String;
                loop {
                    /* первая строка идёт после компиляции C# (на медленном ПК с антивирусом бывает долго), дальше ждём до 5 с */
                    let wait = if first { Duration::from_secs(90) } else { Duration::from_secs(5) };
                    let line = match rx.recv_timeout(wait) {
                        Ok(l) => l,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            reason = if first { "нет ни одной строки за 90 с (компиляция?)".to_string() } else { "процесс замолчал >5 с (завис)".to_string() };
                            break;
                        }
                        Err(_) => {
                            reason = "процесс завершился (труба закрыта)".to_string();
                            break;
                        }
                    };
                    first = false;
                    lines += 1;
                    if st.levels_on.load(Ordering::Relaxed) {
                        off_since = None;
                    } else {
                        let t = *off_since.get_or_insert_with(Instant::now);
                        if t.elapsed() > Duration::from_secs(15) {
                            reason = "levels_on выключен >15 с".to_string();
                            break;
                        }
                    }
                    /* строка: "L R" (пик по каналам) и, если получилось включить захват звука, ещё 64 числа спектра 0..255 */
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let (Ok(l), Ok(r)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>()) {
                            let bins: Vec<u8> = parts[2..].iter().filter_map(|x| x.parse::<u8>().ok()).collect();
                            let silent = l < 0.002 && r < 0.002;
                            if !(silent && last_silent) {
                                emitted += 1;
                                if bins.is_empty() {
                                    let _ = app.emit("audio:level", json!({ "l": l, "r": r }));
                                } else {
                                    let _ = app.emit("audio:level", json!({ "l": l, "r": r, "f": bins }));
                                }
                            }
                            last_silent = silent;
                        }
                    }
                    if lines % 25 == 1 {
                        let head: String = line.trim().chars().take(40).collect();
                        last_line = format!("{} (спектр: {} полос)", head, parts.len().saturating_sub(2));
                        set_info(
                            &st,
                            format!("работает: запуск #{}, {} с, строк {}, отправлено {}, последняя: {}, ts={}", spawns, started.elapsed().as_secs(), lines, emitted, last_line, now_ms()),
                        );
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
                let errtxt = std::fs::read_to_string(&errp).unwrap_or_default();
                let errtxt: String = errtxt.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect();
                set_info(
                    &st,
                    format!("остановлен: запуск #{}, прожил {} с, строк {}, причина: {}; stderr: {}; последняя: {}, ts={}", spawns, started.elapsed().as_secs(), lines, reason, if errtxt.is_empty() { "пусто" } else { errtxt.as_str() }, last_line, now_ms()),
                );
                thread::sleep(Duration::from_secs(2));
            }
        });
    }
    /* сквозные клики: окно прозрачное на всю ширину, ловит мышь только над островом.
       Заодно сообщаем интерфейсу, где курсор: окно музыки лежит над островом, и для страницы острова это «мышь ушла» */
    thread::spawn(move || {
        let mut last = false;
        let mut last_geo = false;
        let mut tick: u32 = 0;
        loop {
            thread::sleep(Duration::from_millis(50));
            let Some(win) = app.get_webview_window("main") else { continue };
            let geo = match (win.cursor_position(), win.inner_position(), win.scale_factor()) {
                (Ok(c), Ok(p), Ok(sf)) => {
                    let r = *st.rect.lock().unwrap();
                    let x = (c.x - p.x as f64) / sf;
                    let y = (c.y - p.y as f64) / sf;
                    r[2] > 0.0 && x >= r[0] - 4.0 && x <= r[0] + r[2] + 4.0 && y >= r[1] - 4.0 && y <= r[1] + r[3] + 4.0
                }
                _ => false,
            };
            if geo != last_geo {
                last_geo = geo;
                let _ = app.emit("cursor:inside", geo);
            }
            let want = st.capture.load(Ordering::Relaxed) || geo;
            tick = tick.wrapping_add(1);
            /* раз в секунду принудительно выставляем состояние, но только пока окно должно ловить мышь:
               place_window/shot_layout трогают его напрямую, и кэш last мог разойтись с реальностью (после скриншота окно
               оставалось «глухим» к кликам). Когда мышь не над островом, ничего не трогаем: перезапись стиля окна
               каждую секунду давала ритмичное мигание свёрнутого острова на части систем */
            if want != last || (want && tick % 20 == 0) {
                let _ = win.set_ignore_cursor_events(!want);
                last = want;
            }
        }
    });
}

fn place_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    if let Ok(Some(mon)) = win.primary_monitor() {
        let pos = mon.position();
        let size = mon.size();
        let sf = mon.scale_factor();
        /* на 4 пикселя ниже высоты монитора: окно, закрывающее экран целиком, Windows считает полноэкранным приложением
           (остров прятался и снова появлялся по кругу), а так не считает */
        let h = ((820.0 * sf) as u32).min(size.height.saturating_sub(4));
        let _ = win.set_position(PhysicalPosition::new(pos.x, pos.y));
        let _ = win.set_size(PhysicalSize::new(size.width, h));
    }
    let _ = win.set_ignore_cursor_events(true);
    let _ = win.show();
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Показать / скрыть", true, None::<&str>)?;
    let palette = MenuItem::with_id(app, "palette", "Командная строка", true, None::<&str>)?;
    let shot = MenuItem::with_id(app, "shot", "Скриншот области", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &palette, &shot, &sep, &quit])?;

    let mut b = TrayIconBuilder::new().tooltip("Nexora").menu(&menu).show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.on_menu_event(|app, ev| {
        let st: State<Arc<AppState>> = app.state();
        match ev.id().as_ref() {
            "toggle" => run_action(app, st.inner(), "toggle"),
            "palette" => run_action(app, st.inner(), "palette"),
            "shot" => run_action(app, st.inner(), "shot"),
            "quit" => app.exit(0),
            _ => {}
        }
    })
    .on_tray_icon_event(|tray, ev| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
            let app = tray.app_handle();
            show_window(app);
            let _ = app.emit("island:toggle", ());
        }
    })
    .build(app)?;
    Ok(())
}

fn main() {
    /* Визуализатор и страница музыки замирали, когда WebView2 считал окно скрытым/перекрытым (остров прозрачный и всегда сверху,
       окно музыки лежит за краем экрана): браузер замедлял таймеры и requestAnimationFrame. Отключаем это замедление. Флаги задаются ТОЛЬКО здесь, для всех окон сразу: у окна музыки и главного окна они обязаны совпадать,
       иначе WebView2 не создаёт второе окно (одна папка данных, разные параметры) и Яндекс Музыка во встроенном окне не открывается. */
    if std::env::var_os("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").is_none() {
        std::env::set_var(
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion,IntensiveWakeUpThrottling --disable-background-timer-throttling --disable-renderer-backgrounding --disable-backgrounding-occluded-windows",
        );
    }
    let state = Arc::new(AppState {
        audio: PsWorker::new("audio", AUDIO_PS),
        media: PsWorker::new("media", MEDIA_PS),
        mctl: PsWorker::new("mediactl", MEDIA_PS),
        notif: PsWorker::new("notif", NOTIF_PS),
        ui_active: AtomicBool::new(false),
        sticky: AtomicBool::new(false),
        capture: AtomicBool::new(false),
        levels_on: AtomicBool::new(false),
        levels_info: Mutex::new(String::from("ещё не запускался")),
        rect: Mutex::new([0.0; 4]),
        cover: Mutex::new((String::new(), String::new())),
        cover_pushed: Mutex::new(String::new()),
        last_media: Mutex::new(None),
        media_fails: AtomicU32::new(0),
        sys: Mutex::new(sysinfo::System::new()),
        hk: Mutex::new(hk_default()),
        clip_last: Mutex::new(String::new()),
        mouse_binds: Mutex::new(Vec::new()),
        mouse_paused: AtomicBool::new(false),
        user_hidden: AtomicBool::new(false),
        game_hidden: AtomicBool::new(false),
        auto_hide: AtomicBool::new(true),
        in_fs: AtomicBool::new(false),
        fs_override: AtomicBool::new(false),
    });

    let st_setup = state.clone();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_window(app);
            let _ = app.emit("island:toggle", ());
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            sys, ui_active, ui_sticky, hit, set_rect, media_key, media_state, media_seek, media_cover, vol_get, vol_set,
            vol_mute, clip_read, clip_write, open, power, theme_windows, minimize_all, autostart_get, autostart_set, quit,
            notif_poll, notif_discord, state_save, state_load, uninstall_app,
            hide_window, auto_hide_set, mouse_pause, hk_set, clip_mark_image, shot_layout, extras::ps_run, extras::run_cmd, extras::launch,
            extras::tcp_ping, extras::xray_find, extras::xray_start, extras::xray_stop, extras::xray_state,
            extras::bp_info, extras::bp_read, extras::bp_start, extras::bp_stop, extras::bp_state,
            extras::tg_start, extras::tg_stop, extras::tg_state,
            extras::shot_capture, levels_active, levels_diag, music::scrape_page, music::music_place, music::music_nav, music::music_eval,
            music::music_exec, music::music_diag, music::music_close
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            extras::ensure_dirs();
            /* автозапуск включается сам при первом запуске после установки (один раз; выключенный вручную потом не включается) */
            thread::spawn(|| {
                let marker = extras::data_dir().join("autostart-default-v1");
                if !marker.exists() {
                    /* маркер ставим только если автозапуск реально включён; иначе попробуем снова при следующем запуске */
                    let on = autostart_get() || autostart_set(true);
                    if on {
                        let _ = std::fs::create_dir_all(extras::data_dir());
                        let _ = std::fs::write(&marker, b"1");
                    }
                }
            });
            /* если прошлый запуск упал при включённом VPN, возвращаем прежние настройки системного прокси */
            thread::spawn(|| extras::restore_proxy_if_stale());
            place_window(&handle);
            build_tray(&handle)?;
            apply_hotkeys(&handle, &st_setup);
            start_threads(handle, st_setup.clone());
            /* прогрев PowerShell-процессов в фоне */
            let s = st_setup.clone();
            thread::spawn(move || {
                s.audio.request("get", Duration::from_secs(15));
                s.media.request("state", Duration::from_secs(15));
                s.mctl.request("ctl none", Duration::from_secs(15));
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("ошибка сборки приложения");

    app.run(move |_handle, event| {
        if let RunEvent::Exit = event {
            state.audio.stop();
            state.media.stop();
            state.mctl.stop();
            state.notif.stop();
            extras::shutdown();
        }
    });
}

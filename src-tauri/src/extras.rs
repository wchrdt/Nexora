/* Дополнительные команды (этап 3): запуск PowerShell-скриптов, командная строка, пинг серверов,
   Xray (VPN), скриншот. Только стандартная библиотека + Tauri, новых зависимостей нет. */

use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const SCRIPTS: &[(&str, &str)] = &[
    ("icon", include_str!("../scripts/icon.ps1")),
    ("pick", include_str!("../scripts/pick.ps1")),
    ("ocr", include_str!("../scripts/ocr.ps1")),
    ("shot", include_str!("../scripts/shot.ps1")),
    ("clipimg", include_str!("../scripts/clipimg.ps1")),
    ("http", include_str!("../scripts/http.ps1")),
    ("bpupdate", include_str!("../scripts/bpupdate.ps1")),
    ("netapps", include_str!("../scripts/netapps.ps1")),
    ("wifi", include_str!("../scripts/wifi.ps1")),
    ("scdesc", include_str!("../scripts/scdesc.ps1")),
];

static PS_LOCK: Mutex<()> = Mutex::new(());
static XRAY: Mutex<Option<Child>> = Mutex::new(None);
static ZAPRET: Mutex<Option<Child>> = Mutex::new(None);
static TGPROXY: Mutex<Option<Child>> = Mutex::new(None);

const XRAY_HTTP_PORT: &str = "127.0.0.1:10809";
const IS_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const REFRESH_PROXY_PS: &str = "$s='[DllImport(\"wininet.dll\")]public static extern bool InternetSetOption(IntPtr a,int b,IntPtr c,int d);';$t=Add-Type -MemberDefinition $s -Name W -Namespace N -PassThru;$t::InternetSetOption([IntPtr]::Zero,39,[IntPtr]::Zero,0)|Out-Null;$t::InternetSetOption([IntPtr]::Zero,37,[IntPtr]::Zero,0)|Out-Null";

fn quiet(cmd: &mut Command) {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

struct Out {
    ok: bool,
    out: String,
    err: String,
    timed_out: bool,
}

fn out_json(o: Out) -> Value {
    json!({ "ok": o.ok, "out": o.out, "err": o.err, "timedOut": o.timed_out })
}

/* запуск процесса с ограничением по времени; stdin_data подаётся в stdin процесса */
fn run_with_timeout(mut cmd: Command, stdin_data: Option<String>, timeout: Duration) -> Out {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    quiet(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Out { ok: false, out: String::new(), err: e.to_string(), timed_out: false },
    };
    if let Some(data) = stdin_data {
        if let Some(mut si) = child.stdin.take() {
            thread::spawn(move || {
                let _ = si.write_all(data.as_bytes());
            });
        }
    }
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let (txo, rxo) = mpsc::channel::<Vec<u8>>();
    let (txe, rxe) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(s) = so.as_mut() {
            let _ = s.read_to_end(&mut b);
        }
        let _ = txo.send(b);
    });
    thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_end(&mut b);
        }
        let _ = txe.send(b);
    });
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break None,
        }
    };
    let out = rxo.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    let err = rxe.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    Out {
        ok: status.map(|s| s.success()).unwrap_or(false),
        out: String::from_utf8_lossy(&out).to_string(),
        err: String::from_utf8_lossy(&err).to_string(),
        timed_out,
    }
}

fn script_path(name: &str) -> Option<PathBuf> {
    let text = SCRIPTS.iter().find(|(n, _)| *n == name)?.1;
    let _g = PS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let p = std::env::temp_dir().join(format!("island-{}.ps1", name));
    let want = format!("\u{feff}{}", text);
    let same = std::fs::read_to_string(&p).map(|c| c == want).unwrap_or(false);
    if !same {
        std::fs::write(&p, want.as_bytes()).ok()?;
    }
    Some(p)
}

fn ps_run_blocking(
    app: Option<&AppHandle>,
    name: &str,
    env: &HashMap<String, String>,
    stdin: Option<String>,
    timeout_ms: u64,
    dialog: bool,
) -> Out {
    let Some(path) = script_path(name) else {
        return Out { ok: false, out: String::new(), err: format!("нет скрипта {}", name), timed_out: false };
    };
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-STA", "-ExecutionPolicy", "Bypass", "-File"]);
    cmd.arg(&path);
    for (k, v) in env {
        cmd.env(k, v);
    }
    /* на время диалога окно острова (оно всегда сверху) опускаем, чтобы диалог не пропал под ним */
    let win = if dialog { app.and_then(|a| a.get_webview_window("main")) } else { None };
    if let Some(w) = &win {
        let _ = w.set_always_on_top(false);
    }
    let r = run_with_timeout(cmd, stdin, Duration::from_millis(timeout_ms));
    if let Some(w) = &win {
        let _ = w.set_always_on_top(true);
    }
    r
}

/* ---------- PowerShell-скрипт по имени ---------- */
#[tauri::command(rename_all = "snake_case")]
pub async fn ps_run(
    app: AppHandle,
    name: String,
    env: Option<HashMap<String, String>>,
    stdin: Option<String>,
    timeout_ms: Option<u64>,
    dialog: Option<bool>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let env = env.unwrap_or_default();
        out_json(ps_run_blocking(
            Some(&app),
            &name,
            &env,
            stdin,
            timeout_ms.unwrap_or(20000),
            dialog.unwrap_or(false),
        ))
    })
    .await
    .map_err(|e| e.to_string())
}

/* ---------- командная строка (cmd /C) ---------- */
#[tauri::command(rename_all = "snake_case")]
pub async fn run_cmd(cmd: String, timeout_ms: Option<u64>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut c = Command::new("cmd.exe");
        #[cfg(windows)]
        {
            c.raw_arg(format!("/C chcp 65001 >nul & {}", cmd));
        }
        #[cfg(not(windows))]
        {
            c.arg("/C").arg(&cmd);
        }
        if let Some(h) = std::env::var_os("USERPROFILE") {
            c.current_dir(h);
        }
        out_json(run_with_timeout(c, None, Duration::from_millis(timeout_ms.unwrap_or(20000))))
    })
    .await
    .map_err(|e| e.to_string())
}

/* ---------- запуск программы / ссылки / папки ---------- */
/* Запуск идёт через ShellExecuteW напрямую (без cmd.exe): цель передаётся как один аргумент
   и никакая командная оболочка её не разбирает. */
#[cfg(windows)]
#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteW(
        hwnd: *mut std::ffi::c_void,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show_cmd: i32,
    ) -> isize;
}

#[tauri::command]
pub fn launch(target: String) -> bool {
    let t = target.trim();
    if t.is_empty() || t.len() > 2048 || t.chars().any(|c| c.is_control()) {
        return false;
    }
    /* ShellExecute раскрывает %переменные% в пути: для обычных путей такие цели не запускаем,
       а для http(s)-ссылок % допустим (проценты в адресах) */
    let lower = t.to_ascii_lowercase();
    let is_web = lower.starts_with("http://") || lower.starts_with("https://");
    if t.contains('%') && !is_web {
        return false;
    }
    #[cfg(windows)]
    {
        /* Nexora работает от имени администратора, и всё, что запускает ShellExecute, наследует эти права.
           Окно с повышенными правами не принимает файлы, перетащенные из обычного проводника (защита UIPI):
           в FL Studio и Telegram нельзя было перенести mp3. Поэтому программы, папки и файлы открываем через
           explorer.exe: он работает без повышения прав и запускает цель от имени обычного пользователя. */
        if !is_web {
            let mut c = Command::new("explorer.exe");
            c.arg(t).creation_flags(CREATE_NO_WINDOW);
            if let Ok(mut child) = c.spawn() {
                thread::spawn(move || {
                    let _ = child.wait();
                });
                return true;
            }
        }
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let op = wide("open");
        let file = wide(t);
        /* SW_SHOWNORMAL = 1; результат больше 32 означает успех */
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        r > 32
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/* ---------- пинг серверов (TCP connect) ---------- */
fn ping_one(host: &str, port: u16) -> i64 {
    if host.is_empty() || port == 0 {
        return -1;
    }
    let t0 = Instant::now();
    /* DNS-запрос в отдельном потоке с ограничением по времени; пробуем несколько адресов, а не только первый */
    let (tx, rx) = mpsc::channel::<Vec<std::net::SocketAddr>>();
    let h = host.to_string();
    thread::spawn(move || {
        let r = (h.as_str(), port).to_socket_addrs().map(|a| a.collect::<Vec<_>>());
        let _ = tx.send(r.unwrap_or_default());
    });
    let addrs = match rx.recv_timeout(Duration::from_millis(3000)) {
        Ok(a) => a,
        Err(_) => return -1,
    };
    for a in addrs.into_iter().take(4) {
        let left = Duration::from_millis(3000).saturating_sub(t0.elapsed()).max(Duration::from_millis(300));
        if TcpStream::connect_timeout(&a, left).is_ok() {
            return t0.elapsed().as_millis() as i64;
        }
    }
    -1
}

#[tauri::command]
pub async fn tcp_ping(items: Vec<Value>) -> Result<Vec<i64>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let n = items.len().min(300);
        let out = Mutex::new(vec![-1i64; n]);
        let idx = AtomicUsize::new(0);
        thread::scope(|s| {
            for _ in 0..10 {
                s.spawn(|| loop {
                    let k = idx.fetch_add(1, Ordering::Relaxed);
                    if k >= n {
                        break;
                    }
                    let host = items[k].get("host").and_then(|v| v.as_str()).unwrap_or("");
                    let port = items[k].get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                    let ms = ping_one(host, port);
                    out.lock().unwrap()[k] = ms;
                });
            }
        });
        out.into_inner().unwrap_or_default()
    })
    .await
    .map_err(|e| e.to_string())
}

/* ---------- Xray (VPN) + системный прокси Windows ---------- */
pub fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("Nexora")
}

/* Создаём %APPDATA%\Nexora\xray заранее, чтобы было куда положить xray.exe */
pub fn ensure_dirs() {
    // Переезд со старого названия: папка W_Island (xray, свой звук) становится Nexora, если новой ещё нет.
    {
        let old = PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("W_Island");
        if old.exists() && !data_dir().exists() {
            let _ = std::fs::rename(&old, data_dir());
        }
    }
    // Постоянная пользовательская папка — сюда можно положить xray\xray.exe.
    let _ = std::fs::create_dir_all(data_dir().join("xray"));
    // Папка рядом с установленным приложением — первый путь поиска Xray.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let _ = std::fs::create_dir_all(dir.join("xray"));
        }
    }

    // В режиме разработки также создаём xray рядом с проектом, где find_xray()
    // уже ищет исполняемый файл (RUN_DEV.bat запускается из src-tauri).
    if let Ok(cwd) = std::env::current_dir() {
        let project_root = if cwd.file_name().map(|x| x == "src-tauri").unwrap_or(false) {
            cwd.parent().map(PathBuf::from)
        } else {
            Some(cwd.clone())
        };
        if let Some(root) = project_root {
            let _ = std::fs::create_dir_all(root.join("xray"));
        }
    }
}

fn find_xray() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..6 {
            match d {
                Some(x) => {
                    d = x.parent().map(|p| p.to_path_buf());
                    dirs.push(x);
                }
                None => break,
            }
        }
    }
    dirs.push(data_dir());
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    for d in dirs {
        let p = d.join("xray").join("xray.exe");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn reg_add(name: &str, ty: &str, data: &str) -> Result<(), String> {
    let mut c = Command::new("reg");
    c.args(["add", IS_KEY, "/v", name, "/t", ty, "/d", data, "/f"]);
    quiet(&mut c);
    let o = c.output().map_err(|e| format!("reg add {}: {}", name, e))?;
    if o.status.success() {
        Ok(())
    } else {
        Err(format!("reg add {}: {}", name, String::from_utf8_lossy(&o.stderr).trim()))
    }
}

fn proxy_backup_path() -> PathBuf {
    data_dir().join("proxy-backup.txt")
}

/* Читает значение из реестра. Ok(None) - параметра точно нет (ключ прочитан, а имени в нём нет),
   Err - реестр прочитать не удалось (это НЕ то же самое, что «параметра нет»). */
fn reg_read(name: &str) -> Result<Option<String>, String> {
    let mut c = Command::new("reg");
    c.args(["query", IS_KEY]);
    quiet(&mut c);
    let o = c.output().map_err(|e| format!("reg query: {}", e))?;
    if !o.status.success() {
        return Err(format!("reg query: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    for line in t.lines() {
        let l = line.trim();
        let mut it = l.splitn(2, char::is_whitespace);
        let (Some(n), Some(rest)) = (it.next(), it.next()) else { continue };
        if !n.eq_ignore_ascii_case(name) {
            continue;
        }
        let mut it2 = rest.trim_start().splitn(2, char::is_whitespace);
        let _ty = it2.next();
        return Ok(Some(it2.next().unwrap_or("").trim().to_string()));
    }
    Ok(None)
}

/* Удаляет параметр. Если его и так нет, это успех. */
fn reg_delete(name: &str) -> Result<(), String> {
    let mut c = Command::new("reg");
    c.args(["delete", IS_KEY, "/v", name, "/f"]);
    quiet(&mut c);
    let o = c.output().map_err(|e| format!("reg delete {}: {}", name, e))?;
    if o.status.success() {
        return Ok(());
    }
    match reg_read(name) {
        Ok(None) => Ok(()),
        _ => Err(format!("reg delete {}: {}", name, String::from_utf8_lossy(&o.stderr).trim())),
    }
}

const PROXY_NAMES: [&str; 3] = ["ProxyEnable", "ProxyServer", "ProxyOverride"];

/* одна запись резервной копии: (имя, существовал ли параметр, значение) */
type ProxyEntry = (String, bool, String);

static PROXY_LOCK: Mutex<()> = Mutex::new(());

/* Разбор и проверка резервной копии: ровно три разных параметра (ProxyEnable, ProxyServer, ProxyOverride),
   флаг 0 или 1, посторонних строк нет. Иначе копия считается повреждённой. */
fn parse_proxy_backup(t: &str) -> Option<Vec<ProxyEntry>> {
    let mut out: Vec<ProxyEntry> = Vec::new();
    for line in t.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut it = line.splitn(3, '=');
        let (Some(name), Some(flag), Some(val)) = (it.next(), it.next(), it.next()) else { return None };
        if !PROXY_NAMES.contains(&name) || out.iter().any(|e| e.0 == name) {
            return None;
        }
        let exists = match flag {
            "1" => true,
            "0" => false,
            _ => return None,
        };
        out.push((name.to_string(), exists, val.to_string()));
    }
    if out.len() == PROXY_NAMES.len() {
        Some(out)
    } else {
        None
    }
}

fn format_proxy_backup(e: &[ProxyEntry]) -> String {
    let mut s = String::new();
    for (n, ex, v) in e {
        s.push_str(&format!("{}={}={}\n", n, if *ex { 1 } else { 0 }, v));
    }
    s
}

/* «нейтральная» копия: прокси выключен, остальное удалить (когда настоящий оригинал утерян) */
fn neutral_proxy_entries() -> Vec<ProxyEntry> {
    vec![
        ("ProxyEnable".to_string(), true, "0x0".to_string()),
        ("ProxyServer".to_string(), false, String::new()),
        ("ProxyOverride".to_string(), false, String::new()),
    ]
}

/* сейчас в Windows включён именно наш прокси Xray? */
fn proxy_is_ours() -> Result<bool, String> {
    let en = reg_read("ProxyEnable")?.map(|v| v.trim().eq_ignore_ascii_case("0x1")).unwrap_or(false);
    let srv = reg_read("ProxyServer")?.map(|v| v.trim().eq_ignore_ascii_case(XRAY_HTTP_PORT)).unwrap_or(false);
    Ok(en && srv)
}

/* убрать повреждённую копию в сторону, чтобы она не мешала */
fn proxy_backup_set_aside(p: &PathBuf) -> Result<(), String> {
    let bad = p.with_extension("bad");
    let _ = std::fs::remove_file(&bad);
    std::fs::rename(p, &bad).map_err(|e| format!("повреждённая резервная копия прокси: {}", e))
}

/* запоминаем прежние настройки прокси Windows перед включением VPN (один раз: пока файл есть, он хранит оригинал).
   Существующая копия сначала проверяется; повреждённая не принимается за оригинал.
   Файл создаётся только если ВСЕ три параметра прочитаны без ошибок и запись удалась. */
fn proxy_backup() -> Result<(), String> {
    let p = proxy_backup_path();
    if p.exists() {
        let ok = std::fs::read_to_string(&p).ok().and_then(|t| parse_proxy_backup(&t)).is_some();
        if ok {
            return Ok(());
        }
        proxy_backup_set_aside(&p)?;
    }
    let mut entries: Vec<ProxyEntry> = Vec::new();
    for n in PROXY_NAMES {
        match reg_read(n)? {
            Some(v) => entries.push((n.to_string(), true, v)),
            None => entries.push((n.to_string(), false, String::new())),
        }
    }
    /* если сейчас стоит наш же прокси (старая копия была потеряна), настоящий оригинал неизвестен:
       сохраняем нейтральное состояние, чтобы после VPN не остаться на мёртвом 127.0.0.1:10809 */
    if proxy_is_ours()? {
        entries = neutral_proxy_entries();
    }
    std::fs::create_dir_all(data_dir()).map_err(|e| format!("папка данных: {}", e))?;
    let body = format_proxy_backup(&entries);
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, body.as_bytes()).map_err(|e| format!("запись резервной копии прокси: {}", e))?;
    std::fs::rename(&tmp, &p).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("запись резервной копии прокси: {}", e)
    })?;
    /* убеждаемся, что копия действительно записана и проходит проверку */
    match std::fs::read_to_string(&p).ok().and_then(|t| parse_proxy_backup(&t)) {
        Some(_) => Ok(()),
        None => Err("резервная копия прокси не прошла проверку".to_string()),
    }
}

/* Ok(false) - сохранённого оригинала нет; Ok(true) - оригинал восстановлен и проверен.
   Резервная копия удаляется только после подтверждённого восстановления всех трёх параметров. */
fn proxy_restore() -> Result<bool, String> {
    let p = proxy_backup_path();
    if !p.exists() {
        return Ok(false);
    }
    let t = std::fs::read_to_string(&p).map_err(|e| format!("чтение резервной копии прокси: {}", e))?;
    let entries = match parse_proxy_backup(&t) {
        Some(e) => e,
        None => {
            /* повреждена: оригинал неизвестен. Откладываем файл и, если в Windows стоит наш прокси, выключаем его */
            proxy_backup_set_aside(&p)?;
            if !proxy_is_ours()? {
                return Ok(false);
            }
            neutral_proxy_entries()
        }
    };
    let mut errs: Vec<String> = Vec::new();
    for (name, exists, val) in &entries {
        let done = if *exists {
            let ty = if name == "ProxyEnable" { "REG_DWORD" } else { "REG_SZ" };
            reg_add(name, ty, val)
        } else {
            reg_delete(name)
        };
        if let Err(e) = done {
            errs.push(e);
            continue;
        }
        /* проверка: значение в реестре действительно стало таким, как в копии */
        match reg_read(name) {
            Ok(cur) => {
                let ok = if *exists {
                    cur.map(|c| c.eq_ignore_ascii_case(val)).unwrap_or(false)
                } else {
                    cur.is_none()
                };
                if !ok {
                    errs.push(format!("{}: значение после восстановления не совпало", name));
                }
            }
            Err(e) => errs.push(e),
        }
    }
    if !errs.is_empty() {
        /* копия остаётся на диске: восстановление будет повторено */
        return Err(errs.join("; "));
    }
    if let Err(e) = std::fs::remove_file(&p) {
        if p.exists() {
            return Err(format!("не удалось удалить резервную копию прокси: {}", e));
        }
    }
    Ok(true)
}

fn refresh_proxy() -> Result<(), String> {
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-Command", REFRESH_PROXY_PS]);
    quiet(&mut c);
    let o = c.output().map_err(|e| format!("уведомление Windows об изменении прокси: {}", e))?;
    if o.status.success() {
        Ok(())
    } else {
        Err("уведомление Windows об изменении прокси не выполнено".to_string())
    }
}

/* значения в реестре уже изменены и проверены; сбой уведомления не критичен (приложения подхватят настройки позже), но не теряется молча */
fn refresh_proxy_logged() {
    if let Err(e) = refresh_proxy() {
        eprintln!("[Nexora] {}", e);
    }
}

fn set_sys_proxy(on: bool) -> Result<(), String> {
    let _g = PROXY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if on {
        /* без подтверждённой резервной копии прокси не меняем: иначе исходные настройки можно потерять */
        proxy_backup()?;
        let r = reg_add("ProxyServer", "REG_SZ", XRAY_HTTP_PORT)
            .and_then(|_| reg_add("ProxyOverride", "REG_SZ", "localhost;127.*;10.*;192.168.*;<local>"))
            .and_then(|_| reg_add("ProxyEnable", "REG_DWORD", "1"));
        if let Err(e) = r {
            /* частично применили: откатываем к сохранённому оригиналу */
            let _ = proxy_restore();
            refresh_proxy_logged();
            return Err(e);
        }
        refresh_proxy_logged();
        Ok(())
    } else {
        /* если сохранённого оригинала нет, мы прокси не меняли, значит и трогать нечего */
        if proxy_restore()? {
            refresh_proxy_logged();
        }
        Ok(())
    }
}

/* при запуске приложения: если от прошлого сеанса остался сохранённый оригинал, а Xray не запущен, возвращаем прокси */
pub fn restore_proxy_if_stale() {
    let running = XRAY.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    if !running && proxy_backup_path().exists() {
        if let Err(e) = set_sys_proxy(false) {
            eprintln!("[Nexora] не удалось вернуть системный прокси: {}", e);
        }
    }
}

fn xray_kill() -> bool {
    let mut g = XRAY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut ch) = g.take() {
        let _ = ch.kill();
        let _ = ch.wait();
        return true;
    }
    false
}

fn log_tail(max: usize) -> String {
    let t = std::fs::read(data_dir().join("xray.log")).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();
    let t = t.trim().to_string();
    if t.chars().count() <= max {
        return t;
    }
    let skip = t.chars().count() - max;
    t.chars().skip(skip).collect()
}

#[tauri::command]
pub fn xray_find() -> Option<String> {
    find_xray().map(|p| p.display().to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn xray_start(cfg_json: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(exe) = find_xray() else {
            return json!({ "ok": false, "needXray": true, "err": "Не найден xray\\xray.exe рядом с островом" });
        };
        xray_kill();
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let cfg_path = dir.join("xray-config.json");
        if std::fs::write(&cfg_path, cfg_json.as_bytes()).is_err() {
            return json!({ "ok": false, "err": "Не удалось записать конфиг Xray" });
        }
        let log_file = match File::create(dir.join("xray.log")) {
            Ok(f) => f,
            Err(e) => return json!({ "ok": false, "err": format!("Не удалось создать лог: {}", e) }),
        };
        let log_file2 = match log_file.try_clone() {
            Ok(f) => f,
            Err(e) => return json!({ "ok": false, "err": format!("Не удалось создать лог: {}", e) }),
        };
        let mut cmd = Command::new(&exe);
        cmd.arg("run").arg("-c").arg(&cfg_path);
        if let Some(p) = exe.parent() {
            cmd.current_dir(p);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::from(log_file)).stderr(Stdio::from(log_file2));
        quiet(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return json!({ "ok": false, "err": format!("Xray не запустился: {}", e) }),
        };
        thread::sleep(Duration::from_millis(1200));
        if let Ok(Some(_)) = child.try_wait() {
            let _ = set_sys_proxy(false);
            return json!({ "ok": false, "err": "Xray не запустился", "log": log_tail(400) });
        }
        *XRAY.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        if let Err(e) = set_sys_proxy(true) {
            /* прокси не включился (или не удалось сохранить оригинал): VPN без прокси бесполезен, гасим Xray */
            xray_kill();
            return json!({ "ok": false, "err": format!("Не удалось включить системный прокси: {}", e) });
        }
        json!({ "ok": true })
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn xray_stop() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        xray_kill();
        match set_sys_proxy(false) {
            Ok(()) => json!({ "ok": true }),
            /* резервная копия осталась на диске: восстановление повторится при следующем запуске */
            Err(e) => json!({ "ok": false, "err": format!("Не удалось вернуть системный прокси: {}", e) }),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn xray_state() -> Value {
    let mut g = XRAY.lock().unwrap_or_else(|e| e.into_inner());
    let mut on = false;
    let mut dead = false;
    if let Some(ch) = g.as_mut() {
        match ch.try_wait() {
            Ok(None) => on = true,
            _ => dead = true,
        }
    }
    if dead {
        *g = None;
    }
    drop(g);
    /* ошибка возврата прокси не теряется: идёт в состояние VPN, копия остаётся, и при каждом опросе пробуем снова,
       пока Xray не запущен */
    let mut proxy_err: Option<String> = None;
    if !on && (dead || proxy_backup_path().exists()) {
        if let Err(e) = set_sys_proxy(false) {
            proxy_err = Some(e);
        }
    }
    json!({ "on": on, "log": log_tail(300), "proxyErr": proxy_err })
}

/* при выходе из приложения: остановить Xray и вернуть системный прокси */
pub fn shutdown() {
    let had = xray_kill();
    if had || proxy_backup_path().exists() {
        if let Err(e) = set_sys_proxy(false) {
            eprintln!("[Nexora] не удалось вернуть системный прокси: {}", e);
        }
    }
    zapret_kill();
    tg_kill();
}

/* ---------- обход блокировок: zapret (winws.exe) и Telegram-прокси ---------- */
fn tail_file(path: PathBuf, max: usize) -> String {
    let t = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();
    let t = t.trim().to_string();
    let n = t.chars().count();
    if n <= max {
        return t;
    }
    t.chars().skip(n - max).collect()
}

/* папка набора zapret (внутри bin\winws.exe): сначала обновлённая в %APPDATA%\Nexora\zapret, затем вшитая рядом с приложением */
fn engine_dir() -> Option<PathBuf> {
    let ok = |d: &PathBuf| d.join("bin").join("winws.exe").exists();
    let d = data_dir().join("zapret");
    if ok(&d) {
        return Some(d);
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..6 {
            match cur {
                Some(x) => {
                    let c = x.join("zapret");
                    if ok(&c) {
                        return Some(c);
                    }
                    cur = x.parent().map(|p| p.to_path_buf());
                }
                None => break,
            }
        }
    }
    None
}

fn zapret_kill() -> bool {
    let mut was = false;
    if let Some(mut c) = ZAPRET.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = c.kill();
        let _ = c.wait();
        was = true;
    }
    /* «осиротевший» winws.exe после сбоя: гасим только процессы из папки нашего набора,
       отдельно установленный пользователем zapret (другая папка) не трогаем */
    if !was {
        if let Some(dir) = engine_dir() {
            let mut cmd = Command::new("powershell.exe");
            cmd.args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                r#"Get-CimInstance Win32_Process -Filter "Name='winws.exe'" | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($env:ISL_DIR, [StringComparison]::OrdinalIgnoreCase) } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }"#,
            ]);
            cmd.env("ISL_DIR", dir.display().to_string());
            let _ = run_with_timeout(cmd, None, Duration::from_secs(8));
        }
    }
    was
}

#[tauri::command]
pub fn bp_info() -> Value {
    let Some(dir) = engine_dir() else {
        return json!({ "has": false });
    };
    let ver = std::fs::read_to_string(dir.join("version.txt")).unwrap_or_default().trim().to_string();
    let mut list: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            let l = n.to_lowercase();
            if l.ends_with(".bat") && !l.starts_with("service") {
                list.push(n);
            }
        }
    }
    list.sort();
    json!({ "has": true, "dir": dir.display().to_string(), "version": ver, "strategies": list })
}

#[tauri::command]
pub fn bp_read(name: String) -> Value {
    if name.contains('/') || name.contains('\\') || !name.to_lowercase().ends_with(".bat") {
        return json!({ "ok": false, "err": "плохое имя файла" });
    }
    let Some(dir) = engine_dir() else {
        return json!({ "ok": false, "err": "нет набора zapret" });
    };
    match std::fs::read(dir.join(&name)) {
        Ok(b) => json!({ "ok": true, "text": String::from_utf8_lossy(&b).to_string(), "dir": dir.display().to_string() }),
        Err(e) => json!({ "ok": false, "err": e.to_string() }),
    }
}

/* Дополнительные сайты для обхода (поддомены zapret подхватывает сам). Дописываются в списки набора при каждом запуске,
   потому что обновление набора перезаписывает папку lists. */
const EXTRA_DOMAINS: &[&str] = &["soundcloud.com", "sndcdn.com", "soundcloud.cloud"];

fn ensure_domains(dir: &PathBuf) {
    let lists = dir.join("lists");
    let _ = std::fs::create_dir_all(&lists);
    /* winws падает с «cannot access ipset file ... failed to register ipset», если файла списка нет или он пустой.
       Flowseal создаёт их в service.bat; здесь делаем то же самое (заглушки из его набора). */
    for (name, stub) in [
        ("ipset-exclude-user.txt", "203.0.113.113/32"),
        ("ipset-exclude.txt", "203.0.113.113/32"),
        ("ipset-all.txt", "203.0.113.113/32"),
        ("list-exclude-user.txt", "domain.example.abc"),
        ("list-exclude.txt", "domain.example.abc"),
    ] {
        let path = lists.join(name);
        let empty = std::fs::read_to_string(&path).map(|t| t.trim().is_empty()).unwrap_or(true);
        if empty {
            let _ = std::fs::write(&path, format!("{}\r\n", stub));
        }
    }
    /* пользовательский список (создаём, если нет) и основной (только если уже есть: на него ссылаются все стратегии) */
    for (name, create) in [("list-general-user.txt", true), ("list-general.txt", false)] {
        let path = lists.join(name);
        let cur = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => {
                if !create {
                    continue;
                }
                String::new()
            }
        };
        let have: Vec<String> = cur.lines().map(|l| l.trim().to_lowercase()).collect();
        let missing: Vec<&&str> = EXTRA_DOMAINS.iter().filter(|d| !have.iter().any(|h| h == **d)).collect();
        if missing.is_empty() {
            continue;
        }
        let mut out = cur.clone();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str("\r\n");
        }
        for d in missing {
            out.push_str(d);
            out.push_str("\r\n");
        }
        let _ = std::fs::write(&path, out);
    }
}

#[tauri::command]
pub async fn bp_start(args: Vec<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(dir) = engine_dir() else {
            return json!({ "ok": false, "err": "Не найден набор zapret (bin\\winws.exe)" });
        };
        ensure_domains(&dir);
        let exe = dir.join("bin").join("winws.exe");
        zapret_kill();
        let _ = std::fs::create_dir_all(data_dir());
        let log_path = data_dir().join("zapret.log");
        let log = match File::create(&log_path) {
            Ok(f) => f,
            Err(e) => return json!({ "ok": false, "err": format!("Не удалось создать лог: {}", e) }),
        };
        let log2 = match log.try_clone() {
            Ok(f) => f,
            Err(e) => return json!({ "ok": false, "err": format!("Не удалось создать лог: {}", e) }),
        };
        let mut cmd = Command::new(&exe);
        cmd.args(&args).current_dir(dir.join("bin")).stdin(Stdio::null()).stdout(Stdio::from(log)).stderr(Stdio::from(log2));
        quiet(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return json!({ "ok": false, "err": format!("winws не запустился: {} (нужны права администратора)", e) }),
        };
        thread::sleep(Duration::from_millis(1200));
        if let Ok(Some(_)) = child.try_wait() {
            return json!({ "ok": false, "err": format!("winws сразу завершился: {}", tail_file(log_path, 300)) });
        }
        *ZAPRET.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        json!({ "ok": true })
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn bp_stop() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        zapret_kill();
        json!({ "ok": true })
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn bp_state() -> Value {
    let mut g = ZAPRET.lock().unwrap_or_else(|e| e.into_inner());
    let mut on = false;
    let mut dead = false;
    if let Some(ch) = g.as_mut() {
        match ch.try_wait() {
            Ok(None) => on = true,
            _ => dead = true,
        }
    }
    if dead {
        *g = None;
    }
    json!({ "on": on })
}

fn tg_exe() -> PathBuf {
    data_dir().join("tgproxy").join("TgWsProxy_windows.exe")
}

fn norm_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_lowercase()
}

/* Завершаем только наш Telegram-прокси: запущенный Nexora дочерний процесс и процессы,
   чей полный путь совпадает с нашим tgproxy\\TgWsProxy_windows.exe. Чужие копии с таким же именем не трогаем. */
fn tg_kill() {
    if let Some(mut c) = TGPROXY.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    let ours = norm_path(&tg_exe());
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    for p in sys.processes().values() {
        if let Some(exe) = p.exe() {
            if norm_path(exe) == ours {
                let _ = p.kill();
            }
        }
    }
}

#[tauri::command]
pub async fn tg_start() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let exe = tg_exe();
        if !exe.exists() {
            return json!({ "ok": false, "need": true, "err": "Прокси для Telegram ещё не скачан" });
        }
        tg_kill();
        let mut cmd = Command::new(&exe);
        if let Some(p) = exe.parent() {
            cmd.current_dir(p);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        match cmd.spawn() {
            Ok(c) => {
                *TGPROXY.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
                json!({ "ok": true })
            }
            Err(e) => json!({ "ok": false, "err": format!("Не запустился: {}", e) }),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn tg_stop() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        tg_kill();
        json!({ "ok": true })
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn tg_state() -> Value {
    let mut g = TGPROXY.lock().unwrap_or_else(|e| e.into_inner());
    let mut on = false;
    let mut dead = false;
    if let Some(ch) = g.as_mut() {
        match ch.try_wait() {
            Ok(None) => on = true,
            _ => dead = true,
        }
    }
    if dead {
        *g = None;
    }
    json!({ "on": on, "has": tg_exe().exists() })
}

/* ---------- скриншот всего основного экрана (остров на время съёмки прячется) ---------- */
#[tauri::command]
pub async fn shot_capture(app: AppHandle) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let win = app.get_webview_window("main");
        let was_visible = win.as_ref().map(|w| w.is_visible().unwrap_or(false)).unwrap_or(false);
        if was_visible {
            if let Some(w) = &win {
                let _ = w.hide();
            }
        }
        thread::sleep(Duration::from_millis(250));
        let o = ps_run_blocking(None, "shot", &HashMap::new(), None, 25000, false);
        if was_visible {
            if let Some(w) = &win {
                let _ = w.show();
            }
        }
        let mut it = o.out.splitn(2, '\n');
        let dims = it.next().unwrap_or("").trim().to_string();
        let b64 = it.next().unwrap_or("").trim().to_string();
        if b64.is_empty() {
            let msg = if o.err.trim().is_empty() { "Не удалось захватить экран".to_string() } else { o.err.trim().to_string() };
            return Err(msg);
        }
        let mut p = dims.split_whitespace();
        let w: u64 = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let h: u64 = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        Ok(json!({ "dataUrl": format!("data:image/png;base64,{}", b64), "w": w, "h": h }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/* ---------- картинка из буфера обмена -> BMP data URL ---------- */
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() / 3 * 4 + 4);
    for ch in data.chunks(3) {
        let b0 = ch[0] as u32;
        let b1 = if ch.len() > 1 { ch[1] as u32 } else { 0 };
        let b2 = if ch.len() > 2 { ch[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        s.push(B64[((n >> 18) & 63) as usize] as char);
        s.push(B64[((n >> 12) & 63) as usize] as char);
        s.push(if ch.len() > 1 { B64[((n >> 6) & 63) as usize] as char } else { '=' });
        s.push(if ch.len() > 2 { B64[(n & 63) as usize] as char } else { '=' });
    }
    s
}

/* rgba -> несжатый 24-битный BMP (при больших размерах уменьшаем в целое число раз) */
pub fn rgba_to_bmp_data_url(width: usize, height: usize, rgba: &[u8]) -> String {
    if width == 0 || height == 0 || rgba.len() < width * height * 4 {
        return String::new();
    }
    let step = (((width * height) as f64 / 1_500_000.0).sqrt().ceil() as usize).max(1);
    let ow = (width / step).max(1);
    let oh = (height / step).max(1);
    let row = (ow * 3 + 3) / 4 * 4;
    let size = 54 + row * oh;
    let mut b: Vec<u8> = Vec::with_capacity(size);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(size as u32).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(ow as i32).to_le_bytes());
    b.extend_from_slice(&(-(oh as i32)).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&((row * oh) as u32).to_le_bytes());
    b.extend_from_slice(&2835u32.to_le_bytes());
    b.extend_from_slice(&2835u32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    for y in 0..oh {
        let sy = y * step;
        for x in 0..ow {
            let i = (sy * width + x * step) * 4;
            b.push(rgba[i + 2]);
            b.push(rgba[i + 1]);
            b.push(rgba[i]);
        }
        for _ in 0..(row - ow * 3) {
            b.push(0);
        }
    }
    format!("data:image/bmp;base64,{}", b64_encode(&b))
}

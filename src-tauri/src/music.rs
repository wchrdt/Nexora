//! Встроенное окно Яндекс Музыки / SoundCloud.
//! В Tauri нет тега <webview>, поэтому сайт открывается в отдельном безрамочном окне WebView2,
//! которое привязано к главному окну (всегда над ним) и кладётся поверх области «Источник» на вкладке «Музыка».
//! Когда остров свёрнут или открыта другая вкладка, окно уводится за пределы экрана (не скрывается),
//! чтобы страница не перезагружалась и музыка не прерывалась.
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, Url, WebviewUrl, WebviewWindowBuilder};

const OFF: i32 = -32000;
/* причина последнего сбоя создания окна музыки (показывается в «Диагностике музыки») */
static MUSIC_ERR: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
fn set_music_err(e: String) {
    if let Ok(mut g) = MUSIC_ERR.lock() {
        *g = e;
    }
}

/* target=_blank и window.open открываем в этом же окне (иначе вход в аккаунт Яндекса открывается «в никуда») */
const INIT_JS: &str = r#"(function(){
  try{
    window.open=function(u){if(u){location.href=u;}return null;};
    document.addEventListener('click',function(e){
      var a=e.target&&e.target.closest?e.target.closest('a[target=_blank]'):null;
      if(a){a.target='_self';}
    },true);
  }catch(e){}
  /* SoundCloud играет через виджет острова, а сам сайт глушим (window.__islMute выставляет интерфейс; как setAudioMuted в Electron) */
  try{
    setInterval(function(){
      try{
        var els=document.querySelectorAll('audio,video');
        for(var i=0;i<els.length;i++){
          var e=els[i];
          if(window.__islMute){ if(!e.muted){e.muted=true;} e.setAttribute('data-islm','1'); }
          else if(e.getAttribute('data-islm')){ e.muted=false; e.removeAttribute('data-islm'); }
        }
      }catch(x){}
    },250);
  }catch(e){}
})();
/* Громкость музыки (ползунок острова). Яндекс Музыка сама задаёт громкость плееру и сбрасывает мою, поэтому громкость
   страницы делается множителем window.__islVol поверх любых её обращений: перехватываются HTMLMediaElement.volume
   и выход Web Audio (AudioDestinationNode). Значение хранится в localStorage страницы, чтобы пережить перезагрузку. */
(function(){
  try{
    if(window.__islVolInit)return; window.__islVolInit=true;
    var saved=1; try{ var sv=parseFloat(localStorage.getItem('__islVol')); if(!isNaN(sv))saved=Math.max(0,Math.min(1,sv)); }catch(e){}
    window.__islVol=saved;
    var d=Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype,'volume');
    var req=new WeakMap();
    var clamp=function(v){ v=+v; if(isNaN(v))v=1; return Math.max(0,Math.min(1,v)); };
    Object.defineProperty(HTMLMediaElement.prototype,'volume',{
      configurable:true, enumerable:true,
      get:function(){ return req.has(this)?req.get(this):d.get.call(this); },
      set:function(v){ var b=clamp(v); req.set(this,b); d.set.call(this,clamp(b*window.__islVol)); }
    });
    var gains=[];
    var oc=AudioNode.prototype.connect;
    AudioNode.prototype.connect=function(dest){
      try{
        if(typeof AudioDestinationNode!=='undefined' && dest instanceof AudioDestinationNode){
          var ctx=dest.context;
          if(!ctx.__islG){ var g=ctx.createGain(); g.gain.value=window.__islVol; oc.call(g,dest); ctx.__islG=g; gains.push(g); }
          var a=Array.prototype.slice.call(arguments); a[0]=ctx.__islG; return oc.apply(this,a);
        }
      }catch(e){}
      return oc.apply(this,arguments);
    };
    window.__islApplyVol=function(v){
      window.__islVol=clamp(v);
      try{ localStorage.setItem('__islVol',String(window.__islVol)); }catch(e){}
      var els=document.querySelectorAll('audio,video');
      for(var i=0;i<els.length;i++){ var b=req.has(els[i])?req.get(els[i]):1; try{ d.set.call(els[i],clamp(b*window.__islVol)); }catch(e){} }
      for(var j=0;j<gains.length;j++){ try{ gains[j].gain.value=window.__islVol; }catch(e){} }
      return els.length+gains.length;
    };
    /* уже созданные элементы (на случай, если страница успела их создать до перехвата) подгоняем сразу */
    if(window.__islVol<1){ try{ window.__islApplyVol(window.__islVol); }catch(e){} }
  }catch(e){}
})();
"#;

/* SoundCloud рисует десктопную вёрстку шириной ~1300 px; в узком окне острова она обрезается и уходит за край.
   Поэтому для SoundCloud масштабируем страницу (zoom WebView2) так, чтобы весь сайт по ширине помещался в окно, как в обычном браузере.
   Яндекс Музыка всегда остаётся в масштабе 100%. Размер самого окна не меняется. */
static LAST_W: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(900);
const SC_DESIGN_W: f64 = 1300.0;

fn is_sc_url(u: &str) -> bool {
    u.parse::<Url>().ok().and_then(|x| x.host_str().map(|h| h.to_ascii_lowercase())).map_or(false, |h| h == "soundcloud.com" || h.ends_with(".soundcloud.com"))
}

fn apply_zoom(win: &tauri::WebviewWindow, sc: bool, w_css: f64) {
    let z = if sc { (w_css / SC_DESIGN_W).clamp(0.4, 1.0) } else { 1.0 };
    let _ = win.set_zoom(z);
}

fn allowed(url: &str) -> bool {
    let Ok(u) = url.parse::<Url>() else { return false };
    if u.scheme() != "https" {
        return false;
    }
    let Some(h) = u.host_str() else { return false };
    let h = h.to_ascii_lowercase();
    if h == "soundcloud.com" || h == "www.soundcloud.com" {
        return true;
    }
    /* точная проверка имени хоста: music.yandex.<домен Яндекса>, а не любой адрес, который просто начинается так же */
    if let Some(rest) = h.strip_prefix("music.yandex.") {
        return ["ru", "com", "by", "kz", "uz", "ua", "com.tr", "az", "com.am", "com.ge", "md", "kg", "tj", "tm", "lt", "lv", "ee", "fr", "co.il"].contains(&rest);
    }
    false
}

/* Скругляем углы самого окна (region), иначе прямоугольник страницы торчит за скруглённым блоком острова */
#[cfg(windows)]
fn round_window(win: &tauri::WebviewWindow, w: u32, h: u32, radius: i32, cx: i32, cy: i32, cw: i32, ch: i32) {
    use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
    if let Ok(hwnd) = win.hwnd() {
        unsafe {
            // Окно WebView2 — отдельное нативное окно: CSS overflow родителя его не обрезает.
            // Поэтому задаём регион только для пересечения контейнера с видимой областью острова.
            let left = cx.clamp(0, w as i32);
            let top = cy.clamp(0, h as i32);
            let right = (left + cw.max(0)).clamp(left, w as i32);
            let bottom = (top + ch.max(0)).clamp(top, h as i32);
            let rr = radius.min((right - left).min(bottom - top) / 2).max(0);
            let rgn = CreateRoundRectRgn(left, top, right.max(left + 1), bottom.max(top + 1), rr * 2, rr * 2);
            let _ = SetWindowRgn(hwnd, Some(rgn), true);
        }
    }
}

#[cfg(not(windows))]
fn round_window(_win: &tauri::WebviewWindow, _w: u32, _h: u32, _radius: i32, _cx: i32, _cy: i32, _cw: i32, _ch: i32) {}

/* Создаёт окно при первом вызове (нужен url), дальше только двигает. Команда async: создание окон в sync-командах на Windows виснет. */
#[tauri::command]
pub async fn music_place(app: AppHandle, url: Option<String>, x: f64, y: f64, w: f64, h: f64, show: bool, r: Option<f64>, clip_x: f64, clip_y: f64, clip_w: f64, clip_h: f64) -> bool {
    let Some(main) = app.get_webview_window("main") else { return false };
    let sf = main.scale_factor().unwrap_or(1.0);
    let Ok(base) = main.inner_position() else { return false };
    let px = base.x + (x * sf).round() as i32;
    let py = base.y + (y * sf).round() as i32;
    let pw = (w * sf).round().max(60.0) as u32;
    let ph = (h * sf).round().max(60.0) as u32;

    let win = match app.get_webview_window("music") {
        Some(win) => win,
        None => {
            let Some(u) = url.as_deref().filter(|s| allowed(s)).and_then(|s| s.parse::<Url>().ok()) else { return false };
            let b = WebviewWindowBuilder::new(&app, "music", WebviewUrl::External(u))
                .title("Nexora music")
                .decorations(false)
                .resizable(false)
                .skip_taskbar(true)
                .always_on_top(true)
                .shadow(false)
                .focused(false)
                .visible(false)
                .inner_size(600.0, 400.0)
                .initialization_script(INIT_JS);
            let b = match b.parent(&main) {
                Ok(b) => b,
                Err(e) => {
                    set_music_err(format!("parent: {}", e));
                    return false;
                }
            };
            let win = match b.build() {
                Ok(w) => w,
                Err(e) => {
                    set_music_err(format!("build: {}", e));
                    return false;
                }
            };
            let _ = win.set_position(PhysicalPosition::new(OFF, OFF));
            let _ = win.show();
            win
        }
    };

    if show {
        /* позиция, размер и «поверх всех» проверяются: при сбое сообщаем false, интерфейс повторит попытку */
        let placed = win.set_size(PhysicalSize::new(pw, ph)).is_ok()
            && win.set_position(PhysicalPosition::new(px, py)).is_ok()
            && win.set_always_on_top(true).is_ok();
        if !placed {
            return false;
        }
        /* скруглённые углы окна, как у блока «Источник» в интерфейсе острова */
        let rad = (r.unwrap_or(14.0) * sf).round().max(0.0) as i32;
        let cx = (clip_x * sf).round() as i32;
        let cy = (clip_y * sf).round() as i32;
        let cw = (clip_w * sf).round().max(0.0) as i32;
        let ch = (clip_h * sf).round().max(0.0) as i32;
        round_window(&win, pw, ph, rad, cx, cy, cw, ch);
        LAST_W.store(w.max(1.0) as u32, std::sync::atomic::Ordering::Relaxed);
        let sc = win.url().map(|u| is_sc_url(u.as_str())).unwrap_or(false);
        apply_zoom(&win, sc, w);
    } else if win.set_position(PhysicalPosition::new(OFF, OFF)).is_err() {
        return false;
    }
    true
}

/* Перейти на адрес внутри окна (только Яндекс Музыка / SoundCloud) */
#[tauri::command]
pub fn music_nav(app: AppHandle, url: String) -> bool {
    if !allowed(&url) {
        return false;
    }
    let Some(w) = app.get_webview_window("music") else { return false };
    apply_zoom(&w, is_sc_url(&url), LAST_W.load(std::sync::atomic::Ordering::Relaxed) as f64);
    let js = format!("location.href={};", serde_json::to_string(&url).unwrap_or_default());
    w.eval(js).is_ok()
}

/* Выполнить скрипт на странице (поиск, перемотка). Результат не возвращается. */
#[tauri::command]
pub fn music_eval(app: AppHandle, js: String) -> bool {
    let Some(w) = app.get_webview_window("music") else { return false };
    if js.len() > 20000 {
        return false;
    }
    w.eval(js).is_ok()
}

/* Выполнить скрипт на странице через WebView2 (ExecuteScript) и вернуть результат (JSON-строка).
   Так интерфейс читает со страницы трек, обложку, время, текст песни и результат поиска — как executeJavaScript у <webview> в Electron.
   При сбое возвращаем причину текстом (её видно в «Диагностике музыки»). */
type ExecTx = std::sync::mpsc::Sender<Result<String, String>>;

#[cfg(windows)]
fn exec_js(w: &tauri::WebviewWindow, js: String, tx: ExecTx) -> Result<(), String> {
    w.with_webview(move |wv| unsafe {
        use webview2_com::ExecuteScriptCompletedHandler;
        use windows::core::PCWSTR;
        let core = match wv.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Err(format!("CoreWebView2: {}", e)));
                return;
            }
        };
        let tx2 = tx.clone();
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |err, result| {
            let _ = tx2.send(match err {
                Ok(()) => Ok(result),
                Err(e) => Err(format!("ExecuteScript: {}", e)),
            });
            Ok(())
        }));
        let wide: Vec<u16> = js.encode_utf16().chain(std::iter::once(0)).collect();
        if let Err(e) = core.ExecuteScript(PCWSTR(wide.as_ptr()), &handler) {
            let _ = tx.send(Err(format!("ExecuteScript (вызов): {}", e)));
        }
    })
    .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn exec_js(_w: &tauri::WebviewWindow, _js: String, _tx: ExecTx) -> Result<(), String> {
    Err("только Windows".to_string())
}

async fn run_exec(app: &AppHandle, js: String) -> Result<String, String> {
    run_exec_label(app, "music", js).await
}

async fn run_exec_label(app: &AppHandle, label: &str, js: String) -> Result<String, String> {
    if js.len() > 20000 {
        return Err("скрипт слишком длинный".to_string());
    }
    let Some(w) = app.get_webview_window(label) else { return Err("окно не создано".to_string()) };
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    exec_js(&w, js, tx)?;
    match tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(6))).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err("нет ответа от страницы (таймаут 6 с)".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub async fn music_exec(app: AppHandle, js: String) -> Result<String, String> {
    run_exec(&app, js).await
}

/* Диагностика окна музыки: есть ли окно, виден ли, отвечает ли страница на ExecuteScript, что она знает о треке */
#[tauri::command]
pub async fn music_diag(app: AppHandle) -> String {
    let Some(w) = app.get_webview_window("music") else {
        let e = MUSIC_ERR.lock().map(|g| g.clone()).unwrap_or_default();
        return format!("окно музыки не создано; ошибка создания: {}", if e.is_empty() { "нет (окно ещё не запрашивалось)" } else { e.as_str() });
    };
    let mut out = format!("visible={:?}; ", w.is_visible().ok());
    if let Ok(p) = w.outer_position() {
        out.push_str(&format!("pos={},{}; ", p.x, p.y));
    }
    if let Ok(sz) = w.inner_size() {
        out.push_str(&format!("size={}x{}; ", sz.width, sz.height));
    }
    let js = "JSON.stringify({href:location.href,ready:document.readyState,hidden:document.hidden,media:document.querySelectorAll('audio,video').length,ms:!!(navigator.mediaSession&&navigator.mediaSession.metadata),title:(navigator.mediaSession&&navigator.mediaSession.metadata&&navigator.mediaSession.metadata.title)||''})".to_string();
    match run_exec(&app, js).await {
        Ok(r) => out.push_str(&format!("exec=OK {}", r)),
        Err(e) => out.push_str(&format!("exec=ОШИБКА {}", e)),
    }
    out
}

#[tauri::command]
pub fn music_close(app: AppHandle) -> bool {
    match app.get_webview_window("music") {
        Some(w) => w.close().is_ok(),
        None => false,
    }
}


/* ---------- Невидимое окно-браузер (поиск текста песни и продюсера) ----------
   Запросы из PowerShell/API часто не проходят (блокировки, защита от ботов), а в настоящем браузере страница открывается.
   Поэтому открываем страницу в скрытом окне WebView2 (за пределами экрана), ждём, пока скрипт js вернёт непустой результат, и закрываем окно.
   Разрешены только https-адреса. Окно каждый раз новое и закрывается сразу — память не занимает. */
static SCRAPE_BUSY: AtomicBool = AtomicBool::new(false);
static SCRAPE_N: AtomicU32 = AtomicU32::new(0);

async fn scrape_inner(app: &AppHandle, url: &str, js: &str, wait_ms: u64) -> Result<String, String> {
    let main = app.get_webview_window("main").ok_or_else(|| "нет главного окна".to_string())?;
    let u: Url = url.parse::<Url>().map_err(|_| "плохой адрес".to_string())?;
    let label = format!("scrape{}", SCRAPE_N.fetch_add(1, Ordering::SeqCst));
    let b = WebviewWindowBuilder::new(app, label.clone(), WebviewUrl::External(u))
        .title("Nexora scrape")
        .decorations(false)
        .resizable(false)
        .skip_taskbar(true)
        .shadow(false)
        .focused(false)
        .visible(false)
        .inner_size(1000.0, 800.0);
    let b = b.parent(&main).map_err(|e| e.to_string())?;
    let win = b.build().map_err(|e| e.to_string())?;
    let _ = win.set_position(PhysicalPosition::new(OFF, OFF));
    let _ = win.show();
    let t0 = Instant::now();
    let mut last_err = String::from("страница не дала результат (не загрузилась или нет данных)");
    let mut result: Option<String> = None;
    while t0.elapsed() < Duration::from_millis(wait_ms) {
        let _ = tauri::async_runtime::spawn_blocking(|| std::thread::sleep(Duration::from_millis(700))).await;
        match run_exec_label(app, &label, js.to_string()).await {
            Ok(r) => {
                let t = r.trim();
                if !t.is_empty() && t != "\"\"" && t != "null" {
                    result = Some(r);
                    break;
                }
            }
            Err(e) => {
                last_err = e;
            }
        }
    }
    let _ = win.close();
    match result {
        Some(r) => Ok(r),
        None => Err(last_err),
    }
}

#[tauri::command]
pub async fn scrape_page(app: AppHandle, url: String, js: String, ms: Option<u64>) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err("адрес не разрешён".to_string());
    }
    if js.len() > 20000 {
        return Err("скрипт слишком длинный".to_string());
    }
    if SCRAPE_BUSY.swap(true, Ordering::SeqCst) {
        return Err("скрытый браузер занят".to_string());
    }
    /* флаг снимается в Drop: даже если команду отменят на середине, «занято» не залипнет */
    struct BusyGuard;
    impl Drop for BusyGuard {
        fn drop(&mut self) {
            SCRAPE_BUSY.store(false, Ordering::SeqCst);
        }
    }
    let _busy = BusyGuard;
    scrape_inner(&app, &url, &js, ms.unwrap_or(20000).min(45000)).await
}

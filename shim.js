/* Прослойка: делает window.islandAPI (как в Electron preload.js) поверх Tauri invoke/listen.
   Этап 5: возврат функций оригинала (проба страницы музыки, SoundCloud-виджет, спектр). Этап 3: VPN (подписка, пинг, Xray), Wi-Fi, OCR, скриншоты, закреплённые приложения, картинки в буфере,
   команды, свой звук, обложки из интернета, Genius. */
(function () {
  const T = window.__TAURI__;
  if (!T) return;
  /* ошибки команд по-прежнему дают null (интерфейс на это рассчитан), но теперь причина не теряется:
     пишем в консоль и запоминаем последние в window.__islErrors (видно в «Диагностике музыки» и в команде «Ошибки команд»). */
  const inv = (c, a) => T.core.invoke(c, a || {}).catch(e => {
    try {
      const list = (window.__islErrors = window.__islErrors || []);
      list.push(new Date().toISOString().slice(11, 19) + ' ' + c + ': ' + String((e && e.message) || e));
      if (list.length > 30) list.shift();
      console.warn('[Nexora] команда ' + c + ' не выполнена:', e);
    } catch (_) {}
    return null;
  });
  const on = (ev, fn) => {
    let un = null, dead = false;
    T.event.listen(ev, e => fn(e.payload)).then(u => { if (dead) u(); else un = u; });
    return () => { dead = true; if (un) un(); };
  };
  const later = v => () => Promise.resolve(v);

  const CHROME_UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36';

  /* ---------- вспомогательное: PowerShell-скрипты, cmd, HTTP ---------- */
  const ps = (name, env, o) => inv('ps_run', Object.assign({ name, env: env || {} }, o || {}));
  const runCmd = (cmd, ms) => inv('run_cmd', { cmd: String(cmd), timeout_ms: ms || 20000 });

  /* HTTP через PowerShell (без CORS): { status, headers{lowercase}, body, err } или null */
  async function http(url, o) {
    o = o || {};
    const ms = o.ms || 9000;
    const env = { ISL_URL: String(url), ISL_UA: o.ua || CHROME_UA, ISL_MODE: o.mode || 'text', ISL_TIMEOUT: String(Math.ceil(ms / 1000)) };
    if (o.headers) env.ISL_HEADERS = JSON.stringify(o.headers);
    const r = await ps('http', env, { timeout_ms: ms + 8000 });
    if (!r || !String(r.out || '').trim()) return null;
    try { return JSON.parse(r.out); } catch (_) { return null; }
  }
  const okStatus = r => r && r.status >= 200 && r.status < 300;
  async function httpJson(url, ms, headers) {
    const r = await http(url, { ms, headers });
    if (!okStatus(r)) return null;
    try { return JSON.parse(r.body); } catch (_) { return null; }
  }
  async function toDataUrl(url) {
    const r = await http(url, { ms: 10000, mode: 'b64' });
    if (!okStatus(r) || !r.body) return null;
    const bytes = r.body.length * 3 / 4;
    if (bytes < 500 || bytes > 6000000) return null;
    const ct = String((r.headers && r.headers['content-type']) || 'image/jpeg').split(';')[0];
    return 'data:' + (/^image\//.test(ct) ? ct : 'image/jpeg') + ';base64,' + r.body;
  }

  /* ---------- обложка трека из интернета ---------- */
  const coverWeb = new Map();
  const flat = s => String(s || '').toLowerCase().replace(/ё/g, 'е').replace(/[^a-z0-9а-я]+/g, '');
  const cleanTitle = s => String(s || '').replace(/\s*[\(\[][^\)\]]*[\)\]]/g, '').replace(/\s+-\s+.*$/, '').trim();
  const artistList = s => String(s || '').split(/\s*(?:,|&|;|\/|\bfeat\.?\b|\bft\.?\b|\bx\b|\bи\b|\bat\b)\s*/i).map(x => x.trim()).filter(Boolean);
  function score(c, title, artists, dur) {
    const t1 = flat(cleanTitle(title)), t2 = flat(cleanTitle(c.title));
    if (!t1 || !t2) return 0;
    let sc = 0;
    if (t1 === t2) sc += 5; else if (t2.includes(t1) || t1.includes(t2)) sc += 2; else return 0;
    const ca = flat(c.artist);
    if (artists.some(a => { const f = flat(a); return f && (ca.includes(f) || f.includes(ca)); })) sc += 4;
    else if (artists.length) return 0;
    if (dur > 0 && c.dur > 0) sc += Math.abs(c.dur - dur) <= 4 ? 2 : (Math.abs(c.dur - dur) > 20 ? -2 : 0);
    return sc;
  }
  function best(cands, title, artists, dur) {
    let b = null, bs = 0;
    for (const c of cands) { const s = score(c, title, artists, dur); if (s > bs) { bs = s; b = c; } }
    return b;
  }
  async function findCoverUrl(artist, title, dur) {
    const artists = artistList(artist);
    const q = [artists[0] || '', cleanTitle(title)].filter(Boolean).join(' ');
    const qFull = [artist || '', cleanTitle(title)].filter(Boolean).join(' ');
    try {
      const j = await httpJson('https://api.music.yandex.net/search?type=track&page=0&nocorrect=false&text=' + encodeURIComponent(qFull), 8000,
        { 'X-Yandex-Music-Client': 'YandexMusicAndroid/24023231', 'Accept': 'application/json' });
      const arr = (j && j.result && j.result.tracks && j.result.tracks.results) || [];
      const b = best(arr.map(t => ({ title: t.title, artist: (t.artists || []).map(a => a.name).join(', '), dur: (t.durationMs || 0) / 1000, uri: t.coverUri || (t.albums && t.albums[0] && t.albums[0].coverUri) })).filter(c => c.uri), title, artists, dur);
      if (b) return 'https://' + String(b.uri).replace(/^https?:\/\//, '').replace('%%', '600x600');
    } catch (_) {}
    try {
      const j = await httpJson('https://itunes.apple.com/search?media=music&entity=song&limit=15&term=' + encodeURIComponent(q), 8000);
      const b = best(((j && j.results) || []).map(t => ({ title: t.trackName, artist: t.artistName, dur: (t.trackTimeMillis || 0) / 1000, uri: t.artworkUrl100 })).filter(c => c.uri), title, artists, dur);
      if (b) return String(b.uri).replace(/\d+x\d+bb/, '600x600bb');
    } catch (_) {}
    try {
      const j = await httpJson('https://api.deezer.com/search?limit=15&q=' + encodeURIComponent(q), 8000);
      const b = best(((j && j.data) || []).map(t => ({ title: t.title, artist: t.artist && t.artist.name, dur: t.duration || 0, uri: t.album && (t.album.cover_xl || t.album.cover_big) })).filter(c => c.uri), title, artists, dur);
      if (b) return b.uri;
    } catch (_) {}
    return null;
  }
  async function findCover(q) {
    const artist = String((q && q.artist) || ''), title = String((q && q.title) || '');
    if (!title) return null;
    const key = flat(artist) + '|' + flat(title);
    const hit = coverWeb.get(key);
    if (hit && (hit.url || Date.now() - hit.at < 30000)) return hit.url || null;
    let url = null;
    try {
      const u = await findCoverUrl(artist, title, Number(q && q.dur) || 0);
      if (u) url = await toDataUrl(u);
    } catch (_) {}
    coverWeb.set(key, { url, at: Date.now() });
    return url;
  }

  /* ---------- Текст песни и продюсер: поиск в браузере (скрытое окно WebView2), без Genius ---------- */
  const normTxt = s => String(s || '').toLowerCase().replace(/\(.*?\)|\[.*?\]/g, ' ').replace(/\s[-–—]\s.*$/, ' ').replace(/[^\p{L}\p{N}]+/gu, ' ').trim();
  /* Яндекс отдаёт «Скриптонит», а на Genius артист записан латиницей — сравниваем по транслитерации и с допуском на опечатки */
  const TRL = { а:'a',б:'b',в:'v',г:'g',д:'d',е:'e',ё:'e',ж:'zh',з:'z',и:'i',й:'i',к:'k',л:'l',м:'m',н:'n',о:'o',п:'p',р:'r',с:'s',т:'t',у:'u',ф:'f',х:'h',ц:'ts',ч:'ch',ш:'sh',щ:'sh',ъ:'',ы:'i',ь:'',э:'e',ю:'yu',я:'ya' };
  const translit = s => String(s || '').toLowerCase().replace(/[а-яё]/g, c => TRL[c] || '');
  const loose = s => translit(normTxt(s)).replace(/ph/g, 'f').replace(/c(?!h)/g, 'k').replace(/y/g, 'i').replace(/[^a-z0-9]+/g, '');
  function lev(a, b) {
    const m = a.length, n = b.length; if (!m || !n) return Math.max(m, n);
    let prev = Array.from({ length: n + 1 }, (_, j) => j);
    for (let i = 1; i <= m; i++) { const cur = [i]; for (let j = 1; j <= n; j++) cur[j] = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1)); prev = cur; }
    return prev[n];
  }
  const sim = (a, b) => { if (!a || !b) return 0; if (a.includes(b) || b.includes(a)) return 1; return 1 - lev(a, b) / Math.max(a.length, b.length); };

  const JS_SERP = String.raw`(()=>{try{
const h=location.hostname;
if(/captcha|showcaptcha/i.test(location.href)||document.querySelector('form[action*="captcha"],.CheckboxCaptcha,#captcha,.g-recaptcha'))return 'CAPTCHA';
const dec=u=>{try{const x=new URL(u,location.href);
  if(/duckduckgo\.com$/.test(x.hostname)&&x.pathname.indexOf('/l/')===0){return decodeURIComponent(x.searchParams.get('uddg')||'');}
  if(/bing\.com$/.test(x.hostname)&&x.pathname.indexOf('/ck/')===0){let v=x.searchParams.get('u')||'';if(v.indexOf('a1')===0)v=v.slice(2);v=v.replace(/-/g,'+').replace(/_/g,'/');while(v.length%4)v+='=';return atob(v);}
  return x.href;}catch(e){return '';}};
let items=[];
if(/duckduckgo/.test(h))items=[...document.querySelectorAll('.result')].map(r=>[r.querySelector('a.result__a'),r.querySelector('.result__snippet')]);
else if(/bing/.test(h))items=[...document.querySelectorAll('li.b_algo')].map(r=>[r.querySelector('h2 a'),r.querySelector('.b_caption p,.b_lineclamp2,.b_lineclamp3,.b_algoSlug,p')]);
else if(/yandex/.test(h))items=[...document.querySelectorAll('li.serp-item')].map(r=>[r.querySelector('a.OrganicTitle-Link,a.Link[href^="http"]'),r.querySelector('.OrganicText,.TextContainer,.Organic-ContentWrapper')]);
const out=[];
for(const [a,sn] of items){
  if(!a)continue;const u=dec(a.getAttribute('href')||a.href||'');
  if(!/^https?:\/\//i.test(u))continue;
  out.push([u,(a.innerText||a.textContent||'').replace(/\s+/g,' ').trim().slice(0,200),((sn&&(sn.innerText||sn.textContent))||'').replace(/\s+/g,' ').trim().slice(0,400)]);
  if(out.length>=10)break;
}
return out.length?JSON.stringify(out):'';
}catch(e){return '';}})()`;
  /* универсальный разбор страницы с текстом песни: берём самый «стихотворный» блок (много коротких строк) */
  const JS_LYRICS = String.raw`(()=>{try{
if(document.readyState==='loading')return '';
if(!window.__t1)window.__t1=Date.now();
const bad=/(cookie|реклам|подписк|комментар|copyright|©|все права|войти|регистрац|скачать|слушать онлайн|поделит|читайте также|похожие|перевод|аккорды|караоке)/i;
const code=/[{}]|<\/?[a-z!]|-->|=>|\b(function|var|const|let|document|window|createElement|getElementsByTagName|undefined|async|return)\b|\w\.\w+\(|^\s*(\/\/|\/\*)|https?:\/\/|\w\s*=\s*['"\d(]/i;
const txt=e=>{try{const c=e.cloneNode(true);c.querySelectorAll('script,style,noscript,iframe,ins,template,svg,button,select').forEach(x=>x.remove());const d=document.createElement('div');d.style.cssText='position:absolute;left:-99999px;top:0;white-space:pre-wrap';d.appendChild(c);document.body.appendChild(d);const t=d.innerText||'';d.remove();return t;}catch(_){return e.innerText||'';}};
const els=[...document.querySelectorAll('div,p,pre,article,section,td,span')];
let best=null,bl=0;
for(const e of els){
  if(e.closest('nav,header,footer,aside,script,style,form'))continue;
  const t0=(e.innerText||'');
  if(t0.length<150||t0.length>9000)continue;
  const t=txt(e);
  const ls=t.split(/\n+/).map(x=>x.trim()).filter(Boolean);
  if(ls.length<8||ls.length>220)continue;
  if(ls.filter(x=>code.test(x)).length>Math.max(1,ls.length*0.03))continue;   /* куски скриптов и вёрстки - не текст песни */
  const short=ls.filter(x=>x.length<=90).length;
  if(short/ls.length<0.88)continue;
  const junk=ls.filter(x=>bad.test(x)).length;
  if(junk>ls.length*0.12)continue;
  /* предпочитаем самый вложенный подходящий блок; из равных — с большим числом строк */
  if(best&&best.contains(e)){best=e;bl=ls.length;continue;}
  if(ls.length>bl){best=e;bl=ls.length;}
}
if(!best){return (Date.now()-window.__t1>5000)?JSON.stringify({url:location.href,title:document.title,h1:'',lines:[]}):'';}
const lines=txt(best).split(/\n+/).map(x=>x.trim()).filter(x=>x&&!/^\[.*\]$/.test(x)&&!bad.test(x)&&!code.test(x));
const h1=(document.querySelector('h1')||{}).innerText||'';
return JSON.stringify({url:location.href,title:document.title,h1:h1.slice(0,200),lines:lines.slice(0,200)});
}catch(e){return '';}})()`;
  /* страховка: любой найденный «текст» проходит проверку - код/вёрстка/реклама вместо песни отбрасываются */
  const CODE_RX = /[{}]|<\/?[a-z!]|-->|=>|\b(function|var|const|let|document|window|createElement|getElementsByTagName|undefined|async|return)\b|\w\.\w+\(|^\s*(\/\/|\/\*)|https?:\/\/|\w\s*=\s*['"\d(]/i;
  function goodLyrics(lines) {
    const a = (lines || []).map(x => String(x || '').trim()).filter(Boolean);
    const ok = a.filter(x => !CODE_RX.test(x));
    if (ok.length < 4) return null;
    if (a.length - ok.length > Math.max(1, a.length * 0.05)) return null;
    const letters = ok.join('').replace(/[^\p{L}]/gu, '').length;
    if (letters < ok.join('').length * 0.55) return null;   /* в строках должны быть в основном буквы */
    return ok;
  }
  const unwrap = r => { try { let v = JSON.parse(r); if (typeof v === 'string') v = v ? JSON.parse(v) : null; return v; } catch (_) { return null; } };
  let scrapeQ = Promise.resolve();
  function scrape(url, js, ms) {
    const run = async () => {
      try { return { ok: true, v: await T.core.invoke('scrape_page', { url, js, ms: ms || 20000 }) }; }
      catch (e) { return { ok: false, err: String(e && e.message || e) }; }
    };
    const p = scrapeQ.then(run, run);
    scrapeQ = p.catch(() => {});
    return p;
  }
  /* поисковая выдача: Bing → DuckDuckGo → Яндекс (какой откроется; капчу пропускаем) */
  const serpCache = new Map();
  async function serp(q) {
    if (serpCache.has(q)) return serpCache.get(q);
    const engines = [
      'https://www.bing.com/search?q=' + encodeURIComponent(q) + '&setlang=ru',
      'https://html.duckduckgo.com/html/?q=' + encodeURIComponent(q),
      'https://yandex.ru/search/?text=' + encodeURIComponent(q)
    ];
    let lastErr = '';
    for (const u of engines) {
      const r = await scrape(u, JS_SERP, 14000);
      if (!r.ok) { lastErr = String(r.err).slice(0, 80); continue; }
      let list = null; try { list = JSON.parse(r.v); if (typeof list === 'string') list = list === 'CAPTCHA' ? null : JSON.parse(list); } catch (_) { list = null; }
      if (Array.isArray(list) && list.length) { serpCache.set(q, list); return list; }
      lastErr = 'пустая выдача или капча';
    }
    throw new Error(lastErr || 'поиск не открылся');
  }
  const BAD_HOST = /(^|\.)(genius\.com|youtube\.com|youtu\.be|vk\.com|vk\.ru|music\.yandex\.\w+|soundcloud\.com|spotify\.com|music\.apple\.com|deezer\.com|tiktok\.com|instagram\.com|facebook\.com|ok\.ru|t\.me|wikipedia\.org|zvuk\.com|mts\.ru|music\.mts\.ru|shazam\.com|amazon\.\w+|ozon\.ru|wildberries\.ru|musixmatch\.com|pinterest\.\w+|twitter\.com|x\.com|reddit\.com)$/i;
  const LYR_HOST = /(text|tekst|lyric|pesn|pesni|slova|songtext|textpesen|teksty|musictext|stihi|текст|песн|слова)/i;
  const hostOf = u => { try { return new URL(u).hostname.replace(/^www\./, ''); } catch (_) { return ''; } };

  /* текст песни: ищем в выдаче страницы с текстами, открываем в скрытом браузере и сверяем название */
  const lyCache = new Map();
  const lyInflight = new Map();
  const JS_GENIUS_LYR = String.raw`(()=>{try{
const cs=[...document.querySelectorAll('[data-lyrics-container="true"]')];
if(!cs.length){if(!window.__g1)window.__g1=Date.now();return (Date.now()-window.__g1>9000)?JSON.stringify({url:location.href,title:document.title,h1:'',lines:[],why:(document.title+' | '+((document.body&&document.body.innerText)||'').replace(/\s+/g,' ').slice(0,100))}):'';}
const lines=cs.map(c=>c.innerText).join('\n').split(/\n+/).map(x=>x.trim()).filter(x=>x&&!/^\[.*\]$/.test(x));
return JSON.stringify({url:location.href,title:document.title,h1:((document.querySelector('h1')||{}).innerText||''),lines:lines.slice(0,250)});
}catch(e){return '';}})()`;
  async function webLyricsRun(artist, title) {
    const trace = []; webDbg.lyrics = '';
    const done = r => { webDbg.lyrics = trace.join(' ; '); return r; };
    /* 0) страница песни на Genius: API отдаёт адрес, а сама страница открывается в Chromium-окне */
    try {
      const g = await geniusFindSong(artist, title);
      if (!g) trace.push('Genius: песня не найдена');
      else {
        const r = await scrape(g.url, JS_GENIUS_LYR, 20000);
        if (!r.ok) trace.push('Genius-страница: ' + String(r.err).slice(0, 70));
        else {
          const d = unwrap(r.v);
          const gl = d && d.lines ? goodLyrics(d.lines) : null;
          if (gl) return done({ ok: true, lyrics: gl, url: d.url || g.url, host: 'genius.com', via: 'Genius' });
          trace.push('Genius-страница без текста: ' + String((d && d.why) || '').slice(0, 90));
        }
      }
    } catch (e) { trace.push('Genius: ' + String(e && e.message || e).slice(0, 80)); }
    const a1 = artistList(artist)[0] || artist, t1 = cleanTitle(title) || title;
    const nt = loose(t1), toks = artistList(artist).map(loose).filter(x => x.length > 1);
    const queries = [(a1 + ' ' + t1 + ' текст песни').trim(), (a1 + ' ' + t1 + ' lyrics').trim()];
    let lastErr = '';
    for (const q of queries) {
      let list;
      try { list = await serp(q); } catch (e) { lastErr = String(e && e.message || e); trace.push('выдача «' + q.slice(0, 30) + '»: ' + lastErr.slice(0, 60)); continue; }
      const all = list.map(([u, t, sn]) => ({ u, host: hostOf(u), t, sn })).filter(c => c.host && !BAD_HOST.test(c.host));
      const cands = all
        .map(c => {
          const hay = loose(c.t + ' ' + c.sn);
          let sc = 0;
          if (nt && hay.includes(nt)) sc += 3;
          if (toks.some(x => hay.includes(x))) sc += 2;
          if (LYR_HOST.test(c.host) || /текст|lyrics|слова/i.test(c.t)) sc += 2;
          return Object.assign(c, { sc });
        })
        .filter(c => c.sc >= 3)
        .sort((x, y) => y.sc - x.sc)
        .slice(0, 4);
      trace.push('выдача «' + q.slice(0, 30) + '»: всего ' + list.length + ', подходит ' + cands.length + (cands.length ? ' (' + cands.map(c => c.host + ':' + c.sc).join(', ') + ')' : ' [' + all.slice(0, 3).map(c => c.host).join(', ') + ']'));
      for (const c of cands) {
        const r = await scrape(c.u, JS_LYRICS, 18000);
        if (!r.ok) { lastErr = String(r.err).slice(0, 80); trace.push(c.host + ': ' + lastErr.slice(0, 50)); continue; }
        const d = unwrap(r.v);
        if (!d || !d.lines || d.lines.length < 8) { trace.push(c.host + ': текст не распознан'); continue; }
        const head = loose((d.title || '') + ' ' + (d.h1 || ''));
        if (nt && !head.includes(nt)) { trace.push(c.host + ': страница не про этот трек'); continue; }
        const cl = goodLyrics(d.lines);
        if (!cl) { trace.push(c.host + ': это не текст песни (код/вёрстка)'); continue; }
        return done({ ok: true, lyrics: cl, url: d.url || c.u, host: c.host, via: 'браузер' });
      }
    }
    return done(lastErr && !/пуст|капч/.test(lastErr) ? { ok: false, err: lastErr } : { ok: false, notFound: true });
  }
  function webLyrics(q) {
    const artist = String(q && q.artist || ''), title = String(q && q.title || '');
    const key = artist + '|' + title;
    if (q && q.force) lyCache.delete(key);
    if (lyCache.has(key)) return Promise.resolve(lyCache.get(key));
    if (lyInflight.has(key)) return lyInflight.get(key);
    const p = (async () => {
      let out;
      try { out = await webLyricsRun(artist, title); } catch (e) { out = { ok: false, err: String(e && e.message || e) }; }
      if (out.ok) lyCache.set(key, out);
      return out;
    })();
    lyInflight.set(key, p);
    p.then(() => lyInflight.delete(key), () => lyInflight.delete(key));
    return p;
  }

  /* продюсер: фразы «prod. …», «продюсер: …», «beat by …» в заголовках и описаниях выдачи */
  const webDbg = { genius: '', serp: '', lyrics: '' };
  const prodCache = new Map();
  const prodInflight = new Map();
  const PROD_RX = /(?:prod\.?\s*by|produced\s+by|prod\.|prod\b|продюсер(?:ы)?|продакшн|бит\s*(?:от|by)|beat\s*(?:by|от)|музыка\s*[:\-–—])\s*[:\-–—]?\s*([^.|\n\r;()\[\]«»"“”]{2,70})/gi;
  function cleanProdName(x) {
    return String(x || '')
      .replace(/\b(?:текст|слова|lyrics|mixed|mix|mastered|master|official|video|аудио|audio|клип|скачать|слушать|feat|ft)\b.*$/i, '')
      .replace(/\s+/g, ' ').replace(/^[\s,:\-–—]+|[\s,:\-–—]+$/g, '').trim();
  }
  /* Как в оригинале на Electron: JSON-API Genius (search/multi → songs/ID). Там запрос шёл через сетевой стек Chromium (net.fetch) и проходил;
     здесь тот же стек Chromium даёт скрытое окно WebView2 — открываем адрес API как обычную страницу и читаем JSON из неё. */
  const JS_JSON = String.raw`(()=>{try{
const pre=document.querySelector('pre');
let t=((pre&&pre.textContent)||(document.body&&document.body.textContent)||'');
const a=t.indexOf('{'),b=t.lastIndexOf('}');
if(a<0||b<=a)return '';
t=t.slice(a,b+1);
JSON.parse(t);
return t;
}catch(e){return '';}})()`;
  async function geniusJson(url) {
    const r = await scrape(url, JS_JSON, 15000);
    if (!r.ok) throw new Error(String(r.err).slice(0, 80));
    const d = unwrap(r.v);
    if (!d || typeof d !== 'object') throw new Error('Genius не вернул JSON');
    return d;
  }
  async function geniusFindSong(artist, title) {
    const a1 = artistList(artist)[0] || artist, t1 = cleanTitle(title) || title;
    const query = (loose(a1) + ' ' + loose(t1)).trim();
    const sr = await geniusJson('https://genius.com/api/search/multi?per_page=5&q=' + encodeURIComponent(query));
    const hits = [];
    for (const sec of (sr.response && sr.response.sections) || []) for (const h of sec.hits || []) if (h.type === 'song' && h.result) hits.push(h.result);
    const nt = loose(t1), na = loose(artist);
    const toks = na.split(' ').filter(x => x.length > 1);
    let best = null, bs = 0;
    for (const r of hits) {
      const rt = loose(r.title);
      const ra = loose((r.primary_artist && r.primary_artist.name) || '') + ' ' + loose(r.artist_names || '');
      let sc = 0;
      if (rt === nt) sc += 3; else if (rt && nt && (rt.includes(nt) || nt.includes(rt))) sc += 2;
      if (toks.some(x => ra.includes(x))) sc += 2;
      if (sc > bs) { bs = sc; best = r; }
    }
    return (!best || bs < 4) ? null : best;
  }
  async function geniusApiProducers(artist, title) {
    const best = await geniusFindSong(artist, title);
    if (!best) return { ok: false, notFound: true };
    const d = await geniusJson('https://genius.com/api/songs/' + best.id);
    const song = (d.response && d.response.song) || {};
    const names = a => (a || []).map(x => x.name).filter(Boolean);
    const producers = names(song.producer_artists);
    if (!producers.length) return { ok: false, notFound: true };
    return { ok: true, url: best.url, host: 'genius.com', producers, writers: names(song.writer_artists), via: 'Genius' };
  }
  async function webProducersRun(artist, title) {
    let gerr = '';
    try {
      const g = await geniusApiProducers(artist, title);
      if (g.ok) return g;
      gerr = g.notFound ? 'Genius: трек/продюсер не найден' : '';
    } catch (e) { gerr = 'Genius: ' + String(e && e.message || e).slice(0, 100); }
    webDbg.genius = gerr;
    const a1 = artistList(artist)[0] || artist, t1 = cleanTitle(title) || title;
    const nt = loose(t1), toks = artistList(artist).map(loose).filter(x => x.length > 1);
    const queries = [(a1 + ' ' + t1 + ' продюсер бит prod by').trim(), (a1 + ' ' + t1 + ' producer').trim()];
    const votes = new Map(); let url = '', host = '', lastErr = '', any = false;
    for (const q of queries) {
      let list;
      try { list = await serp(q); } catch (e) { lastErr = String(e && e.message || e); continue; }
      any = true;
      for (const [u, t, sn] of list) {
        const txt = t + ' . ' + sn, hay = loose(txt);
        if (nt && !hay.includes(nt)) continue;                         /* про этот трек */
        if (toks.length && !toks.some(x => hay.includes(x))) continue; /* и про этого артиста */
        PROD_RX.lastIndex = 0; let m;
        while ((m = PROD_RX.exec(txt))) {
          const raw = cleanProdName(m[1]);
          if (!raw || raw.length < 2 || loose(raw) === nt) continue;
          raw.split(/\s*(?:,|&|\/|\bx\b|\bи\b|\band\b|\+)\s*/i).map(cleanProdName).filter(n => n && n.length >= 2 && n.length <= 30 && !/^(?:https?|www)/i.test(n)).forEach(n => {
            const k = loose(n); if (!k) return;
            const v = votes.get(k) || { name: n, c: 0 }; v.c++; votes.set(k, v);
            if (!url) { url = u; host = hostOf(u); }
          });
        }
      }
      if (votes.size) break;
    }
    if (votes.size) {
      const top = [...votes.values()].sort((a, b) => b.c - a.c).slice(0, 3).map(v => v.name);
      return { ok: true, producers: top, url, host, via: 'браузер' };
    }
    if (!any) return { ok: false, err: (webDbg.genius ? webDbg.genius + ' | ' : '') + (lastErr || 'поиск не открылся') };
    return { ok: false, notFound: true, dbg: webDbg.genius };
  }
  function webProducers(q) {
    const artist = String(q && q.artist || ''), title = String(q && q.title || '');
    const key = artist + '|' + title;
    if (prodCache.has(key)) return Promise.resolve(prodCache.get(key));
    if (prodInflight.has(key)) return prodInflight.get(key);
    const p = (async () => {
      let out;
      try { out = await webProducersRun(artist, title); } catch (e) { out = { ok: false, err: String(e && e.message || e) }; }
      if (out.ok || out.notFound) prodCache.set(key, out);
      return out;
    })();
    prodInflight.set(key, p);
    p.then(() => prodInflight.delete(key), () => prodInflight.delete(key));
    return p;
  }
  const JS_INFO = String.raw`(()=>{try{
if(location.href==='about:blank'||document.readyState!=='complete')return '';
const t=((document.body&&document.body.innerText)||'').replace(/\s+/g,' ').trim();
return JSON.stringify({href:location.href.slice(0,120),title:(document.title||'').slice(0,80),len:t.length,head:t.slice(0,160),pre:!!document.querySelector('pre')});
}catch(e){return '';}})()`;
  async function webSelfTest(q) {
    const out = [];
    const probe = async (name, url) => {
      const t0 = Date.now();
      const r = await scrape(url, JS_INFO, 16000);
      const dt = ((Date.now() - t0) / 1000).toFixed(1) + 'с';
      if (!r.ok) { out.push(name + ': ОШИБКА (' + dt + ') ' + String(r.err).slice(0, 140)); return; }
      const d = unwrap(r.v);
      out.push(name + ': открылась (' + dt + ') ' + (d ? 'url=' + d.href + ' | заголовок="' + d.title + '" | символов=' + d.len + ' | pre=' + d.pre + ' | начало: ' + d.head : String(r.v).slice(0, 160)));
    };
    await probe('Genius API', 'https://genius.com/api/search/multi?per_page=1&q=eminem');
    await probe('Bing', 'https://www.bing.com/search?q=eminem+lose+yourself&setlang=ru');
    await probe('DuckDuckGo', 'https://html.duckduckgo.com/html/?q=eminem+lose+yourself');
    await probe('Яндекс', 'https://yandex.ru/search/?text=eminem+lose+yourself');
    const artist = String(q && q.artist || ''), title = String(q && q.title || '');
    if (title) {
      out.push('— трек: ' + artist + ' — ' + title);
      try { const g = await geniusApiProducers(artist, title); out.push('Genius прод: ' + JSON.stringify(g).slice(0, 200)); }
      catch (e) { out.push('Genius прод: ОШИБКА ' + String(e && e.message || e).slice(0, 160)); }
      try { const l = await webLyricsRun(artist, title); out.push('Трасса текста: ' + (webDbg.lyrics || '—')); out.push('Текст: ' + (l.ok ? 'найден, строк=' + l.lyrics.length + ', сайт=' + l.host : JSON.stringify(l).slice(0, 200))); }
      catch (e) { out.push('Текст из поиска: ОШИБКА ' + String(e && e.message || e).slice(0, 160)); }
    } else out.push('(трек не играет — проверка по треку пропущена)');
    out.push('последние причины: ' + JSON.stringify(webDbg));
    return out.join('\n');
  }
  function webForget(q) {
    const key = String(q && q.artist || '') + '|' + String(q && q.title || '');
    prodCache.delete(key); lyCache.delete(key);
  }

  /* ---------- VPN: подписка, пинг, Xray ---------- */
  function b64dec(s) {
    s = String(s).trim().replace(/-/g, '+').replace(/_/g, '/').replace(/\s+/g, '');
    while (s.length % 4) s += '=';
    const bin = atob(s);
    return new TextDecoder().decode(Uint8Array.from(bin, c => c.charCodeAt(0)));
  }
  function parseServerLink(line) {
    line = line.trim();
    const mm = line.match(/^([a-z0-9]+):\/\//i);
    if (!mm) return null;
    const proto = mm[1].toLowerCase();
    try {
      if (proto === 'vmess') {
        const j = JSON.parse(b64dec(line.slice(8)));
        return { proto, name: j.ps || j.add, host: j.add, port: +j.port, link: line };
      }
      if (proto === 'vless' || proto === 'trojan') {
        const u = new URL(line);
        return { proto, name: decodeURIComponent(u.hash.replace(/^#/, '')) || u.hostname, host: u.hostname, port: +u.port, link: line };
      }
      if (proto === 'ss') {
        let body = line.slice(5), name = '';
        const h = body.indexOf('#');
        if (h >= 0) { name = decodeURIComponent(body.slice(h + 1)); body = body.slice(0, h); }
        body = body.split('?')[0];
        let hp;
        if (body.includes('@')) hp = body.slice(body.lastIndexOf('@') + 1);
        else hp = b64dec(body).slice(b64dec(body).lastIndexOf('@') + 1);
        const i = hp.lastIndexOf(':');
        const host = hp.slice(0, i).replace(/^\[|\]$/g, ''), port = +hp.slice(i + 1);
        return { proto, name: name || host, host, port, link: line };
      }
    } catch (_) {}
    return null;
  }
  function parseSubscription(text) {
    let t = String(text || '').trim();
    if (!t) return [];
    if (!/:\/\//.test(t.split(/\r?\n/)[0])) { try { t = b64dec(t); } catch (_) {} }
    return t.split(/\r?\n/).map(parseServerLink).filter(s => s && s.host && s.port > 0);
  }
  async function fetchSubText(url, ua) {
    const r = await http(url, { ua, ms: 15000, headers: { 'Accept': '*/*' } });
    if (!r || !r.status) throw new Error((r && r.err) || 'нет соединения');
    const h = r.headers || {};
    return { status: r.status, body: r.body || '', headers: { title: h['profile-title'] || '', userinfo: h['subscription-userinfo'] || '', announce: h['announce'] || '' } };
  }
  async function subFetch(url) {
    url = String(url || '').trim();
    if (!/^https?:\/\//i.test(url)) {
      if (/^happ:\/\//i.test(url)) return { ok: false, err: 'Ссылка happ:// зашифрована и острову недоступна. Нужна обычная https://' };
      return { ok: false, err: 'Ссылка должна начинаться с https://' };
    }
    const log = [];
    for (const ua of ['v2rayN/6.45', 'Happ/2.5.0', 'Mozilla/5.0']) {
      try {
        const r = await fetchSubText(url, ua);
        const servers = parseSubscription(r.body);
        log.push(`${ua}: HTTP ${r.status}, серверов ${servers.length}`);
        if (/^\s*happ:\/\/crypt/i.test(r.body)) return { ok: false, err: 'Провайдер отдаёт зашифрованную подписку (happ://crypt), остров её не прочитает.', log };
        if (servers.length) return { ok: true, servers, title: r.headers.title, userinfo: r.headers.userinfo, announce: r.headers.announce, ua, log };
        if (r.status >= 400) continue;
        log.push('начало ответа: ' + r.body.slice(0, 60).replace(/\s+/g, ' '));
      } catch (e) { log.push(`${ua}: ошибка ${e && e.message}`); }
    }
    return { ok: false, err: 'Не удалось получить список серверов', log };
  }

  const XRAY_HTTP = 10809, XRAY_SOCKS = 10808;
  function buildXrayConfig(link) {
    let out;
    const stream = (net, sec, o) => {
      const s = { network: net === 'h2' ? 'http' : (net || 'tcp'), security: sec || 'none' };
      if (sec === 'tls') s.tlsSettings = { serverName: o.sni || o.host || undefined, fingerprint: o.fp || 'chrome', allowInsecure: !!o.insecure, alpn: o.alpn ? String(o.alpn).split(',') : undefined };
      if (sec === 'reality') s.realitySettings = { serverName: o.sni || '', fingerprint: o.fp || 'chrome', publicKey: o.pbk || '', shortId: o.sid || '', spiderX: o.spx || '' };
      if (net === 'ws') s.wsSettings = { path: o.path || '/', headers: o.host ? { Host: o.host } : {} };
      else if (net === 'grpc') s.grpcSettings = { serviceName: o.serviceName || o.path || '', multiMode: o.mode === 'multi' };
      else if (net === 'httpupgrade') s.httpupgradeSettings = { path: o.path || '/', host: o.host || '' };
      else if (net === 'xhttp' || net === 'splithttp') { s.network = 'xhttp'; s.xhttpSettings = { path: o.path || '/', host: o.host || '', mode: o.mode || 'auto' }; }
      else if (net === 'h2') s.httpSettings = { path: o.path || '/', host: o.host ? [o.host] : [] };
      else if (net === 'tcp' && o.headerType === 'http') s.tcpSettings = { header: { type: 'http', request: { path: [o.path || '/'], headers: { Host: o.host ? [o.host] : [] } } } };
      return s;
    };
    const proto = link.slice(0, link.indexOf('://')).toLowerCase();
    if (proto === 'vless' || proto === 'trojan') {
      const u = new URL(link); const P = Object.fromEntries(u.searchParams);
      const o = { sni: P.sni, host: P.host, fp: P.fp, pbk: P.pbk, sid: P.sid, spx: P.spx, path: P.path, serviceName: P.serviceName, mode: P.mode, alpn: P.alpn, headerType: P.headerType, insecure: P.allowInsecure === '1' || P.insecure === '1' };
      const sec = P.security || (proto === 'trojan' ? 'tls' : 'none');
      const st = stream(P.type || 'tcp', sec, o);
      if (proto === 'vless') out = { protocol: 'vless', settings: { vnext: [{ address: u.hostname, port: +u.port, users: [{ id: decodeURIComponent(u.username), encryption: P.encryption || 'none', flow: P.flow || undefined }] }] }, streamSettings: st };
      else out = { protocol: 'trojan', settings: { servers: [{ address: u.hostname, port: +u.port, password: decodeURIComponent(u.username) }] }, streamSettings: st };
    } else if (proto === 'vmess') {
      const j = JSON.parse(b64dec(link.slice(8)));
      const sec = j.tls === 'tls' ? 'tls' : 'none';
      const st = stream(j.net || 'tcp', sec, { sni: j.sni, host: j.host, path: j.path, fp: j.fp, alpn: j.alpn, serviceName: j.path, headerType: j.type });
      out = { protocol: 'vmess', settings: { vnext: [{ address: j.add, port: +j.port, users: [{ id: j.id, alterId: +j.aid || 0, security: j.scy || 'auto' }] }] }, streamSettings: st };
    } else if (proto === 'ss') {
      let body = link.slice(5); const h = body.indexOf('#'); if (h >= 0) body = body.slice(0, h); body = body.split('?')[0];
      let cred, hp;
      if (body.includes('@')) { const i = body.lastIndexOf('@'); cred = body.slice(0, i); hp = body.slice(i + 1); try { if (!cred.includes(':')) cred = b64dec(cred); else cred = decodeURIComponent(cred); } catch (_) {} }
      else { const d = b64dec(body); const i = d.lastIndexOf('@'); cred = d.slice(0, i); hp = d.slice(i + 1); }
      const ci = cred.indexOf(':'), pi = hp.lastIndexOf(':');
      out = { protocol: 'shadowsocks', settings: { servers: [{ address: hp.slice(0, pi).replace(/^\[|\]$/g, ''), port: +hp.slice(pi + 1), method: cred.slice(0, ci), password: cred.slice(ci + 1) }] } };
    } else throw new Error('Неизвестный тип сервера: ' + proto);
    out.tag = 'proxy';
    return {
      log: { loglevel: 'warning' },
      inbounds: [
        { tag: 'http-in', listen: '127.0.0.1', port: XRAY_HTTP, protocol: 'http', settings: {} },
        { tag: 'socks-in', listen: '127.0.0.1', port: XRAY_SOCKS, protocol: 'socks', settings: { udp: true } }
      ],
      outbounds: [out, { tag: 'direct', protocol: 'freedom' }, { tag: 'block', protocol: 'blackhole' }],
      routing: { domainStrategy: 'IPIfNonMatch', rules: [{ type: 'field', ip: ['geoip:private'], outboundTag: 'direct' }] }
    };
  }
  async function vpnConnect(link) {
    const t = await runCmd('tasklist /FO CSV /NH', 8000);
    if (t && /"happ[^"]*\.exe"/i.test(String(t.out || ''))) return { ok: false, err: 'Закрой Happ: вместе с ним остров работать не будет' };
    let cfg;
    try { cfg = buildXrayConfig(String(link || '')); } catch (e) { return { ok: false, err: 'Не разобрал сервер: ' + (e && e.message) }; }
    const r = await inv('xray_start', { cfg_json: JSON.stringify(cfg) });
    return r || { ok: false, err: 'Нет ответа от приложения' };
  }

  /* ---------- Wi-Fi ---------- */
  async function wifiList() {
    const r = await runCmd('netsh wlan show networks mode=bssid', 12000);
    if (!r || !r.ok) return [];
    const networks = [];
    for (const b of String(r.out || '').split(/\r?\n\r?\n/)) {
      const ssid = (b.match(/SSID\s+\d+\s*:\s*(.+)/) || [])[1];
      if (!ssid) continue;
      const auth = (b.match(/Authentication\s*:\s*(.+)/) || b.match(/Проверка подлинности\s*:\s*(.+)/) || [])[1] || '';
      const signal = (b.match(/Signal\s*:\s*(\d+)%/) || b.match(/Сигнал\s*:\s*(\d+)%/) || [])[1] || 50;
      networks.push({ ssid: ssid.trim(), auth: auth.trim(), signal: +signal, open: /open|открыт/i.test(auth) });
    }
    return networks.slice(0, 12);
  }
  /* переключатель Wi-Fi (радиомодуль через Windows.Devices.Radios): возвращает {ok,on,err} */
  async function wifiRadio(state) {
    const env = state === undefined || state === null ? { ISL_OP: 'get' } : { ISL_OP: 'set', ISL_STATE: state ? 'on' : 'off' };
    const r = await ps('wifi', env, { timeout_ms: 20000 });
    if (!r) return { ok: false, on: null, err: 'нет ответа от приложения' };
    if (r.timed_out) return { ok: false, on: null, err: 'Windows не ответила вовремя' };
    try { return JSON.parse(String(r.out || '').trim()); } catch (e) { return { ok: false, on: null, err: String(r.err || 'не разобрал ответ').slice(0, 200) }; }
  }
  /* описание трека SoundCloud (там лежит текст песни): страница трека -> window.__sc_hydration -> sound.description */
  const scDescCache = new Map();
  /* основной путь - скрытый браузер (WebView2, как настоящий Chrome): страница трека отдаёт описание в window.__sc_hydration; запасной - PowerShell (scdesc.ps1) */
  const JS_SC_DESC = String.raw`(()=>{try{
const h=window.__sc_hydration;
if(Array.isArray(h)){const s=h.find(x=>x&&x.hydratable==='sound');if(s&&s.data)return JSON.stringify({d:String(s.data.description||'')});}
if(!window.__t0)window.__t0=Date.now();
const el=document.querySelector('.truncatedAudioInfo__content,[class*="truncatedAudioInfo"],.listenInfo__description');
if(el&&(el.innerText||'').trim().length>20)return JSON.stringify({d:(el.innerText||'').trim()});
return (Date.now()-window.__t0>9000)?JSON.stringify({d:''}):'';
}catch(e){return '';}})()`;
  async function scDescription(url) {
    window.__scDescWhy = '';
    url = String(url || '').split('#')[0].split('?')[0];
    if (!/^https?:\/\/(www\.|m\.)?soundcloud\.com\/[^\/]+\/[^\/]+/i.test(url)) { window.__scDescWhy = 'адрес не трека'; return ''; }
    if (scDescCache.has(url)) return scDescCache.get(url);
    let why = '';
    for (let i = 0; i < 6; i++) {
      const r = await scrape(url, JS_SC_DESC, 20000);
      if (r.ok) {
        const d = unwrap(r.v);
        if (d && typeof d.d === 'string') { scDescCache.set(url, d.d); window.__scDescWhy = d.d ? '' : 'у трека пустое описание'; return d.d; }
        why = 'ответ не разобран'; break;
      }
      why = String(r.err || '').slice(0, 60);
      if (/занят/.test(why)) { await new Promise(res => setTimeout(res, 2500)); continue; }   /* скрытый браузер занят поиском продюсера/текста - ждём очереди */
      break;
    }
    const r2 = await ps('scdesc', { ISL_URL: url, ISL_UA: CHROME_UA }, { timeout_ms: 25000 });
    let d2 = null;
    try { d2 = JSON.parse(String(r2 && r2.out || '').trim()); } catch (_) { d2 = null; }
    if (!d2 || !d2.ok) { window.__scDescWhy = why || 'PowerShell: ' + String(d2 && d2.err || 'нет ответа').slice(0, 50); return ''; }
    scDescCache.set(url, d2.desc || '');
    window.__scDescWhy = d2.desc ? '' : (why || 'описание пустое');
    return d2.desc || '';
  }
  async function wifiConnect(ssid) {
    ssid = String(ssid || '');
    if (!ssid || /["&|<>^%\r\n]/.test(ssid)) { inv('open', { target: 'ms-settings:network-wifi' }); return false; }
    const r = await runCmd('netsh wlan connect name="' + ssid + '"', 15000);
    const ok = !!(r && r.ok);
    if (!ok) inv('open', { target: 'ms-settings:network-wifi' });
    return ok;
  }

  /* ---------- закреплённые приложения ---------- */
  const iconCache = new Map();
  async function appIcon(target) {
    const key = String(target || '');
    if (iconCache.has(key)) return iconCache.get(key);
    const r = await ps('icon', { ISL_PATH: key }, { timeout_ms: 25000 });
    const b = String((r && r.out) || '').trim();
    if (/^[A-Za-z0-9+/=]{200,}$/.test(b)) {
      const u = 'data:image/png;base64,' + b;
      iconCache.set(key, u);
      return u;
    }
    return null;
  }
  async function pickExe() {
    const r = await ps('pick', { ISL_OP: 'exe' }, { dialog: true, timeout_ms: 600000 });
    const p = String((r && r.out) || '').trim();
    return p || null;
  }
  const LAUNCH_MAP = { explorer: 'explorer.exe', notepad: 'notepad.exe', calc: 'calc.exe', terminal: 'wt.exe', browser: 'https://www.google.com' };
  async function launch(name) {
    const v = String(name || '');
    if (v.startsWith('path|')) {
      const t = v.slice(5);
      return t ? !!(await inv('launch', { target: t })) : false;
    }
    const t = LAUNCH_MAP[v];
    return t ? !!(await inv('launch', { target: t })) : false;
  }

  /* ---------- OCR, скриншоты, картинки ---------- */
  async function ocr(dataUrl) {
    const r = await ps('ocr', {}, { stdin: String(dataUrl || ''), timeout_ms: 45000 });
    if (!r) return { ok: false, err: 'Нет ответа от приложения' };
    const text = String(r.out || '').trim();
    if (text === 'ERR:no-language') return { ok: false, err: 'В Windows не установлен язык распознавания текста (Параметры → Время и язык → Язык)' };
    if (!r.ok && !text) return { ok: false, err: 'Ошибка распознавания: ' + String(r.err || '').split('\n')[0] };
    return { ok: true, text };
  }
  async function saveShot(d) {
    const r = await ps('pick', { ISL_OP: 'save' }, { stdin: String(d || ''), dialog: true, timeout_ms: 600000 });
    const p = String((r && r.out) || '').trim();
    return p || false;
  }
  async function clipWriteImage(d) {
    const r = await ps('clipimg', {}, { stdin: String(d || ''), timeout_ms: 20000 });
    inv('clip_mark_image');
    return !!(r && r.ok);
  }
  function onClipImage(fn) {
    return on('clipboard:image', p => {
      if (!p || !p.dataUrl) return;
      const img = new Image();
      img.onload = () => {
        try {
          const c = document.createElement('canvas');
          c.width = img.naturalWidth; c.height = img.naturalHeight;
          c.getContext('2d').drawImage(img, 0, 0);
          fn({ dataUrl: c.toDataURL('image/png'), w: p.w, h: p.h, at: p.at });
        } catch (_) { fn(p); }
      };
      img.onerror = () => fn(p);
      img.src = p.dataUrl;
    });
  }

  /* ---------- свой звук при открытии ---------- */
  async function soundPick() {
    const r = await ps('pick', { ISL_OP: 'sound' }, { dialog: true, timeout_ms: 600000 });
    const t = String((r && r.out) || '').trim();
    if (!t) return null;
    try { return JSON.parse(t); } catch (_) { return null; }
  }
  async function soundRead() {
    const r = await ps('pick', { ISL_OP: 'soundread' }, { timeout_ms: 20000 });
    const t = String((r && r.out) || '').trim();
    return t.startsWith('data:') ? t : null;
  }
  async function soundClear() {
    const r = await ps('pick', { ISL_OP: 'soundclear' }, { timeout_ms: 10000 });
    return !!(r && r.ok);
  }

  /* POST без CORS (для чата с AI): { status, body, err } или null */
  async function httpPost(url, headers, body, ms) {
    ms = ms || 120000;
    const env = { ISL_URL: String(url), ISL_UA: CHROME_UA, ISL_MODE: 'text', ISL_METHOD: 'POST', ISL_TIMEOUT: String(Math.ceil(ms / 1000)) };
    if (headers) env.ISL_HEADERS = JSON.stringify(headers);
    const b64 = btoa(unescape(encodeURIComponent(String(body || ''))));
    const r = await ps('http', env, { stdin: b64, timeout_ms: ms + 8000 });
    if (!r || !String(r.out || '').trim()) return null;
    try { return JSON.parse(r.out); } catch (_) { return null; }
  }

  window.islandAPI = {
    native: true,
    httpPost,
    /* обход блокировок (zapret) и Telegram-прокси */
    bpInfo: () => inv('bp_info'),
    bpRead: name => inv('bp_read', { name: String(name) }),
    bpStart: args => inv('bp_start', { args: args || [] }),
    bpStop: () => inv('bp_stop'),
    bpState: () => inv('bp_state'),
    bpUpdate: async (comp, force, check) => {
      const env = { ISL_COMP: String(comp), ISL_FORCE: force ? '1' : '0', ISL_CHECK: check ? '1' : '0' };
      const r = await ps('bpupdate', env, { timeout_ms: 360000 });
      if (!r || !String(r.out || '').trim()) return { ok: false, err: (r && (r.err || (r.timedOut ? 'таймаут' : ''))) || 'нет ответа от скрипта обновления' };
      try { return JSON.parse(String(r.out).trim().split('\n').pop()); } catch (_) { return { ok: false, err: 'не разобрал ответ обновления' }; }
    },
    probe: async (url, ms) => { const r = await http(url, { ms: ms || 6000 }); return r ? r.status : 0; },
    tgStart: () => inv('tg_start'),
    tgStop: () => inv('tg_stop'),
    tgState: () => inv('tg_state'),
    hit: inside => { inv('hit', { inside: !!inside }); },
    active: v => { inv('ui_active', { v: !!v }); },
    /* «залипший» остров: Rust присылает mouse:down на каждый клик мышью, пока флаг включён */
    sticky: v => { inv('ui_sticky', { v: !!v }); },
    onMouseDown: fn => on('mouse:down', fn),
    /* приложения и процессы с активными интернет-соединениями (без самой Nexora) */
    netApps: async () => {
      const r = await ps('netapps', { NX_MODE: 'list' }, { timeout_ms: 15000 });
      if (!r || !String(r.out || '').trim()) return null;
      try { const a = JSON.parse(String(r.out).trim().split('\n').pop()); return Array.isArray(a) ? a : null; } catch (_) { return null; }
    },
    netKill: async (exe, pids) => {
      const r = await ps('netapps', { NX_MODE: 'kill', NX_NAME: String(exe || ''), NX_PIDS: (Array.isArray(pids) ? pids : [pids]).join(',') }, { timeout_ms: 20000 });
      try { return JSON.parse(String((r && r.out) || '').trim().split('\n').pop()); }
      catch (_) { return { ok: false, killed: 0, err: String((r && (r.err || (r.timedOut ? 'таймаут' : r.out))) || 'нет ответа от скрипта').slice(0, 160) }; }
    },

    sys: () => inv('sys'),
    onSys: fn => on('sys', fn),

    media: k => inv('media_key', { key: k }),
    mediaState: () => inv('media_state'),
    mediaSeek: sec => inv('media_seek', { sec: +sec || 0 }),
    mediaCover: () => inv('media_cover'),
    findCover,
    vpnOpen: later(false),
    vpnStatus: later(null),
    subFetch,
    subPing: async list => {
      const items = (Array.isArray(list) ? list : []).slice(0, 300).map(s => ({ host: String((s && s.host) || ''), port: +(s && s.port) || 0 }));
      const r = await inv('tcp_ping', { items });
      return Array.isArray(r) ? r : items.map(() => -1);
    },
    vpnConnect,
    vpnDisconnect: async () => (await inv('xray_stop')) || { ok: true },
    vpnState: async () => (await inv('xray_state')) || { on: false, log: '' },
    hkSet: m => inv('hk_set', { map: m || {} }),
    mousePause: v => inv('mouse_pause', { v: !!v }),
    autoHide: v => inv('auto_hide_set', { v: !!v }),
    soundPick,
    soundRead,
    soundClear,
    webLyrics,
    webProducers,
    webForget,
    webSelfTest,
    loopbackStart: later(false),
    /* встроенное окно Яндекс Музыки / SoundCloud */
    musicPlace: (url, x, y, w, h, show, r, clipX, clipY, clipW, clipH) => inv('music_place', { url: url || null, x, y, w, h, show: !!show, r: +r || null, clipX, clipY, clipW, clipH }),
    musicNav: url => inv('music_nav', { url: String(url || '') }),
    httpGet: (url, ms) => http(String(url), { ms: ms || 12000 }),
    musicEval: js => inv('music_eval', { js: String(js || '') }),
    /* выполнить скрипт на странице и получить результат (JSON-строка) или null */
    musicExec: js => T.core.invoke('music_exec', { js: String(js || '') }).then(r => { window.__execErr = ''; return r; }).catch(e => { window.__execErr = String(e); return null; }),
    musicClose: () => inv('music_close'),
    musicDiag: () => inv('music_diag'),
    /* живой микшер: уровень системного звука */
    levelsActive: v => inv('levels_active', { v: !!v }),
    levelsDiag: () => inv('levels_diag'),
    onLevel: fn => on('audio:level', fn),
    /* курсор над островом (по данным Rust: окно музыки лежит поверх острова) */
    onCursor: fn => on('cursor:inside', fn),
    onMediaState: fn => on('media:state', fn),

    clipRead: () => inv('clip_read'),
    clipWrite: t => inv('clip_write', { text: String(t == null ? '' : t) }),
    clipWriteImage,
    onClip: fn => on('clipboard', fn),
    onClipImage,

    open: t => inv('open', { target: String(t) }),
    volGet: () => inv('vol_get'),
    volSet: v => inv('vol_set', { v: +v || 0 }),
    volMute: () => inv('vol_mute'),
    pickExe,
    appIcon,
    ocr,
    launch,
    power: a => inv('power', { action: String(a) }),

    wifiList,
    wifiRadio,
    scDescription,
    wifiConnect,

    windowsTheme: light => inv('theme_windows', { light: !!light }),
    minimizeAll: async () => (await inv('minimize_all')) === true,

    capture: async () => {
      try { return await T.core.invoke('shot_capture'); }
      catch (e) { throw new Error(String(e && e.message || e)); }
    },
    shotLayout: async f => (await inv('shot_layout', { full: !!f })) === true,
    saveShot,

    runCommand: async cmd => {
      const r = await runCmd(String(cmd || ''), 20000);
      if (!r) return { ok: false, out: '', err: 'Нет ответа от приложения' };
      return { ok: !!r.ok, out: String(r.out || '') + String(r.err || ''), err: r.ok ? '' : (r.timedOut ? 'Команда выполнялась слишком долго' : 'Команда завершилась с ошибкой') };
    },

    autostartGet: () => inv('autostart_get'),
    autostartSet: v => inv('autostart_set', { v: !!v }),

    quit: () => { inv('quit'); },
    hide: () => { inv('hide_window'); },

    onToggle: fn => on('island:toggle', fn),
    onPalette: fn => on('palette', fn),
    onShot: fn => on('shot', fn)
  };

  /* прямоугольник острова -> Rust: окно прозрачное, клики сквозь него везде, кроме острова */
  let lastKey = '';
  function report() {
    const el = document.querySelector('.island');
    if (!el) return;
    const r = el.getBoundingClientRect();
    const k = [r.left, r.top, r.width, r.height].map(Math.round).join(',');
    if (k === lastKey) return;
    lastKey = k;
    inv('set_rect', { x: r.left, y: r.top, w: r.width, h: r.height });
  }
  document.addEventListener('DOMContentLoaded', () => {
    const el = document.querySelector('.island');
    if (el) {
      new ResizeObserver(report).observe(el);
      el.addEventListener('transitionend', report);
    }
    report();
    setTimeout(report, 500);
  });
})();

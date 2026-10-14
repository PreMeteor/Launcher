const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open: pickPath } = window.__TAURI__.dialog;
const { openUrl } = window.__TAURI__.opener;

const $ = (id) => document.getElementById(id);
let status = null;
let updating = false;

function gb(bytes) {
  if (bytes >= 1e9) return (bytes / 1e9).toFixed(2) + ' GB';
  return (bytes / 1e6).toFixed(1) + ' MB';
}

function selectedServer() {
  return status?.servers.find((x) => x.name === $('server').value);
}

function showView(id) {
  for (const v of ['signin-form', 'signup-form', 'ready']) $(v).hidden = v !== id;
}

// --- signed out: sign in or create an account ------------------------------

function showSignIn() {
  showView('signin-form');
  $('server').disabled = false;
  $('to-signup').parentElement.hidden = !selectedServer()?.signup_url;
  if (!$('user').value && status?.settings.last_user) {
    $('user').value = status.settings.last_user;
    $('remember').checked = true;
  }
  ($('user').value ? $('password') : $('user')).focus();
}

async function busy(button, label, work) {
  const text = button.textContent;
  button.disabled = true;
  button.textContent = label;
  try {
    await work();
  } finally {
    button.disabled = false;
    button.textContent = text;
  }
}

$('signin-form').addEventListener('submit', (ev) => {
  ev.preventDefault();
  $('signin-error').textContent = '';
  busy($('signin'), 'SIGNING IN…', async () => {
    try {
      render(await invoke('sign_in', {
        server: $('server').value,
        user: $('user').value,
        password: $('password').value,
        remember: $('remember').checked,
      }));
      $('password').value = '';
      autoUpdate();
    } catch (e) {
      $('signin-error').textContent = String(e);
    }
  });
});

$('signup-form').addEventListener('submit', (ev) => {
  ev.preventDefault();
  $('signup-error').textContent = '';
  if ($('su-password').value !== $('su-repeat').value) {
    $('signup-error').textContent = 'The passwords don’t match.';
    return;
  }
  busy($('signup'), 'CREATING…', async () => {
    try {
      render(await invoke('sign_up', {
        server: $('server').value,
        user: $('su-user').value,
        password: $('su-password').value,
        email: $('su-email').value,
      }));
      for (const id of ['su-password', 'su-repeat']) $(id).value = '';
      autoUpdate();
    } catch (e) {
      $('signup-error').textContent = String(e);
    }
  });
});

$('to-signup').addEventListener('click', (ev) => {
  ev.preventDefault();
  $('signup-error').textContent = '';
  $('su-user').value = $('user').value;
  showView('signup-form');
  $('su-user').focus();
});

$('to-signin').addEventListener('click', (ev) => {
  ev.preventDefault();
  showSignIn();
});

$('server').addEventListener('change', () => {
  if (!status?.signed_in) showSignIn();
});

// --- signed in: the patcher, then play ------------------------------------

// What the big button does: 'update', 'cancel', 'play', or nothing (disabled).
let action = null;

function setAction(kind, label, enabled = true) {
  action = kind;
  $('action').textContent = label;
  $('action').disabled = !enabled;
}

function lines({ title = '', time = '', speed = '', bytes = '', bar = null, cls = '' }) {
  const set = (id, text) => { $(id).textContent = text; $(id).title = text; };
  set('status-line', title);
  $('status-line').className = 'st-title ' + cls;
  set('time-line', time);
  set('speed-line', speed);
  set('bytes-line', bytes);
  if (bar !== null) $('bar').style.width = Math.min(100, bar * 100) + '%';
}

function render(s) {
  status = s;
  const sel = $('server');
  const want = s.signed_in || sel.value || s.settings.last_server;
  sel.replaceChildren(...s.servers.map((srv) => new Option(srv.name, srv.name)));
  if (want && s.servers.some((x) => x.name === want)) sel.value = want;
  $('xivboot-row').hidden = s.platform !== 'macos';

  const inst = s.install;
  if (!s.signed_in) {
    if ($('ready').hidden === false || ($('signin-form').hidden && $('signup-form').hidden)) showSignIn();
    return;
  }
  showView('ready');
  $('server').disabled = true;
  $('account-line').textContent = s.settings.last_user ? `${s.settings.last_user} on ${s.signed_in}` : s.signed_in;

  if (s.updating || updating) return; // progress events own the panel meanwhile

  const dir = s.settings.game_dir;
  const no = (title) => (lines({ title, bar: 0 }), setAction(null, 'PLAY', false));
  if (!dir) return no('Choose the game folder in Settings.');
  if (!inst?.is_install) return no('That folder isn\u2019t a FINAL FANTASY XIV install.');
  // A fresh disc install has no ffxivgame.exe yet: the update brings it.
  if (s.update && s.update.count > 0) {
    lines({
      title: 'An update is required.',
      time: `${s.update.count} patch${s.update.count > 1 ? 'es' : ''} to install`,
      bytes: gb(s.update.bytes),
      bar: 0,
      cls: 'warn',
    });
    return setAction('update', 'UPDATE');
  }
  if (s.manifest_error) {
    if (!inst.has_game_exe) return no('Could not reach the patch server to install the game.');
    // The patch server is unreachable: say so, but still let them play what they have.
    lines({ title: 'Could not check for updates.', time: s.manifest_error, bar: 0, cls: 'warn' });
    return setAction('play', 'PLAY');
  }
  if (!inst.has_game_exe) return no('ffxivgame.exe is missing from the game folder.');
  if (s.platform === 'macos' && !s.settings.xivboot) return no('Choose the recompiled client in Settings.');
  lines({ title: 'The game is up to date.', time: 'Ready to play.', bar: 1, cls: 'ok' });
  setAction('play', 'PLAY');
}

async function refresh(fromServer) {
  try {
    render(await invoke('get_status', { refresh: fromServer }));
  } catch (e) {
    lines({ title: String(e) });
  }
}

function autoUpdate() {
  if (status?.signed_in && status.update?.count > 0 && !status.updating && !updating) startUpdate();
}

// Download speed over the last 15 seconds. A running average decays toward zero whenever bytes
// stop moving (installing, or checking a patch already on disk), which made the estimate explode.
let samples = [];
const WINDOW_MS = 15000;

function measure(now, bytes) {
  const last = samples[samples.length - 1];
  // A patch found already downloaded jumps the count in one step: start the window over.
  if (last && bytes - last[1] > 64e6) samples = [];
  samples.push([now, bytes]);
  while (samples.length > 2 && now - samples[0][0] > WINDOW_MS) samples.shift();
  const [t0, b0] = samples[0];
  return now - t0 >= 3000 ? ((bytes - b0) * 1000) / (now - t0) : 0;
}

function remaining(sec) {
  if (!Number.isFinite(sec) || sec <= 0) return 'calculating\u2026';
  if (sec < 90) return 'less than 2 minutes';
  if (sec < 3600) return `approximately ${Math.round(sec / 60)} minutes`;
  const h = Math.floor(sec / 3600);
  const m = Math.round((sec % 3600) / 60);
  return `approximately ${h} hour${h > 1 ? 's' : ''}${m ? ` ${m} min` : ''}`;
}

function onProgress(p) {
  const rate = measure(performance.now(), p.downloaded_bytes);

  if (p.downloading) {
    lines({
      title: `Downloading ${p.downloading}`,
      // Under 10 KB/s is a stall or a pause, not a speed worth projecting from.
      time: `Time remaining: ${remaining(rate > 1e4 ? (p.total_bytes - p.downloaded_bytes) / rate : NaN)}`,
      speed: rate > 0 ? `Download speed: ${(rate / 1e6).toFixed(2)} MB/s` : 'Download speed: \u2014',
      bytes: `${gb(p.downloaded_bytes)} / ${gb(p.total_bytes)}`,
      bar: p.total_bytes ? p.downloaded_bytes / p.total_bytes : 0,
    });
  } else {
    // Everything is downloaded: the bar follows the install.
    const done = p.count ? (p.applied + (p.applying ? p.apply_fraction : 0)) / p.count : 0;
    lines({
      title: p.applying ? `Installing ${p.applying}` : 'Finishing…',
      time: `Patch ${Math.min(p.applied + 1, p.count)} of ${p.count}`,
      bytes: `${Math.floor(done * 100)}%`,
      bar: done,
    });
  }
}

async function startUpdate() {
  updating = true;
  samples = [];
  $('play-error').textContent = '';
  setAction('cancel', 'CANCEL');
  lines({ title: 'Starting the update…', bar: 0 });
  try {
    await invoke('start_update');
  } catch (e) {
    updating = false;
    await refresh(false);
    $('play-error').textContent = String(e);
  }
}

$('action').addEventListener('click', async () => {
  $('play-error').textContent = '';
  if (action === 'update') return startUpdate();
  if (action === 'cancel') {
    setAction(null, 'CANCELLING…', false);
    return invoke('cancel_update');
  }
  if (action === 'play') {
    setAction(null, 'STARTING…', false);
    try {
      // The launcher hides while the game runs and comes back when it quits.
      await invoke('play');
      setAction(null, 'GAME RUNNING', false);
    } catch (e) {
      $('play-error').textContent = String(e);
      setAction('play', 'PLAY');
    }
  }
});

listen('game-exited', () => refresh(false));
listen('update-progress', (e) => { if (updating) onProgress(e.payload); });
listen('update-finished', async (e) => {
  updating = false;
  await refresh(false);
  if (!e.payload.ok) $('play-error').textContent = e.payload.error;
});

$('sign-out').addEventListener('click', async (ev) => {
  ev.preventDefault();
  if (updating) return;
  try {
    render(await invoke('sign_out'));
    showSignIn();
  } catch (e) {
    $('play-error').textContent = String(e);
  }
});

// --- news ------------------------------------------------------------------

let slide = 0;
let slideTimer = null;

function showSlide(i) {
  const slides = [...$('slides').children];
  if (!slides.length) return;
  slide = (i + slides.length) % slides.length;
  slides.forEach((el, n) => el.classList.toggle('on', n === slide));
  [...$('dots').children].forEach((el, n) => el.classList.toggle('on', n === slide));
  clearInterval(slideTimer);
  if (slides.length > 1) slideTimer = setInterval(() => showSlide(slide + 1), 7000);
}

function linkTo(el, link) {
  if (!link) return;
  el.dataset.link = link;
  el.addEventListener('click', () => openUrl(link));
}

function renderNews(news) {
  const banners = news?.banners || [];
  $('slides').replaceChildren(...banners.map((b) => {
    const el = document.createElement('div');
    el.className = 'slide';
    const img = document.createElement('img');
    img.src = b.image;
    img.alt = b.title || '';
    const cap = document.createElement('div');
    cap.className = 'caption';
    cap.textContent = b.title || '';
    el.append(img, cap);
    linkTo(el, b.link);
    return el;
  }));
  $('dots').replaceChildren(...banners.map((_, n) => {
    const d = document.createElement('button');
    d.setAttribute('aria-label', `Banner ${n + 1}`);
    d.addEventListener('click', () => showSlide(n));
    return d;
  }));
  $('carousel').hidden = !banners.length;
  $('prev').hidden = $('next').hidden = banners.length < 2;
  showSlide(0);

  const items = news?.items || [];
  $('headlines').replaceChildren(...items.map((it) => {
    const li = document.createElement('li');
    const date = document.createElement('span');
    date.className = 'date';
    date.textContent = it.date;
    const title = document.createElement('span');
    title.className = 'title';
    if (it.category) {
      const tag = document.createElement('span');
      tag.className = 'tag';
      tag.textContent = `[${it.category}]`;
      title.append(tag);
    }
    title.append(it.title);
    li.append(date, title);
    linkTo(li, it.link);
    return li;
  }));
  $('headlines').parentElement.hidden = !items.length;
  $('news-empty').hidden = !!(items.length || banners.length);
}

async function loadNews() {
  try {
    renderNews(await invoke('get_news'));
  } catch (e) {
    renderNews(null);
    $('news-empty').textContent = 'News is unavailable right now.';
  }
}

$('prev').addEventListener('click', () => showSlide(slide - 1));
$('next').addEventListener('click', () => showSlide(slide + 1));

// --- settings -------------------------------------------------------------

function serverRow(srv = {}) {
  const row = $('server-row').content.firstElementChild.cloneNode(true);
  for (const input of row.querySelectorAll('input')) input.value = srv[input.dataset.k] || '';
  row.querySelector('.remove').addEventListener('click', () => row.remove());
  return row;
}

$('open-settings').addEventListener('click', () => {
  const s = status.settings;
  $('s-game-dir').value = s.game_dir || '';
  $('s-cache-dir').value = s.patch_cache_dir || '';
  $('s-keep').checked = !!s.keep_patches;
  $('s-xivboot').value = s.xivboot || '';
  $('s-servers').replaceChildren(...s.servers.map(serverRow));
  $('settings-error').textContent = '';
  $('settings').showModal();
});

$('s-add-server').addEventListener('click', () => $('s-servers').append(serverRow()));

for (const btn of document.querySelectorAll('[data-pick]')) {
  btn.addEventListener('click', async () => {
    const path = await pickPath({ directory: !!btn.dataset.dir, multiple: false });
    if (path) $(btn.dataset.pick).value = path;
  });
}

$('s-save').addEventListener('click', async (ev) => {
  ev.preventDefault();
  const opt = (v) => (v.trim() ? v.trim() : null);
  const servers = [...$('s-servers').querySelectorAll('fieldset')].map((row) => {
    const srv = {};
    for (const input of row.querySelectorAll('input')) srv[input.dataset.k] = input.value.trim();
    if (!srv.signup_url) delete srv.signup_url;
    return srv;
  }).filter((srv) => srv.name && srv.login_url && srv.lobby_host);

  const settings = {
    ...status.settings,
    game_dir: opt($('s-game-dir').value),
    patch_cache_dir: opt($('s-cache-dir').value),
    keep_patches: $('s-keep').checked,
    xivboot: opt($('s-xivboot').value),
    servers,
  };
  try {
    render(await invoke('save_settings', { settings }));
    $('settings').close();
    autoUpdate();
    loadNews();
  } catch (e) {
    $('settings-error').textContent = String(e);
  }
});

refresh(true);
loadNews();

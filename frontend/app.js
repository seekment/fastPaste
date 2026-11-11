/* fastPaste front-end: rendering + keyboard-driven interaction.
   References macOS Paste (https://apps.apple.com/cn/mac/story/id1655204803):
   centered translucent overlay, time-grouped horizontal card rows, instant search. */
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrent } = window.__TAURI__.webviewWindow;

const $ = (s) => document.querySelector(s);
const content = $('#content');
const searchEl = $('#search');

let state = {
  cards: [],        // flat list in display order
  cursor: -1,
  filter: 'all',
  query: '',
  stackSel: [],     // paste-stack ordered selection (FR-A02)
  keepOpen: false,
  paused: false,
};

/* ---------------- rendering ---------------- */
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

// FR-Q04: highlight query terms in previews (all terms, case-insensitive).
function highlightText(text, query) {
  const terms = String(query).trim().split(/\s+/).filter(Boolean);
  if (!terms.length) return escapeHtml(text);
  const pat = terms.map(t => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|');
  let re;
  try { re = new RegExp(pat, 'gi'); } catch { return escapeHtml(text); }
  let out = '', last = 0, m;
  while ((m = re.exec(text)) !== null) {
    out += escapeHtml(text.slice(last, m.index)) + '<mark>' + escapeHtml(m[0]) + '</mark>';
    last = m.index + m[0].length;
    if (m.index === re.lastIndex) re.lastIndex++;
  }
  return out + escapeHtml(text.slice(last));
}

function typeLabel(t) {
  return { text: '文本', link: '链接', image: '图片', files: '文件', html: '富文本', rtf: '富文本' }[t] || t;
}

function cardEl(c) {
  const el = document.createElement('div');
  el.className = 'card';
  el.dataset.id = c.id;
  if (state.cursor >= 0 && state.cards[state.cursor] && state.cards[state.cursor].id === c.id) el.classList.add('selected');
  const stackIdx = state.stackSel.indexOf(c.id);
  if (stackIdx >= 0) {
    const n = document.createElement('div');
    n.className = 'stackno'; n.textContent = stackIdx + 1;
    el.appendChild(n);
  }
  if (c.thumb) {
    const img = document.createElement('img');
    img.className = 'thumb'; img.src = c.thumb;
    el.appendChild(img);
  } else {
    const pv = document.createElement('div');
    pv.className = 'preview';
    pv.innerHTML = highlightText(c.preview || typeLabel(c.contentType), state.query);
    el.appendChild(pv);
  }
  if (c.pinned) {
    const b = document.createElement('div');
    b.className = 'badge'; b.textContent = '★';
    el.appendChild(b);
  }
  const meta = document.createElement('div');
  meta.className = 'meta';
  const src = (c.sourceApp || 'unknown').replace(/\.(exe|app)$/i, '');
  if (c.contentType === 'link') {
    try {
      const host = new URL(c.preview.trim()).hostname.replace(/^www\./, '');
      meta.innerHTML = `<span>🔗</span><span style="color:#8ab4ff">${host}</span>`;
    } catch { meta.innerHTML = `<span>🔗</span><span>${src}</span>`; }
  } else if (c.contentType === 'files') {
    const names = c.preview.split('\n').filter(Boolean);
    meta.innerHTML = `<span class="fileicon">📄</span><span>${names[0] || ''}${names.length > 1 ? ' 等 ' + names.length + ' 项' : ''}</span>`;
  } else {
    meta.innerHTML = `<span>${typeLabel(c.contentType)}</span><span>·</span><span>${src}</span>`;
  }
  el.appendChild(meta);
  el.addEventListener('click', (e) => {
    if (e.metaKey || e.ctrlKey) { toggleStack(c.id); } else { selectCard(c.id); }
  });
  el.addEventListener('dblclick', () => paste(c.id, false));
  return el;
}

function groupOf(ts) {
  const d = new Date(ts), now = new Date();
  const day = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.floor((day(now) - day(d)) / 86400000);
  if (diff <= 0) return '今天';
  if (diff === 1) return '昨天';
  if (diff < 7) return '近 7 天';
  return '更早';
}

function render() {
  content.innerHTML = '';
  if (!state.cards.length) {
    content.innerHTML = '<div class="empty">暂无历史记录 — 复制任意内容后回到这里</div>';
    return;
  }
  const pinned = state.cards.filter(c => c.pinned);
  const rest = state.cards.filter(c => !c.pinned);
  const sections = [];
  if (pinned.length) sections.push(['已置顶', pinned]);
  const groups = {};
  for (const c of rest) (groups[groupOf(c.createdAt)] ||= []).push(c);
  for (const g of ['今天', '昨天', '近 7 天', '更早']) if (groups[g]) sections.push([g, groups[g]]);
  for (const [label, cards] of sections) {
    const h = document.createElement('div');
    h.className = 'group-label'; h.textContent = label;
    const row = document.createElement('div');
    row.className = 'row';
    for (const c of cards) row.appendChild(cardEl(c));
    content.appendChild(h);
    content.appendChild(row);
  }
}

/* ---------------- data ---------------- */
async function refresh() {
  let cards;
  if (state.query.trim()) {
    cards = await invoke('search', { query: state.query, filter: state.filter, app: null, limit: 300 });
  } else {
    cards = await invoke('recent', { filter: state.filter, limit: 300 });
  }
  state.cards = cards;
  if (state.cursor >= cards.length) state.cursor = cards.length - 1;
  render();
}

let debounce;
searchEl.addEventListener('input', () => {
  clearTimeout(debounce);
  debounce = setTimeout(() => { state.query = searchEl.value; state.cursor = -1; refresh(); }, 60);
});
document.querySelectorAll('#tabs .tab[data-f]').forEach(t => t.addEventListener('click', () => {
  document.querySelectorAll('#tabs .tab').forEach(x => x.classList.remove('active'));
  t.classList.add('active');
  state.filter = t.dataset.f; state.cursor = -1; refresh();
}));

function selectCard(id) {
  state.cursor = state.cards.findIndex(c => c.id === id);
  render();
  const sel = document.querySelector('.card.selected');
  if (sel) sel.scrollIntoView({ block: 'nearest', inline: 'nearest' });
}

function toggleStack(id) {
  const i = state.stackSel.indexOf(id);
  if (i >= 0) state.stackSel.splice(i, 1); else state.stackSel.push(id);
  render();
}

/* ---------------- actions ---------------- */
async function paste(id, plain) {
  try {
    await invoke('paste_item', { id, plain, keepOpen: state.keepOpen });
    if (!state.keepOpen) getCurrent().hide();
    else refresh();
  } catch (e) { toast('粘贴失败：' + e); }
}

async function pasteStack() {
  if (state.stackSel.length < 2) return toast('按住 ⌘/Ctrl 点击可多选');
  try {
    await invoke('paste_stack', { ids: state.stackSel, separator: '\n', keepOpen: state.keepOpen });
    state.stackSel = [];
    if (!state.keepOpen) getCurrent().hide(); else refresh();
  } catch (e) { toast('顺序粘贴失败：' + e); }
}

async function copyOnly(id, plain) {
  try { await invoke('copy_item', { id, plain }); toast('已复制到剪贴板'); }
  catch (e) { toast('复制失败：' + e); }
}

function toast(msg) {
  const t = $('#toast'); t.textContent = msg; t.classList.add('show');
  clearTimeout(t._h); t._h = setTimeout(() => t.classList.remove('show'), 1800);
}

/* context menu (FR-K05) */
let menuEl = null;
document.addEventListener('contextmenu', (e) => {
  const card = e.target.closest('.card');
  if (!card) return;
  e.preventDefault();
  closeMenu();
  const id = Number(card.dataset.id);
  menuEl = document.createElement('div');
  menuEl.style.cssText = `position:fixed;left:${e.clientX}px;top:${e.clientY}px;z-index:99;background:rgba(40,40,46,.95);border-radius:10px;padding:4px;font-size:12px;box-shadow:0 8px 30px rgba(0,0,0,.5)`;
  const items = [
    ['粘贴', () => paste(id, false)],
    ['粘贴为纯文本', () => paste(id, true)],
    ['仅复制到剪贴板', () => copyOnly(id, false)],
    ['复制纯文本', () => copyOnly(id, true)],
    ['置顶 / 取消置顶', async () => {
      const c = state.cards.find(x => x.id === id);
      await invoke('pin_item', { id, pinned: !c.pinned }); refresh();
    }],
    ['在浏览器中打开', async () => { // FR-A04
      const c = state.cards.find(x => x.id === id);
      if (c && c.contentType === 'link') window.open(c.preview.trim(), '_blank');
    }],
    ['删除', async () => { await invoke('delete_item', { id }); refresh(); }],
  ];
  for (const [label, fn] of items) {
    const it = document.createElement('div');
    it.textContent = label;
    it.style.cssText = 'padding:6px 14px;border-radius:6px;cursor:default';
    it.onmouseenter = () => it.style.background = 'rgba(255,255,255,.1)';
    it.onmouseleave = () => it.style.background = 'none';
    it.onclick = () => { closeMenu(); fn(); };
    menuEl.appendChild(it);
  }
  document.body.appendChild(menuEl);
});
function closeMenu() { if (menuEl) { menuEl.remove(); menuEl = null; } }
document.addEventListener('click', (e) => { if (menuEl && !menuEl.contains(e.target)) closeMenu(); });

/* ---------------- keyboard (FR-K02/K03) ---------------- */
document.addEventListener('keydown', (e) => {
  if ($('#settings').classList.contains('open')) { if (e.key === 'Escape') closeSettings(); return; }
  if (e.key === 'Escape') {
    if (state.stackSel.length) { state.stackSel = []; render(); return; }
    getCurrent().hide();
    return;
  }
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    const n = state.cards.length;
    if (!n) return;
    state.cursor = state.cursor < 0
      ? (e.key === 'ArrowDown' ? 0 : n - 1)
      : (state.cursor + (e.key === 'ArrowDown' ? 1 : -1) + n) % n;
    render();
    const sel = document.querySelector('.card.selected');
    if (sel) sel.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    return;
  }
  if (e.key === 'Enter' && state.cursor >= 0) {
    e.preventDefault();
    const c = state.cards[state.cursor];
    if (state.stackSel.length) { pasteStack(); return; }
    paste(c.id, e.shiftKey); // FR-A01: Shift+Enter = 纯文本
  }
});
// auto-focus search when panel opens (FR-K02)
listen('panel-shown', () => { searchEl.focus(); state.cursor = -1; searchEl.select(); refresh(); });
listen('clip', () => { if (!state.query) refresh(); });
listen('monitor-suspended', (b) => toast(b.payload ? '采集已暂时挂起（剪贴板繁忙）' : '采集已恢复'));

/* ---------------- settings (FR-B03) ---------------- */
async function openSettings() {
  const cfg = await invoke('get_config');
  $('#cfg-hotkey').value = cfg.hotkey;
  $('#cfg-days').value = cfg.retentionDays ?? '';
  $('#cfg-maxmb').value = cfg.maxDiskBytes ? Math.round(cfg.maxDiskBytes / 1048576) : '';
  $('#cfg-keepopen').checked = cfg.pasteKeepOpen;
  $('#cfg-pause').checked = cfg.pauseRecording;
  $('#cfg-aware').checked = cfg.pasteAware;
  $('#cfg-layout').value = cfg.panelLayout || 'overlay';
  $('#cfg-encrypt').checked = !!cfg.encrypt;
  $('#cfg-autolaunch').checked = !!cfg.autoLaunch;
  state.keepOpen = cfg.pasteKeepOpen;
  $('#logbox').style.display = 'none';
  renderExcludes(cfg.excludedApps);
  $('#settings').classList.add('open');
}
function closeSettings() { $('#settings').classList.remove('open'); }
$('#settingsbtn').addEventListener('click', openSettings);
$('#cfg-close').addEventListener('click', closeSettings);
listen('open-settings', openSettings);

function renderExcludes(list) {
  const box = $('#excludesettings');
  box.innerHTML = '';
  for (const app of list) {
    const chip = document.createElement('span');
    chip.className = 'chip';
    chip.innerHTML = `${app} <b>×</b>`;
    chip.querySelector('b').onclick = async () => { await invoke('remove_exclude', { app }); openSettings(); };
    box.appendChild(chip);
  }
}
$('#cfg-addexcl').addEventListener('click', async () => {
  const v = $('#cfg-excl').value.trim();
  if (!v) return;
  await invoke('add_exclude', { app: v });
  $('#cfg-excl').value = '';
  openSettings();
});
async function saveSettings() {
  const cfg = await invoke('get_config');
  cfg.retentionDays = $('#cfg-days').value ? Number($('#cfg-days').value) : null;
  cfg.maxDiskBytes = $('#cfg-maxmb').value ? Number($('#cfg-maxmb').value) * 1048576 : null;
  cfg.pasteKeepOpen = $('#cfg-keepopen').checked;
  cfg.pasteAware = $('#cfg-aware').checked;
  cfg.panelLayout = $('#cfg-layout').value;
  cfg.encrypt = $('#cfg-encrypt').checked;
  cfg.autoLaunch = $('#cfg-autolaunch').checked;
  await invoke('set_config', { cfg });
  await invoke('pause_recording', { pause: $('#cfg-pause').checked });
  await invoke('set_auto_launch', { enabled: cfg.autoLaunch });
  state.keepOpen = cfg.pasteKeepOpen;
  toast('设置已保存（热键修改请点右侧"应用"按钮）');
}

// FR-B03: apply hotkey change without restart
$('#cfg-savehotkey')?.addEventListener('click', async () => {
  const v = $('#cfg-hotkey').value.trim();
  if (!v) return;
  try { await invoke('update_hotkey', { hotkey: v }); toast('热键已生效：' + v); }
  catch (e) { toast('热键设置失败：' + e); }
});

// FR-B06: diagnostics viewer (logs never contain clipboard content, NFR-S02)
$('#cfg-logs')?.addEventListener('click', async () => {
  const box = $('#logbox');
  if (box.style.display === 'block') { box.style.display = 'none'; return; }
  const lines = await invoke('diagnostics');
  box.textContent = lines.slice(-100).join('\n') || '（暂无日志）';
  box.style.display = 'block';
});
// hotkey uses its own 应用 button (needs re-registration, not a config save)
['cfg-days', 'cfg-maxmb', 'cfg-keepopen', 'cfg-aware', 'cfg-pause', 'cfg-layout', 'cfg-encrypt', 'cfg-autolaunch']
  .forEach(id => $('#' + id).addEventListener('change', saveSettings));
$('#cfg-export').addEventListener('click', async () => {
  const p = await window.__TAURI__.dialog.save({ defaultPath: 'fastpaste-backup.zip' });
  if (p) { await invoke('export_archive', { path: p }); toast('已导出'); }
});
$('#cfg-import').addEventListener('click', async () => {
  const p = await window.__TAURI__.dialog.open({ filters: [{ name: 'zip', extensions: ['zip'] }] });
  if (p) { const n = await invoke('import_archive', { path: p }); toast(`已导入 ${n} 条（重复项自动跳过）`); refresh(); }
});
$('#cfg-clear').addEventListener('click', async () => {
  if (confirm('确定清空全部历史记录？该操作不可恢复。')) { await invoke('clear_all'); toast('已清空'); refresh(); }
});

/* init */
(async () => {
  const cfg = await invoke('get_config');
  state.keepOpen = cfg.pasteKeepOpen;
  searchEl.focus();
  refresh();
})();

/* PC-Monitor dashboard.
 *
 * One page per sidebar item, selected by the URL hash (#/overview, #/preview,
 * #/metrics, #/terminals, #/settings). Hash routing instead of five HTML files
 * for one practical reason: the EventSource connection must survive a page
 * change. Reconnecting on every navigation would re-send `hello` and throw away
 * the catalog/ui state we just received.
 *
 * Data comes from one place: the SSE stream of snapshots the Python bridge
 * builds from real `catalog` / `ui` / `data` frames. This file formats what it
 * is given (SPEC 4.4/4.5) and never invents a number.
 */

'use strict';

const PAGE_TITLES = {
  overview: '概览',
  preview: '面板预览',
  metrics: '指标目录',
  terminals: '终端',
  editor: '布局编辑',
  settings: '设置',
};

const SCALE_MINI = 4;            // panel pixel -> canvas pixel (integer, crisp)
const SCALE_BIG = 8;
const TRUNC_CHARS = 20;          // SPEC D-18
const HISTORY_S = 60;
const TREND_S = 5;
const FLAT_EPSILON = 0.3;
const KPI_PREFERRED = ['cpu.total', 'mem.used', 'cpu.temp', 'gpu.load'];

let atlas = null;
let latest = null;
let activePage = 'overview';
let history = {};

//: Why the key could not be kept, in words the operator can act on. Declared
//: here, with the rest of the top-level state, because boot() at the bottom of
//: this file calls pairedKey() -- a `let` declared down in the pairing section
//: would still be in its temporal dead zone at that moment, and the page died
//: with "Cannot access 'keyStoreError' before initialization" before issuing a
//: single request.
const KEY_STORE = 'pcmon.key';
let keyStoreError = '';

//: The paired key, kept in memory because navigation is where it was lost:
//: private-mode browsers keep site data for one document only.
let memoryKey = '';
let searchTerm = '';

const el = (id) => document.getElementById(id);
const visible = (page) => activePage === page;

// ------------------------------------------------------------ formatting ---

/* SPEC D-21..D-29. The exponent trick matters: `33.45 * 10` is 334.49999999
 * in binary, so the naive round yields 33.4 where the golden CSV demands 33.5. */
function fmtNum(value, dec, unit) {
  if (value === null || value === undefined) return '--';
  if (typeof value === 'boolean') return value ? 'ON' : 'OFF';
  if (typeof value === 'string') return truncText(value);
  if (typeof value !== 'number' || !isFinite(value)) return '--';

  let digits = Number.isFinite(dec) ? Math.trunc(dec) : 0;
  if (digits < 0) digits = 0;
  if (digits > 3) digits = 3;

  const negative = value < 0;
  const shifted = Number(Math.abs(value).toString() + 'e' + digits);
  if (!isFinite(shifted)) return '--';
  const rounded = Math.round(shifted);
  let text = (rounded / Math.pow(10, digits)).toFixed(digits);
  if (negative && rounded !== 0) text = '-' + text;   // SPEC D-23: never "-0"
  return text + (unit || '');                          // SPEC D-25: no space
}

function truncText(text) {
  const flat = String(text).replace(/[^\x20-\x7e]/g, '?');
  return flat.length > TRUNC_CHARS ? flat.slice(0, TRUNC_CHARS - 1) + '>' : flat;
}

function metricOf(state, id) {
  return (state.catalog.metrics || []).find((m) => m && m.id === id) || null;
}

function intervalOf(state, id) {
  const metric = metricOf(state, id);
  const ms = metric && Number(metric.interval_ms);
  return ms > 0 ? ms : 1000;
}

function valueEntry(state, id) {
  return (id && state.values[id]) || null;
}

/* SPEC D-21: three intervals without a value and the panel shows "--". */
function isStale(state, id) {
  const entry = valueEntry(state, id);
  if (!entry) return true;
  return (state.now - entry.ts) > 3 * (intervalOf(state, id) / 1000);
}

function pushHistory(state) {
  for (const metric of state.catalog.metrics || []) {
    if (!metric || !metric.id) continue;
    const entry = valueEntry(state, metric.id);
    if (!entry || typeof entry.v !== 'number') continue;
    const series = history[metric.id] || (history[metric.id] = []);
    const last = series[series.length - 1];
    if (last && last.t === entry.ts) continue;          // same sample, no dupes
    series.push({ t: entry.ts, v: entry.v });
    const cutoff = state.now - HISTORY_S - 5;
    while (series.length && series[0].t < cutoff) series.shift();
  }
}

function trendOf(state, id) {
  const series = history[id] || [];
  if (series.length < 2) return { dir: 'flat', delta: 0 };
  const newest = series[series.length - 1];
  let older = series[0];
  for (let i = series.length - 1; i >= 0; i -= 1) {
    if (newest.t - series[i].t >= TREND_S) { older = series[i]; break; }
  }
  const delta = newest.v - older.v;
  if (Math.abs(delta) < FLAT_EPSILON) return { dir: 'flat', delta: 0 };
  return { dir: delta > 0 ? 'up' : 'down', delta };
}

function trendCell(state, id) {
  const cell = document.createElement('td');
  cell.className = 'text-right tabular text-xs';
  const trend = trendOf(state, id);
  if (isStale(state, id)) {
    cell.classList.add('text-gray-400');
    cell.textContent = '—';
  } else if (trend.dir === 'up') {
    cell.classList.add('text-green-500');
    cell.textContent = '▲ ' + Math.abs(trend.delta).toFixed(1);
  } else if (trend.dir === 'down') {
    cell.classList.add('text-red-500');
    cell.textContent = '▼ ' + Math.abs(trend.delta).toFixed(1);
  } else {
    cell.classList.add('text-yellow-500');
    cell.textContent = '– 持平';
  }
  return cell;
}

// ------------------------------------------------------------------- KPI ---

function kpiMetrics(state) {
  const metrics = (state.catalog.metrics || []).filter((m) => m && m.id);
  const picked = [];
  for (const id of KPI_PREFERRED) {
    const metric = metrics.find((m) => m.id === id);
    if (metric) picked.push(metric);
  }
  for (const metric of metrics) {
    if (picked.length >= 4) break;
    if (!picked.includes(metric)) picked.push(metric);
  }
  return picked.slice(0, 4);
}

function emptyCard(title, hint) {
  const card = document.createElement('div');
  card.className = 'bg-white rounded-xl shadow-sm border border-gray-100 p-4 md:p-6';
  const heading = document.createElement('div');
  heading.className = 'font-semibold tracking-tight text-sm md:text-base';
  heading.textContent = title;
  const text = document.createElement('p');
  text.className = 'text-sm text-gray-400 mt-1';
  text.textContent = hint;
  card.append(heading, text);
  return card;
}

function renderKpis(state) {
  const host = el('kpis');
  const metrics = kpiMetrics(state);
  host.innerHTML = '';
  if (!metrics.length) {
    host.append(emptyCard('还没有 catalog', 'daemon 连上后 KPI 会出现'));
    return;
  }

  for (const metric of metrics) {
    const stale = isStale(state, metric.id);
    const entry = valueEntry(state, metric.id);
    const trend = trendOf(state, metric.id);

    const card = document.createElement('div');
    card.className = 'kpi bg-white rounded-xl shadow-sm border border-gray-100 p-4 md:p-6 cursor-default';

    const label = document.createElement('div');
    label.className = 'text-xs text-gray-400 truncate';
    label.textContent = metric.label || metric.id;
    label.title = metric.id;

    const valueRow = document.createElement('div');
    valueRow.className = 'flex items-center gap-2 mt-1';
    const value = document.createElement('span');
    value.className = 'kpi-value text-2xl md:text-3xl font-semibold tracking-tight tabular';
    value.textContent = stale ? '--' : fmtNum(entry && entry.v, 0, '');
    const unit = document.createElement('span');
    unit.className = 'text-sm text-gray-400';
    unit.textContent = metric.unit || '';
    valueRow.append(value, unit);

    const foot = document.createElement('div');
    foot.className = 'flex items-center gap-2 mt-2 text-xs';
    const delta = document.createElement('span');
    if (stale) {
      delta.className = 'text-gray-400';
      delta.textContent = '无数据';
    } else if (trend.dir === 'up') {
      delta.className = 'text-green-500 font-medium';
      delta.textContent = '▲ ' + Math.abs(trend.delta).toFixed(1);
    } else if (trend.dir === 'down') {
      delta.className = 'text-red-500 font-medium';
      delta.textContent = '▼ ' + Math.abs(trend.delta).toFixed(1);
    } else {
      delta.className = 'text-yellow-500 font-medium';
      delta.textContent = '– 持平';
    }
    const windowLabel = document.createElement('span');
    windowLabel.className = 'text-gray-400';
    windowLabel.textContent = (trend.dir === 'flat' && !stale) ? '' : '较 ' + TREND_S + ' 秒前';
    foot.append(delta, windowLabel);

    card.append(label, valueRow, foot);
    host.appendChild(card);
  }
}

function renderSkeletons() {
  const host = el('kpis');
  host.innerHTML = '';
  for (let i = 0; i < 4; i += 1) {
    const card = document.createElement('div');
    card.className = 'bg-white rounded-xl shadow-sm border border-gray-100 p-4 md:p-6';
    card.innerHTML =
      '<div class="skeleton" style="height:10px;width:40%"></div>' +
      '<div class="skeleton" style="height:26px;width:65%;margin-top:10px"></div>' +
      '<div class="skeleton" style="height:10px;width:30%;margin-top:10px"></div>';
    host.appendChild(card);
  }
}

// ----------------------------------------------------------------- chart ---

function sizeCanvas(canvas) {
  const ratio = window.devicePixelRatio || 1;
  const rect = canvas.getBoundingClientRect();
  const w = Math.max(1, Math.round(rect.width * ratio));
  const h = Math.max(1, Math.round(rect.height * ratio));
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
  return { w, h, ratio };
}

function renderChart(state) {
  const canvas = el('chart');
  const { w, h, ratio } = sizeCanvas(canvas);
  const ctx = canvas.getContext('2d');
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  const width = w / ratio;
  const height = h / ratio;

  ctx.fillStyle = '#ffffff';
  ctx.fillRect(0, 0, width, height);

  const padLeft = 34;
  const padBottom = 18;
  const plotW = Math.max(10, width - padLeft - 6);
  const plotH = Math.max(10, height - padBottom - 6);

  // y axis fixed 0..100 because both series are percentages: a self-scaling
  // chart makes a 3% wobble look like a crash.
  ctx.strokeStyle = '#f3f4f6';
  ctx.lineWidth = 1;
  ctx.fillStyle = '#9ca3af';
  ctx.font = '11px "Segoe UI", system-ui, sans-serif';
  ctx.textAlign = 'right';
  ctx.textBaseline = 'middle';
  for (let pct = 0; pct <= 100; pct += 25) {
    const y = 6 + plotH * (1 - pct / 100);
    ctx.beginPath();
    ctx.moveTo(padLeft, y);
    ctx.lineTo(padLeft + plotW, y);
    ctx.stroke();
    ctx.fillText(pct + '%', padLeft - 6, y);
  }

  const now = state.now;
  const drawSeries = (id, color) => {
    const series = (history[id] || []).filter((p) => now - p.t <= HISTORY_S);
    if (series.length < 2) return null;
    ctx.beginPath();
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    ctx.lineJoin = 'round';
    series.forEach((point, index) => {
      const x = padLeft + plotW * (1 - (now - point.t) / HISTORY_S);
      const y = 6 + plotH * (1 - Math.min(100, Math.max(0, point.v)) / 100);
      if (index === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();
    const lastPoint = series[series.length - 1];
    return {
      x: padLeft + plotW * (1 - (now - lastPoint.t) / HISTORY_S),
      y: 6 + plotH * (1 - Math.min(100, Math.max(0, lastPoint.v)) / 100),
      v: lastPoint.v,
      color,
    };
  };

  const cpu = drawSeries('cpu.total', '#6366f1');
  const mem = drawSeries('mem.used', '#10b981');
  for (const head of [cpu, mem]) {
    if (!head) continue;
    ctx.beginPath();
    ctx.arc(head.x, head.y, 3, 0, Math.PI * 2);
    ctx.fillStyle = '#ffffff';
    ctx.fill();
    ctx.strokeStyle = head.color;
    ctx.lineWidth = 2;
    ctx.stroke();
  }

  ctx.fillStyle = '#9ca3af';
  ctx.textAlign = 'left';
  ctx.textBaseline = 'alphabetic';
  ctx.fillText('-60s', padLeft, height - 4);
  ctx.textAlign = 'right';
  ctx.fillText('现在', width - 6, height - 4);

  el('chart-legend').textContent = (!cpu && !mem)
    ? '还没有数据'
    : 'CPU ' + (cpu ? cpu.v.toFixed(1) : '--') + '%  ·  内存 ' +
      (mem ? mem.v.toFixed(1) : '--') + '%';
}

// ---------------------------------------------------------- panel preview ---

function glyphFor(code) {
  const index = code - atlas.first;
  if (index >= 0 && index < atlas.glyphs.length) return atlas.glyphs[index];
  return atlas.glyphs['?'.charCodeAt(0) - atlas.first];
}

function drawText(ctx, text, x, y, size, color, align) {
  const glyphH = atlas.h;
  const unit = (size && size > 0 ? size : glyphH) / glyphH;
  const cellW = atlas.w * unit;
  const flat = truncText(text);
  const totalW = flat.length * cellW;
  let startX = x;
  if (align === 'right') startX = x - totalW;
  else if (align === 'center') startX = x - Math.floor(totalW / 2);

  ctx.fillStyle = color || '#ffffff';
  for (let i = 0; i < flat.length; i += 1) {
    const rows = glyphFor(flat.charCodeAt(i));
    const cellX = startX + i * cellW;
    for (let row = 0; row < glyphH; row += 1) {
      const bits = rows[row];
      if (!bits) continue;
      for (let col = 0; col < atlas.w; col += 1) {
        if (bits & (0x80 >> col)) ctx.fillRect(cellX + col * unit, y + row * unit, unit, unit);
      }
    }
  }
}

/* Draws the daemon's `ui` nodes into one canvas at a given integer scale and
 * reports what could not be drawn, so both call sites say the same thing. */
function drawPanel(canvas, state, scale) {
  const ctx = canvas.getContext('2d', { alpha: false });
  const w = state.display.w;
  const h = state.display.h;
  if (canvas.width !== w * scale || canvas.height !== h * scale) {
    canvas.width = w * scale;
    canvas.height = h * scale;
  }
  ctx.setTransform(scale, 0, 0, scale, 0, 0);
  ctx.imageSmoothingEnabled = false;
  ctx.fillStyle = (state.ui && state.ui.bg) || '#000000';
  ctx.fillRect(0, 0, w, h);

  const report = { nodes: 0, clipped: 0, unsupported: {} };
  if (!state.ui) return report;

  const nodes = Array.isArray(state.ui.nodes) ? state.ui.nodes : [];
  report.nodes = nodes.length;

  for (const node of nodes) {
    if (!node || typeof node.type !== 'string') continue;
    const nx = Number(node.x) || 0;
    const ny = Number(node.y) || 0;
    if (node.type === 'rect' &&
        (nx + (Number(node.w) || 0) > w || ny + (Number(node.h) || 0) > h)) report.clipped += 1;
    if ((node.type === 'text' || node.type === 'num') &&
        (nx > w - 8 || ny + 16 > h)) report.clipped += 1;

    switch (node.type) {
      case 'rect': {
        let rectW = Number(node.w) || 0;
        if (node.bind_w) {
          const entry = valueEntry(state, node.bind_w);
          const min = Number.isFinite(Number(node.min)) ? Number(node.min) : 0;
          const max = Number.isFinite(Number(node.max)) ? Number(node.max) : 100;
          let ratio = 0;
          if (entry && typeof entry.v === 'number' && max > min) {
            ratio = (entry.v - min) / (max - min);
          }
          ratio = Math.min(1, Math.max(0, isFinite(ratio) ? ratio : 0));
          rectW *= ratio;
        }
        const rectH = Number(node.h) || 0;
        if (node.fill) {
          ctx.fillStyle = node.fill;
          ctx.fillRect(nx, ny, Math.max(0, rectW), rectH);
        } else if (node.color) {
          ctx.strokeStyle = node.color;
          ctx.lineWidth = Number(node.width) || 1;
          ctx.strokeRect(nx + 0.5, ny + 0.5, Math.max(0, rectW - 1), Math.max(0, rectH - 1));
        }
        break;
      }
      case 'path': {
        const pts = Array.isArray(node.pts) ? node.pts : [];
        if (pts.length < 4) break;
        ctx.beginPath();
        ctx.moveTo(nx + Number(pts[0]), ny + Number(pts[1]));
        for (let i = 2; i + 1 < pts.length; i += 2) {
          ctx.lineTo(nx + Number(pts[i]), ny + Number(pts[i + 1]));
        }
        if (node.closed) ctx.closePath();
        if (node.fill) { ctx.fillStyle = node.fill; ctx.fill(); }
        if (node.color) {
          ctx.strokeStyle = node.color;
          ctx.lineWidth = Number(node.width) || 1;
          ctx.stroke();
        }
        break;
      }
      case 'text':
        drawText(ctx, node.text || '', nx, ny, Number(node.size) || atlas.h,
                 node.color, node.align);
        break;
      case 'num': {
        const stale = isStale(state, node.bind);
        const entry = valueEntry(state, node.bind);
        const text = stale ? '--' : fmtNum(entry && entry.v,
                                           node.dec === undefined ? 0 : node.dec,
                                           node.unit || '');
        drawText(ctx, text, nx, ny, Number(node.size) || atlas.h, node.color, node.align);
        break;
      }
      default:
        report.unsupported[node.type] = (report.unsupported[node.type] || 0) + 1;
    }
  }
  return report;
}

function noteFrom(state, report) {
  const notes = [];
  const missing = Object.keys(report.unsupported).map((t) => t + '×' + report.unsupported[t]);
  if (missing.length) notes.push('本页未实现的节点类型: ' + missing.join(', '));
  if (report.clipped) {
    notes.push(report.clipped + ' 个节点超出 ' + state.display.w + '×' +
               state.display.h + '，面板上会被裁掉');
  }
  return notes.join(' · ');
}

function renderPreviewPage(state) {
  const report = drawPanel(el('panel-big'), state, SCALE_BIG);
  el('scene').textContent = state.ui
    ? 'scene ' + state.ui.scene + ' · ver ' + state.ui.ver + ' · ' + report.nodes + ' 节点'
    : '—';
  el('preview-note').textContent = noteFrom(state, report);
  el('preview-empty').style.display = state.ui ? 'none' : 'block';
  renderNodeTable(state);
}

function renderNodeTable(state) {
  const body = el('node-body');
  const empty = el('node-empty');
  const nodes = (state.ui && Array.isArray(state.ui.nodes)) ? state.ui.nodes : [];
  el('node-count').textContent = nodes.length ? nodes.length + ' 个' : '—';

  body.innerHTML = '';
  empty.style.display = nodes.length ? 'none' : 'block';
  nodes.forEach((node, index) => {
    if (!node) return;
    const tr = document.createElement('tr');
    const cells = [
      String(index),
      String(node.type || '?'),
      (node.x === undefined ? '—' : node.x + ', ' + node.y),
      (node.w === undefined
        ? (node.size === undefined ? '—' : 'size ' + node.size)
        : node.w + '×' + node.h),
      node.bind || node.bind_w || '—',
      node.color || node.fill || '—',
    ];
    cells.forEach((text, column) => {
      const td = document.createElement('td');
      td.textContent = text;
      if (column === 1) td.className = 'font-mono text-xs';
      if (column === 4) td.className = 'font-mono text-xs text-gray-500';
      tr.appendChild(td);
    });
    body.appendChild(tr);
  });
}

// ----------------------------------------------------------------- table ---

function renderTable(state) {
  const body = el('table-body');
  const metrics = (state.catalog.metrics || []).filter((m) => m && m.id);
  el('catalog-ver').textContent = state.catalog.ver ? 'ver ' + state.catalog.ver : '—';

  const empty = el('table-empty');
  if (!metrics.length) {
    body.innerHTML = '';
    empty.style.display = 'block';
    empty.textContent = '等待 catalog…';
    return;
  }

  const term = searchTerm.trim().toLowerCase();
  const rows = metrics.filter((m) => !term ||
    String(m.id).toLowerCase().includes(term) ||
    String(m.label || '').toLowerCase().includes(term));

  body.innerHTML = '';
  empty.style.display = rows.length ? 'none' : 'block';
  if (!rows.length) empty.textContent = '没有匹配「' + searchTerm + '」的指标';

  for (const metric of rows) {
    const stale = isStale(state, metric.id);
    const entry = valueEntry(state, metric.id);
    const age = entry ? Math.max(0, state.now - entry.ts) : null;

    const tr = document.createElement('tr');

    const name = document.createElement('td');
    name.innerHTML =
      '<span class="font-medium"></span><span class="text-gray-400 text-xs font-mono"></span>';
    name.firstChild.textContent = metric.label || metric.id;
    name.lastChild.textContent = ' ' + metric.id;

    const value = document.createElement('td');
    value.className = 'text-right tabular font-medium' + (stale ? ' text-gray-400' : '');
    value.textContent = stale ? '--' : fmtNum(entry && entry.v, 0, metric.unit || '');

    const ageCell = document.createElement('td');
    ageCell.className = 'text-right tabular text-xs text-gray-400';
    ageCell.textContent = age === null ? '—' : age.toFixed(1) + 's';

    const range = document.createElement('td');
    const min = Number.isFinite(Number(metric.min)) ? Number(metric.min) : 0;
    const max = Number.isFinite(Number(metric.max)) ? Number(metric.max) : 100;
    let ratio = 0;
    if (!stale && entry && typeof entry.v === 'number' && max > min) {
      ratio = Math.min(1, Math.max(0, (entry.v - min) / (max - min)));
    }
    range.innerHTML = '<div class="track"><i></i></div>';
    range.firstChild.firstChild.style.width = (ratio * 100).toFixed(0) + '%';

    const status = document.createElement('td');
    const pill = document.createElement('span');
    pill.className = 'pill ' + (stale ? 'pill-lost' : 'pill-live');
    pill.textContent = stale ? '超时' : '正常';
    status.appendChild(pill);

    tr.append(name, value, trendCell(state, metric.id), ageCell, range, status);
    body.appendChild(tr);
  }
}

// ------------------------------------------------------------- terminals ---

function renderTerminals(state) {
  const link = state.link;
  const rows = [
    ['client_id', link.client_id],
    ['角色', 'thick（要数据，自己画界面）'],
    ['声明显示', state.display.w + '×' + state.display.h],
    ['daemon', link.host + ':' + link.port],
    ['连接时长', link.since_s === null ? '—' : Math.round(link.since_s) + 's'],
    ['最近一帧', link.age_s === null ? '—' : link.age_s + 's 前'],
    ['状态', link.lost ? 'LINK LOST' : (link.up ? '实时' : '未连接')],
  ];
  const body = el('link-body');
  body.innerHTML = '';
  for (const [key, value] of rows) {
    const tr = document.createElement('tr');
    const th = document.createElement('th');
    th.textContent = key;
    const td = document.createElement('td');
    td.className = 'tabular';
    td.textContent = String(value);
    tr.append(th, td);
    body.appendChild(tr);
  }

  const meanings = {
    frames: '收到的总帧数',
    catalog: '指标目录（含版本变更）',
    ui: '界面结构（节点列表）',
    data: '数值帧',
    ping: '心跳',
    deny: '被 daemon 拒绝（token 不符）',
    bad_lines: '无法解析的行',
    other: '其它类型（按协议静默忽略）',
  };
  const frames = el('frames-body');
  frames.innerHTML = '';
  const counts = state.counts || {};
  const keys = Object.keys(meanings).filter((k) => counts[k] !== undefined);
  if (!keys.length) {
    const tr = document.createElement('tr');
    const td = document.createElement('td');
    td.colSpan = 3;
    td.className = 'text-gray-400';
    td.textContent = '还没有收到任何帧';
    tr.appendChild(td);
    frames.appendChild(tr);
    return;
  }
  for (const key of keys) {
    const tr = document.createElement('tr');
    const name = document.createElement('td');
    name.className = 'font-mono text-xs';
    name.textContent = key;
    const value = document.createElement('td');
    value.className = 'text-right tabular font-medium';
    value.textContent = String(counts[key]);
    const note = document.createElement('td');
    note.className = 'text-xs text-gray-400';
    note.textContent = meanings[key] || '';
    tr.append(name, value, note);
    frames.appendChild(tr);
  }
}

// -------------------------------------------------------------- settings ---

function renderSettings(state) {
  const rows = [
    ['显示的尺寸', state.display.w + '×' + state.display.h + ' px', 'webui 的 --size'],
    ['daemon 地址', state.link.host + ':' + state.link.port, 'webui 的 --daemon'],
    ['页面端口', location.port || '80', 'webui 的 --port'],
    ['客户端 id', state.link.client_id, 'webui 的 --client-id'],
    ['token', '来自 ./daemon.token（不显示内容）', 'daemon 每次启动重写'],
    ['推送间隔', '500 ms（SSE）', 'webui 的 PUSH_INTERVAL_S'],
    ['字模', '8×16 点阵，95 个可打印 ASCII', 'pcmon/font8x16.py'],
    ['数据来源', 'catalog / ui / data 帧', 'PROTOCOL.md v0.4'],
  ];
  const body = el('config-body');
  body.innerHTML = '';
  for (const [key, value, source] of rows) {
    const tr = document.createElement('tr');
    const th = document.createElement('td');
    th.className = 'font-medium';
    th.textContent = key;
    const td = document.createElement('td');
    td.className = 'tabular';
    td.textContent = value;
    const from = document.createElement('td');
    from.className = 'text-xs text-gray-400 font-mono';
    from.textContent = source;
    tr.append(th, td, from);
    body.appendChild(tr);
  }
}

// ---------------------------------------------------------------- chrome ---

function renderChrome(state) {
  const link = state.link;
  const badge = el('badge');
  if (link.lost) {
    badge.className = 'pill pill-lost';
    badge.textContent = 'LINK LOST · ' + (link.age_s || 0) + 's 无数据';
  } else if (link.up) {
    badge.className = 'pill pill-live';
    badge.textContent = '实时';
  } else if (link.socket) {
    badge.className = 'pill pill-wait';
    badge.textContent = '连接中…';
  } else {
    badge.className = 'pill pill-down';
    badge.textContent = '未连接';
  }

  el('meta').textContent =
    link.host + ':' + link.port +
    (link.since_s ? ' · 已连接 ' + Math.round(link.since_s) + 's' : '') +
    ' · ' + state.display.w + '×' + state.display.h;

  const counts = state.counts || {};
  el('counts').textContent =
    'frames ' + (counts.frames || 0) + ' · ui ' + (counts.ui || 0) +
    ' · data ' + (counts.data || 0);

  el('side-daemon').textContent = link.host + ':' + link.port;
  el('warn').textContent = link.err ? 'daemon: ' + link.err : '';

  const staleCount = (state.catalog.metrics || [])
    .filter((m) => m && m.id && isStale(state, m.id)).length;
  el('bell-count').textContent = String(staleCount);
  el('bell').title = staleCount ? staleCount + ' 个指标数据超时' : '没有异常';
}

// ----------------------------------------------------------------- router ---

function routeFromHash() {
  const match = location.hash.match(/^#\/([a-z]+)/);
  return (match && PAGE_TITLES[match[1]]) ? match[1] : 'overview';
}

/* Switching pages is a class toggle, not a reload: the SSE connection stays
 * open, so the catalog and the ui we already received are still there. */
function applyRoute() {
  activePage = routeFromHash();
  for (const section of document.querySelectorAll('.page')) {
    section.classList.toggle('active', section.dataset.page === activePage);
  }
  for (const link of document.querySelectorAll('.nav-item')) {
    if (link.getAttribute('href') === '#/' + activePage) {
      link.setAttribute('aria-current', 'page');
    } else {
      link.removeAttribute('aria-current');
    }
  }
  el('page-title').textContent = PAGE_TITLES[activePage];
  el('sidebar').classList.remove('open');
  el('scrim').classList.remove('open');
  document.querySelector('main').scrollTop = 0;
  if (activePage === 'editor') {
    // Load once per page visit rather than once per session: the applied
    // layouts can change on disk while the dashboard is open (a daemon start, a
    // second browser tab), and a stale 「已应用 ver」 would then be a lie.
    editorWireOnce();
    if (!editor.loaded || !editorIsDirty()) {
      editorLoad();
    } else {
      renderEditor();
    }
  }
  if (latest) render(latest);   // a hidden canvas has no size, so redraw on show
}

// ------------------------------------------------------------------- boot ---

function render(state) {
  latest = state;
  // The editor module reads the same snapshot for its preview and its bind
  // hints. Exposed on window because editor.js loads as a separate script and
  // the two must not keep two copies of the readings.
  window.__state = state;
  if (!atlas) return;
  pushHistory(state);
  renderChrome(state);
  if (visible('overview')) {
    renderKpis(state);
    renderChart(state);
    const report = drawPanel(el('panel-mini'), state, SCALE_MINI);
    el('mini-scene').textContent = state.ui ? report.nodes + ' 节点' : '—';
  }
  if (visible('preview')) renderPreviewPage(state);
  if (visible('metrics')) renderTable(state);
  if (visible('terminals')) renderTerminals(state);
  if (visible('editor') && editor.loaded) {
    // Re-draw only: the draft must survive every incoming frame, so nothing here
    // may touch editor.draft.
    drawEditorPreview();
    updateBindHints(state);
  }
  if (visible('settings')) renderSettings(state);
}

async function loadFont() {
  const resp = await fetch('/api/font', { headers: authHeaders() });
  if (resp.status === 401) {
    showPairing('这台设备还没有配对。请从运行仪表盘的电脑上读取 6 位配对码。');
    throw new Error('unpaired');
  }
  const data = await resp.json();
  atlas = {
    w: data.w, h: data.h, first: data.first,
    glyphs: data.glyphs.map((rows) => Uint8Array.from(rows)),
  };
}

function connect() {
  // EventSource cannot send headers; the server accepts the key on this one
  // route only (see _who in webui.py).
  const key = pairedKey();
  const source = new EventSource('/events' + (key ? '?key=' + encodeURIComponent(key) : ''));
  source.onmessage = (event) => {
    try {
      render(JSON.parse(event.data));
    } catch (err) {
      console.error('bad snapshot', err);
    }
  };
  source.onerror = () => {
    const badge = el('badge');
    badge.className = 'pill pill-down';
    badge.textContent = 'HTTP 断开';
  };
}

function wireUi() {
  el('search').addEventListener('input', (event) => {
    searchTerm = event.target.value || '';
    if (latest) renderTable(latest);
  });

  const sidebar = el('sidebar');
  const scrim = el('scrim');
  el('menu').addEventListener('click', () => {
    sidebar.classList.toggle('open');
    scrim.classList.toggle('open');
  });
  scrim.addEventListener('click', () => {
    sidebar.classList.remove('open');
    scrim.classList.remove('open');
  });

  el('bell').addEventListener('click', () => {
    location.hash = '#/metrics';
  });

  window.addEventListener('hashchange', applyRoute);
  window.addEventListener('resize', () => {
    if (latest) render(latest);
  });
}

async function boot() {
  renderSkeletons();
  wireUi();
  await loadFont();
  applyRoute();
  connect();
}

boot().catch((err) => {
  // This catch is why the white page stayed unexplained: a throw inside boot()
  // becomes a rejected promise here, so no 'error' event fires and nothing
  // reached the server. Report it before deciding what to paint.
  if (window.pcmonReport) {
    window.pcmonReport({
      kind: 'boot',
      msg: String((err && err.message) || err),
      stack: String((err && err.stack) || '').slice(0, 600),
    });
  }
  // "unpaired" already put the pairing overlay on screen. Painting 启动失败 on top
  // of it reads as a second, unrelated fault -- which is exactly how a pairing
  // problem got mistaken for a broken page.
  if (String((err && err.message) || err) === 'unpaired') return;
  const badge = el('badge');
  badge.className = 'pill pill-down';
  badge.textContent = '启动失败';
  el('warn').textContent = String(err);
});

// ---- pairing (PROTOCOL.md s.2.2) -----------------------------------------
// A browser cannot hold a pre-shared key, so it earns one: the operator reads a
// 6-digit code off the machine running the dashboard, types it here, and both
// sides derive the same key with HKDF. The key never travels over the wire.


function pairedKey() {
  if (memoryKey) return memoryKey;
  try {
    const kept = localStorage.getItem(KEY_STORE);
    if (kept) return kept;
  } catch (err) {
    keyStoreError = 'localStorage: ' + (err && err.message ? err.message : err);
  }
  try {
    return sessionStorage.getItem(KEY_STORE) || '';
  } catch (err) {
    keyStoreError += '; sessionStorage: ' +
      (err && err.message ? err.message : err);
  }
  return '';
}

/** Store the key and prove it stuck. Returns true only after a read-back. */
function storeKey(value) {
  keyStoreError = '';
  memoryKey = value;      // in memory first: persistence is optional
  if (!value) {
    try { localStorage.removeItem(KEY_STORE); } catch (err) { /* ignore */ }
    try { sessionStorage.removeItem(KEY_STORE); } catch (err) { /* ignore */ }
    return true;
  }
  // The checkbox failure this replaces: the write was wrapped in a bare catch,
  // so a browser that refuses site data looked exactly like a successful pairing
  // -- until the reload, where the overlay came straight back.
  try {
    localStorage.setItem(KEY_STORE, value);
    if (localStorage.getItem(KEY_STORE) === value) return true;
    keyStoreError = 'localStorage write did not stick';
  } catch (err) {
    keyStoreError = 'localStorage: ' + (err && err.message ? err.message : err);
  }
  // sessionStorage dies with the tab, but it survives the reload that follows
  // pairing -- which is what makes a browser with site data blocked usable.
  try {
    sessionStorage.setItem(KEY_STORE, value);
    if (sessionStorage.getItem(KEY_STORE) === value) {
      keyStoreError += ' (fell back to sessionStorage)';
      return true;
    }
    keyStoreError += '; sessionStorage write did not stick';
  } catch (err) {
    keyStoreError += '; sessionStorage: ' +
      (err && err.message ? err.message : err);
  }
  return true;            // memoryKey holds it; storage is best effort
}

function authHeaders(extra) {
  const key = pairedKey();
  const headers = extra || {};
  if (key) headers.Authorization = 'Bearer ' + key;
  return headers;
}

function bytesToHex(bytes) {
  return Array.from(bytes).map((b) => b.toString(16).padStart(2, '0')).join('');
}

/** K = HKDF-SHA256(code, "client_id|client_nonce|daemon_nonce", "pcmon-pair-v1", 16) */
async function deriveKeyHex(code, clientId, clientNonce, daemonNonce) {
  const enc = new TextEncoder();
  const base = await crypto.subtle.importKey('raw', enc.encode(code), 'HKDF',
                                            false, ['deriveBits']);
  const bits = await crypto.subtle.deriveBits({
    name: 'HKDF',
    hash: 'SHA-256',
    salt: enc.encode(clientId + '|' + clientNonce + '|' + daemonNonce),
    info: enc.encode('pcmon-pair-v1'),
  }, base, 128);
  return bytesToHex(new Uint8Array(bits));
}

function showPairing(message) {
  const overlay = el('pairing');
  if (!overlay) return;
  overlay.classList.remove('hidden');
  const note = el('pair-msg');
  if (note && message) note.textContent = message;
}

function hidePairing() {
  const overlay = el('pairing');
  if (overlay) overlay.classList.add('hidden');
}

// One outstanding /pair/begin, kept so the code the user reads can be claimed.
let pendingPair = null;

async function requestCode() {
  const resp = await fetch('/pair/begin', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name: 'browser' }),
  });
  if (!resp.ok) throw new Error('服务器拒绝了配对请求 (' + resp.status + ')');
  pendingPair = await resp.json();
  el('pair-msg').textContent =
    '配对码已生成，在运行仪表盘的电脑上读取（后台运行时见启动器重定向的日志 ' +
    'tmp\\webui_live.out 里的 PAIR code= 一行），然后输入。';
  el('pair-code').focus();
  return pendingPair;
}

async function claimWithCode(code) {
  if (!pendingPair) throw new Error('请先点"获取配对码"');
  const clientNonce = bytesToHex(crypto.getRandomValues(new Uint8Array(16)));
  const keyPromise = deriveKeyHex(code, pendingPair.client_id, clientNonce,
                                  pendingPair.daemon_nonce);
  const claimResp = await fetch('/pair/claim', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      pair_id: pendingPair.pair_id,
      code: code,
      client_nonce: clientNonce,
    }),
  });
  if (!claimResp.ok) throw new Error('配对码不对，或者已经过期');
  // The key has to be on this side before the reload. Reloading with nothing
  // stored just shows the overlay again, which reads as "pairing failed" when
  // in fact the claim succeeded -- so refuse to reload and say why.
  const storedOk = storeKey(await keyPromise);
  // If persistence failed, say so where the operator can read it: a browser
  // that keeps no site data means pairing again after every reload.
  if (window.pcmonReport && keyStoreError) {
    window.pcmonReport({kind: 'store', msg: keyStoreError, stored: storedOk});
  }
  if (!storedOk) {
    throw new Error('配对已通过，但这个浏览器不让网页保存数据（' +
                    keyStoreError + '）。请在浏览器设置里允许本站保存数据后重试。');
  }
  hidePairing();
  // No reload: the key now lives in memory, so the page can come up
  // without a navigation that might drop it (see memoryKey above).
  loadFont().catch(function () {}).then(function () {
    applyRoute();
    connect();
  });
}

function wirePairing() {
  const get = el('pair-get');
  const button = el('pair-go');
  const input = el('pair-code');
  if (!get || !button || !input) return;

  get.addEventListener('click', async () => {
    get.disabled = true;
    el('pair-msg').textContent = '正在申请配对码…';
    try {
      await requestCode();
    } catch (err) {
      el('pair-msg').textContent = err.message || String(err);
    } finally {
      get.disabled = false;
    }
  });

  const submit = async () => {
    const code = (input.value || '').trim();
    if (!/^\d{6}$/.test(code)) {
      el('pair-msg').textContent = '请输入 6 位数字';
      return;
    }
    button.disabled = true;
    el('pair-msg').textContent = '配对中…';
    try {
      await claimWithCode(code);
    } catch (err) {
      el('pair-msg').textContent = err.message || String(err);
      button.disabled = false;
    }
  };
  button.addEventListener('click', submit);
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') submit();
  });
}

window.addEventListener('DOMContentLoaded', wirePairing);

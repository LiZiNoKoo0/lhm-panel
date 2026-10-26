/**
 * Layout editor: the only place in this dashboard that writes anything.
 *
 * The rule it exists to honour is PROTOCOL 22.5:
 *
 *   what the glass shows = the *applied* layout, and only that
 *
 * Everything in this file before 「应用」 is a draft. A draft lives in this
 * variable and nowhere else -- not in localStorage, not on the server. Reload
 * the page and it is gone, which is exactly what a draft is. The alternative
 * (a saved draft) sounds friendlier and is a trap: the daemon would be one
 * restart away from pushing half-finished work onto a device nobody is
 * watching, and that failure is indistinguishable from a layout bug.
 *
 * Deliberately not a drag-and-drop canvas. Dragging needs hit-testing, snapping
 * and undo to feel like anything but a toy; none of that is needed to prove the
 * round trip works, and the round trip is the feature. Numeric fields first.
 */

const editor = {
  /** The applied set as last read from the server: {ver, scenes:{i:{title,bg,nodes}}}. */
  appliedVer: 0,
  applied: {},
  /** The draft. Null until loaded. Never persisted anywhere. */
  draft: null,
  /** Which scene index is open in the editor. */
  scene: 0,
  loaded: false,
  busy: false,
};

const EDITOR_FIELD_HINTS = {
  type: '类型',
  x: 'x',
  y: 'y',
  w: '宽',
  h: '高',
  fill: '填充',
  color: '描边/文字色',
  text: '文字',
  bind: '绑定指标',
  dec: '小数位',
  unit: '单位',
  size: '字号',
};

function editorDraftScene() {
  if (!editor.draft) return null;
  return editor.draft[String(editor.scene)] || null;
}

function editorClone(value) {
  return JSON.parse(JSON.stringify(value));
}

/** True when the draft differs from what is applied. Drives the apply button. */
function editorIsDirty() {
  if (!editor.draft) return false;
  return JSON.stringify(editor.draft) !== JSON.stringify(editor.applied);
}

function editorSetNote(text, kind) {
  const note = el('editor-note');
  if (!note) return;
  if (!text) {
    note.classList.add('hidden');
    note.textContent = '';
    return;
  }
  note.textContent = text;
  note.classList.remove('hidden');
  note.className = 'text-xs rounded-lg px-3 py-2 mb-4 ' + (
    kind === 'error'
      ? 'bg-red-50 text-red-700 border border-red-100'
      : 'bg-indigo-50 text-indigo-700 border border-indigo-100'
  );
}

async function editorLoad() {
  try {
    // authHeaders() lives in app.js; both files are loaded as classic scripts,
    // so this route has to carry the key too -- without it the editor 401s even
    // on a perfectly paired browser, and the failure surfaces as "startup failed".
    const response = await fetch('/api/layout', {
      cache: 'no-store',
      headers: authHeaders(),
    });
    if (response.status === 401) {
      // Unpaired is the page's problem, not the editor's: hand it to the gate.
      showPairing('这台设备还没有配对。请在运行仪表盘的电脑上读取 6 位配对码。');
      throw new Error('unpaired');
    }
    const data = await response.json();
    editor.appliedVer = Number(data.ver) || 1;
    editor.applied = data.scenes || {};
    editor.draft = editorClone(editor.applied);
    editor.loaded = true;
    // Open the first page that exists, so an operator editing one page does not
    // stare at an empty editor for a page index they never used.
    const indices = Object.keys(editor.applied).map(Number).sort((a, b) => a - b);
    if (indices.length && !editor.applied[String(editor.scene)]) {
      editor.scene = indices[0];
    }
  } catch (err) {
    editorSetNote('读不到 /api/layout：' + err.message, 'error');
    editor.loaded = false;
  }
  renderEditor();
}

/** Create a page in the draft when the applied set has none. */
function editorEnsureScene() {
  if (!editor.draft) editor.draft = {};
  const key = String(editor.scene);
  if (!editor.draft[key]) {
    editor.draft[key] = {title: 'P' + (editor.scene + 1), bg: '#000000', nodes: []};
  }
  return editor.draft[key];
}

function renderEditor() {
  if (!editor.loaded) {
    if (el('editor-ver')) el('editor-ver').textContent = '未加载';
    return;
  }

  const dirty = editorIsDirty();
  const ver = el('editor-ver');
  if (ver) {
    ver.textContent = '已应用 ver ' + editor.appliedVer;
    ver.className = 'pill ' + (dirty ? 'pill-warn' : 'pill-ok');
  }
  const dirtyLabel = el('editor-dirty');
  if (dirtyLabel) {
    dirtyLabel.textContent = dirty ? '有未应用的修改' : '与屏上一致';
    dirtyLabel.className = 'text-xs tabular ' + (dirty ? 'text-amber-600' : 'text-gray-500');
  }
  const applyBtn = el('editor-apply');
  if (applyBtn) applyBtn.disabled = editor.busy || !dirty;

  // Page picker: every page in the draft, plus the next free index so a new page
  // can be started without a separate "new page" dialog.
  const picker = el('editor-page');
  if (picker) {
    const indices = Object.keys(editor.draft || {}).map(Number).sort((a, b) => a - b);
    const nextIndex = indices.length ? Math.max.apply(null, indices) + 1 : 0;
    const wanted = indices.concat([nextIndex]);
    picker.innerHTML = '';
    for (const index of wanted) {
      const option = document.createElement('option');
      option.value = String(index);
      const scene = (editor.draft || {})[String(index)];
      option.textContent = scene
        ? (scene.title || '页面 ' + index)
        : '＋ 新建页面 ' + index;
      if (index === editor.scene) option.selected = true;
      picker.appendChild(option);
    }
  }

  const scene = editorDraftScene();
  const titleInput = el('editor-title');
  const bgInput = el('editor-bg');
  if (titleInput) {
    titleInput.value = scene ? (scene.title || '') : '';
    titleInput.disabled = !scene;
  }
  if (bgInput) {
    bgInput.value = scene ? (scene.bg || '#000000') : '';
    bgInput.disabled = !scene;
  }

  renderEditorNodes(scene);
  drawEditorPreview();
}

function renderEditorNodes(scene) {
  const host = el('editor-nodes');
  if (!host) return;
  host.innerHTML = '';
  if (!scene) {
    const empty = document.createElement('p');
    empty.className = 'text-sm text-gray-400';
    empty.textContent = '还没有页面。点上面的「+ 数值」会新建一个。';
    host.appendChild(empty);
    return;
  }
  if (!scene.nodes.length) {
    const empty = document.createElement('p');
    empty.className = 'text-sm text-gray-400';
    empty.textContent = '这个页面还没有节点。';
    host.appendChild(empty);
    return;
  }

  scene.nodes.forEach((node, index) => {
    const row = document.createElement('div');
    row.className = 'border border-gray-100 rounded-lg p-2 flex flex-col gap-2';

    const head = document.createElement('div');
    head.className = 'flex items-center gap-2 text-xs';
    const badge = document.createElement('span');
    badge.className = 'font-mono text-gray-400';
    badge.textContent = '#' + index;
    const kind = document.createElement('span');
    kind.className = 'font-semibold';
    kind.textContent = node.type;
    const spacer = document.createElement('span');
    spacer.className = 'ml-auto flex items-center gap-1';
    const up = editorSmallButton('↑', () => editorMoveNode(index, -1));
    const down = editorSmallButton('↓', () => editorMoveNode(index, 1));
    const del = editorSmallButton('删除', () => editorRemoveNode(index));
    spacer.append(up, down, del);
    head.append(badge, kind, spacer);

    const fields = document.createElement('div');
    fields.className = 'grid grid-cols-2 md:grid-cols-4 gap-2';
    for (const field of editorNodeFields(node)) {
      fields.appendChild(editorField(node, index, field));
    }

    row.append(head, fields);
    host.appendChild(row);
  });
}

function editorSmallButton(label, onClick) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'px-1.5 py-0.5 rounded border border-gray-200 text-gray-500 hover:bg-gray-50';
  button.textContent = label;
  button.addEventListener('click', onClick);
  return button;
}

/**
 * Which fields this node kind actually has.
 *
 * Mirrors `EDITABLE` in pcmon/layouts.py: the server refuses anything else with
 * a named field, so showing a control the server would reject would just be a
 * slower way to fail.
 */
/** Font sizes the firmware can actually draw, shown to the operator as a scale.
 *
 * The panel has one 8x16 glyph table (firmware/panel-core/src/font8x16.rs) and
 * draws 32/48 by integer magnification of that same table, so the wire value is
 * a pixel height while the thing a human thinks about is "how many times bigger
 * than normal". Both are kept below: PANEL_FONTS is what goes on the wire
 * (PROTOCOL 7 `size`), FONT_SCALES maps it to the label in the dropdown.
 *
 * A free-text size box cannot work: anything not in this list is silently
 * rewritten by `fit_nodes` before the frame leaves the daemon, so the operator
 * types 28, the daemon sends 16, and nothing on the glass changes.
 *
 * Keep in step with FONT_SIZES in firmware/panel-core/src/ui.rs.
 */
const PANEL_FONTS = [16, 32, 48];

/** Wire size -> the multiplier label shown in the editor. */
const FONT_SCALES = {16: '1x', 32: '2x', 48: '3x'};

function editorNodeFields(node) {
  const common = ['x', 'y'];
  if (node.type === 'rect') {
    const fields = common.concat(['w', 'h', 'fill']);
    if (node.color !== undefined) fields.push('color');
    if (node.bind_w !== undefined) fields.push('bind_w');
    return fields;
  }
  if (node.type === 'text') {
    return common.concat(['text', 'size', 'color']);
  }
  if (node.type === 'num') {
    return common.concat(['bind', 'dec', 'unit', 'size', 'color']);
  }
  return common;
}

/** Colour fields get a swatch; the hex box stays usable for exact values. */
const EDITOR_COLOUR_FIELDS = ['fill', 'color'];

function editorField(node, index, field) {
  const label = document.createElement('label');
  label.className = 'text-[11px] text-gray-500 flex flex-col gap-0.5';

  // `size` is not editable: the firmware has one glyph table. See PANEL_FONTS.
  if (field === 'size') {
    const fixed = PANEL_FONTS.length === 1;
    label.textContent = fixed
      ? (EDITOR_FIELD_HINTS[field] || field) + '（固件固定）'
      : (EDITOR_FIELD_HINTS[field] || field);
    if (fixed) {
      const fixedBox = document.createElement('input');
      fixedBox.className =
        'text-xs px-1.5 py-1 rounded border border-gray-200 bg-gray-100 text-gray-400 font-mono';
      fixedBox.value = String(PANEL_FONTS[0]);
      fixedBox.disabled = true;
      fixedBox.title = '这块板只有一套 8x16 字模，其他字号会被 daemon 改回 16';
      label.appendChild(fixedBox);
      return label;
    }
    // More than one real size: a select, so the value can only be one the
    // terminal promised to draw. Labelled as a multiplier (1x/2x/3x) rather
    // than as pixels: "32" means nothing to an operator looking at a 160x80
    // panel, "2x" says how much bigger than normal this will be.
    label.textContent = EDITOR_FIELD_HINTS[field] || field;
    const pick = document.createElement('select');
    pick.className = 'text-xs px-1.5 py-1 rounded border border-gray-200 bg-white font-mono';
    for (const size of PANEL_FONTS) {
      const option = document.createElement('option');
      option.value = String(size);
      option.textContent = FONT_SCALES[size] || String(size);
      if (Number(node[field]) === size) option.selected = true;
      pick.appendChild(option);
    }
    // A node that predates this field (or came from a hand-edited file) may
    // carry a size that is not in the list; show it so the operator can see
    // what the firmware will snap it to instead of silently changing it.
    if (node[field] !== undefined && PANEL_FONTS.indexOf(Number(node[field])) < 0) {
      const odd = document.createElement('option');
      odd.value = String(node[field]);
      odd.textContent = String(node[field]) + '（会被改成 ' + (FONT_SCALES[16] || '1x') + '）';
      odd.selected = true;
      pick.appendChild(odd);
    }
    pick.addEventListener('change', () => editorSetField(index, field, pick.value));
    label.appendChild(pick);
    return label;
  }

  label.textContent = EDITOR_FIELD_HINTS[field] || field;

  const colour = EDITOR_COLOUR_FIELDS.indexOf(field) >= 0;
  const row = document.createElement('span');
  row.className = 'flex items-center gap-1';

  const input = document.createElement('input');
  const value = node[field];
  input.className =
    (colour ? 'flex-1 min-w-0 ' : '') +
    'text-xs px-1.5 py-1 rounded border border-gray-200 bg-white font-mono';
  input.value = value === undefined || value === null ? '' : String(value);
  input.dataset.node = String(index);
  input.dataset.field = field;
  if (field === 'bind' || field === 'bind_w') {
    // A typo here produces a node bound to a metric that does not exist: it
    // draws `--` forever and looks like a data problem, not a typo. Listing the
    // real catalog ids is cheap and removes that whole class of confusion.
    input.setAttribute('list', 'editor-bind-options');
  }
  input.addEventListener('change', () => editorSetField(index, field, input.value));
  row.appendChild(input);

  if (colour) {
    // type=color only speaks #rrggbb in lowercase; the wire format is the same
    // shape, so the two can share one value with no translation layer.
    const swatch = document.createElement('input');
    swatch.type = 'color';
    swatch.className = 'editor-swatch';
    swatch.value = editorNormaliseColour(value);
    swatch.title = '选色';
    // Live dragging must not re-render the list: the picker is a native popup
    // that only survives while its element does, and renderEditor rebuilds every
    // node row. Writing straight into the draft keeps the preview and the hex
    // box in step while the popup stays open; the full re-render waits for
    // change, which fires when the picker closes.
    swatch.addEventListener('input', () => {
      input.value = swatch.value;
      const scene = editorDraftScene();
      if (scene && scene.nodes[index]) scene.nodes[index][field] = swatch.value;
      drawEditorPreview();
    });
    swatch.addEventListener('change', () => {
      editorSetField(index, field, swatch.value);
    });
    row.appendChild(swatch);
  }

  label.appendChild(row);
  return label;
}

/** `#RRGGBB` for the colour input, falling back to black when the value is not one. */
function editorNormaliseColour(value) {
  if (typeof value === 'string' && /^#[0-9a-fA-F]{6}$/.test(value)) {
    return value.toLowerCase();
  }
  return '#000000';
}

function editorSetField(index, field, raw) {
  const scene = editorDraftScene();
  if (!scene || !scene.nodes[index]) return;
  const node = scene.nodes[index];
  const numeric = ['x', 'y', 'w', 'h', 'dec', 'size'].indexOf(field) >= 0;
  if (raw === '') {
    if (field === 'color' || field === 'bind_w' || field === 'unit') {
      delete node[field];
    } else if (numeric) {
      node[field] = 0;
    } else {
      node[field] = '';
    }
  } else if (numeric) {
    const parsed = Number(raw);
    node[field] = Number.isFinite(parsed) ? Math.trunc(parsed) : 0;
  } else {
    node[field] = raw;
  }
  renderEditor();
}

function editorAddNode(type) {
  if (!editor.loaded) return;
  const scene = editorEnsureScene();
  const base = {type: type, x: 8, y: 8};
  if (type === 'num') {
    // Bind to the first metric the catalog actually has, so a new node shows a
    // number straight away instead of `--` that looks like a failure.
    const state = window.__state || {};
    const first = ((state.catalog || {}).metrics || [])[0];
    scene.nodes.push(Object.assign(base, {
      bind: first ? first.id : 'cpu.total',
      dec: 0,
      unit: first ? (first.unit || '') : '',
      size: 16,
      color: '#FFFFFF',
    }));
  } else if (type === 'text') {
    scene.nodes.push(Object.assign(base, {text: 'TEXT', size: 16, color: '#FFFFFF'}));
  } else {
    scene.nodes.push(Object.assign(base, {w: 40, h: 8, fill: '#44DD88'}));
  }
  renderEditor();
}

function editorRemoveNode(index) {
  const scene = editorDraftScene();
  if (!scene) return;
  scene.nodes.splice(index, 1);
  renderEditor();
}

function editorMoveNode(index, delta) {
  const scene = editorDraftScene();
  if (!scene) return;
  const target = index + delta;
  if (target < 0 || target >= scene.nodes.length) return;
  const moved = scene.nodes.splice(index, 1)[0];
  scene.nodes.splice(target, 0, moved);
  renderEditor();
}

/** Preview the draft, not the applied layout: the operator is looking at edits. */
function drawEditorPreview() {
  const canvas = el('editor-canvas');
  if (!canvas) return;
  const scene = editorDraftScene();

  // Size from the panel's own dimensions, like drawPanel does -- not from
  // getBoundingClientRect. The editor page is unhidden by a class toggle, so at
  // the moment this runs the browser has not laid it out yet and the rect is
  // 0x0; sizing from that produced a 1x1 canvas, which reads as "the preview is
  // black". A fixed integer scale also keeps the pixels crisp.
  const PANEL_W = 160;
  const PANEL_H = 80;
  const scale = 4;
  if (canvas.width !== PANEL_W * scale || canvas.height !== PANEL_H * scale) {
    canvas.width = PANEL_W * scale;
    canvas.height = PANEL_H * scale;
  }
  const ctx = canvas.getContext('2d', { alpha: false });
  if (!ctx) return;
  ctx.setTransform(scale, 0, 0, scale, 0, 0);
  ctx.imageSmoothingEnabled = false;

  ctx.fillStyle = (scene && scene.bg) || '#000000';
  ctx.fillRect(0, 0, PANEL_W, PANEL_H);

  if (!scene) {
    ctx.fillStyle = '#666666';
    ctx.font = '8px monospace';
    ctx.fillText('no page', 4, 12);
    return;
  }

  // Values are read from the live link so the preview moves while editing. A
  // stale value draws `--`, same rule as the glass (PROTOCOL 9.1).
  const state = window.__state || {};
  for (const node of scene.nodes) {
    if (node.type === 'rect') {
      ctx.fillStyle = node.fill || '#FFFFFF';
      ctx.fillRect(node.x || 0, node.y || 0, node.w || 0, node.h || 0);
      if (node.color) {
        ctx.strokeStyle = node.color;
        ctx.lineWidth = 1;
        ctx.strokeRect((node.x || 0) + 0.5, (node.y || 0) + 0.5,
                       Math.max(0, (node.w || 0) - 1), Math.max(0, (node.h || 0) - 1));
      }
    } else if (node.type === 'text') {
      drawText(ctx, node.text || '', node.x || 0, node.y || 0,
               node.size || 16, node.color || '#FFFFFF', node.align);
    } else if (node.type === 'num') {
      const stale = node.bind ? isStale(state, node.bind) : true;
      const entry = node.bind ? valueEntry(state, node.bind) : null;
      const text = stale ? '--' : fmtNum(entry && entry.v, node.dec || 0, node.unit || '');
      drawText(ctx, text, node.x || 0, node.y || 0,
               node.size || 16, node.color || '#FFFFFF', node.align);
    }
  }

  const label = el('editor-preview-ver');
  if (label) label.textContent = 'draft · 页面 ' + editor.scene;
}

async function editorApply() {
  if (!editor.draft || editor.busy) return;
  editor.busy = true;
  renderEditor();
  editorSetNote('正在应用…', 'info');
  try {
    const response = await fetch('/api/layout/apply', {
      method: 'POST',
      headers: authHeaders({'Content-Type': 'application/json'}),
      body: JSON.stringify({scenes: editor.draft}),
    });
    const data = await response.json();
    if (!response.ok || !data.ok) {
      editorSetNote('应用被拒绝：' + (data.error || response.status), 'error');
      return;
    }
    // Adopt the server's answer rather than the draft: it is what the terminals
    // will get (including the ver the daemon will announce).
    editor.appliedVer = Number(data.ver) || editor.appliedVer;
    editor.applied = data.scenes || editor.draft;
    editor.draft = editorClone(editor.applied);
    editorSetNote(
      '已应用 ver ' + editor.appliedVer + (data.pushed ? '，已推送' : '，但推送失败（daemon 重连后会重发）'),
      'info'
    );
  } catch (err) {
    editorSetNote('应用失败：' + err.message, 'error');
  } finally {
    editor.busy = false;
    renderEditor();
  }
}

function editorRevert() {
  editor.draft = editorClone(editor.applied);
  editorSetNote('已丢弃未应用的修改', 'info');
  renderEditor();
}

function editorWireOnce() {
  if (editor.wired) return;
  editor.wired = true;

  const apply = el('editor-apply');
  if (apply) apply.addEventListener('click', editorApply);
  const revert = el('editor-revert');
  if (revert) revert.addEventListener('click', editorRevert);

  // Page picker. The binding must exist before the handler that closes over it:
  // an earlier edit split this block and left `picker` referenced but never
  // declared, which threw ReferenceError during boot and took the whole editor
  // down with it.
  const picker = el('editor-page');
  if (picker) {
    picker.addEventListener('change', () => {
      editor.scene = Number(picker.value) || 0;
      editorEnsureScene();
      renderEditor();
    });
  }
  const title = el('editor-title');
  if (title) {
    title.addEventListener('change', () => {
      const scene = editorEnsureScene();
      scene.title = title.value;
      renderEditor();
    });
  }
  const bg = el('editor-bg');
  if (bg) {
    bg.addEventListener('change', () => {
      const scene = editorEnsureScene();
      scene.bg = bg.value || '#000000';
      renderEditor();
    });
  }
  const addNum = el('editor-add-num');
  if (addNum) addNum.addEventListener('click', () => editorAddNode('num'));
  const addText = el('editor-add-text');
  if (addText) addText.addEventListener('click', () => editorAddNode('text'));
  const addRect = el('editor-add-rect');
  if (addRect) addRect.addEventListener('click', () => editorAddNode('rect'));

  // Catalog ids, for the bind inputs' datalist.
  const list = document.createElement('datalist');
  list.id = 'editor-bind-options';
  document.body.appendChild(list);
}

/**
 * Keep the bind datalist in step with the catalog.
 *
 * Called on every snapshot. Rebuilding it only when the id set changes keeps a
 * 1 Hz stream from thrashing the DOM while still picking up a plugin that came
 * or went (PROTOCOL 6).
 */
function updateBindHints(state) {
  const list = document.getElementById('editor-bind-options');
  if (!list) return;
  const metrics = ((state.catalog || {}).metrics) || [];
  const ids = metrics.map((m) => m.id).filter(Boolean);
  const key = ids.join('\u0000');
  if (list.dataset.key === key) return;
  list.dataset.key = key;
  list.innerHTML = '';
  for (const metric of metrics) {
    if (!metric.id) continue;
    const option = document.createElement('option');
    option.value = metric.id;
    option.label = metric.label ? metric.label + ' (' + metric.id + ')' : metric.id;
    list.appendChild(option);
  }
}

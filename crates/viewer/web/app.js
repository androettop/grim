import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s).replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
const q = (o) => Object.entries(o).map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join('&');

async function api(route, params) {
  const r = await fetch(`/api/${route}?${q(params || {})}`);
  if (!r.ok) throw new Error(await r.text());
  return r.json();
}

// Unreal is Z-up and left-handed; swapping Y and Z gives three.js' Y-up (mirrored, rendered double-sided).
const ue = (x, y, z) => [x, z, y];

let exportsList = [];
let current = null;
let active3d = null;

// ---------------------------------------------------------------- navigation

async function init() {
  const pkgs = await api('packages');
  $('pkg').innerHTML = pkgs.map((p) => `<option value="${esc(p.p)}">${esc(p.p)}</option>`).join('');
  $('pkg').onchange = () => selectPackage($('pkg').value);
  $('filter').oninput = renderList;
  window.onhashchange = fromHash;
  if (location.hash.length > 1) await fromHash();
  else await selectPackage(pkgs[0].p);
}

async function fromHash() {
  const h = Object.fromEntries(new URLSearchParams(location.hash.slice(1)));
  if (!h.p) return;
  if ($('pkg').value !== h.p || !exportsList.length) {
    $('pkg').value = h.p;
    await selectPackage(h.p, true);
  }
  if (h.i !== undefined && (!current || current.p !== h.p || current.i !== +h.i)) openObject(h.p, +h.i);
}

async function selectPackage(p, keepHash) {
  $('list').innerHTML = '<div class="row">loading…</div>';
  exportsList = await api('exports', { p });
  exportsList.p = p;
  renderList();
  if (!keepHash) location.hash = q({ p });
}

function renderList() {
  const f = $('filter').value.toLowerCase();
  const rows = exportsList.filter((e) => !f || e.name.toLowerCase().includes(f) || e.class.toLowerCase().includes(f));
  $('list').innerHTML = rows.slice(0, 3000).map((e) =>
    `<div class="row st-${e.status}${current && current.i === e.i ? ' sel' : ''}" data-i="${e.i}">` +
    `<span class="cls">${esc(e.class)}</span><span class="nm">${esc(e.name)}</span></div>`).join('') +
    (rows.length > 3000 ? `<div class="row">… ${rows.length - 3000} more, refine the filter</div>` : '');
  for (const el of $('list').children) {
    if (el.dataset.i !== undefined) el.onclick = () => { location.hash = q({ p: exportsList.p, i: el.dataset.i }); };
  }
}

async function openObject(p, i) {
  current = { p, i };
  renderList();
  disposeActive();
  $('view').innerHTML = '';
  $('details').innerHTML = 'loading…';
  const o = await api('object', { p, i });
  $('title').innerHTML = `<b>${esc(o.name)}</b> <span class="sub">${esc(o.class)} · export ${i} · ` +
    `${o.size} bytes @ 0x${o.offset.toString(16)} · flags ${o.flags} · ${o.status}</span>`;
  renderDetails(o);
  try {
    const view = { texture: viewTexture, sound: viewSound, mesh: viewMesh, anim: viewAnim, model: viewModel, palette: viewPalette, font: viewFont }[o.kind];
    if (view) await view(p, i, o);
    applyHashToggles();
  } catch (e) {
    $('view').innerHTML = `<pre class="err">${esc(e.message)}</pre>`;
  }
}

function renderDetails(o) {
  let h = '';
  if (o.error) h += `<pre class="err">${esc(o.error)}</pre><pre>${esc(o.hexdump)}</pre>`;
  if (o.properties && o.properties.length) {
    h += '<table>' + o.properties.map((p) => `<tr><td>${esc(p.name)}${p.index ? `[${p.index}]` : ''}</td><td>${fmtValue(p.value)}</td></tr>`).join('') + '</table>';
  }
  if (o.state_frame) h += `<pre>state frame: ${esc(o.state_frame)}</pre>`;
  if (o.debug) h += `<pre>${esc(o.debug)}</pre>`;
  $('details').innerHTML = h;
  for (const a of $('details').querySelectorAll('a[data-p]')) a.onclick = () => { location.hash = q({ p: a.dataset.p, i: a.dataset.i }); };
}

function fmtValue(v) {
  if (v && typeof v === 'object') {
    if (v.p !== undefined) return `<a data-p="${esc(v.p)}" data-i="${v.i}">${esc(v.class)} ${esc(v.name)}</a>`;
    if (v.unresolved) return `<span class="err">${esc(v.unresolved)} (${esc(v.error)})</span>`;
  }
  return esc(typeof v === 'string' ? v : JSON.stringify(v));
}

function applyHashToggles() {
  const h = new URLSearchParams(location.hash.slice(1));
  for (const [k, v] of h) {
    const el = document.getElementById(k);
    if (el && el.type === 'checkbox') { el.checked = v === '1'; el.dispatchEvent(new Event('change')); }
    else if (el && el.tagName === 'SELECT' && k !== 'pkg') { el.value = v; el.dispatchEvent(new Event('change')); }
  }
}

function tools(html) {
  let d = $('tools');
  if (!d) {
    d = document.createElement('div');
    d.id = 'tools';
    $('view').appendChild(d);
  }
  d.insertAdjacentHTML('beforeend', html);
  return d;
}

// ---------------------------------------------------------------- 2D views

async function viewTexture(p, i, o) {
  const t = o.texture;
  if (![...t.mips, ...t.comp_mips].some((m) => m[2] > 0)) return viewProcedural(o);
  const opts = (list, comp) => list.map((m, k) => `<option value="${comp}:${k}">${comp ? 'comp ' : ''}mip ${k}: ${m[0]}×${m[1]}</option>`).join('');
  const bar = tools(`<select id="mip">${opts(t.mips, 0)}${opts(t.comp_mips, 1)}</select>
    <label>${esc(t.format)}${t.comp_format ? ' / ' + esc(t.comp_format) : ''}</label>
    <label><input type="checkbox" id="zoom" checked> zoom</label>`);
  const img = document.createElement('img');
  img.className = 'tex';
  $('view').appendChild(img);
  const show = () => {
    const [comp, mip] = $('mip').value.split(':');
    img.src = `/api/texture?${q({ p, i, mip, comp })}`;
    img.onerror = async () => { img.style.display = 'none'; const r = await fetch(img.src); bar.insertAdjacentHTML('beforeend', `<label class="err">${esc(await r.text())}</label>`); };
    img.onload = () => { img.style.display = ''; const z = $('zoom').checked ? Math.max(1, Math.floor(512 / Math.max(img.naturalWidth, img.naturalHeight))) : 1; img.style.width = `${img.naturalWidth * z}px`; };
  };
  $('mip').onchange = show;
  $('zoom').onchange = show;
  show();
}

// Procedural textures (Fire/Wet/Wave/Ice/Scripted) store empty mips: the engine generates
// them at runtime. Show what the file does contain: palette and source textures.
async function viewProcedural(o) {
  const t = o.texture;
  const d = document.createElement('div');
  d.style.padding = '16px';
  d.innerHTML = `<p>Procedural texture (${t.mips.map((m) => `${m[0]}×${m[1]}`).join(', ')}): its pixels are ` +
    'generated at runtime, so there is nothing to preview until the engine implements it.</p>';
  $('view').appendChild(d);
  const refs = (o.properties || []).filter((pr) => pr.value && pr.value.p !== undefined);
  const pal = refs.find((pr) => pr.name === 'Palette');
  if (pal) {
    const po = await api('object', { p: pal.value.p, i: pal.value.i });
    if (po.palette) {
      const c = document.createElement('canvas');
      c.width = 512; c.height = 24;
      const g = c.getContext('2d');
      po.palette.forEach(([r, gg, b], k) => { g.fillStyle = `rgb(${r},${gg},${b})`; g.fillRect(k * 2, 0, 2, 24); });
      d.insertAdjacentHTML('beforeend', '<p>Palette</p>');
      d.appendChild(c);
    }
  }
  for (const pr of refs.filter((pr) => /texture$/i.test(pr.name))) {
    d.insertAdjacentHTML('beforeend', `<p>${esc(pr.name)}: ${fmtValue(pr.value)}</p>`);
    const img = document.createElement('img');
    img.className = 'tex';
    img.style.margin = '0';
    img.src = `/api/texture?${q({ p: pr.value.p, i: pr.value.i })}`;
    img.onerror = () => { img.replaceWith(Object.assign(document.createElement('p'), { textContent: '(no preview: procedural or empty)' })); };
    d.appendChild(img);
  }
  for (const a of d.querySelectorAll('a[data-p]')) a.onclick = () => { location.hash = q({ p: a.dataset.p, i: a.dataset.i }); };
}

function viewPalette(p, i, o) {
  const c = document.createElement('canvas');
  c.width = c.height = 16 * 20;
  c.style.margin = '16px';
  const g = c.getContext('2d');
  o.palette.forEach(([r, gg, b, a], k) => {
    g.fillStyle = `rgb(${r},${gg},${b})`;
    g.fillRect((k % 16) * 20, Math.floor(k / 16) * 20, 19, 19);
  });
  $('view').appendChild(c);
}

function viewFont(p, i, o) {
  if (!o.font.length) { $('view').innerHTML = '<pre>no glyph pages (system font, see kw_tail)</pre>'; return; }
  const wrap = document.createElement('div');
  wrap.style.padding = '48px 16px 16px';
  $('view').appendChild(wrap);
  for (const page of o.font) {
    if (!page.texture || page.texture.p === undefined) continue;
    const img = new Image();
    img.src = `/api/texture?${q({ p: page.texture.p, i: page.texture.i })}`;
    img.onload = () => {
      const c = document.createElement('canvas');
      c.width = img.naturalWidth; c.height = img.naturalHeight;
      c.className = 'tex';
      c.style.margin = '0 8px 8px 0';
      const g = c.getContext('2d');
      g.drawImage(img, 0, 0);
      g.strokeStyle = 'rgba(255,90,180,.8)';
      for (const [u, v, w, h] of page.chars) if (w && h) g.strokeRect(u + .5, v + .5, w - 1, h - 1);
      wrap.appendChild(c);
    };
  }
}

function viewSound(p, i, o) {
  const s = o.sound;
  const d = document.createElement('div');
  d.style.padding = '16px';
  d.innerHTML = `<p>${esc(s.format)} · ${s.duration.toFixed(3)} s · ${s.sample_rate || '?'} Hz · flags ${s.flags}` +
    `${s.has_trailer ? ' · has trailer' : ''}</p>` +
    `<audio controls src="/api/sound?${q({ p, i })}"></audio>`;
  $('view').appendChild(d);
}

// ---------------------------------------------------------------- 3D scaffolding

function disposeActive() {
  if (active3d) { active3d.stop = true; active3d.renderer.dispose(); active3d = null; }
}

function make3d() {
  const view = $('view');
  const renderer = new THREE.WebGLRenderer({ antialias: true, preserveDrawingBuffer: true });
  renderer.setPixelRatio(window.devicePixelRatio);
  renderer.setSize(view.clientWidth, view.clientHeight);
  renderer.setClearColor(0x202329);
  view.appendChild(renderer.domElement);
  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(50, view.clientWidth / view.clientHeight, 1, 1e6);
  const controls = new OrbitControls(camera, renderer.domElement);
  scene.add(new THREE.AmbientLight(0xffffff, 1));
  const ctx = { renderer, scene, camera, controls, onFrame: null, stop: false };
  const loop = (t) => {
    if (ctx.stop) return;
    if (ctx.onFrame) ctx.onFrame(t / 1000);
    controls.update();
    renderer.render(scene, camera);
    requestAnimationFrame(loop);
  };
  requestAnimationFrame(loop);
  new ResizeObserver(() => {
    renderer.setSize(view.clientWidth, view.clientHeight);
    camera.aspect = view.clientWidth / view.clientHeight;
    camera.updateProjectionMatrix();
  }).observe(view);
  active3d = ctx;
  return ctx;
}

function frame(ctx, box) {
  const c = box.getCenter(new THREE.Vector3());
  const r = Math.max(box.getSize(new THREE.Vector3()).length() / 2, 1);
  ctx.camera.near = r / 100;
  ctx.camera.far = r * 200;
  ctx.camera.position.set(c.x + r * 1.2, c.y + r * 0.6, c.z + r * 1.6);
  ctx.camera.updateProjectionMatrix();
  ctx.controls.target.copy(c);
}

const texCache = new Map();
function textureFor(ref, onSize) {
  if (!ref || ref.p === undefined) return null;
  const key = `${ref.p}#${ref.i}`;
  let t = texCache.get(key);
  if (!t) {
    t = new THREE.TextureLoader().load(`/api/texture?${q({ p: ref.p, i: ref.i })}`);
    t.wrapS = t.wrapT = THREE.RepeatWrapping;
    t.colorSpace = THREE.SRGBColorSpace;
    t.flipY = false;
    texCache.set(key, t);
  }
  if (onSize) {
    if (t.image && t.image.width) onSize(t.image.width, t.image.height);
    else { const prev = t.onUpdate; t.onUpdate = () => { if (prev) prev(); onSize(t.image.width, t.image.height); }; }
  }
  return t;
}

function lineSegments(color) {
  const geo = new THREE.BufferGeometry();
  geo.setAttribute('position', new THREE.Float32BufferAttribute([], 3));
  const mat = new THREE.LineBasicMaterial({ color, depthTest: false });
  const l = new THREE.LineSegments(geo, mat);
  l.renderOrder = 10;
  return l;
}

// ---------------------------------------------------------------- skeletal math

function boneMatrix(q, p, conj) {
  const quat = new THREE.Quaternion(q[0], q[1], q[2], q[3]);
  if (conj) quat.conjugate();
  return new THREE.Matrix4().compose(new THREE.Vector3(p[0], p[1], p[2]), quat.normalize(), new THREE.Vector3(1, 1, 1));
}

// World matrices from local ones; a bone whose parent is itself (or index 0 for bone 0) is a root.
function worldMatrices(parents, locals) {
  const out = [];
  for (let b = 0; b < locals.length; b++) {
    const par = parents[b];
    out[b] = (b === 0 || par === b || par < 0 || par >= b) ? locals[b].clone() : out[par].clone().multiply(locals[b]);
  }
  return out;
}

// Decoder for the KnowWonder animation pools (see grim-assets animation.rs); the alternative
// rotation decodings stay selectable for comparison.
function prepareAnim(a, opt) {
  let ia = 0, ib = 0, ic = 0;
  const moves = a.moves.map((m) => ({
    ...m,
    tracks: m.tracks.map((t, k) => {
      const rots = [], poss = [], times = [];
      let acc = 0;
      for (let j = 0; j < t.c; j++) { acc += a.pool_c[ic + j]; times.push(acc); }
      for (let j = 0; j < t.a; j++) {
        const o = (ia + j) * 3;
        const [mode, scale] = opt.rotScale.split(':');
        const x = a.pool_a[o] / +scale, y = a.pool_a[o + 1] / +scale, z = a.pool_a[o + 2] / +scale;
        if (mode === 'sin') {
          // q = sin(v / 32767 * π/2) per component, W implicit (see grim-assets animation.rs).
          const [qx, qy, qz] = [a.pool_a[o], a.pool_a[o + 1], a.pool_a[o + 2]].map((v) => Math.sin(v / 32767 * Math.PI / 2));
          rots.push(new THREE.Quaternion(qx, qy, qz, Math.sqrt(Math.max(0, 1 - qx * qx - qy * qy - qz * qz))));
        } else {
          rots.push(new THREE.Quaternion(x, y, z, Math.sqrt(Math.max(0, 1 - x * x - y * y - z * z))));
        }
      }
      for (let j = 0; j < t.b; j++) {
        const o = (ib + j) * 3, s = t.x / 32767;
        poss.push(new THREE.Vector3(a.pool_b[o] * s, a.pool_b[o + 1] * s, a.pool_b[o + 2] * s));
      }
      ia += t.a; ib += t.b; ic += t.c;
      const bone = m.bone_indices.length ? m.bone_indices[k] : m.start_bone + k;
      return { bone, rots, poss, times, raw: t };
    }),
  }));
  return { moves, bones: a.bones, seqs: a.seqs };
}

function keyAt(keys, times, f, lerp) {
  if (keys.length === 1) return keys[0].clone();
  const tm = times.length === keys.length ? times : keys.map((_, k) => k);
  let k = 0;
  while (k + 1 < keys.length && tm[k + 1] <= f) k++;
  if (k + 1 >= keys.length) return keys[k].clone();
  const span = tm[k + 1] - tm[k];
  const u = span > 0 ? (f - tm[k]) / span : 0;
  return lerp(keys[k].clone(), keys[k + 1], u);
}

// Local transforms (in anim bone order) for sequence `s` at frame `f`, or null if a bone has no track.
function animLocals(pa, s, f, opt) {
  const m = pa.moves[s];
  const locals = pa.bones.map(() => null);
  for (const t of m.tracks) {
    if (!t.rots.length) continue;
    const q = keyAt(t.rots, t.times, f, (a, b, u) => a.slerp(b, u));
    if (opt.conjAnim) q.conjugate();
    const p = t.poss.length ? keyAt(t.poss, t.times, f, (a, b, u) => a.lerp(b, u)) : new THREE.Vector3();
    locals[t.bone] = { q, p };
  }
  return locals;
}

function animControls(pa, onChange) {
  const bar = tools(`<select id="seq">${pa.seqs.map((s, k) => `<option value="${k}">${esc(s.name)} (${s.frames}f @ ${s.rate})</option>`).join('')}</select>
    <button id="play" class="on">pause</button>
    <input id="fr" type="range" min="0" max="1" step="0.01" value="0" style="width:180px">
    <label id="frl">0</label>
    <label><input type="checkbox" id="conjAnim" checked> conj anim quats</label>
    <label>rot decode <select id="rotScale"><option value="sin:32767">sin(v·π/2)</option><option value="w:21845">w=√ /21845</option></select></label>
    <label><input type="checkbox" id="usePos" checked> anim positions</label>`);
  const h = new URLSearchParams(location.hash.slice(1));
  const st = { seq: +(h.get('seq') || 0), frame: +(h.get('frame') || 0), playing: !h.has('frame'), opt: {} };
  $('seq').value = st.seq;
  const read = () => {
    st.opt = { conjAnim: $('conjAnim').checked, rotScale: $('rotScale').value, usePos: $('usePos').checked };
    onChange(st);
  };
  $('seq').onchange = () => { st.seq = +$('seq').value; st.frame = 0; read(); };
  $('play').onclick = () => { st.playing = !st.playing; $('play').textContent = st.playing ? 'pause' : 'play'; $('play').classList.toggle('on', st.playing); };
  $('fr').oninput = () => { st.playing = false; $('play').textContent = 'play'; st.frame = +$('fr').value * Math.max(0, pa.seqs[st.seq].frames - 1); };
  for (const id of ['conjAnim', 'rotScale', 'usePos']) $(id).onchange = read;
  st.tick = (dt) => {
    const s = pa.seqs[st.seq];
    if (!s) return;
    if (st.playing && s.frames > 1) st.frame = (st.frame + dt * (s.rate || 30)) % (s.frames - 1);
    $('fr').value = s.frames > 1 ? st.frame / (s.frames - 1) : 0;
    $('frl').textContent = st.frame.toFixed(1);
  };
  read();
  return { st, bar };
}

// ---------------------------------------------------------------- mesh view

async function viewMesh(p, i) {
  const m = await api('mesh', { p, i });
  const ctx = make3d();
  const root = new THREE.Group();
  ctx.scene.add(root);
  const bar = tools(`<label><input type="checkbox" id="conjBind" checked> conj bind quats</label>
    <label><input type="checkbox" id="useLocal" checked> skin from LocalPoints</label>
    <label><input type="checkbox" id="bindPose"> bind pose</label>
    <label><input type="checkbox" id="showBones" checked> bones</label>
    <label><input type="checkbox" id="wire"> wire</label>
    <label id="info"></label>`);

  let points, wedges, uvScale = 1;
  if (m.kind === 'skeletal') {
    points = m.points;
    if (m.ext_wedges.length) wedges = m.ext_wedges;
    else { wedges = m.wedges; uvScale = 1 / 256; }
  } else {
    points = m.frames.slice(0, m.frame_verts * 3);
    wedges = m.wedges;
    uvScale = 1 / 256;
  }
  $('info').textContent = `${points.length / 3} points · ${wedges.length / 3} wedges · ${m.faces.length / 4} faces · ${(m.bones || []).length} bones`;

  // One geometry per material; vertices reference points so they can be re-skinned.
  const groups = new Map();
  for (let f = 0; f < m.faces.length; f += 4) {
    const mat = m.faces[f + 3];
    if (!groups.has(mat)) groups.set(mat, []);
    groups.get(mat).push(m.faces[f], m.faces[f + 1], m.faces[f + 2]);
  }
  const meshes = [];
  for (const [mat, ws] of groups) {
    const pos = new Float32Array(ws.length * 3), uv = new Float32Array(ws.length * 2), src = new Int32Array(ws.length);
    ws.forEach((w, k) => { src[k] = wedges[w * 3]; uv[k * 2] = wedges[w * 3 + 1] * uvScale; uv[k * 2 + 1] = wedges[w * 3 + 2] * uvScale; });
    const geo = new THREE.BufferGeometry();
    geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
    geo.setAttribute('uv', new THREE.BufferAttribute(uv, 2));
    const matInfo = m.materials[mat];
    const texRef = matInfo ? m.textures[matInfo[0]] : m.textures[mat];
    const map = textureFor(texRef);
    const material = new THREE.MeshBasicMaterial({ map, color: map ? 0xffffff : 0x9aa4b0, side: THREE.DoubleSide, alphaTest: 0.5 });
    const mesh = new THREE.Mesh(geo, material);
    root.add(mesh);
    meshes.push({ mesh, src, pos });
  }
  const writePositions = (P) => {
    for (const { mesh, src, pos } of meshes) {
      for (let k = 0; k < src.length; k++) {
        const v = ue(P[src[k] * 3], P[src[k] * 3 + 1], P[src[k] * 3 + 2]);
        pos[k * 3] = v[0]; pos[k * 3 + 1] = v[1]; pos[k * 3 + 2] = v[2];
      }
      mesh.geometry.attributes.position.needsUpdate = true;
      mesh.geometry.computeBoundingSphere();
    }
  };
  writePositions(points);
  const box = new THREE.Box3().setFromObject(root);
  frame(ctx, box);
  $('wire').onchange = () => meshes.forEach(({ mesh }) => { mesh.material.wireframe = $('wire').checked; });

  if (m.kind !== 'skeletal') return;

  // Influences: bone_weight_idx[b] = [first, count, ...] into bone_weights (point, weight/65535).
  // LocalPoints has one entry per BoneWeight (verified on skHarryDiaryMesh): the influence's
  // position in bone space, so skinning needs no inverse bind matrices.
  const infl = [];
  const hasLocal = m.local_points.length / 3 === m.bone_weights.length / 2;
  if (!hasLocal) $('useLocal').checked = false;
  if (m.bone_weight_idx.length === m.bones.length) {
    m.bone_weight_idx.forEach(([first, count], b) => {
      for (let k = first; k < first + count; k++) {
        const pt = m.bone_weights[k * 2], w = m.bone_weights[k * 2 + 1] / 65535;
        (infl[pt] = infl[pt] || []).push([b, w, k]);
      }
    });
  } else {
    bar.insertAdjacentHTML('beforeend', `<label class="err">bone_weight_idx (${m.bone_weight_idx.length}) ≠ bones (${m.bones.length})</label>`);
  }
  const parents = m.bones.map((b) => b.parent);
  const bindLocals = () => m.bones.map((b) => boneMatrix(b.q, b.p, $('conjBind').checked));
  let bindWorld = worldMatrices(parents, bindLocals());
  let bindInv = bindWorld.map((w) => w.clone().invert());

  const bones = lineSegments(0xffd24d);
  root.add(bones);
  const drawBones = (world) => {
    const arr = [];
    world.forEach((w, b) => {
      const par = parents[b];
      if (b === 0 || par === b) return;
      const a = new THREE.Vector3().setFromMatrixPosition(w), c = new THREE.Vector3().setFromMatrixPosition(world[par]);
      arr.push(...ue(a.x, a.y, a.z), ...ue(c.x, c.y, c.z));
    });
    bones.geometry.setAttribute('position', new THREE.Float32BufferAttribute(arr, 3));
    bones.visible = $('showBones').checked;
  };
  const P = new Float32Array(points.length);
  const skinPoints = (world) => {
    const useLocal = hasLocal && $('useLocal').checked;
    const skin = useLocal ? world : world.map((w, b) => w.clone().multiply(bindInv[b]));
    const v = new THREE.Vector3(), acc = new THREE.Vector3();
    for (let k = 0; k < points.length / 3; k++) {
      const inf = infl[k];
      if (!inf) { P[k * 3] = points[k * 3]; P[k * 3 + 1] = points[k * 3 + 1]; P[k * 3 + 2] = points[k * 3 + 2]; continue; }
      acc.set(0, 0, 0);
      for (const [b, w, wk] of inf) {
        const src = useLocal ? m.local_points : points, o = useLocal ? wk * 3 : k * 3;
        v.set(src[o], src[o + 1], src[o + 2]).applyMatrix4(skin[b]);
        acc.addScaledVector(v, w);
      }
      P[k * 3] = acc.x; P[k * 3 + 1] = acc.y; P[k * 3 + 2] = acc.z;
    }
    writePositions(P);
  };
  drawBones(bindWorld);
  skinPoints(bindWorld);
  const refreshBind = () => { skinPoints(bindWorld); drawBones(bindWorld); };
  $('useLocal').onchange = refreshBind;
  $('conjBind').onchange = () => { bindWorld = worldMatrices(parents, bindLocals()); bindInv = bindWorld.map((w) => w.clone().invert()); refreshBind(); };

  if (!m.default_animation || m.default_animation.p === undefined) {
    bar.insertAdjacentHTML('beforeend', '<label>no default animation</label>');
    return;
  }
  const pa0 = await api('anim', { p: m.default_animation.p, i: m.default_animation.i });
  bar.insertAdjacentHTML('beforeend', `<label>anim: <a id="animlink">${esc(m.default_animation.name)}</a></label>`);
  $('animlink').onclick = () => { location.hash = q({ p: m.default_animation.p, i: m.default_animation.i }); };
  const animToMesh = pa0.bones.map((b) => m.bones.findIndex((mb) => mb.name.toLowerCase() === b.name.toLowerCase()));
  let pa = null;
  const { st } = animControls(pa0, (s) => { pa = prepareAnim(pa0, s.opt); });
  let last = 0;
  ctx.onFrame = (t) => {
    const dt = last ? t - last : 0;
    last = t;
    st.tick(dt);
    if (!pa.moves[st.seq] || $('bindPose').checked) { skinPoints(bindWorld); drawBones(bindWorld); return; }
    const al = animLocals(pa, st.seq, st.frame, st.opt);
    const locals = m.bones.map((b, k) => {
      const ai = animToMesh.indexOf(k);
      const l = ai >= 0 ? al[ai] : null;
      if (!l) return boneMatrix(b.q, b.p, $('conjBind').checked);
      const pos = st.opt.usePos && pa.moves[st.seq].tracks.some((t) => t.bone === ai && t.poss.length) ? l.p : new THREE.Vector3(...b.p);
      return new THREE.Matrix4().compose(pos, l.q.normalize(), new THREE.Vector3(1, 1, 1));
    });
    const world = worldMatrices(parents, locals);
    skinPoints(world);
    drawBones(world);
  };
}

// ---------------------------------------------------------------- animation-only view (skeleton from keys)

async function viewAnim(p, i) {
  const a = await api('anim', { p, i });
  const ctx = make3d();
  const lines = lineSegments(0xffd24d);
  ctx.scene.add(lines);
  const dots = new THREE.Points(new THREE.BufferGeometry(), new THREE.PointsMaterial({ color: 0x6cb2ff, size: 4, sizeAttenuation: false }));
  ctx.scene.add(dots);
  let pa = null;
  const { st } = animControls(a, (s) => { pa = prepareAnim(a, s.opt); });
  const parents = a.bones.map((b) => b.parent);
  let framed = false, last = 0;
  ctx.onFrame = (t) => {
    const dt = last ? t - last : 0;
    last = t;
    st.tick(dt);
    if (!pa.moves[st.seq]) return;
    const al = animLocals(pa, st.seq, st.frame, st.opt);
    const locals = al.map((l) => l ? new THREE.Matrix4().compose(st.opt.usePos ? l.p : new THREE.Vector3(), l.q.normalize(), new THREE.Vector3(1, 1, 1)) : new THREE.Matrix4());
    const world = worldMatrices(parents, locals);
    const seg = [], pts = [];
    world.forEach((w, b) => {
      const v = new THREE.Vector3().setFromMatrixPosition(w);
      pts.push(...ue(v.x, v.y, v.z));
      if (b > 0 && parents[b] !== b) {
        const c = new THREE.Vector3().setFromMatrixPosition(world[parents[b]]);
        seg.push(...ue(v.x, v.y, v.z), ...ue(c.x, c.y, c.z));
      }
    });
    lines.geometry.setAttribute('position', new THREE.Float32BufferAttribute(seg, 3));
    dots.geometry.setAttribute('position', new THREE.Float32BufferAttribute(pts, 3));
    if (!framed) { dots.geometry.computeBoundingBox(); frame(ctx, dots.geometry.boundingBox.clone().expandByScalar(10)); framed = true; }
  };
}

// ---------------------------------------------------------------- BSP / level view

async function viewModel(p, i) {
  const m = await api('model', { p, i });
  const ctx = make3d();
  const root = new THREE.Group();
  ctx.scene.add(root);
  tools(`<label><input type="checkbox" id="invis"> show invisible</label>
    <label><input type="checkbox" id="actors" checked> actors</label>
    <label><input type="checkbox" id="wire"> wire</label>
    <label><input type="checkbox" id="twoSided"> two-sided</label>
    <label>${m.polys.length} polys · ${m.textures.length} textures${m.actors ? ` · ${m.actors.length} actors` : ''}</label>`);
  const byTex = new Map();
  for (const poly of m.polys) {
    const key = `${poly.t}|${poly.f & m.invisible_flag ? 1 : 0}`;
    if (!byTex.has(key)) byTex.set(key, []);
    byTex.get(key).push(poly);
  }
  const meshes = [];
  for (const [key, polys] of byTex) {
    const [t, invisible] = key.split('|').map(Number);
    const pos = [], uv = [];
    for (const poly of polys) {
      const n = poly.v.length / 3;
      for (let k = 1; k + 1 < n; k++) {
        for (const j of [0, k, k + 1]) {
          pos.push(...ue(poly.v[j * 3], poly.v[j * 3 + 1], poly.v[j * 3 + 2]));
          uv.push(poly.uv[j * 2], poly.uv[j * 2 + 1]);
        }
      }
    }
    const geo = new THREE.BufferGeometry();
    geo.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3));
    const uvAttr = new THREE.Float32BufferAttribute(uv, 2);
    geo.setAttribute('uv', uvAttr);
    const map = textureFor(m.textures[t], (w, h) => {
      for (let k = 0; k < uv.length; k += 2) { uvAttr.array[k] = uv[k] / w; uvAttr.array[k + 1] = uv[k + 1] / h; }
      uvAttr.needsUpdate = true;
    });
    // BSP faces point into the playable space; the Y/Z swap flips winding, hence BackSide.
    const mesh = new THREE.Mesh(geo, new THREE.MeshBasicMaterial({ map, color: map ? 0xffffff : 0x7a828c, side: THREE.BackSide, alphaTest: 0.5 }));
    mesh.visible = !invisible;
    mesh.userData.invisible = !!invisible;
    root.add(mesh);
    meshes.push(mesh);
  }
  const box = new THREE.Box3().setFromObject(root);
  if (m.actors) {
    const pts = m.actors.flatMap((a) => ue(...a.loc));
    const g = new THREE.BufferGeometry();
    g.setAttribute('position', new THREE.Float32BufferAttribute(pts, 3));
    const dots = new THREE.Points(g, new THREE.PointsMaterial({ color: 0xff5ab4, size: 5, sizeAttenuation: false }));
    root.add(dots);
    $('actors').onchange = () => { dots.visible = $('actors').checked; };
  }
  frame(ctx, box.isEmpty() ? new THREE.Box3(new THREE.Vector3(-512, -512, -512), new THREE.Vector3(512, 512, 512)) : box);
  $('invis').onchange = () => meshes.forEach((me) => { if (me.userData.invisible) me.visible = $('invis').checked; });
  $('wire').onchange = () => meshes.forEach((me) => { me.material.wireframe = $('wire').checked; });
  $('twoSided').onchange = () => meshes.forEach((me) => { me.material.side = $('twoSided').checked ? THREE.DoubleSide : THREE.BackSide; me.material.needsUpdate = true; });
}

init();

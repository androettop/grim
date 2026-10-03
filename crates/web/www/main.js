// Unpacks the disc in a worker, keeps the files in IndexedDB so the next visit skips the disc,
// and starts the game at Privet Drive.
import init, { add_file, play } from './pkg/grim_web.js';

const $ = (id) => document.getElementById(id);
const status = (text) => { $('status').textContent = text; };

function store() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open('grim', 1);
    req.onupgradeneeded = () => req.result.createObjectStore('files');
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function done(tx) {
  return new Promise((resolve, reject) => { tx.oncomplete = resolve; tx.onerror = () => reject(tx.error); });
}

async function cached(db) {
  const tx = db.transaction('files');
  const req = tx.objectStore('files').getAllKeys();
  await done(tx);
  return req.result;
}

function unpack(file, db) {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
    const files = [];
    worker.onmessage = async ({ data: m }) => {
      if (m.languages) {
        const language = m.languages.length === 1 ? m.languages[0]
          : prompt(`This disc has no English dialog. Language (${m.languages.join(', ')}):`, m.languages[0]);
        worker.postMessage({ file, language });
      } else if (m.path) {
        files.push([m.path, m.data]);
        status(`Unpacking the disc: ${m.done} of ${m.total} files`);
      } else if (m.error) {
        worker.terminate();
        reject(new Error(m.error));
      } else if (m.finished) {
        worker.terminate();
        status('Keeping the game in this browser…');
        const tx = db.transaction('files', 'readwrite');
        tx.objectStore('files').clear();
        for (const [path, data] of files) tx.objectStore('files').put(data, path);
        await done(tx).catch((e) => console.warn('not cached:', e));
        resolve(files);
      }
    };
    worker.postMessage({ file });
  });
}

async function load(db) {
  const tx = db.transaction('files');
  const os = tx.objectStore('files');
  const keys = os.getAllKeys(), values = os.getAll();
  await done(tx);
  return keys.result.map((k, i) => [k, values.result[i]]);
}

async function start(files) {
  status('Loading the level…');
  await new Promise((r) => setTimeout(r, 30));
  for (const [path, data] of files) add_file(path, new Uint8Array(data));
  files.length = 0;
  document.body.classList.add('playing');
  $('grim').focus();
  play('PrivetDr', new URLSearchParams(location.search).has('debug'));
}

async function main() {
  await init();
  const db = await store();
  const fail = (e) => status(`Error: ${e.message || e}`);
  if ((await cached(db)).length) {
    $('play').hidden = false;
    status('The game is already in this browser: press Play, or pick another disc.');
    $('play').onclick = () => load(db).then(start).catch(fail);
  }
  $('file').onchange = () => {
    const file = $('file').files[0];
    if (file) unpack(file, db).then(start).catch(fail);
  };
}

main().catch((e) => status(`Error: ${e.message || e}`));

#!/usr/bin/env node
/**
 * Does the browser's WebCrypto HKDF derive the same bytes Python's
 * `pcmon.pairing.derive_key` does? (TASKS.md C5, the one item that was only
 * ever asserted, never proven.)
 *
 * It runs `deriveKeyHex` verbatim from `webstatic/app.js`, so a mismatch in the
 * parameter *strings* -- salt order, info label, bit length -- fails here
 * instead of failing silently in someone's browser.
 *
 * The proof is that the server accepts the key: /pair/claim derives its own key
 * from the same inputs, and the claimed key then has to open /api/font.
 *
 *   node tools/check_web_hkdf.mjs
 *   PCMON_WEBUI=https://127.0.0.1:7803 PCMON_PAIR_LOG=tmp/webui_live.out \
 *     node --use-system-ca tools/check_web_hkdf.mjs
 *
 * The dashboard prints `PAIR code=...` to stdout; when it runs under the
 * launcher that stream is redirected, so this reads the file instead.
 */
import { readFileSync } from 'node:fs';
import { webcrypto } from 'node:crypto';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const crypto = webcrypto;
const BASE = process.env.PCMON_WEBUI || 'https://127.0.0.1:7803';
const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const LOG = process.env.PCMON_PAIR_LOG
  ? resolve(ROOT, process.env.PCMON_PAIR_LOG)
  : resolve(ROOT, 'tmp', 'webui_live.out');

// ---- verbatim from webstatic/app.js ----------------------------------------
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
// ---------------------------------------------------------------------------

const post = async (path, body) => {
  const r = await fetch(BASE + path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return { status: r.status, json: await r.json().catch(() => null) };
};

let failed = 0;
const check = (name, ok, detail) => {
  console.log((ok ? 'ok   ' : 'FAIL ') + name + (detail ? '  (' + detail + ')' : ''));
  if (!ok) failed++;
};

try {
  const begin = await post('/pair/begin', { name: 'browser' });
  check('/pair/begin 200', begin.status === 200, 'status ' + begin.status);
  check('code is NOT in the response', !begin.json?.code);
  if (begin.status !== 200) throw new Error('cannot continue without a pending pair');

  // what the operator reads off the machine
  let code = null;
  for (const line of readFileSync(LOG, 'utf8').split(/\r?\n/)) {
    const m = /PAIR code=(\d{6}) client=(\S+)/.exec(line);
    if (m && m[2] === begin.json.client_id) code = m[1];
  }
  check('code reached the operator log', !!code, LOG);

  const clientNonce = bytesToHex(crypto.getRandomValues(new Uint8Array(16)));
  const key = await deriveKeyHex(code, begin.json.client_id, clientNonce,
                                 begin.json.daemon_nonce);
  console.log('     key', key);

  const claim = await post('/pair/claim', {
    pair_id: begin.json.pair_id, code, client_nonce: clientNonce,
  });
  check('/pair/claim 200 (server derived the same key)',
        claim.status === 200, 'status ' + claim.status);

  const ok = await fetch(BASE + '/api/font', {
    headers: { Authorization: 'Bearer ' + key },
  });
  check('claimed key opens /api/font', ok.status === 200, 'status ' + ok.status);

  const bad = await fetch(BASE + '/api/font', {
    headers: { Authorization: 'Bearer ' + '0'.repeat(32) },
  });
  check('wrong key is refused', bad.status === 401, 'status ' + bad.status);
} catch (err) {
  console.error('error:', err.message);
  failed++;
}

console.log(failed
  ? '\nWEB HKDF FAILED (' + failed + ' failing)'
  : '\nWEB HKDF OK - WebCrypto and Python derive the same key');
process.exit(failed ? 1 : 0);

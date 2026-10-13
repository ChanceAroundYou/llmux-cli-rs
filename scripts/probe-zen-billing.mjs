#!/usr/bin/env node
/**
 * probe-zen-billing.mjs — OpenCode Zen billing 独立探针（不进 llmux 主干）
 * 目标：验证 `_server?id=c83b78...` 的 JS 包装 `;0x...;((self.$R...["server-fn:..."]=[...])`
 *       能否解出 {customerID, balance}，与现行 `balance/1e8` 一致。
 * 用法：
 *   node scripts/probe-zen-billing.mjs --cookie 'auth=Fe26.2**...'        # 线上探测
 *   node scripts/probe-zen-billing.mjs --text ';0x...;((self.$R...))'    # 离线样本
 *   node scripts/probe-zen-billing.mjs --self-test                       # 内置用例
 */
const OC_BILLING_ID = 'c83b78a614689c38ebee981f9b39a8b377716db85c1fd7dbab604adc02d3313d';
const OC_WORKSPACES_ID = 'def39973159c7f0483d8793a822b8dbb10d067e12c65455fcb4608459ba0234f';

function extractBalancedJson(s) {
  const bytes = Buffer.from(s);
  let depth = 0, inStr = false, esc = false, end = null;
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i];
    if (esc) { esc = false; continue; }
    if (inStr) { if (b === 0x5c) esc = true; else if (b === 0x22) inStr = false; continue; }
    if (b === 0x22) { inStr = true; continue; }
    if (b === 0x5b || b === 0x7b) depth++;
    else if (b === 0x5d || b === 0x7d) { depth--; if (depth === 0) { end = i + 1; break; } }
  }
  if (end == null) return null;
  try { return JSON.parse(s.slice(0, end)); } catch { return null; }
}

function ocUnwrapPayload(text) {
  const anchor = text.indexOf('server-fn');
  if (anchor !== -1) {
    const after = text.slice(anchor);
    const rel = after.indexOf(']=');
    if (rel !== -1) {
      const cand = after.slice(rel + 2).trimStart();
      const p = cand.search(/[\[{]/);
      if (p !== -1) { const v = extractBalancedJson(cand.slice(p)); if (v) return v; }
    }
    const eq = after.indexOf('=');
    if (eq !== -1) {
      const cand = after.slice(eq + 1).trimStart();
      const p = cand.search(/[\[{]/);
      if (p !== -1) { const v = extractBalancedJson(cand.slice(p)); if (v) return v; }
    }
  }
  const br = text.indexOf('['), cu = text.indexOf('{');
  let start = -1;
  if (br !== -1 && cu !== -1) start = Math.min(br, cu);
  else if (br !== -1) start = br;
  else if (cu !== -1) start = cu;
  else return null;
  return extractBalancedJson(text.slice(start));
}

function findBillingBalance(v) {
  if (Array.isArray(v)) { for (const it of v) { const r = findBillingBalance(it); if (r != null) return r; } return null; }
  if (v && typeof v === 'object') {
    if (typeof v.customerID === 'string' && v.customerID && typeof v.balance === 'number') return v.balance;
    for (const k of Object.keys(v)) { const r = findBillingBalance(v[k]); if (r != null) return r; }
  }
  return null;
}
function findBalanceAny(v) {
  if (Array.isArray(v)) { for (const it of v) { const r = findBalanceAny(it); if (r != null) return r; } return null; }
  if (v && typeof v === 'object') {
    for (const k of ['balance','amount','walletBalance','wallet_balance']) if (typeof v[k] === 'number') return v[k];
    for (const k of Object.keys(v)) { const r = findBalanceAny(v[k]); if (r != null) return r; }
  }
  return null;
}
function scanBalanceText(text) {
  let pos = 0, best = null;
  while (true) {
    const idx = text.indexOf('balance', pos);
    if (idx === -1) break;
    const tail = text.slice(idx + 7);
    const colon = tail.indexOf(':'); if (colon === -1) { pos = idx + 7; continue; }
    const after = tail.slice(colon + 1).trimStart().replace(/^["'=\s]+/, '');
    const m = after.match(/^[-0-9.eE]+/);
    if (!m) { pos = idx + 7; continue; }
    const n = Number(m[0]); if (Number.isFinite(n) && Math.abs(n) > 1e-9) if (best == null || Math.abs(n) > Math.abs(best)) best = n;
    pos = idx + 7;
  }
  return best == null ? null : (Math.abs(best) > 1e5 ? best / 1e8 : best);
}

function parseBalance(text) {
  const unwrapped = ocUnwrapPayload(text);
  if (unwrapped) {
    const raw = findBillingBalance(unwrapped);
    if (raw != null) return { dollars: raw / 1e8, raw, mode: 'unwrap+billing', unwrapped };
    const any = findBalanceAny(unwrapped);
    if (any != null) return { dollars: Math.abs(any) > 1e5 ? any / 1e8 : any, raw: any, mode: 'unwrap+any', unwrapped };
  }
  try {
    const v = JSON.parse(text);
    const raw = findBillingBalance(v); if (raw != null) return { dollars: raw / 1e8, raw, mode: 'json+billing' };
    const any = findBalanceAny(v); if (any != null) return { dollars: Math.abs(any) > 1e5 ? any / 1e8 : any, raw: any, mode: 'json+any' };
  } catch {}
  const scanned = scanBalanceText(text);
  if (scanned != null) return { dollars: scanned, raw: scanned, mode: 'text-scan' };
  return null;
}

async function probeLive(cookie) {
  const headers = {
    cookie, 'x-server-id': OC_BILLING_ID, 'x-server-instance': `server-fn:${crypto.randomUUID()}`,
    'user-agent': 'Mozilla/5.0', origin: 'https://opencode.ai', referer: 'https://opencode.ai', accept: 'text/javascript, application/json;q=0.9, */*;q=0.8',
  };
  // 1) workspaces -> wrk ids
  let wsText;
  try {
    const r = await fetch(`https://opencode.ai/_server?id=${OC_WORKSPACES_ID}`, { headers: { ...headers, referer: 'https://opencode.ai' } });
    wsText = await r.text();
    console.log('[workspaces]', r.status, wsText.slice(0, 200).replace(/\n/g, ' '));
  } catch (e) { console.error('workspaces fetch failed', e.message); return; }
  const ids = [...wsText.matchAll(/wrk_[A-Za-z0-9]+/g)].map(m => m[0].slice(4));
  const unique = [...new Set(ids)];
  console.log('wrk ids:', unique.length ? unique : '(none)');
  if (!unique.length) {
    // wallet-only fallback: billing without ws_id
    try {
      const r = await fetch(`https://opencode.ai/_server?id=${OC_BILLING_ID}`, { headers });
      const t = await r.text();
      console.log('[billing no-ws]', r.status, t.slice(0, 300).replace(/\n/g,' '));
      const p = parseBalance(t);
      console.log('parse:', p ? `$${p.dollars.toFixed(2)} (${p.mode} raw=${p.raw})` : 'MISS');
    } catch (e) { console.error(e.message); }
    return;
  }
  for (const id of unique.slice(0, 3)) {
    const url = `https://opencode.ai/_server?id=${OC_BILLING_ID}&args=${encodeURIComponent(JSON.stringify([`wrk_${id}`]))}`;
    const h2 = { ...headers, referer: `https://opencode.ai/workspace/wrk_${id}` };
    const r = await fetch(url, { headers: h2 });
    const t = await r.text();
    console.log(`\n[billing wrk_${id}]`, r.status, t.slice(0, 400).replace(/\n/g,' '));
    const p = parseBalance(t);
    console.log('parse:', p ? `$${p.dollars.toFixed(2)} (${p.mode} raw=${p.raw})` : 'MISS');
    if (p) break;
  }
}

function selfTest() {
  const cases = [
    { name: 'plain json', text: JSON.stringify({ customerID: 'cus_1', balance: 108000000 }), want: 1.08 },
    { name: 'js wrapper', text: ';0x0000010f;((self.$R=self.$R||{})["server-fn:774b2a51-775d-4f67-9087-f4686472c9"]=[{"customerID":"cus_1","balance":250000000}])', want: 2.5 },
  ];
  let ok = 0;
  for (const c of cases) {
    const p = parseBalance(c.text);
    const pass = p && Math.abs(p.dollars - c.want) < 1e-6;
    console.log(`${pass ? '✓' : '✗'} ${c.name}:`, p ? `$${p.dollars} ${p.mode}` : 'MISS', pass ? '' : `(want $${c.want})`);
    if (pass) ok++;
  }
  console.log(`\n${ok}/${cases.length} passed`);
  process.exit(ok === cases.length ? 0 : 1);
}

const args = process.argv.slice(2);
if (args.includes('--self-test')) selfTest();
else if (args.includes('--text')) {
  const idx = args.indexOf('--text');
  const t = args[idx + 1] || '';
  const p = parseBalance(t);
  console.log(p ? `parsed $${p.dollars.toFixed(2)} raw=${p.raw} mode=${p.mode}` : 'MISS');
  if (p?.unwrapped) console.log(JSON.stringify(p.unwrapped).slice(0, 500));
} else if (args.includes('--cookie')) {
  const idx = args.indexOf('--cookie');
  const c = args[idx + 1] || '';
  if (!c) { console.error('need --cookie value'); process.exit(1); }
  const ck = c.includes('=') ? c : `auth=${c}`;
  probeLive(ck);
} else {
  console.log('Usage: node scripts/probe-zen-billing.mjs --self-test | --text "<js>" | --cookie "auth=Fe26..."');
}

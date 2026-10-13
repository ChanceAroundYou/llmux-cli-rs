#!/usr/bin/env node
/**
 * probe-deepseek-cookie.mjs — DeepSeek platform Cookie 独立探针（不进 llmux 主干）
 * 目标：验证 `.thumbcache_6b2e5483f9d858d7c661c5e276b6a6ae` 会话 Cookie 能否拉到站内余额/细分。
 * 用法：
 *   node scripts/probe-deepseek-cookie.mjs --cookie 'Wert; .thumbcache_6b2e5483f9d858d7c661c5e276b6a6ae=O24Gvoqt...'  # 完整 Cookie 串
 *   node scripts/probe-deepseek-cookie.mjs --cookie 'O24GvoqtXd/EDeJIV9TLf04wUAV/PSf5K4MdlP0SUsPmC5xY7QiVKZn8vYe9aNJTMAnczbmaMNZb9eZ5H8mnYg=='  # 裸 value
 *   node scripts/probe-deepseek-cookie.mjs --self-test
 */
const THUMB = '.thumbcache_6b2e5483f9d858d7c661c5e276b6a6ae';
const CANDIDATES = [
  'https://platform.deepseek.com/api/v0/users/current/balance',
  'https://platform.deepseek.com/api/v0/users/balance',
  'https://platform.deepseek.com/api/v0/balance',
];

function normalizeCookie(raw) {
  const t = raw.trim().replace(/^Cookie:\s*/i, '');
  if (!t) return '';
  if (t.includes('=')) return t;
  return `${THUMB}=${t}`;
}
function findInfos(v) {
  if (!v || typeof v !== 'object') return null;
  if (Array.isArray(v)) { for (const it of v) { const r = findInfos(it); if (r) return r; } return null; }
  if (Array.isArray(v.balance_infos)) return v.balance_infos;
  if (v.data && Array.isArray(v.data.balance_infos)) return v.data.balance_infos;
  for (const k of Object.keys(v)) { const r = findInfos(v[k]); if (r) return r; }
  return null;
}
function prettyBalance(infos) {
  for (const it of infos) {
    console.log(`  ${it.currency}: total=${it.total_balance} granted=${it.granted_balance} topped=${it.topped_up_balance}`);
  }
}

async function probe(cookieRaw) {
  const cookie = normalizeCookie(cookieRaw);
  if (!cookie) { console.error('empty cookie'); process.exit(1); }
  console.log('Cookie:', cookie.slice(0, 80) + (cookie.length > 80 ? '…' : ''));
  const ua = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36';
  let hit = false;
  for (const url of CANDIDATES) {
    console.log(`\nGET ${url}`);
    try {
      const r = await fetch(url, {
        headers: { cookie, accept: 'application/json', 'user-agent': ua, referer: 'https://platform.deepseek.com/', origin: 'https://platform.deepseek.com' },
      });
      const text = await r.text();
      console.log(`  HTTP ${r.status} ${r.statusText} body ${text.length}B`);
      console.log('  ', text.slice(0, 600).replace(/\n/g, ' '));
      if (!r.ok) continue;
      let v; try { v = JSON.parse(text); } catch { continue; }
      const infos = findInfos(v);
      if (infos) { console.log('  balance_infos:'); prettyBalance(infos); hit = true; break; }
      console.log('  (no balance_infos in this response)');
    } catch (e) {
      console.log('  fetch error:', e.message);
    }
  }
  if (!hit) console.log('\nNo candidate returned balance_infos — cookie may be expired or endpoint shifted. Try inspecting platform.deepseek.com Network for the real api/v0 path.');
}

function selfTest() {
  const raw = 'O24GvoqtXd/test%3D%3D';
  const norm = normalizeCookie(raw);
  const ok1 = norm === `${THUMB}=${raw}`;
  const raw2 = `${THUMB}=abc; foo=bar`;
  const norm2 = normalizeCookie(raw2);
  const ok2 = norm2 === raw2;
  console.log(`${ok1 ? '✓' : '✗'} bare value -> thumbcache=`);
  console.log(`${ok2 ? '✓' : '✗'} full cookie passthrough`);
  process.exit(ok1 && ok2 ? 0 : 1);
}

const args = process.argv.slice(2);
if (args.includes('--self-test')) selfTest();
else if (args.includes('--cookie')) {
  const i = args.indexOf('--cookie');
  const c = args[i + 1] || '';
  if (!c) { console.error('need --cookie value'); process.exit(1); }
  probe(c);
} else {
  console.log('Usage: node scripts/probe-deepseek-cookie.mjs --cookie "<cookie>" | --self-test');
}

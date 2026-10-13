#!/usr/bin/env node
/**
 * probe-zen-free.mjs — zen-free sidecar 自检（不进 llmux 主干）
 *
 * 跑三协议各一发 + 断言 usage 非零。风格对齐 scripts/probe-zen-billing.mjs。
 * 免费档有全局速率配额（FreeUsageLimitError），429 时按提示退避后重跑，不要
 * 误判为 sidecar 故障；真正的实现回归是 404/400 形状变化或空 usage。
 *
 * 用法（sidecar 起在默认 6446，本机或经 docker exec）：
 *   node scripts/probe-zen-free.mjs --key oc-... [--base http://127.0.0.1:6446]
 *   node scripts/probe-zen-free.mjs --self-test   # 不联网，纯断言解析逻辑
 */
const args = process.argv.slice(2);
function flag(name, def = null) {
  const i = args.indexOf(name);
  return i !== -1 && i + 1 < args.length ? args[i + 1] : def;
}

const BASE = (flag("--base") || process.env.ZEN_FREE_BASE || "http://127.0.0.1:6446").replace(/\/$/, "");
const KEY = flag("--key") || process.env.ZEN_API_KEY || "";

async function post(path, body, keyStyle = "bearer") {
  const headers = { "content-type": "application/json" };
  if (keyStyle === "bearer") headers.authorization = `Bearer ${KEY}`;
  else headers["x-api-key"] = KEY;
  const r = await fetch(`${BASE}${path}`, { method: "POST", headers, body: JSON.stringify(body) });
  const text = await r.text();
  let json = null;
  try { json = JSON.parse(text); } catch {}
  return { status: r.status, json, text: text.slice(0, 300) };
}

function isRateLimited(r) {
  return r.status === 429 ||
    /FreeUsageLimitError|rate limit/i.test(r.text);
}

/** 非零 usage 断言：三种协议的 usage 形状各不相同，逐个认。 */
function checkUsage(tag, r) {
  if (isRateLimited(r)) return { tag, pass: null, note: "429 rate-limited, retry later" };
  if (r.status !== 200 || !r.json) return { tag, pass: false, note: `HTTP ${r.status}: ${r.text}` };
  const u = r.json.usage || {};
  const pt = u.prompt_tokens ?? u.input_tokens ?? 0;
  const ct = u.completion_tokens ?? u.output_tokens ?? 0;
  const ok = Number(pt) > 0 && Number(ct) > 0;
  return { tag, pass: ok, note: ok ? `prompt/input=${pt} completion/output=${ct}` : `zero usage: ${JSON.stringify(u).slice(0, 120)}` };
}

function checkText(tag, r) {
  if (isRateLimited(r)) return { tag, pass: null, note: "429 rate-limited, retry later" };
  if (r.status !== 200 || !r.json) return { tag, pass: false, note: `HTTP ${r.status}: ${r.text}` };
  const texts = (r.json.content || []).filter((b) => b.type === "text").map((b) => b.text || "");
  const ok = texts.join("").trim().length > 0;
  return { tag, pass: ok, note: ok ? texts.join(" | ").slice(0, 80) : "empty text blocks" };
}

async function probeLive() {
  if (!KEY) { console.error("need --key oc-... (or ZEN_API_KEY env)"); process.exit(2); }
  const results = [];

  const health = await fetch(`${BASE}/health`).then((r) => r.json()).catch((e) => ({ error: e.message }));
  const healthOk = health.status === "ok" && health.models > 0;
  results.push({ tag: "health", pass: healthOk, note: healthOk ? `models=${health.models} free=${health.free}` : JSON.stringify(health).slice(0, 120) });

  const models = await fetch(`${BASE}/v1/models`).then((r) => r.json()).catch(() => null);
  const ids = (models?.data || []).map((m) => m.id);
  const hasNew = ids.includes("cat-jam-free") || ids.includes("mimo-v2.6-flash-free");
  results.push({ tag: "models/dynamic", pass: ids.length > 6 && hasNew, note: `${ids.length} ids, has post-v12 free=${hasNew}` });
  const chatModel = ids.find((id) => id === "big-pickle") || ids.find((id) => id.endsWith("-free") && !id.startsWith("muse-spark")) || "big-pickle";

  results.push(checkUsage("chat", await post("/v1/chat/completions", {
    model: chatModel, stream: false,
    messages: [{ role: "user", content: "Say exactly: hello world" }],
  })));
  results.push(checkText("anthropic", await post("/v1/messages", {
    model: chatModel, max_tokens: 64,
    messages: [{ role: "user", content: "Say exactly: hello world" }],
  }, "x-api-key")));
  const muse = ids.find((id) => id.startsWith("muse-spark")) || "muse-spark-1.3-contributor-free";
  results.push(checkUsage("responses", await post("/v1/responses", {
    model: muse, stream: false, input: "Say exactly: hello world",
  })));

  let ok = 0, fail = 0, skip = 0;
  for (const r of results) {
    const mark = r.pass === true ? "✓" : r.pass === null ? "○" : "✗";
    console.log(`${mark} ${r.tag}: ${r.note}`);
    if (r.pass === true) ok++; else if (r.pass === null) skip++; else fail++;
  }
  console.log(`\n${ok} passed, ${fail} failed, ${skip} rate-limited`);
  process.exit(fail ? 1 : 0);
}

function selfTest() {
  const cases = [
    { name: "chat usage", u: { prompt_tokens: 300, completion_tokens: 16 }, want: true },
    { name: "anthropic usage", u: { input_tokens: 300, output_tokens: 16 }, want: true },
    { name: "responses usage", u: { input_tokens: 561, output_tokens: 146, total_tokens: 707 }, want: true },
    { name: "zero usage fails", u: { prompt_tokens: 0, completion_tokens: 0 }, want: false },
  ];
  let ok = 0;
  for (const c of cases) {
    const pt = c.u.prompt_tokens ?? c.u.input_tokens ?? 0;
    const ct = c.u.completion_tokens ?? c.u.output_tokens ?? 0;
    const got = Number(pt) > 0 && Number(ct) > 0;
    const pass = got === c.want;
    console.log(`${pass ? "✓" : "✗"} ${c.name}`);
    if (pass) ok++;
  }
  console.log(`\n${ok}/${cases.length} passed`);
  process.exit(ok === cases.length ? 0 : 1);
}

if (args.includes("--self-test")) selfTest();
else probeLive();

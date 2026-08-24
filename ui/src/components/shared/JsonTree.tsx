import React, { useState } from 'react';
import { cn } from '@/lib/utils';

// Recursive collapsible JSON renderer for the log detail view. Non-JSON
// payloads (SSE streams) fall back to a plain pre-wrap block.

function JsonNode({ value, name, depth, allExpanded }: { value: any; name?: string; depth: number; allExpanded?: boolean }) {
  // allExpanded drives the initial state only (log detail remounts the tree via
  // key when 展开全部/收起全部 is clicked).
  const [open, setOpen] = useState(allExpanded ?? depth < 2);

  if (value === null || typeof value !== 'object') {
    let text = typeof value === 'string' ? JSON.stringify(value) : String(value);
    if (text.length > 8000) text = `${text.slice(0, 8000)}… (${value.length.toLocaleString()} chars)`;
    const color =
      typeof value === 'string' ? 'text-success'
      : typeof value === 'number' ? 'text-foreground'
      : typeof value === 'boolean' ? 'text-warning'
      : 'text-muted-foreground';
    return (
      <div className="pl-4">
        {name !== undefined && <span className="text-primary">{name}: </span>}
        <span className={cn(color, 'break-all')}>{text}</span>
      </div>
    );
  }

  const isArr = Array.isArray(value);
  const entries: [string, any][] = isArr
    ? value.map((v, i) => [String(i), v])
    : Object.entries(value as Record<string, any>);
  const openBr = isArr ? '[' : '{';
  const closeBr = isArr ? ']' : '}';
  const isEmpty = entries.length === 0;

  return (
    <div className="pl-4">
      <div className="flex items-start">
        <button
          type="button"
          onClick={() => setOpen(v => !v)}
          aria-label={open ? 'Collapse' : 'Expand'}
          className="w-4 shrink-0 text-muted-foreground/70 hover:text-foreground transition-colors"
        >
          {isEmpty ? '' : open ? '▾' : '▸'}
        </button>
        <div className="min-w-0">
          <span className="text-primary">{name !== undefined ? `${name}: ` : ''}</span>
          <span className="text-muted-foreground">{openBr}</span>
          {isEmpty && <span className="text-muted-foreground">{closeBr}</span>}
          {!isEmpty && !open && <span className="text-muted-foreground"> … {closeBr}</span>}
          {!isEmpty && open && (
            <>
              <div className="border-l border-border/40 ml-1.5">
                {entries.map(([k, v]) => (
                  <JsonNode key={k} value={v} name={k} depth={depth + 1} allExpanded={allExpanded} />
                ))}
              </div>
              <span className="text-muted-foreground">{closeBr}</span>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

// JSON.parse, falling back to SSE payloads ("data: {...}" lines, e.g. captured
// streaming responses) which render as an event array tree instead of raw text.
// Truncated JSON (16k capture cap cuts bodies mid-string, e.g. every large
// request) is repaired by re-parsing the longest valid prefix with closed braces.
function parseStructured(text: string): { value: any; truncated?: boolean } | null {
  try {
    return { value: JSON.parse(text) };
  } catch { /* not plain JSON — try SSE / truncated prefix */ }

  const events: any[] = [];
  let sawData = false;
  for (const rawLine of text.split('\n')) {
    const line = rawLine.trim();
    if (!line.startsWith('data:')) continue;
    sawData = true;
    const payload = line.slice(5).trim();
    if (!payload) continue;
    if (payload === '[DONE]') { events.push('[DONE]'); continue; }
    try { events.push(JSON.parse(payload)); } catch { events.push(payload); }
  }
  if (sawData) return { value: events };

  // Truncated-JSON repair: scan for safe cut points (after , { [ outside
  // strings, plus value-string starts after ':'), track open containers, try
  // the longest prefix + closers. A cut inside a giant string value (typical
  // 16k capture) degrades that value to an empty string so the structure
  // around it still renders as a tree.
  const candidates: { cut: number; closers: string; close: string }[] = [];
  let inStr = false;
  let escaped = false;
  const stack: string[] = [];
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (inStr) {
      if (escaped) escaped = false;
      else if (ch === '\\') escaped = true;
      else if (ch === '"') inStr = false;
      continue;
    }
    if (ch === '"') { inStr = true; continue; }
    if (ch === '{') stack.push('}');
    else if (ch === '[') stack.push(']');
    else if (ch === '}' || ch === ']') stack.pop();
    else if (ch === ',' || ch === '{' || ch === '[') {
      candidates.push({ cut: i + 1, closers: [...stack].reverse().join(''), close: '' });
    } else if (ch === ':' && text[i + 1] === '"') {
      // value-string start: degrade that value to an empty string
      candidates.push({ cut: i + 1, closers: [...stack].reverse().join(''), close: '""' });
    }
  }
  // Final candidate: keep the whole truncated text. If the cut lands inside a
  // string (typical 16k capture of a giant message body) close that string and
  // the open containers — the truncated content itself stays visible.
  candidates.push({ cut: text.length, closers: [...stack].reverse().join(''), close: '"' });
  // Strip a trailing partial escape (e.g. "..."\u12 or "..."\) before closing.
  const stripPartialEscape = (s: string) => s.replace(/\\u[0-9a-fA-F]{0,3}$/, '').replace(/\\$/, '');
  for (let c = candidates.length - 1; c >= 0; c--) {
    const { cut, closers, close } = candidates[c];
    let prefix = text.slice(0, cut).trimEnd().replace(/,$/, '');
    if (close) prefix = `${stripPartialEscape(prefix)}${close}`;
    try {
      return { value: JSON.parse(prefix + closers), truncated: true };
    } catch { /* try earlier cut point */ }
  }
  return null;
}

export function JsonView({ text, className, allExpanded, truncatedNotice }: { text: string; className?: string; allExpanded?: boolean; truncatedNotice?: React.ReactNode }) {
  const trimmed = text.trim();
  if (!trimmed) return <span className="text-xs text-muted-foreground">—</span>;
  const structured = parseStructured(trimmed);
  if (structured) {
    return (
      <div className={cn('text-xs font-mono text-foreground/90', className)}>
        <JsonNode value={structured.value} depth={0} allExpanded={allExpanded} />
        {structured.truncated && <div className="mt-1 text-[10px] text-muted-foreground">{truncatedNotice || '…'}</div>}
      </div>
    );
  }
  return (
    <pre className={cn('text-xs font-mono whitespace-pre-wrap break-all text-foreground/90', className)}>
      {text}
    </pre>
  );
}

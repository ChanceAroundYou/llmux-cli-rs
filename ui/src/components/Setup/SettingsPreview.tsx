import React from 'react';
import { RotateCcw, FileJson } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { cn } from './utils';
import { CopyButton } from '../CopyButton';

const HIGHLIGHT_KEYS = new Set([
  'ANTHROPIC_BASE_URL', 'ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN',
  'ANTHROPIC_DEFAULT_OPUS_MODEL', 'ANTHROPIC_DEFAULT_SONNET_MODEL', 'ANTHROPIC_DEFAULT_HAIKU_MODEL',
]);

// 将 settings 对象扁平化为 "key.subkey": value 的行列表，便于 diff
function flattenToLines(obj: Record<string, any>, prefix = ''): Array<{ key: string; line: string }> {
  const result: Array<{ key: string; line: string }> = [];
  for (const [k, v] of Object.entries(obj)) {
    const fullKey = prefix ? `${prefix}.${k}` : k;
    if (v !== null && typeof v === 'object' && !Array.isArray(v)) {
      result.push(...flattenToLines(v, fullKey));
    } else {
      result.push({ key: fullKey, line: `"${k}": ${JSON.stringify(v)}` });
    }
  }
  return result;
}

// 生成 diff 行：compared to current
type DiffLine = { type: 'unchanged' | 'removed' | 'added'; line: string; key: string };

function buildDiff(current: Record<string, any> | null, preview: Record<string, any>): DiffLine[] {
  const currentLines = current ? flattenToLines(current) : [];
  const previewLines = flattenToLines(preview);

  const currentMap = new Map(currentLines.map(l => [l.key, l.line]));
  const previewMap = new Map(previewLines.map(l => [l.key, l.line]));

  const allKeys = new Set([...currentMap.keys(), ...previewMap.keys()]);
  const result: DiffLine[] = [];

  for (const key of allKeys) {
    const cur = currentMap.get(key);
    const nxt = previewMap.get(key);
    if (cur === nxt) {
      result.push({ type: 'unchanged', line: cur!, key });
    } else {
      if (cur !== undefined) result.push({ type: 'removed', line: cur, key });
      if (nxt !== undefined) result.push({ type: 'added', line: nxt, key });
    }
  }

  return result;
}

function DiffView({ current, preview }: { current: Record<string, any> | null; preview: Record<string, any> }) {
  const lines = buildDiff(current, preview);
  const hasChanges = lines.some(l => l.type !== 'unchanged');

  return (
    <div className="text-[11px] font-mono leading-relaxed space-y-px overflow-x-auto">
      {!hasChanges && (
        <div className="text-muted-foreground/50 italic mb-1">无变更</div>
      )}
      {lines.map((l, i) => {
        const leafKey = l.key.split('.').pop() ?? l.key;
        const highlighted = HIGHLIGHT_KEYS.has(leafKey);
        return (
          <div
            key={i}
            className={cn(
              'px-2 rounded flex items-start gap-2 whitespace-nowrap',
              l.type === 'removed' && 'bg-red-500/10 text-red-400',
              l.type === 'added' && 'bg-green-500/10 text-green-400',
              l.type === 'unchanged' && (highlighted ? 'text-foreground/70' : 'text-muted-foreground/80'),
            )}
          >
            <span className="shrink-0 w-3 select-none">
              {l.type === 'removed' ? '−' : l.type === 'added' ? '+' : ' '}
            </span>
            <span className={cn(highlighted && l.type !== 'unchanged' && 'font-bold')}>{l.line}</span>
          </div>
        );
      })}
    </div>
  );
}

interface Props {
  settings: Record<string, any> | null;   // 当前文件内容
  preview: Record<string, any> | null;     // 将要写入的内容
  exists: boolean;
  loading: boolean;
  onRefresh: () => void;
}

export function SettingsPreview({ settings, preview, exists, loading, onRefresh }: Props) {
  const { t } = useTranslation();
  const [tab, setTab] = React.useState<'diff' | 'current'>('diff');

  // 有 preview 时默认显示 diff，没有则显示当前
  React.useEffect(() => {
    setTab(preview ? 'diff' : 'current');
  }, [!!preview]);

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-1 bg-muted/40 rounded-lg p-0.5">
          {preview && (
            <button
              onClick={() => setTab('diff')}
              className={cn(
                'px-2.5 py-1 rounded-md text-[11px] font-bold transition-all',
                tab === 'diff' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'
              )}
            >
              <FileJson size={11} className="inline mr-1" />
              Diff
            </button>
          )}
          <button
            onClick={() => setTab('current')}
            className={cn(
              'px-2.5 py-1 rounded-md text-[11px] font-bold transition-all',
              tab === 'current' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'
            )}
          >
            {exists ? '当前' : '预览'}
          </button>
        </div>

        <div className="flex items-center gap-2">
          {exists && <span className="text-[10px] text-muted-foreground font-mono">~/.claude/settings.json</span>}
          <button onClick={onRefresh} className="p-1 hover:bg-muted rounded-lg transition-colors" title={t('setup.refresh')}>
            <RotateCcw size={11} className="text-muted-foreground" />
          </button>
        </div>
      </div>

      <div className="rounded-xl border border-border bg-muted/20 overflow-hidden">
        <div className="flex items-center justify-between px-4 py-2 border-b border-border/40 bg-muted/10">
          <div className="flex gap-1.5">
            <div className="w-2 h-2 rounded-full bg-red-500/60" />
            <div className="w-2 h-2 rounded-full bg-yellow-500/60" />
            <div className="w-2 h-2 rounded-full bg-green-500/60" />
          </div>
          <div className="flex items-center gap-1.5">
            {tab === 'diff' && preview && (
              <span className="text-[10px] text-muted-foreground">变更预览</span>
            )}
            {tab === 'current' && (preview ?? settings) && (
              <CopyButton value={JSON.stringify(preview ?? settings, null, 2)} size={12} />
            )}
          </div>
        </div>

        <div className="p-4 max-h-[400px] overflow-y-auto">
          {loading ? (
            <div className="text-[11px] text-muted-foreground font-mono">{t('setup.loading')}</div>
          ) : tab === 'diff' && preview ? (
            <DiffView current={settings} preview={preview} />
          ) : (settings ?? preview) ? (
            <pre className="text-[11px] leading-relaxed font-mono text-foreground/80 whitespace-pre overflow-x-auto">
              {JSON.stringify(settings ?? preview, null, 2)}
            </pre>
          ) : (
            <div className="text-[11px] text-muted-foreground font-mono space-y-1">
              <div>{t('setup.noSettingsFile')}</div>
              <div className="text-muted-foreground/50"># {t('setup.noSettingsHint')}</div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

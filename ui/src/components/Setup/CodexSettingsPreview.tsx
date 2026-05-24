import React, { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { RotateCcw, FileJson } from 'lucide-react';
import { cn } from './utils';
import { CopyButton } from '../CopyButton';

interface Props {
  currentAuth: Record<string, any> | null;
  previewAuth: Record<string, any> | null;
  currentToml: string | null;
  previewToml: string | null;
  exists: boolean;
  loading: boolean;
  onRefresh: () => void;
}

type DiffLine = { type: 'unchanged' | 'removed' | 'added'; line: string };

function computeLineDiff(oldStr: string, newStr: string): DiffLine[] {
  const oldLines = oldStr.split('\n');
  const newLines = newStr.split('\n');
  const m = oldLines.length;
  const n = newLines.length;

  // LCS table
  const dp: number[][] = Array.from({ length: m + 1 }, () => new Array(n + 1).fill(0));
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      if (oldLines[i - 1] === newLines[j - 1]) {
        dp[i][j] = dp[i - 1][j - 1] + 1;
      } else {
        dp[i][j] = Math.max(dp[i - 1][j], dp[i][j - 1]);
      }
    }
  }

  // Backtrack
  const result: DiffLine[] = [];
  let i = m, j = n;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && oldLines[i - 1] === newLines[j - 1]) {
      result.push({ type: 'unchanged', line: oldLines[i - 1] });
      i--; j--;
    } else if (j > 0 && (i === 0 || dp[i][j - 1] >= dp[i - 1][j])) {
      result.push({ type: 'added', line: newLines[j - 1] });
      j--;
    } else {
      result.push({ type: 'removed', line: oldLines[i - 1] });
      i--;
    }
  }
  return result.reverse();
}

function DiffLines({ lines }: { lines: DiffLine[] }) {
  const hasChanges = lines.some(l => l.type !== 'unchanged');
  return (
    <div className="text-xs font-mono leading-relaxed space-y-px overflow-x-auto">
      {!hasChanges && (
        <div className="text-muted-foreground/50 italic mb-1">无变更</div>
      )}
      {lines.map((l, i) => (
        <div
          key={i}
          className={cn(
            'px-2 rounded flex items-start gap-2 whitespace-nowrap',
            l.type === 'removed' && 'bg-destructive/10 text-destructive',
            l.type === 'added' && 'bg-success/10 text-success',
            l.type === 'unchanged' && 'text-muted-foreground/80',
          )}
        >
          <span className="shrink-0 w-3 select-none">
            {l.type === 'removed' ? '−' : l.type === 'added' ? '+' : ' '}
          </span>
          <span>{l.line}</span>
        </div>
      ))}
    </div>
  );
}

function FileCard({
  title,
  currentContent,
  previewContent,
  isDiff,
  emptyText,
}: {
  title: string;
  currentContent: string | null;
  previewContent: string | null;
  isDiff: boolean;
  emptyText: string;
}) {
  const diffLines = useMemo(() => {
    if (!isDiff || !currentContent || !previewContent) return null;
    return computeLineDiff(currentContent, previewContent);
  }, [isDiff, currentContent, previewContent]);

  const displayContent = isDiff && previewContent ? previewContent : currentContent;

  return (
    <div className="rounded-xl border border-border bg-muted/20 overflow-hidden">
      <div className="flex items-center justify-between px-4 py-2 border-b border-border/40 bg-muted/10">
        <div className="flex items-center gap-2">
          <div className="flex gap-1">
            <div className="w-2 h-2 rounded-full bg-destructive/60" />
            <div className="w-2 h-2 rounded-full bg-warning/60" />
            <div className="w-2 h-2 rounded-full bg-success/60" />
          </div>
          <span className="text-[10px] font-bold text-muted-foreground uppercase tracking-widest">{title}</span>
        </div>
        <CopyButton value={displayContent ?? ''} size={12} />
      </div>
      <div className="p-3 max-h-[360px] overflow-y-auto">
        {diffLines ? (
          <DiffLines lines={diffLines} />
        ) : (
          <pre className="text-[10px] leading-relaxed font-mono text-foreground/70 whitespace-pre overflow-x-auto">
            {displayContent ?? emptyText}
          </pre>
        )}
      </div>
    </div>
  );
}

export function CodexSettingsPreview({
  currentAuth, previewAuth, currentToml, previewToml,
  exists, loading, onRefresh,
}: Props) {
  const { t } = useTranslation();
  const [tab, setTab] = React.useState<'diff' | 'current'>('diff');
  const hasPreview = !!(previewAuth || previewToml);

  React.useEffect(() => {
    setTab(hasPreview ? 'diff' : 'current');
  }, [hasPreview]);

  const isDiff = tab === 'diff' && hasPreview;
  const hasCurrent = exists && (!!currentAuth || !!currentToml);

  const authCurrent = currentAuth ? JSON.stringify(currentAuth, null, 2) : null;
  const authPreview = previewAuth ? JSON.stringify(previewAuth, null, 2) : null;
  const tomlCurrent = currentToml && String(currentToml).trim() ? currentToml : null;
  const tomlPreview = previewToml && String(previewToml).trim() ? previewToml : null;

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-1 bg-muted/40 rounded-lg p-0.5">
          {hasPreview && (
            <button
              onClick={() => setTab('diff')}
              className={cn(
                'px-2.5 py-1 rounded-md text-xs font-semibold transition-all',
                tab === 'diff' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground',
              )}
            >
              <FileJson size={11} className="inline mr-1" />
              Diff
            </button>
          )}
          <button
            onClick={() => setTab('current')}
            className={cn(
              'px-2.5 py-1 rounded-md text-xs font-semibold transition-all',
              tab === 'current' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground',
            )}
          >
            {hasCurrent ? '当前' : '预览'}
          </button>
        </div>
        <div className="flex items-center gap-2">
          {exists && <span className="text-xs text-muted-foreground font-mono">~/.codex/</span>}
          <button onClick={onRefresh} className="p-1 hover:bg-muted rounded-lg transition-colors" title={t('setup.refresh')}>
            <RotateCcw size={11} className="text-muted-foreground" />
          </button>
        </div>
      </div>

      {loading ? (
        <div className="text-xs text-muted-foreground italic">{t('setup.loading')}</div>
      ) : (
        <div className="space-y-2">
          <FileCard
            title="auth.json"
            currentContent={authCurrent}
            previewContent={authPreview}
            isDiff={isDiff}
            emptyText="— 空 —"
          />
          <FileCard
            title="config.toml"
            currentContent={tomlCurrent}
            previewContent={tomlPreview}
            isDiff={isDiff}
            emptyText="— 空 —"
          />
        </div>
      )}
    </div>
  );
}

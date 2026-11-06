import React, { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertCircle, CheckCircle2, ExternalLink, Hourglass, Loader2, LogIn } from 'lucide-react';
import { apiFetch } from '@/lib/api';
import { Dialog } from './Modal';
import { CopyButton } from './CopyButton';
import { Button } from '@/components/ui/button';

// 与后端 `routes/system/codex_device.rs::session_json` 一对一。
// 注意这里没有 device_auth_id：后端刻意不回传（见那边的注释）。
type SessionView = {
  session_id: string;
  status: 'pending' | 'authorizing' | 'completed' | 'failed';
  user_code: string;
  verification_url: string;
  expires_at_ms: number;
  remaining_secs: number;
  poll_count: number;
  message: string;
  identity?: { account_id?: string; email?: string | null; plan_type?: string | null } | null;
  models?: number | null;
};

// 后端按 ChatGPT 给的 interval（实测 5 秒）轮询上游；界面这边只看内存里的状态，
// 3 秒一次足够让人感觉是「实时」的，也不会把 /api 打成噪声。
const POLL_MS = 3000;

interface Props {
  isOpen: boolean;
  onClose: () => void;
  account: { id: number; alias: string } | null;
  /** 登录成功后通知外层刷新账户与模型列表。 */
  onSignedIn?: (account: { id: number; alias: string }) => void;
}

export const CodexLoginDialog: React.FC<Props> = ({ isOpen, onClose, account, onSignedIn }) => {
  const { t } = useTranslation();
  const [session, setSession] = useState<SessionView | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const sessionRef = useRef<SessionView | null>(null);
  const notifiedRef = useRef(false);

  useEffect(() => { sessionRef.current = session; }, [session]);

  // 关闭即收尾：还在等人授权的会话在这里取消（后端的轮询任务下一轮自己退出），
  // 状态清空，下次打开是干净的一次新登录。
  useEffect(() => {
    if (isOpen) return;
    const current = sessionRef.current;
    if (current && (current.status === 'pending' || current.status === 'authorizing')) {
      void apiFetch('/api/system/codex-device-cancel', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ session_id: current.session_id }),
      }).catch(() => undefined);
    }
    setSession(null);
    setStarting(false);
    setError(null);
    notifiedRef.current = false;
  }, [isOpen]);

  const startLogin = async () => {
    if (!account) return;
    setError(null);
    setStarting(true);
    try {
      const res = await apiFetch('/api/system/codex-device', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ account_id: account.id }),
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        setError(data.message || data.error || `HTTP ${res.status}`);
        setStarting(false);
        return;
      }
      setSession(data as SessionView);
      setNow(Date.now());
      setStarting(false);
    } catch (e: any) {
      setError(e?.message || 'network error');
      setStarting(false);
    }
  };

  // 只在还没出结果时轮；出结果的那一刻自动停（依赖里带 status，不需要额外的 flag）。
  useEffect(() => {
    const id = session?.session_id;
    if (!isOpen || !id) return;
    if (session!.status !== 'pending' && session!.status !== 'authorizing') return;
    const timer = window.setInterval(async () => {
      try {
        const res = await apiFetch(`/api/system/codex-device?session_id=${encodeURIComponent(id)}`);
        if (!res.ok) return; // 会话已被清掉：状态就停在最后一次已知的，让人关窗口重来
        const data = await res.json().catch(() => ({}));
        if (data && data.session_id) setSession(data as SessionView);
      } catch {
        // 网络抖动：下一轮再查
      }
      setNow(Date.now());
    }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [isOpen, session?.session_id, session?.status]);

  const completed = session?.status === 'completed';
  useEffect(() => {
    if (completed && account && !notifiedRef.current) {
      notifiedRef.current = true;
      onSignedIn?.(account);
    }
  }, [completed, account, onSignedIn]);

  const remainingSecs = session ? Math.max(0, Math.floor((session.expires_at_ms - now) / 1000)) : 0;
  const waiting = session?.status === 'pending' || session?.status === 'authorizing';

  return (
    <Dialog
      isOpen={isOpen}
      onClose={onClose}
      title={`${t('codex.login.title')} · ${account?.alias ?? ''}`}
      description={t('codex.login.description')}
      size="md"
      closeOnOverlay={!waiting}
      footer={
        <div className="flex items-center justify-between gap-2 w-full">
          <span className="text-[11px] text-muted-foreground min-w-0">{session ? t('codex.login.caution') : ''}</span>
          <div className="flex items-center gap-2 shrink-0">
            {waiting && (
              <Button variant="outline" size="sm" onClick={onClose}>
                {t('codex.login.cancel')}
              </Button>
            )}
            {(completed || session?.status === 'failed' || session) && (
              <Button variant={completed ? 'default' : 'outline'} size="sm" onClick={onClose}>
                {t('codex.login.close')}
              </Button>
            )}
          </div>
        </div>
      }
    >
      <div className="space-y-4">
        {error && (
          <div className="p-3 bg-destructive/10 border border-destructive/20 rounded-lg flex items-start gap-2 text-destructive text-xs">
            <AlertCircle size={14} className="shrink-0 mt-0.5" />
            <div className="min-w-0">
              <p className="font-medium">{t('codex.login.failed')}</p>
              <p className="mt-1 break-words">{error}</p>
            </div>
          </div>
        )}

        {!session && !starting && (
          <div className="space-y-3">
            <ol className="text-xs text-muted-foreground leading-relaxed">
              <li>{t('codex.login.stepHint')}</li>
            </ol>
            <Button size="sm" onClick={startLogin} disabled={!account}>
              <LogIn size={14} />
              {t('codex.login.start')}
            </Button>
          </div>
        )}

        {starting && (
          <div className="flex items-center gap-2 text-xs text-muted-foreground py-2">
            <Loader2 size={14} className="animate-spin" />
            {t('codex.login.starting')}
          </div>
        )}

        {session && (
          <div className="space-y-3">
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0">
                <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{t('codex.login.code')}</div>
                <div className="font-mono text-2xl font-semibold tracking-widest break-all">
                  {session.user_code}
                </div>
              </div>
              <div className="flex items-center gap-1 shrink-0">
                <CopyButton value={session.user_code} title={t('codex.login.copyCode')} />
                <Button variant="outline" size="sm" asChild>
                  <a href={session.verification_url} target="_blank" rel="noopener noreferrer">
                    <ExternalLink size={14} />
                    {t('codex.login.openPage')}
                  </a>
                </Button>
              </div>
            </div>

            {session.status === 'pending' && (
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                <Hourglass size={14} className="shrink-0" />
                <span className="min-w-0">
                  {t('codex.login.waiting', { poll: session.poll_count })}
                  {' · '}
                  {t('codex.login.expiresIn', { seconds: remainingSecs })}
                </span>
              </div>
            )}

            {session.status === 'authorizing' && (
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                <Loader2 size={14} className="animate-spin shrink-0" />
                {t('codex.login.authorizing')}
              </div>
            )}

            {session.status === 'failed' && (
              <div className="p-3 bg-destructive/10 border border-destructive/20 rounded-lg flex items-start gap-2 text-destructive text-xs">
                <AlertCircle size={14} className="shrink-0 mt-0.5" />
                <div className="min-w-0">
                  <p className="font-medium">{t('codex.login.failed')}</p>
                  <p className="mt-1 break-words">{session.message}</p>
                </div>
              </div>
            )}

            {session.status === 'completed' && (
              <div className="p-3 bg-success/10 border border-success/20 rounded-lg flex items-start gap-2 text-success text-xs">
                <CheckCircle2 size={14} className="shrink-0 mt-0.5" />
                <div className="min-w-0">
                  <p className="font-medium">{t('codex.login.completed')}</p>
                  <p className="mt-1 text-muted-foreground break-words">
                    {t('codex.login.identity', {
                      email: session.identity?.email ?? '—',
                      plan: session.identity?.plan_type ?? '—',
                      workspace: session.identity?.account_id ?? '—',
                    })}
                  </p>
                  {typeof session.models === 'number' && (
                    <p className="mt-1 text-muted-foreground">{t('codex.login.models', { count: session.models })}</p>
                  )}
                </div>
              </div>
            )}

            {session.status === 'failed' && (
              <Button variant="outline" size="sm" onClick={startLogin}>
                <LogIn size={14} />
                {t('codex.login.restart')}
              </Button>
            )}
          </div>
        )}
      </div>
    </Dialog>
  );
};

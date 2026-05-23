import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ExternalLink, Check, X, Loader2, Code2 } from 'lucide-react';
import { CopyButton } from './CopyButton';

interface Props {
  isOpen: boolean;
  onClose: () => void;
  provider: 'openai' | 'claude';
}

export default function WebLoginWizard({ isOpen, onClose, provider }: Props) {
  const { t } = useTranslation();
  const [isSyncing, setIsSyncing] = useState(false);

  if (!isOpen) return null;

  const config = {
    openai: {
      url: 'https://chatgpt.com',
      tokenName: '__Secure-next-auth.session-token'
    },
    claude: {
      url: 'https://claude.ai',
      tokenName: 'sessionKey'
    }
  };

  const script = `
(async function() {
  console.log('%c${t('auth.script.start')}', 'color: #3b82f6; font-weight: bold; font-size: 14px;');
  let token = '';
  let provider = '${provider}';

  if (provider === 'openai') {
    try {
      const res = await fetch('/api/auth/session');
      const data = await res.json();
      token = data.accessToken;
      if (token) console.log('${t('auth.script.tokenFound', { provider: 'OpenAI' })}');
    } catch (e) {
      console.warn('${t('auth.script.tokenCookie')}');
      token = document.cookie.split('; ').find(row => row.startsWith('__Secure-next-auth.session-token='))?.split('=')[1];
    }
  } else if (provider === 'claude') {
    token = document.cookie.split('; ').find(row => row.startsWith('sessionKey='))?.split('=')[1];
    
    if (!token) {
      console.log('${t('auth.script.interceptor').replace(/'/g, "\\'")}');
      alert('${t('auth.script.interceptor').replace(/'/g, "\\'")}');
      
      const oldFetch = window.fetch;
      window.fetch = function() {
        const authHeader = arguments[1]?.headers?.['cookie'] || '';
        const match = authHeader.match(/sessionKey=([^;]+)/);
        if (match) {
          console.log('✅ Intercepted sessionKey via Fetch!');
          fetch('http://localhost:25975/api/auth/web-session', {
             method: 'POST',
             mode: 'cors',
             headers: { 'Content-Type': 'application/json' },
             body: JSON.stringify({ provider: 'claude', token: match[1], alias: 'Web-Claude-Auto' })
          });
        }
        return oldFetch.apply(this, arguments);
      };
      return;
    }
  }

  if (!token) {
    console.error('${t('auth.script.notFound')}');
    alert('${t('auth.script.notFound')}');
    return;
  }

  console.log('${t('auth.script.syncing')}');
  fetch('http://localhost:25975/api/auth/web-session', {
    method: 'POST',
    mode: 'cors',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      provider: provider,
      token: token,
      alias: 'Web-' + provider + '-' + new Date().getHours() + ':' + new Date().getMinutes()
    })
  }).then(r => r.json()).then(data => {
    if (data.success) {
      console.log('%c${t('auth.script.success')}', 'color: #10b981; font-weight: bold;');
      alert('${t('auth.script.success')}');
    } else {
      alert('${t('auth.script.error')}' + data.error);
    }
  }).catch(err => alert('${t('auth.script.error')}' + err.message));
})();
  `.trim();

  const handleCopyFinished = () => {
    setIsSyncing(true);
    setTimeout(() => {
      setIsSyncing(false);
    }, 1500);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4">
      <div className="bg-card border rounded-2xl shadow-2xl max-w-2xl w-full overflow-hidden animate-in zoom-in-95 duration-200">
        <div className="flex justify-between items-center px-6 py-4 border-b bg-muted/30">
          <div className="flex items-center gap-2">
            <div className="p-1.5 bg-primary/10 text-primary rounded-lg">
              <Code2 size={18} />
            </div>
            <h2 className="text-xl font-bold">{t('auth.webLogin')}</h2>
          </div>
          <button onClick={onClose} className="p-1 hover:bg-muted rounded-full transition-colors text-muted-foreground hover:text-foreground">
            <X size={20} />
          </button>
        </div>
        
        <div className="p-6 space-y-6 overflow-y-auto max-h-[85vh]">
          <p className="text-sm text-muted-foreground leading-relaxed">
            {t('auth.webSubtitle')}
          </p>

          <div className="space-y-6">
            <div className="flex items-start gap-4">
              <div className="flex-shrink-0 bg-primary/10 text-primary w-7 h-7 flex items-center justify-center rounded-full font-bold text-sm">1</div>
              <div className="flex-1">
                <p className="font-medium mb-2">{t('auth.step1')}</p>
                <a 
                  href={config[provider].url} 
                  target="_blank" 
                  rel="noreferrer"
                  className="inline-flex items-center gap-2 bg-primary text-primary-foreground hover:opacity-90 px-4 py-2 rounded-lg text-sm font-medium transition-all shadow-sm"
                >
                  {t('auth.step1Btn', { provider: provider.toUpperCase() })}
                  <ExternalLink size={14} />
                </a>
              </div>
            </div>

            <div className="flex items-start gap-4">
              <div className="flex-shrink-0 bg-primary/10 text-primary w-7 h-7 flex items-center justify-center rounded-full font-bold text-sm">2</div>
              <div className="flex-1 pt-1">
                <p className="font-medium">{t('auth.step2')}</p>
              </div>
            </div>

            <div className="flex items-start gap-4">
              <div className="flex-shrink-0 bg-primary/10 text-primary w-7 h-7 flex items-center justify-center rounded-full font-bold text-sm">3</div>
              <div className="flex-1">
                <p className="font-medium mb-3">{t('auth.step3')}</p>
                
                <div className="relative group bg-slate-950 rounded-xl border border-white/10 overflow-hidden">
                  <div className="flex items-center justify-between px-4 py-2 bg-white/5 border-b border-white/5 text-[10px] uppercase tracking-widest text-white/40 font-bold">
                    <span>Javascript</span>
                    <span>Session Capturer</span>
                  </div>
                  
                  <pre className="p-5 text-[11px] font-mono text-blue-100/80 overflow-auto whitespace-pre h-64 scrollbar-thin scrollbar-thumb-white/20">
                    {script}
                  </pre>

                  <div className="absolute top-12 right-6">
                    <CopyButton 
                      value={script} 
                      size={14} 
                      className="bg-white/10 hover:bg-white/20 text-white rounded-lg backdrop-blur border border-white/10 shadow-2xl p-2.5" 
                      title={t('auth.copyScript')}
                      onCopy={handleCopyFinished}
                    />
                  </div>
                </div>

                {isSyncing && (
                  <div className="mt-4 flex items-center justify-center gap-3 py-2 bg-primary/5 rounded-lg text-primary text-sm font-semibold border border-primary/10 animate-pulse">
                    <Loader2 size={16} className="animate-spin" />
                    {t('auth.syncing')}
                  </div>
                )}
              </div>
            </div>
          </div>
        </div>

        <div className="bg-muted/50 px-6 py-4 flex justify-end gap-3 border-t">
          <button 
            onClick={onClose}
            className="px-6 py-2 rounded-lg font-medium hover:bg-muted transition-colors text-sm"
          >
            {t('common.cancel')}
          </button>
        </div>
      </div>
    </div>
  );
}

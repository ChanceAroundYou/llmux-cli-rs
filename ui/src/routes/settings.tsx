import React, { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useSettingsStore } from '../stores/settings';
import {
  Settings as SettingsIcon,
  Shield,
  Terminal,
  Monitor,
  RefreshCw,
  Loader2,
  CheckCircle2,
  Download,
  Upload
} from 'lucide-react';
import { ConfirmDialog } from '../components/Modal';

function cn(...classes: (string | undefined | null | false)[]) {
  return classes.filter(Boolean).join(' ');
}

const SettingGroup = ({ title, icon: Icon, children }: { title: string, icon: any, children: React.ReactNode }) => (
  <div className="space-y-4">
    <div className="flex items-center gap-2 text-xs font-bold text-muted-foreground uppercase tracking-widest px-1">
      <Icon size={14} />
      <span>{title}</span>
    </div>
    <div className="p-1 border border-border rounded-xl bg-card">
      <div className="divide-y divide-border/40">
        {children}
      </div>
    </div>
  </div>
);

const SettingItem = ({ label, description, children }: { label: string, description?: string, children: React.ReactNode }) => (
  <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-4 p-4">
    <div className="space-y-0.5">
      <div className="text-sm font-bold">{label}</div>
      {description && <div className="text-[11px] text-muted-foreground font-medium">{description}</div>}
    </div>
    <div className="flex items-center">
      {children}
    </div>
  </div>
);

export default function Settings() {
  const { t } = useTranslation();
  const { config, fetchSettings, updateSettings } = useSettingsStore();
  const [localConfig, setLocalConfig] = useState<Record<string, any>>({});
  const [showSaved, setShowSaved] = useState(false);
  const [showRestartModal, setShowRestartModal] = useState(false);
  const [errorModal, setErrorModal] = useState<{title: string, message: string} | null>(null);
  const [isPurging, setIsPurging] = useState(false);
  const [isConfirmOpen, setIsConfirmOpen] = useState(false);
  const [isExporting, setIsExporting] = useState(false);
  const [isImporting, setIsImporting] = useState(false);
  const [importResult, setImportResult] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    fetchSettings();
  }, []);

  useEffect(() => {
    if (config) setLocalConfig(config);
  }, [config]);

  const handleAutoSave = async (updatedConfig: Record<string, any>, isPortChange = false) => {
    // 端口校验逻辑
    if (isPortChange) {
      const port = parseInt(updatedConfig.port);
      if (isNaN(port) || port < 1024 || port > 65535) {
        setErrorModal({
          title: t('settings.invalidPortTitle', '端口范围错误'),
          message: t('settings.invalidPort', '端口号必须在 1024 - 65535 之间，请重新输入。')
        });
        setLocalConfig({...updatedConfig, port: config.port || '25975'});
        return;
      }
      
      const reserved = [3306, 5432, 6379, 8080, 27017];
      if (reserved.includes(port)) {
        setErrorModal({
          title: t('settings.reservedPortTitle', '端口已被占用'),
          message: t('settings.reservedPort', '该端口已被常用数据库或 Web 服务占用，为了避免冲突，请选择其他端口。')
        });
        return;
      }
    }

    setLocalConfig(updatedConfig);
    await updateSettings(updatedConfig);
    setShowSaved(true);
    setTimeout(() => setShowSaved(false), 2000);

    if (isPortChange) {
      setShowRestartModal(true);
    }
  };

  const handleExport = async () => {
    setIsExporting(true);
    try {
      const res = await fetch('/api/export');
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `llmux-config-${Date.now()}.json`;
      a.click();
      URL.revokeObjectURL(url);
    } finally {
      setIsExporting(false);
    }
  };

  const handleImport = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    setIsImporting(true);
    setImportResult(null);
    try {
      const text = await file.text();
      const json = JSON.parse(text);
      const res = await fetch('/api/import', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(json) });
      const data = await res.json();
      if (data.success) {
        const { accounts, aliases, keys } = data.imported;
        setImportResult(t('settings.importSuccess', { accounts, aliases, keys }));
      }
    } catch (err) {
      console.error('Import failed:', err);
    } finally {
      setIsImporting(false);
      if (fileInputRef.current) fileInputRef.current.value = '';
    }
  };

  const handlePurge = async () => {    setIsConfirmOpen(false);
    setIsPurging(true);
    try {
      const res = await fetch('/api/settings/reset', { method: 'POST' });
      if (res.ok) {
        window.location.href = '/'; 
      }
    } catch (err) {
      console.error('Purge failed:', err);
    } finally {
      setIsPurging(false);
    }
  };

  return (
    <div className="max-w-3xl mx-auto space-y-10 animate-in fade-in duration-500">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className="p-2 bg-primary/10 text-primary rounded-lg">
            <SettingsIcon size={24} />
          </div>
          <div>
            <h1 className="text-2xl font-bold tracking-tight">{t('common.settings')}</h1>
            <p className="text-sm text-muted-foreground">{t('settings.subtitle')}</p>
          </div>
        </div>
        <div className="flex items-center gap-2">
           {showSaved && (
             <div className="flex items-center gap-1.5 text-[10px] font-bold text-green-500 bg-green-500/10 px-3 py-1.5 rounded-full animate-in zoom-in duration-300">
                <CheckCircle2 size={12} />
                <span>{t('common.saved')}</span>
             </div>
           )}
        </div>
      </div>

      <div className="space-y-8">
        <SettingGroup title={t('settings.infra')} icon={Terminal}>
           <SettingItem label={t('settings.port')} description={t('settings.portDesc')}>
             <div className="relative flex items-center gap-2">
                <input
                  type="text"
                  value={localConfig.port || '25975'}
                  onChange={e => setLocalConfig({...localConfig, port: e.target.value})}
                  onKeyDown={e => e.key === 'Enter' && handleAutoSave(localConfig, true)}
                  className="bg-muted/50 border border-border rounded-lg px-3 py-1.5 text-xs font-bold w-24 text-center outline-none focus:ring-1 focus:ring-primary/20 transition-all"
                />
                <button
                  onClick={() => handleAutoSave(localConfig, true)}
                  className="p-1.5 bg-primary/10 text-primary rounded-md hover:bg-primary hover:text-primary-foreground transition-all duration-200"
                  title={t('common.save')}
                >
                  <CheckCircle2 size={14} />
                </button>
             </div>
           </SettingItem>
        </SettingGroup>

        <SettingGroup title={t('settings.ui')} icon={Monitor}>
           <SettingItem label={t('settings.theme')} description={t('settings.themeDesc')}>
             <div className="flex border border-border rounded-lg overflow-hidden text-[10px] font-bold">
                <button 
                  onClick={() => handleAutoSave({...localConfig, theme: 'dark'})}
                  className={cn("px-4 py-1.5 transition-colors", localConfig.theme !== 'light' ? 'bg-primary text-primary-foreground' : 'hover:bg-muted')}
                >
                  {t('settings.themeDark')}
                </button>
                <button 
                  onClick={() => handleAutoSave({...localConfig, theme: 'light'})}
                  className={cn("px-4 py-1.5 transition-colors", localConfig.theme === 'light' ? 'bg-primary text-primary-foreground' : 'hover:bg-muted')}
                >
                  {t('settings.themeLight')}
                </button>
             </div>
           </SettingItem>
        </SettingGroup>

        <SettingGroup title={t('settings.security')} icon={Shield}>
           <SettingItem label={t('settings.purge')} description={t('settings.purgeDesc')}>
             <button 
                onClick={() => setIsConfirmOpen(true)}
                disabled={isPurging}
                className="px-3 py-1.5 text-[10px] font-bold text-red-500 border border-red-500/20 rounded-lg hover:bg-red-500 hover:text-white transition-all disabled:opacity-50"
              >
                {isPurging ? <Loader2 size={12} className="animate-spin inline mr-1" /> : null}
                {t('settings.purgeBtn')}
             </button>
           </SettingItem>
        </SettingGroup>

        <SettingGroup title={t('settings.sync')} icon={RefreshCw}>
          <SettingItem label={t('settings.export')} description={t('settings.exportDesc')}>
            <div className="flex items-center gap-2">
              <span className="text-[9px] text-amber-500 font-medium whitespace-nowrap">{t('settings.exportWarning')}</span>
              <button
                onClick={handleExport}
                disabled={isExporting}
                className="flex items-center gap-1.5 px-3 py-1.5 text-[10px] font-bold border border-border rounded-lg hover:bg-muted transition-all disabled:opacity-50 shrink-0"
              >
                {isExporting ? <Loader2 size={12} className="animate-spin" /> : <Download size={12} />}
                {t('settings.export')}
              </button>
            </div>
          </SettingItem>
          <SettingItem label={t('settings.import')} description={t('settings.importDesc')}>
            <div className="flex flex-col items-end gap-1.5">
              <input ref={fileInputRef} type="file" accept=".json" className="hidden" onChange={handleImport} />
              <button
                onClick={() => fileInputRef.current?.click()}
                disabled={isImporting}
                className="flex items-center gap-1.5 px-3 py-1.5 text-[10px] font-bold border border-border rounded-lg hover:bg-muted transition-all disabled:opacity-50"
              >
                {isImporting ? <Loader2 size={12} className="animate-spin" /> : <Upload size={12} />}
                {t('settings.import')}
              </button>
              {importResult && (
                <span className="text-[9px] text-green-500 font-medium max-w-[200px] text-right">{importResult}</span>
              )}
            </div>
          </SettingItem>
        </SettingGroup>
      </div>

      <ConfirmDialog 
        isOpen={isConfirmOpen}
        onClose={() => setIsConfirmOpen(false)}
        onConfirm={handlePurge}
        title={t('settings.purge')}
        description={
          <div className="space-y-4">
             <div className="p-3 bg-red-500/10 border border-red-500/20 rounded-xl flex gap-3">
                <Shield size={20} className="text-red-500 shrink-0" />
                <p className="text-xs font-bold text-red-600 leading-relaxed">
                  {t('settings.purgeConfirmTitle', '【危险操作】确定要彻底重置系统并清空数据库吗？')}
                </p>
             </div>
             <div>
                <p className="text-[11px] font-black text-muted-foreground uppercase tracking-widest mb-2 px-1">
                   {t('settings.purgeWillWipe', '这将永久抹除：')}
                 </p>
                <div className="grid grid-cols-1 gap-1">
                   {[
                     t('settings.wipeAccounts', '1. 所有服务商账户信息'),
                     t('settings.wipeAliases', '2. 所有自定义模型别名'),
                     t('settings.wipeKeys', '3. 所有客户端访问密钥 (API Keys)'),
                     t('settings.wipeLogs', '4. 所有历史用量统计和日志')
                   ].map((item, idx) => (
                     <div key={idx} className="flex items-center gap-2 px-3 py-1.5 bg-muted/40 rounded-lg text-[11px] font-medium text-muted-foreground border border-transparent hover:border-border/50">
                        <div className="w-1 h-1 rounded-full bg-red-400" />
                        {item}
                     </div>
                   ))}
                </div>
             </div>
             <p className="text-[10px] text-red-500/70 italic px-1">
                {t('settings.purgeIrreversible', '此操作不可撤销，系统将回到初始状态。')}
             </p>
          </div>
        }
        confirmText={t('settings.purgeBtn')}
        cancelText={t('common.cancel')}
        variant="danger"
        size="md"
        requireInput="reset"
        isLoading={isPurging}
      />

      <ConfirmDialog 
        isOpen={showRestartModal}
        onClose={() => setShowRestartModal(false)}
        onConfirm={() => setShowRestartModal(false)}
        title={t('settings.restartRequired', '需要重新启动')}
        description={
          <p className="text-sm font-medium text-muted-foreground leading-relaxed px-1">
            {t('settings.restartDesc', '服务端口已修改成功。为了使更改生效，请手动关闭当前运行的 LLMux 进程并重新启动。')}
          </p>
        }
        confirmText={t('common.done', '知道了')}
        variant="info"
        size="sm"
      />

      <ConfirmDialog 
        isOpen={!!errorModal}
        onClose={() => setErrorModal(null)}
        onConfirm={() => setErrorModal(null)}
        title={errorModal?.title || ''}
        description={
          <p className="text-sm font-medium text-muted-foreground leading-relaxed px-1">
            {errorModal?.message}
          </p>
        }
        confirmText={t('common.done', '知道了')}
        variant="warning"
        size="sm"
      />
    </div>
  );
}

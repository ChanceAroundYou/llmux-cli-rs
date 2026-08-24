import React, { useEffect, useRef, useState } from 'react';
import { apiFetch } from "@/lib/api";
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
import { cn } from '../lib/utils';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';

const SettingGroup = ({ title, icon: Icon, children }: { title: string, icon: any, children: React.ReactNode }) => (
  <div className="space-y-4">
    <div className="flex items-center gap-2 text-xs font-semibold text-muted-foreground uppercase tracking-widest px-1">
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
      <div className="text-sm font-semibold">{label}</div>
      {description && <div className="text-xs text-muted-foreground font-medium">{description}</div>}
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
      const res = await apiFetch('/api/export');
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
      const res = await apiFetch('/api/import', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(json) });
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
      const res = await apiFetch('/api/settings/reset', { method: 'POST' });
      if (res.ok) {
        window.location.href = import.meta.env.BASE_URL; 
      }
    } catch (err) {
      console.error('Purge failed:', err);
    } finally {
      setIsPurging(false);
    }
  };

  return (
    <div className="max-w-3xl mx-auto space-y-10 animate-fadeIn duration-500">
      <div className="flex items-center justify-between">
        <div className="flex items-start gap-3">
          <div className="p-2 bg-primary/10 text-primary rounded-lg mt-1.5">
            <SettingsIcon size={24} />
          </div>
          <div>
            <h1 className="text-2xl font-semibold tracking-tight">{t('common.settings')}</h1>
            <p className="text-sm text-muted-foreground">{t('settings.subtitle')}</p>
          </div>
        </div>
        <div className="flex items-center gap-2">
           {showSaved && (
             <div className="flex items-center gap-1.5 text-xs font-semibold text-success bg-success/10 px-3 py-1.5 rounded-full animate-in zoom-in duration-300">
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
                <Input
                  type="text"
                  value={localConfig.port || '25975'}
                  onChange={e => setLocalConfig({...localConfig, port: e.target.value})}
                  onKeyDown={e => e.key === 'Enter' && handleAutoSave(localConfig, true)}
                  className="h-auto py-1.5 text-xs font-semibold w-24 text-center"
                />
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => handleAutoSave(localConfig, true)}
                  className="h-8 w-8 text-primary hover:text-primary-foreground"
                  title={t('common.save')}
                >
                  <CheckCircle2 size={14} />
                </Button>
             </div>
           </SettingItem>
        </SettingGroup>

        <SettingGroup title={t('settings.ui')} icon={Monitor}>
           <SettingItem label={t('settings.theme')} description={t('settings.themeDesc')}>
             <div className="flex border border-border rounded-lg overflow-hidden">
                <Button
                  variant={localConfig.theme === 'dark' ? "default" : "ghost"}
                  size="sm"
                  onClick={() => handleAutoSave({...localConfig, theme: 'dark'})}
                  className="rounded-none h-auto py-1.5 text-xs font-semibold"
                >
                  {t('settings.themeDark')}
                </Button>
                <Button
                  variant={localConfig.theme === 'light' ? "default" : "ghost"}
                  size="sm"
                  onClick={() => handleAutoSave({...localConfig, theme: 'light'})}
                  className="rounded-none h-auto py-1.5 text-xs font-semibold"
                >
                  {t('settings.themeLight')}
                </Button>
                <Button
                  variant={!localConfig.theme || localConfig.theme === 'system' ? "default" : "ghost"}
                  size="sm"
                  onClick={() => handleAutoSave({...localConfig, theme: 'system'})}
                  className="rounded-none h-auto py-1.5 text-xs font-semibold"
                >
                  {t('settings.themeSystem')}
                </Button>
             </div>
           </SettingItem>
        </SettingGroup>

        <SettingGroup title={t('settings.security')} icon={Shield}>
           <SettingItem label={t('settings.purge')} description={t('settings.purgeDesc')}>
             <Button
                variant="destructive"
                size="sm"
                onClick={() => setIsConfirmOpen(true)}
                disabled={isPurging}
              >
                {isPurging ? <Loader2 size={12} className="animate-spin mr-1" /> : null}
                {t('settings.purgeBtn')}
             </Button>
           </SettingItem>
        </SettingGroup>

        <SettingGroup title={t('settings.sync')} icon={RefreshCw}>
          <SettingItem label={t('settings.export')} description={t('settings.exportDesc')}>
            <div className="flex items-center gap-2">
              <span className="text-[9px] text-warning font-medium whitespace-nowrap">{t('settings.exportWarning')}</span>
              <Button
                variant="outline"
                size="sm"
                onClick={handleExport}
                disabled={isExporting}
              >
                {isExporting ? <Loader2 size={12} className="animate-spin" /> : <Download size={12} />}
                {t('settings.export')}
              </Button>
            </div>
          </SettingItem>
          <SettingItem label={t('settings.import')} description={t('settings.importDesc')}>
            <div className="flex flex-col items-end gap-1.5">
              <input ref={fileInputRef} type="file" accept=".json" className="hidden" onChange={handleImport} />
              <Button
                variant="outline"
                size="sm"
                onClick={() => fileInputRef.current?.click()}
                disabled={isImporting}
              >
                {isImporting ? <Loader2 size={12} className="animate-spin" /> : <Upload size={12} />}
                {t('settings.import')}
              </Button>
              {importResult && (
                <span className="text-[9px] text-success font-medium max-w-[200px] text-right">{importResult}</span>
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
             <div className="p-3 bg-destructive/10 border border-destructive/20 rounded-xl flex gap-3">
                <Shield size={20} className="text-destructive shrink-0" />
                <p className="text-xs font-semibold text-destructive leading-relaxed">
                  {t('settings.purgeConfirmTitle', '【危险操作】确定要彻底重置系统并清空数据库吗？')}
                </p>
             </div>
             <div>
                <p className="text-xs font-semibold text-muted-foreground uppercase tracking-widest mb-2 px-1">
                   {t('settings.purgeWillWipe', '这将永久抹除：')}
                 </p>
                <div className="grid grid-cols-1 gap-1">
                   {[
                     t('settings.wipeAccounts', '1. 所有服务商账户信息'),
                     t('settings.wipeAliases', '2. 所有自定义模型别名'),
                     t('settings.wipeKeys', '3. 所有客户端访问密钥 (API Keys)'),
                   ].map((item, idx) => (
                     <div key={idx} className="flex items-center gap-2 px-3 py-1.5 bg-muted/40 rounded-lg text-xs font-medium text-muted-foreground border border-transparent hover:border-border/50">
                        <div className="w-1 h-1 rounded-full bg-destructive" />
                        {item}
                     </div>
                   ))}
                </div>
             </div>
             <p className="text-xs text-destructive/70 italic px-1">
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
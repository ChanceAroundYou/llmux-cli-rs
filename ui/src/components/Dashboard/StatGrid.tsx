import { LucideIcon, Users, Zap, Key, Shield } from 'lucide-react';

interface StatCardProps {
  icon: LucideIcon;
  label: string;
  value: string;
  color: string;
}

const StatCard = ({ icon: Icon, label, value, color }: StatCardProps) => (
  <div className="premium-card p-4 transition-all hover:scale-[1.02]">
    <div className="flex items-center gap-3">
      <div className={`p-2 rounded-xl bg-background border border-border shadow-sm ${color}`}>
        <Icon size={20} />
      </div>
      <div>
        <div className="text-[10px] font-bold text-muted-foreground uppercase tracking-widest">{label}</div>
        <div className="text-xl font-bold mt-0.5 tracking-tight">{value}</div>
      </div>
    </div>
  </div>
);

interface StatGridProps {
  accountCount: number;
  aliasCount: number;
  keyCount: number;
  healthyCount: number;
  t: (key: string) => string;
}

export const StatGrid = ({ accountCount, aliasCount, keyCount, healthyCount, t }: StatGridProps) => (
  <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
    <StatCard
      icon={Users}
      label={t('dashboard.stats.accounts')}
      value={String(accountCount)}
      color="text-blue-500"
    />
    <StatCard
      icon={Zap}
      label={t('dashboard.stats.aliases')}
      value={String(aliasCount)}
      color="text-amber-500"
    />
    <StatCard
      icon={Key}
      label={t('dashboard.stats.apiKeys')}
      value={String(keyCount)}
      color="text-purple-500"
    />
    <StatCard
      icon={Shield}
      label={t('dashboard.stats.healthy')}
      value={String(healthyCount)}
      color="text-green-500"
    />
  </div>
);

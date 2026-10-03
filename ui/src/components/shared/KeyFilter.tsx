import React from 'react';
import { useTranslation } from 'react-i18next';
import { apiFetch } from '@/lib/api';
import { Button } from '@/components/ui/button';

/** 与后端 usage_logs.api_key_id 对应（迁移 0026）。 */
export interface ApiKeyOption {
  id: number;
  name: string;
}

interface KeyFilterProps {
  /** null = 不筛选。 */
  value: number | null;
  onChange: (id: number | null) => void;
  className?: string;
}

/**
 * 按网关密钥筛选的下拉组。
 *
 * 用仓库既有的「描边按钮组」样式（见 logs.tsx 的成功/失败筛选），而不是
 * `components/ui/select.tsx` —— 那个 Radix 封装全仓无人使用，密钥名长短不定时
 * 也不如按钮组直观。密钥多于 5 把时横向会挤，届时换成 Select 是自然的升级路径。
 *
 * 密钥列表自己拉一次并缓存到模块级：三个页面（统计 / 日志 / 仪表盘）都会用到，
 * 而密钥几乎不变，没必要每个组件各拉一次。
 */
let cache: ApiKeyOption[] | null = null;
let inflight: Promise<ApiKeyOption[]> | null = null;

async function loadKeys(): Promise<ApiKeyOption[]> {
  if (cache) return cache;
  if (!inflight) {
    inflight = apiFetch('/api/keys')
      .then(async r => (r.ok ? await r.json() : []))
      .then((d: any) => {
        cache = Array.isArray(d)
          ? d.map((k: any) => ({ id: k.id, name: k.name }))
          : [];
        return cache;
      })
      .catch(() => {
        // 拉不到就当没有筛选器 —— 静默降级，但别让一个下拉拖垮整页。
        cache = [];
        return cache;
      });
  }
  return inflight;
}

export default function KeyFilter({ value, onChange, className }: KeyFilterProps) {
  const { t } = useTranslation();
  const [keys, setKeys] = React.useState<ApiKeyOption[]>(cache ?? []);

  React.useEffect(() => {
    if (keys.length) return;
    let alive = true;
    loadKeys().then(k => { if (alive) setKeys(k); });
    return () => { alive = false; };
  }, [keys.length]);

  // 当前选中的密钥可能已被删掉 —— 回落到「全部」，否则 Select 会显示一个空标签。
  React.useEffect(() => {
    if (value !== null && keys.length && !keys.some(k => k.id === value)) {
      onChange(null);
    }
  }, [keys, value, onChange]);

  if (!keys.length) return null;

  return (
    <div className={`flex border border-border rounded-lg overflow-hidden ${className ?? ''}`}>
      <Button
        variant={value === null ? 'default' : 'ghost'}
        size="sm"
        onClick={() => onChange(null)}
        className="rounded-none h-auto py-1.5 text-xs font-semibold"
      >
        {t('common.allKeys', { defaultValue: '全部密钥' })}
      </Button>
      {keys.map(k => (
        <Button
          key={k.id}
          variant={value === k.id ? 'default' : 'ghost'}
          size="sm"
          onClick={() => onChange(k.id)}
          className="rounded-none h-auto py-1.5 text-xs font-semibold max-w-[160px] truncate"
        >
          {k.name}
        </Button>
      ))}
    </div>
  );
}
import React from 'react';
import { useTranslation } from 'react-i18next';
import { apiFetch } from '@/lib/api';
import FilterSelect from './FilterSelect';

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
 * 按网关密钥筛选的单向下拉。
 *
 * 密钥名长短不一、数量会涨，按钮组部署到统计 / 日志 / 仪表盘三页后会横向挤爆；
 * 统一走共享的 FilterSelect（Radix Select），「全部密钥」即不筛选。
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
    <FilterSelect
      className={className}
      value={value === null ? null : String(value)}
      options={keys.map(k => ({ value: String(k.id), label: k.name }))}
      allLabel={t('common.allKeys', { defaultValue: '全部密钥' })}
      onChange={next => onChange(next === null ? null : Number(next))}
    />
  );
}
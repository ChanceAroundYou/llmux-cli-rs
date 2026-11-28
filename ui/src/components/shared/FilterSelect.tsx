import React from 'react';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { cn } from '@/lib/utils';

export interface FilterOption {
  value: string;
  label: string;
}

interface FilterSelectProps {
  /** null = 全部。 */
  value: string | null;
  options: FilterOption[];
  allLabel: string;
  onChange: (value: string | null) => void;
  className?: string;
}

// Radix 不允许空字符串作为 item value；用一个不会出现在真实筛选值中的哨兵表示「全部」。
const ALL = '__filter_all__';

/**
 * 统计 / 日志页共用的单向下拉筛选器：选中「全部」回调 null。
 * 沿用页面既有筛选控件的紧凑描边样式，不引入新的视觉语言。
 */
export default function FilterSelect({
  value,
  options,
  allLabel,
  onChange,
  className,
}: FilterSelectProps) {
  return (
    <Select
      value={value ?? ALL}
      onValueChange={next => onChange(next === ALL ? null : next)}
    >
      <SelectTrigger
        className={cn(
          'h-auto w-auto min-w-[108px] max-w-[200px] py-1.5 text-xs font-semibold',
          className,
        )}
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={ALL}>{allLabel}</SelectItem>
        {options.map(option => (
          <SelectItem key={option.value} value={option.value}>
            {option.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

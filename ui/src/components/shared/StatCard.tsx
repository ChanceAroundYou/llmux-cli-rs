import React from 'react';
import { LucideIcon } from 'lucide-react';
import { cn } from "@/lib/utils"

export type StatTrend = 'primary' | 'success' | 'warning' | 'destructive' | 'neutral';

interface StatCardProps {
  label: string;
  value: string | number;
  subtitle?: React.ReactNode;
  icon: LucideIcon;
  trend?: {
    value: string | number;
    label?: string;
    type?: StatTrend;
  };
  className?: string;
}

const trendStyles: Record<StatTrend, string> = {
  primary: "text-primary bg-primary/10 border-primary/20",
  success: "text-success bg-success/10 border-success/20",
  warning: "text-warning bg-warning/10 border-warning/20",
  destructive: "text-destructive bg-destructive/10 border-destructive/20",
  neutral: "text-muted-foreground bg-muted border-border",
};

const iconStyles: Record<StatTrend, string> = {
  primary: "text-primary bg-primary/10",
  success: "text-success bg-success/10",
  warning: "text-warning bg-warning/10",
  destructive: "text-destructive bg-destructive/10",
  neutral: "text-muted-foreground bg-muted",
};

export function StatCard({ 
  label, 
  value, 
  subtitle, 
  icon: Icon, 
  trend,
  className 
}: StatCardProps) {
  const trendType = trend?.type || 'neutral';
  
  return (
    <div className={cn(
      "bg-card border border-border p-6 rounded-xl shadow-sm transition-all duration-200 hover:shadow-md hover:border-border/80",
      className
    )}>
      <div className="flex justify-between items-start">
        <div className="space-y-1">
          <p className="text-sm font-medium text-muted-foreground tracking-tight">{label}</p>
          <div className="flex items-baseline gap-2">
            <h3 className="text-3xl font-bold text-foreground tracking-tighter">{value}</h3>
            {trend && (
              <span className={cn(
                "text-[10px] font-bold px-1.5 py-0.5 rounded-full border uppercase tracking-wider",
                trendStyles[trendType]
              )}>
                {trend.value}
              </span>
            )}
          </div>
        </div>
        <div className={cn('p-2.5 rounded-lg shrink-0', iconStyles[trendType])}>
          <Icon size={20} />
        </div>
      </div>
      
      {subtitle && (
        <div className="mt-4 flex items-center text-xs text-muted-foreground font-medium">
          {subtitle}
        </div>
      )}
    </div>
  );
}

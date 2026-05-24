import React from 'react';
import { 
  Zap, 
  ChevronRight, 
  LayoutGrid, 
  RefreshCcw 
} from 'lucide-react';
import { CopyButton } from '../CopyButton';
import { cn } from '@/lib/utils';
import { parseServerDate } from '@/utils/date';
import { TFunction } from 'i18next';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import {
  Card,
  CardContent,
  CardFooter,
  CardHeader,
} from "@/components/ui/card"

interface ModelCardProps {
  model: {
    id: string;
    owned_by: string;
  };
  testResult?: {
    success: boolean;
    latency?: number;
    error?: string;
    loading?: boolean;
    lastChecked?: string;
    limitsCache?: any;
    limitsUpdatedAt?: string;
  };
  onTest: (modelId: string, providerId: string) => void;
  onAssign: (model: any) => void;
  isQueueRunning: boolean;
  t: TFunction;
  i18n: any;
}

export const ModelCard: React.FC<ModelCardProps> = ({
  model,
  testResult,
  onTest,
  onAssign,
  isQueueRunning,
  t,
  i18n
}) => {
  const limits = testResult?.limitsCache;
  const limitsUpdatedAt = testResult?.limitsUpdatedAt;
  
  const renderLimits = () => {
    if (!limits) return null;
    const remaining = parseInt(limits['x-ratelimit-remaining-tokens'] ?? limits['x-quota-remaining'] ?? -1);
    const total = parseInt(limits['x-ratelimit-limit-tokens'] ?? limits['x-quota-total'] ?? -1);
    if (remaining < 0 || total <= 0) return null;
    
    const pct = Math.max(0, Math.min(100, (remaining / total) * 100));
    const color = pct > 50 ? 'bg-success' : pct > 15 ? 'bg-warning' : 'bg-destructive';
    
    return (
      <div className="mt-3 space-y-1.5">
        <div className="flex justify-between text-[10px] font-black text-muted-foreground/40 uppercase tracking-widest">
          <span>Tokens</span>
          <span>{remaining.toLocaleString()} / {total.toLocaleString()}</span>
        </div>
        <div className="h-1.5 w-full rounded-full bg-muted overflow-hidden border border-border/50">
          <div
            className={cn("h-full rounded-full transition-all duration-1000 ease-out shadow-[0_0_8px_rgba(var(--primary),0.3)]", color)}
            style={{ width: `${pct}%` }}
          />
        </div>
      </div>
    );
  };

  return (
    <Card className="group flex flex-col justify-between min-h-[200px] hover:border-primary/40 hover:shadow-[0_8px_30px_-4px_rgba(0,0,0,0.05)] dark:hover:shadow-[0_8px_30px_-4px_rgba(0,0,0,0.3)] transition-all duration-300 relative overflow-hidden border-border/60">
      <div className="absolute left-0 top-0 w-1 h-full bg-primary/5 group-hover:bg-primary/20 transition-colors" />
      
      <CardHeader className="p-5 pb-0 space-y-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <div className="w-6 h-6 rounded-lg bg-primary/10 flex items-center justify-center border border-primary/10">
               <LayoutGrid size={12} className="text-primary" />
            </div>
            <span className="text-[10px] font-black text-primary uppercase tracking-[0.2em]">{model.owned_by}</span>
          </div>
          <div className="flex items-center gap-1.5">
             {testResult?.loading ? (
               <RefreshCcw size={12} className="animate-spin text-muted-foreground/50" />
             ) : testResult ? (
               <Badge 
                 variant={testResult.success ? "secondary" : "destructive"} 
                 className={cn(
                   "w-2 h-2 rounded-full p-0 border-none min-w-0 ring-4 ring-background",
                   testResult.success 
                     ? "bg-success shadow-[0_0_12px_rgba(34,197,94,0.6)]" 
                     : "bg-destructive shadow-[0_0_12px_rgba(239,68,68,0.6)]"
                 )} 
               />
             ) : null}
          </div>
        </div>

        <div className="flex items-start justify-between gap-2">
          <h3 className="font-bold text-sm tracking-tight line-clamp-2 leading-snug group-hover:text-primary transition-colors">
            {model.id}
          </h3>
          <CopyButton 
            value={model.id} 
            size={12} 
            className="mt-0.5 opacity-0 group-hover:opacity-100 transition-all bg-muted/50 hover:bg-muted" 
            title={t('models.actions.copyName')} 
          />
        </div>
      </CardHeader>

      <CardContent className="p-5 pt-2 flex-1">
        <div className="flex items-center gap-2">
          {testResult?.latency != null && (
            <Badge variant="outline" className={cn(
              "text-[10px] font-black border-none px-1.5 py-0.5",
              testResult.success ? "bg-success/5 text-success" : "bg-destructive/5 text-destructive"
            )}>
              {(testResult.latency / 1000).toFixed(2)}s
            </Badge>
          )}
          {testResult?.lastChecked && (
            <span className="text-[9px] text-muted-foreground/40 font-bold uppercase tracking-wider">
              {parseServerDate(testResult.lastChecked).toLocaleString(i18n.language, { 
                month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' 
              })}
            </span>
          )}
        </div>

        {testResult?.error && (
          <p className="text-[10px] text-destructive font-bold line-clamp-2 opacity-70 bg-destructive/5 p-2 rounded-xl border border-destructive/10 mt-3 animate-in fade-in slide-in-from-top-1" title={testResult.error}>
            {testResult.error}
          </p>
        )}

        {renderLimits()}
      </CardContent>
      
      <CardFooter className="p-4 border-t border-border/40 bg-muted/5 flex items-center justify-between">
         <Button 
           variant="ghost" 
           size="sm"
           onClick={() => onTest(model.id, model.owned_by)}
           disabled={testResult?.loading || isQueueRunning}
           className="h-7 px-2 flex items-center gap-1.5 text-[10px] font-black uppercase tracking-widest text-muted-foreground hover:text-warning hover:bg-warning/5 transition-all disabled:opacity-30"
         >
           <Zap size={11} className={cn(testResult?.success && "text-warning fill-warning")} />
           {testResult?.loading ? t('models.testing') : t('models.testBtn')}
         </Button>
         <Button 
           variant="ghost"
           size="sm"
           onClick={() => onAssign(model)}
           className="h-7 px-2 flex items-center gap-1 text-[10px] font-black uppercase tracking-widest text-primary hover:bg-primary/5 hover:gap-2 transition-all"
         >
           {t('models.actions.assign')}
           <ChevronRight size={12} />
         </Button>
      </CardFooter>
    </Card>
  );
};
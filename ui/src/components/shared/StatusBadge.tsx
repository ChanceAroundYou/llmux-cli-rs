import React from 'react';
import { cn } from "@/lib/utils"
import { Badge } from "@/components/ui/badge"

export type StatusType = 'online' | 'offline' | 'warning' | 'error' | 'unknown';

interface StatusBadgeProps {
  status: StatusType;
  label?: string;
  className?: string;
  dot?: boolean;
}

const statusConfig: Record<StatusType, { variant: "default" | "secondary" | "destructive" | "outline", className: string, label: string }> = {
  online: { 
    variant: "secondary", 
    className: "bg-success/10 text-success border-success/20 hover:bg-success/20", 
    label: "Online" 
  },
  offline: { 
    variant: "destructive", 
    className: "bg-destructive/10 text-destructive border-destructive/20 hover:bg-destructive/20", 
    label: "Offline" 
  },
  warning: { 
    variant: "outline", 
    className: "bg-warning/10 text-warning border-warning/20 hover:bg-warning/20", 
    label: "Warning" 
  },
  error: { 
    variant: "destructive", 
    className: "bg-destructive text-destructive-foreground", 
    label: "Error" 
  },
  unknown: { 
    variant: "outline", 
    className: "bg-muted text-muted-foreground border-border", 
    label: "Unknown" 
  },
};

export function StatusBadge({ status, label, className, dot = true }: StatusBadgeProps) {
  const config = statusConfig[status];
  
  return (
    <Badge 
      variant={config.variant} 
      className={cn("gap-1.5 px-2 py-0.5 font-semibold transition-all duration-200", config.className, className)}
    >
      {dot && (
        <span className={cn(
          "h-1.5 w-1.5 rounded-full shrink-0",
          status === 'online' ? "bg-success animate-pulse" : 
          status === 'offline' ? "bg-destructive" :
          status === 'warning' ? "bg-warning" :
          status === 'error' ? "bg-destructive-foreground" :
          "bg-muted-foreground/50"
        )} />
      )}
      {label || config.label}
    </Badge>
  );
}

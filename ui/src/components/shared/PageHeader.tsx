import { cn } from "@/lib/utils"

interface PageHeaderProps {
  title: string
  subtitle?: string
  action?: React.ReactNode
  icon?: React.ReactNode
  className?: string
}

export function PageHeader({ title, subtitle, action, icon, className }: PageHeaderProps) {
  return (
    <header className={cn("flex flex-col sm:flex-row justify-between items-start sm:items-center gap-4", className)}>
      <div className="flex items-start gap-3">
        {icon && <div className="p-2 bg-primary/10 text-primary rounded-lg mt-1.5">{icon}</div>}
        <div>
          <h1 className="text-xl font-semibold tracking-tight text-foreground">{title}</h1>
          {subtitle && <p className="text-sm text-muted-foreground mt-1">{subtitle}</p>}
        </div>
      </div>
      {action && <div className="flex items-center gap-3 w-full sm:w-auto">{action}</div>}
    </header>
  )
}

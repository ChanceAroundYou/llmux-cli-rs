import { cn } from "@/lib/utils"

type Status = "online" | "offline" | "unknown"

interface StatusDotProps {
  status: Status
  className?: string
}

const statusStyles: Record<Status, string> = {
  online: "bg-success",
  offline: "bg-destructive",
  unknown: "bg-muted-foreground/30",
}

export function StatusDot({ status, className }: StatusDotProps) {
  return (
    <span className="relative flex h-2.5 w-2.5 shrink-0">
      {status === "online" && (
        <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-success opacity-75" />
      )}
      <span className={cn("relative inline-flex rounded-full h-2.5 w-2.5", statusStyles[status], className)} />
    </span>
  )
}

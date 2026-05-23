import React, { useState, useEffect } from 'react'
import { AlertTriangle, Info, CheckCircle2, AlertCircle } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import {
  Dialog as ShadDialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/ui/dialog'
import { cn } from '@/lib/utils'

type DialogSize = 'sm' | 'md' | 'lg' | 'xl' | 'full'
type DialogVariant = 'default' | 'danger' | 'success' | 'warning' | 'info'

interface DialogProps {
  isOpen: boolean
  onClose: () => void
  title?: string
  description?: React.ReactNode
  children?: React.ReactNode
  footer?: React.ReactNode
  size?: DialogSize
  variant?: DialogVariant
  closeOnOverlay?: boolean
  hideClose?: boolean
}

const sizeMap: Record<DialogSize, string> = {
  sm: 'max-w-sm',
  md: 'max-w-lg',
  lg: 'max-w-2xl',
  xl: 'max-w-4xl',
  full: 'max-w-[calc(100vw-2rem)]',
}

const variantIcon: Record<DialogVariant, React.ReactNode | null> = {
  default: null,
  danger: <AlertTriangle size={20} className="text-destructive shrink-0" />,
  success: <CheckCircle2 size={20} className="text-success shrink-0" />,
  warning: <AlertCircle size={20} className="text-warning shrink-0" />,
  info: <Info size={20} className="text-info shrink-0" />,
}

const variantHeaderColor: Record<DialogVariant, string> = {
  default: '',
  danger: 'border-destructive/10 bg-destructive/5',
  success: 'border-success/10 bg-success/5',
  warning: 'border-warning/10 bg-warning/5',
  info: 'border-info/10 bg-info/5',
}

export function Dialog({
  isOpen,
  onClose,
  title,
  description,
  children,
  footer,
  size = 'md',
  variant = 'default',
  closeOnOverlay = true,
  hideClose = false,
}: DialogProps) {
  const icon = variantIcon[variant]
  const headerColor = variantHeaderColor[variant]

  return (
    <ShadDialog open={isOpen} onOpenChange={(open) => !open && onClose()}>
      <DialogContent
        className={cn(sizeMap[size], 'p-0 gap-0')}
        onInteractOutside={closeOnOverlay ? undefined : (e) => e.preventDefault()}
        hideClose={hideClose}
      >
        {(title || description) && (
          <DialogHeader className={cn('flex flex-row items-start gap-3 px-6 py-4 border-b border-border', headerColor)}>
            {icon}
            <div className="flex-1 min-w-0">
              {title && <DialogTitle className="text-base font-semibold tracking-tight">{title}</DialogTitle>}
              {description && <DialogDescription className="text-xs text-muted-foreground mt-0.5">{description}</DialogDescription>}
            </div>
          </DialogHeader>
        )}
        {children && <div className="px-6 py-4">{children}</div>}
        {footer && <DialogFooter className="px-6 py-4 border-t border-border bg-muted/30">{footer}</DialogFooter>}
      </DialogContent>
    </ShadDialog>
  )
}

interface ConfirmDialogProps {
  isOpen: boolean
  onClose: () => void
  onConfirm: () => void | Promise<void>
  title: string
  description?: React.ReactNode
  confirmText?: string
  cancelText?: string
  variant?: 'danger' | 'warning' | 'info' | 'success'
  isLoading?: boolean
  size?: DialogSize
  requireInput?: string
}

export function ConfirmDialog({
  isOpen,
  onClose,
  onConfirm,
  title,
  description,
  confirmText,
  cancelText,
  variant = 'warning',
  isLoading = false,
  size = 'sm',
  requireInput,
}: ConfirmDialogProps) {
  const { t } = useTranslation()
  const [inputValue, setInputValue] = useState('')

  useEffect(() => {
    if (!isOpen) setInputValue('')
  }, [isOpen])

  const confirmColor = {
    danger: 'bg-destructive text-destructive-foreground hover:bg-destructive/90',
    warning: 'bg-warning text-warning-foreground hover:bg-warning/90',
    info: 'bg-info text-info-foreground hover:bg-info/90',
    success: 'bg-success text-success-foreground hover:bg-success/90',
  }[variant]

  const isConfirmDisabled = isLoading || (requireInput ? inputValue !== requireInput : false)

  return (
    <Dialog
      isOpen={isOpen}
      onClose={onClose}
      title={title}
      description={description}
      size={size}
      variant={variant}
      closeOnOverlay={!isLoading}
      hideClose={isLoading}
      footer={
        <>
          <button
            onClick={onClose}
            disabled={isLoading}
            className="px-4 py-2 text-sm font-medium border border-border rounded-lg hover:bg-muted transition-colors disabled:opacity-50"
          >
            {cancelText || t('common.cancel')}
          </button>
          <button
            onClick={onConfirm}
            disabled={isConfirmDisabled}
            className={cn('px-4 py-2 text-sm font-medium rounded-lg transition-colors disabled:opacity-60 flex items-center gap-2', confirmColor)}
          >
            {isLoading && (
              <span className="w-3.5 h-3.5 border-2 border-white/30 border-t-white rounded-full animate-spin" />
            )}
            {confirmText || t('common.confirm')}
          </button>
        </>
      }
    >
      {requireInput && (
        <div className="mt-4 pt-4 border-t border-border/50">
          <label className="block text-xs font-medium text-muted-foreground mb-2">
            {t('common.pleaseType')} <span className="text-foreground bg-muted px-1.5 py-0.5 rounded select-all">{requireInput}</span> {t('common.toConfirm')}
          </label>
          <input
            type="text"
            value={inputValue}
            onChange={(e) => setInputValue(e.target.value)}
            disabled={isLoading}
            className="w-full bg-background border border-border rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-primary/20"
            placeholder={requireInput}
            autoComplete="off"
          />
        </div>
      )}
    </Dialog>
  )
}

interface LegacyModalProps {
  isOpen: boolean
  onClose: () => void
  title: string
  children: React.ReactNode
}

/** @deprecated 请使用 Dialog 代替 */
export const Modal = ({ isOpen, onClose, title, children }: LegacyModalProps) => (
  <Dialog isOpen={isOpen} onClose={onClose} title={title} size="md">
    {children}
  </Dialog>
)

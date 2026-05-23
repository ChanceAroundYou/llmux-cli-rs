import React, { useEffect, useCallback } from 'react';
import { X, AlertTriangle, Info, CheckCircle2, AlertCircle } from 'lucide-react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

// ——————————————————————————————————————————
// 工具函数
// ——————————————————————————————————————————
function cn(...classes: (string | undefined | null | false)[]) {
  return classes.filter(Boolean).join(' ');
}

// ——————————————————————————————————————————
// Dialog (通用全场景弹窗)
// ——————————————————————————————————————————
type DialogSize = 'sm' | 'md' | 'lg' | 'xl' | 'full';
type DialogVariant = 'default' | 'danger' | 'success' | 'warning' | 'info';

interface DialogProps {
  isOpen: boolean;
  onClose: () => void;
  title?: string;
  description?: React.ReactNode;
  children?: React.ReactNode;
  /** 底部自定义操作按钮（如确认/取消） */
  footer?: React.ReactNode;
  size?: DialogSize;
  variant?: DialogVariant;
  /** 点击遮罩层是否可关闭，默认 true */
  closeOnOverlay?: boolean;
  /** 是否隐藏关闭按钮 */
  hideClose?: boolean;
}

const sizeMap: Record<DialogSize, string> = {
  sm:   'max-w-sm',
  md:   'max-w-lg',
  lg:   'max-w-2xl',
  xl:   'max-w-4xl',
  full: 'max-w-[calc(100vw-2rem)]',
};

const variantIcon: Record<DialogVariant, React.ReactNode | null> = {
  default: null,
  danger:  <AlertTriangle size={20} className="text-red-500 shrink-0" />,
  success: <CheckCircle2 size={20} className="text-green-500 shrink-0" />,
  warning: <AlertCircle size={20} className="text-amber-500 shrink-0" />,
  info:    <Info size={20} className="text-blue-500 shrink-0" />,
};

const variantHeaderColor: Record<DialogVariant, string> = {
  default: '',
  danger:  'border-red-500/10 bg-red-500/5',
  success: 'border-green-500/10 bg-green-500/5',
  warning: 'border-amber-500/10 bg-amber-500/5',
  info:    'border-blue-500/10 bg-blue-500/5',
};

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
  const handleEsc = useCallback((e: KeyboardEvent) => {
    if (e.key === 'Escape') onClose();
  }, [onClose]);

  useEffect(() => {
    if (!isOpen) return;
    document.body.style.overflow = 'hidden';
    window.addEventListener('keydown', handleEsc);
    return () => {
      document.body.style.overflow = '';
      window.removeEventListener('keydown', handleEsc);
    };
  }, [isOpen, handleEsc]);

  if (!isOpen) return null;

  const icon = variantIcon[variant];
  const headerColor = variantHeaderColor[variant];

  return createPortal(
    <div className="fixed inset-0 z-[200] flex items-center justify-center p-4 sm:p-6">
      {/* 背景遮罩 */}
      <div
        className="absolute inset-0 bg-background/75 backdrop-blur-sm animate-in fade-in duration-200"
        onClick={closeOnOverlay ? onClose : undefined}
      />

      {/* 面板 */}
      <div
        className={cn(
          'relative w-full bg-card border border-border shadow-2xl rounded-2xl overflow-hidden',
          'animate-in zoom-in-95 slide-in-from-bottom-4 duration-300',
          sizeMap[size]
        )}
        onClick={e => e.stopPropagation()}
      >
        {/* 头部 */}
        {(title || !hideClose) && (
          <div className={cn('flex items-start justify-between px-6 py-4 border-b border-border gap-4', headerColor)}>
            <div className="flex items-center gap-3 min-w-0">
              {icon}
              <div className="min-w-0">
                {title && (
                  <h3 className="text-base font-bold tracking-tight leading-tight">{title}</h3>
                )}
                {description && (
                  <p className="text-xs text-muted-foreground mt-0.5 leading-relaxed">{description}</p>
                )}
              </div>
            </div>
            {!hideClose && (
              <button
                onClick={onClose}
                className="shrink-0 p-1.5 hover:bg-muted rounded-lg text-muted-foreground hover:text-foreground transition-colors"
                aria-label="Close"
              >
                <X size={16} />
              </button>
            )}
          </div>
        )}

        {/* 正文内容 */}
        {children && (
          <div className="p-6 overflow-y-auto max-h-[70vh]">
            {children}
          </div>
        )}

        {/* 底部操作区 */}
        {footer && (
          <div className="px-6 py-4 border-t border-border bg-muted/30 flex items-center justify-end gap-3">
            {footer}
          </div>
        )}
      </div>
    </div>,
    document.body
  );
}

// ——————————————————————————————————————————
// 便捷常用变体 (Confirm Dialog)
// ——————————————————————————————————————————
interface ConfirmDialogProps {
  isOpen: boolean;
  onClose: () => void;
  onConfirm: () => void | Promise<void>;
  title: string;
  description?: React.ReactNode;
  confirmText?: string;
  cancelText?: string;
  variant?: 'danger' | 'warning' | 'info' | 'success';
  isLoading?: boolean;
  size?: DialogSize;
  requireInput?: string;
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
  const { t } = useTranslation();
  const [inputValue, setInputValue] = React.useState('');

  // Reset input when modal opens/closes
  React.useEffect(() => {
    if (!isOpen) setInputValue('');
  }, [isOpen]);

  const confirmColor = {
    danger:  'bg-red-500 text-white hover:bg-red-600',
    warning: 'bg-amber-500 text-white hover:bg-amber-600',
    info:    'bg-blue-500 text-white hover:bg-blue-600',
    success: 'bg-green-500 text-white hover:bg-green-600',
  }[variant];

  const isConfirmDisabled = isLoading || (requireInput ? inputValue !== requireInput : false);

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
            className="px-4 py-2 text-sm font-bold border border-border rounded-lg hover:bg-muted transition-all disabled:opacity-50"
          >
            {cancelText || t('common.cancel')}
          </button>
          <button
            onClick={onConfirm}
            disabled={isConfirmDisabled}
            className={cn('px-4 py-2 text-sm font-bold rounded-lg transition-all disabled:opacity-60 flex items-center gap-2', confirmColor)}
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
          <label className="block text-xs font-bold text-muted-foreground mb-2">
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
  );
}

// ——————————————————————————————————————————
// 向后兼容：老版 Modal = Dialog (别名)
// ——————————————————————————————————————————
interface LegacyModalProps {
  isOpen: boolean;
  onClose: () => void;
  title: string;
  children: React.ReactNode;
}

/** @deprecated 请使用 Dialog 代替 */
export const Modal = ({ isOpen, onClose, title, children }: LegacyModalProps) => (
  <Dialog isOpen={isOpen} onClose={onClose} title={title} size="md">
    {children}
  </Dialog>
);

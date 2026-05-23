import React, { useState } from 'react';
import { Copy, Check } from 'lucide-react';

interface CopyButtonProps {
  value: string;
  size?: number;
  className?: string;
  title?: string;
  onCopy?: () => void;
}

export const CopyButton: React.FC<CopyButtonProps> = ({ 
  value, 
  size = 14, 
  className = "", 
  title = "Copy",
  onCopy
}) => {
  const [copied, setCopied] = useState(false);

  const handleCopy = (e: React.MouseEvent) => {
    e.stopPropagation();
    navigator.clipboard.writeText(value);
    setCopied(true);
    if (onCopy) onCopy();
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <button
      onClick={handleCopy}
      className={`p-1.5 hover:bg-muted rounded-lg transition-all active:scale-95 ${className}`}
      title={title}
    >
      {copied ? (
        <Check size={size} className="text-green-500 animate-in zoom-in duration-200" />
      ) : (
        <Copy size={size} className="transition-colors" />
      )}
    </button>
  );
};

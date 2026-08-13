// 快速配置：VSCode Copilot 模型配置（可视化编辑 models，可增删，实时 JSON 预览）
import React, { useCallback, useEffect, useMemo, useState } from "react";
import { Bot, Globe, Info, Plus, Trash2 } from "lucide-react";
import { Link } from "react-router-dom";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import { Input } from "@/components/ui/Input";
import { Select } from "@/components/ui/Select";
import { FileCard } from "../FilePreview";
import type { AliasResponse } from "@/types/model";

interface VscodeModel {
  id: string;
  name: string;
  url: string;
  toolCalling: boolean;
  vision: boolean;
  maxInputTokens: number;
  maxOutputTokens: number;
  thinking: boolean;
}

// 单个别名 → VSCode 模型：id 为目标模型名、name 为别名、url 指向本网关
function buildOne(a: AliasResponse, gatewayUrl: string): VscodeModel {
  return {
    id: a.target_model,
    name: a.alias,
    url: `${gatewayUrl}/v1`,
    toolCalling: true,
    vision: true,
    maxInputTokens: 1000000,
    maxOutputTokens: 16000,
    thinking: true,
  };
}

// 初始从网关别名生成
function buildVscodeModels(
  aliases: AliasResponse[],
  gatewayUrl: string,
): VscodeModel[] {
  return aliases.map((a) => buildOne(a, gatewayUrl));
}

function emptyModel(gatewayUrl: string): VscodeModel {
  return {
    id: "",
    name: "",
    url: `${gatewayUrl}/v1`,
    toolCalling: true,
    vision: true,
    maxInputTokens: 1000000,
    maxOutputTokens: 16000,
    thinking: true,
  };
}

const fieldLabel =
  "mb-1 block text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/70";

// 马卡龙彩色能力开关（pill 样式）
function CapabilityPill({
  label,
  active,
  activeClass,
  onClick,
}: {
  label: string;
  active: boolean;
  activeClass: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "rounded-full border px-2.5 py-1 text-xs font-semibold transition-all",
        active
          ? activeClass
          : "border-border text-muted-foreground hover:border-muted-foreground/50",
      )}
    >
      {label}
    </button>
  );
}

interface Props {
  aliases: AliasResponse[];
  gatewayUrl: string;
}

export const VSCodePanel: React.FC<Props> = ({ aliases, gatewayUrl }) => {
  const { t } = useT();
  // 命令面板快捷键：macOS 为 Cmd，其余平台为 Ctrl
  const isMac =
    typeof navigator !== "undefined" && navigator.userAgent.includes("Mac");
  const shortcut = isMac ? "Cmd + Shift + P" : "Ctrl + Shift + P";

  const [models, setModels] = useState<VscodeModel[]>(() =>
    buildVscodeModels(aliases, gatewayUrl),
  );

  // 别名变化时把新增别名追加到模型列表（保留用户已编辑项）。
  // 也覆盖"挂载后 aliases 才异步到达"的场景：首帧 models 为空，到达后补齐。
  useEffect(() => {
    setModels((prev) => {
      const existing = new Set(prev.map((m) => m.name));
      const added = aliases.filter((a) => !existing.has(a.alias));
      if (added.length === 0) return prev;
      return [...prev, ...added.map((a) => buildOne(a, gatewayUrl))];
    });
  }, [aliases, gatewayUrl]);

  const updateField = useCallback(
    (index: number, patch: Partial<VscodeModel>) => {
      setModels((prev) =>
        prev.map((m, i) => (i === index ? { ...m, ...patch } : m)),
      );
    },
    [],
  );

  const addModel = useCallback(() => {
    setModels((prev) => [...prev, emptyModel(gatewayUrl)]);
  }, [gatewayUrl]);

  const removeModel = useCallback((index: number) => {
    setModels((prev) => prev.filter((_, i) => i !== index));
  }, []);

  // 别名 → 目标模型 映射（id/name 下拉联动依据）
  const aliasOptions = useMemo(
    () => aliases.map((a) => ({ alias: a.alias, target: a.target_model })),
    [aliases],
  );

  // 选别名 → name 同步 id 为对应目标模型（id 隐藏不展示）
  const selectByName = useCallback(
    (index: number, alias: string) => {
      const found = aliasOptions.find((o) => o.alias === alias);
      updateField(index, { name: alias, id: found ? found.target : "" });
    },
    [aliasOptions, updateField],
  );

  const json = useMemo(() => JSON.stringify(models, null, 2), [models]);

  if (aliases.length === 0) {
    return (
      <div className="flex items-start gap-3 rounded-xl border border-dashed border-border bg-muted/30 p-5 text-sm text-muted-foreground">
        <Info size={16} className="mt-0.5 shrink-0 text-warning" />
        <span>
          {t("setup.vscode.noAliases")}{" "}
          <Link
            to="/models"
            className="font-medium text-primary underline underline-offset-2"
          >
            {t("setup.vscode.goModels")}
          </Link>
        </span>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-start gap-3 rounded-xl border border-primary/20 bg-primary/5 p-4 text-xs leading-relaxed text-primary/80">
        <Info size={15} className="mt-0.5 shrink-0" />
        <span className="whitespace-pre-line">
          {t("setup.vscode.hint", { shortcut })}
        </span>
      </div>

      <div className="grid grid-cols-1 gap-5 xl:grid-cols-2">
        {/* 左列：模型编辑（限高滚动，参考首页两栏盒子） */}
        <div
          className="min-h-0 space-y-2 overflow-y-auto pr-1"
          style={{ height: "calc(100dvh - 460px)", minHeight: 280 }}
        >
          <div className="flex items-center justify-between px-1">
            <span className="text-xs font-semibold uppercase tracking-widest text-muted-foreground">
              {t("setup.vscode.modelsList")}
            </span>
            <span className="text-xs text-muted-foreground">
              {t("setup.vscode.modelsHint", { n: models.length })}
            </span>
          </div>

          {models.map((m, i) => (
            <div
              key={i}
              className="group space-y-2 rounded-xl border border-border bg-card p-3 transition-all hover:border-primary/30"
            >
              <div className="flex items-center gap-2">
                <span className="rounded-lg bg-primary/10 p-1.5 text-primary">
                  <Bot size={13} />
                </span>
                <div className="min-w-0 flex-1">
                  <Select
                    value={m.name}
                    onChange={(v) => selectByName(i, v)}
                    options={aliasOptions.map((o) => ({
                      value: o.alias,
                      label: o.alias,
                    }))}
                    placeholder={t("setup.vscode.selectModel")}
                  />
                </div>
                <button
                  type="button"
                  onClick={() => removeModel(i)}
                  className="shrink-0 rounded-lg p-1.5 text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive"
                  title={t("common.delete")}
                >
                  <Trash2 size={13} />
                </button>
              </div>

              <div>
                <label className={fieldLabel}>url</label>
                <div className="flex items-center gap-2 rounded-lg border border-border bg-muted/20 px-2 focus-within:border-primary">
                  <Globe size={12} className="shrink-0 text-muted-foreground" />
                  <input
                    value={m.url}
                    onChange={(e) => updateField(i, { url: e.target.value })}
                    placeholder="http://localhost:25999/v1"
                    className="min-w-0 flex-1 bg-transparent py-1.5 font-mono text-xs text-foreground focus:outline-none"
                  />
                </div>
              </div>

              <div className="flex flex-wrap items-center gap-1.5 pt-0.5">
                <CapabilityPill
                  label="toolCalling"
                  active={m.toolCalling}
                  activeClass="border-primary/40 bg-primary/15 text-primary"
                  onClick={() => updateField(i, { toolCalling: !m.toolCalling })}
                />
                <CapabilityPill
                  label="vision"
                  active={m.vision}
                  activeClass="border-success/40 bg-success/15 text-success"
                  onClick={() => updateField(i, { vision: !m.vision })}
                />
                <CapabilityPill
                  label="thinking"
                  active={m.thinking}
                  activeClass="border-warning/40 bg-warning/15 text-warning"
                  onClick={() => updateField(i, { thinking: !m.thinking })}
                />
              </div>

              <div className="grid grid-cols-2 gap-2">
                <div>
                  <label className={fieldLabel}>maxInputTokens</label>
                  <Input
                    type="number"
                    value={String(m.maxInputTokens)}
                    onChange={(v) =>
                      updateField(i, { maxInputTokens: parseInt(v, 10) || 0 })
                    }
                    className="h-8 py-0 font-mono text-xs"
                  />
                </div>
                <div>
                  <label className={fieldLabel}>maxOutputTokens</label>
                  <Input
                    type="number"
                    value={String(m.maxOutputTokens)}
                    onChange={(v) =>
                      updateField(i, { maxOutputTokens: parseInt(v, 10) || 0 })
                    }
                    className="h-8 py-0 font-mono text-xs"
                  />
                </div>
              </div>
            </div>
          ))}

          <button
            type="button"
            onClick={addModel}
            className="flex w-full items-center justify-center gap-2 rounded-xl border border-dashed border-border py-2.5 text-xs font-semibold text-muted-foreground transition-colors hover:border-primary/50 hover:bg-primary/5 hover:text-primary"
          >
            <Plus size={14} />
            {t("setup.vscode.addModel")}
          </button>
        </div>

        {/* 右列：JSON 预览（等高滚动） */}
        <div
          className="min-h-0 space-y-3 overflow-y-auto pr-1"
          style={{ height: "calc(100dvh - 460px)", minHeight: 280 }}
        >
          <FileCard
            title="models"
            currentContent={json}
            previewContent={null}
            isDiff={false}
            language="json"
          />
          <p className="px-1 text-xs text-muted-foreground">
            {t("setup.vscode.note")}
          </p>
        </div>
      </div>
    </div>
  );
};

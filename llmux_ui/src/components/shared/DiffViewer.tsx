// 单栏 diff 查看器：react-diff-viewer-continued 封装
// 颜色用项目 CSS 变量（hsl(var(--token))）→ 明暗/色板自动跟随主题
import React from "react";
import ReactDiffViewer, {
  type ReactDiffViewerStylesOverride,
  DiffMethod,
  type HighlightTheme,
} from "react-diff-viewer-continued";
import { useThemeStore, getSystemTheme } from "@/stores/theme";

interface DiffViewerProps {
  oldValue: string;
  newValue: string;
  /** 限制最大高度，超出内部滚动 */
  maxHeight?: string;
  /** 对比方式：默认行级文本；JSON 结构对比（对象配置如 Claude settings）用 JSON */
  compareMethod?: DiffMethod;
  /** 语法高亮语言（如 "json"、"toml"）——内部用 refractor，语言文件自动分包 */
  highlightLanguage?: string;
  /** 是否折叠未变区块（纯展示高亮时应为 false，避免全内容折叠成一行） */
  showDiffOnly?: boolean;
}

/** 主题 token → CSS 变量引用（跟随明暗/色板） */
const token = (name: string) => `hsl(var(--${name}))`;

/**
 * 语法高亮配色：全部映射到项目语义色 token，跟随色板 + 明暗。
 * 关键原则：
 *  - primary / success / destructive / warning：浅色模式用 -foreground（深字），深色模式用语义色本身（亮字）
 *  - accent / secondary：它们的 -foreground 明暗都适配（accent-foreground dark 88%、light 38%），直接统一用
 * 这样 token 色相完全来自当前色板（lavender/mint/peach...），切换主题自动变化。
 */
const HIGHLIGHT_THEMES: Record<"light" | "dark", HighlightTheme> = {
  light: {
    default: token("foreground"),
    comment: token("muted-foreground"),
    prolog: token("muted-foreground"),
    doctype: token("muted-foreground"),
    cdata: token("muted-foreground"),
    punctuation: token("muted-foreground"),
    property: token("accent-foreground"),
    tag: token("success-foreground"),
    boolean: token("accent-foreground"),
    number: token("accent-foreground"),
    constant: token("accent-foreground"),
    symbol: token("accent-foreground"),
    deleted: token("destructive-foreground"),
    selector: token("secondary-foreground"),
    "attr-name": token("secondary-foreground"),
    string: token("foreground"),
    char: token("foreground"),
    builtin: token("accent-foreground"),
    inserted: token("success-foreground"),
    operator: token("destructive-foreground"),
    entity: token("success-foreground"),
    url: token("accent-foreground"),
    "attr-value": token("foreground"),
    keyword: token("destructive-foreground"),
    atrule: token("destructive-foreground"),
    "class-name": token("primary-foreground"),
    function: token("primary-foreground"),
    regex: token("warning-foreground"),
    important: token("warning-foreground"),
    variable: token("warning-foreground"),
  },
  dark: {
    default: token("foreground"),
    comment: token("muted-foreground"),
    prolog: token("muted-foreground"),
    doctype: token("muted-foreground"),
    cdata: token("muted-foreground"),
    punctuation: token("muted-foreground"),
    property: token("accent-foreground"),
    tag: token("success"),
    boolean: token("accent-foreground"),
    number: token("accent-foreground"),
    constant: token("accent-foreground"),
    symbol: token("accent-foreground"),
    deleted: token("destructive"),
    selector: token("secondary-foreground"),
    "attr-name": token("secondary-foreground"),
    string: token("foreground"),
    char: token("foreground"),
    builtin: token("accent-foreground"),
    inserted: token("success"),
    operator: token("destructive"),
    entity: token("success"),
    url: token("accent-foreground"),
    "attr-value": token("foreground"),
    keyword: token("destructive"),
    atrule: token("destructive"),
    "class-name": token("primary"),
    function: token("primary"),
    regex: token("warning"),
    important: token("warning"),
    variable: token("warning"),
  },
};

export const DiffViewer: React.FC<DiffViewerProps> = ({
  oldValue,
  newValue,
  maxHeight = "360px",
  compareMethod,
  highlightLanguage,
  showDiffOnly = false,
}) => {
  // 订阅主题：useDarkTheme 决定库用哪套 variables（明暗跟随切换）
  const scheme = useThemeStore((s) => s.scheme);
  const isDark = scheme === "system" ? getSystemTheme() === "dark" : scheme === "dark";
  const styles: ReactDiffViewerStylesOverride = {
    // 明暗两套变量：值全是 CSS 变量引用，主题切换自动生效。
    // 背景用语义色低透明（浅绿/浅红），文字用 -foreground（深色字，浅色模式）
    // 或语义色本身（亮字，深色模式）——避免 foreground 深色被误当背景。
    variables: {
      light: {
        diffViewerBackground: "transparent",
        diffViewerColor: token("foreground"),
        addedBackground: "hsl(var(--success) / 0.15)",
        addedColor: token("success-foreground"),
        wordAddedBackground: "hsl(var(--success) / 0.4)",
        removedBackground: "hsl(var(--destructive) / 0.15)",
        removedColor: token("destructive-foreground"),
        wordRemovedBackground: "hsl(var(--destructive) / 0.35)",
        addedGutterBackground: "hsl(var(--success) / 0.25)",
        removedGutterBackground: "hsl(var(--destructive) / 0.25)",
        gutterBackground: "hsl(var(--muted) / 0.5)",
        gutterColor: token("muted-foreground"),
        addedGutterColor: token("success-foreground"),
        removedGutterColor: token("destructive-foreground"),
        codeFoldBackground: "hsl(var(--muted) / 0.4)",
        codeFoldContentColor: token("muted-foreground"),
        emptyLineBackground: "transparent",
      },
      dark: {
        diffViewerBackground: "transparent",
        diffViewerColor: token("foreground"),
        addedBackground: "hsl(var(--success) / 0.18)",
        addedColor: token("success"),
        wordAddedBackground: "hsl(var(--success) / 0.45)",
        removedBackground: "hsl(var(--destructive) / 0.18)",
        removedColor: token("destructive"),
        wordRemovedBackground: "hsl(var(--destructive) / 0.4)",
        addedGutterBackground: "hsl(var(--success) / 0.3)",
        removedGutterBackground: "hsl(var(--destructive) / 0.3)",
        gutterBackground: "hsl(var(--muted) / 0.6)",
        gutterColor: token("muted-foreground"),
        addedGutterColor: token("success"),
        removedGutterColor: token("destructive"),
        codeFoldBackground: "hsl(var(--muted) / 0.5)",
        codeFoldContentColor: token("muted-foreground"),
        emptyLineBackground: "transparent",
      },
    },
    // 折叠提示条：圆角 + 可点
    codeFold: {
      backgroundColor: "hsl(var(--muted) / 0.4)",
      borderRadius: "0.5rem",
      color: "hsl(var(--muted-foreground))",
      cursor: "pointer",
      fontSize: "11px",
      padding: "2px 8px",
    },
    codeFoldGutter: { backgroundColor: "transparent", minWidth: "auto", width: "auto" },
    // 容器：填满父级宽度，不强制 1000px 最小宽（库默认会撑破窄容器）
    diffContainer: {
      width: "100%",
      minWidth: 0,
      borderRadius: "0.75rem",
      border: "1px solid hsl(var(--border))",
      fontSize: "12px",
      fontFamily: "var(--font-mono, ui-monospace, monospace)",
      pre: { lineHeight: "1.5em" },
    },
    // +/- 标记列：收窄 + 不换行
    marker: {
      width: "20px",
      paddingLeft: "6px",
      paddingRight: "6px",
      color: "hsl(var(--muted-foreground))",
      whiteSpace: "nowrap",
    },
    // 行号列：收窄对齐
    gutter: {
      minWidth: "32px",
      width: "32px",
      padding: "0 6px",
      whiteSpace: "nowrap",
      fontSize: "10px",
    },
    lineNumber: { color: "hsl(var(--muted-foreground))" },
    // 行：行高与项目排版一致
    line: { fontSize: "12px", lineHeight: "1.5em" },
    contentText: { whiteSpace: "pre-wrap", lineBreak: "anywhere" },
    // 隐藏空 gutter 占位（库用空 td 撑行高，这里保持透明即可，无需额外处理）
    emptyGutter: { background: "transparent" },
    emptyLine: { background: "transparent" },
  };

  return (
    <div className="min-w-0 overflow-auto" style={{ maxHeight }}>
      <ReactDiffViewer
        oldValue={oldValue}
        newValue={newValue}
        splitView={false}
        showDiffOnly={showDiffOnly}
        extraLinesSurroundingDiff={2}
        leftTitle=""
        compareMethod={compareMethod}
        highlightLanguage={highlightLanguage}
        highlightTheme={HIGHLIGHT_THEMES[isDark ? "dark" : "light"]}
        useDarkTheme={isDark}
        styles={styles}
      />
    </div>
  );
};

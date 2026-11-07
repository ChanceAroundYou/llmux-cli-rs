# 上游 UI 吸收分析（只分析，不改代码）

日期：2026-11-07。分析对象：

| 侧 | 位置 | 规模 |
|---|---|---|
| 上游 | `upstream/main:llmux_ui/`（解到 `/tmp/llmux_ui_upstream/llmux_ui/` 只读副本，取 `upstream/main` = `3b68a2d` v0.5.26） | `src/**` 152 个文件里 84 个 `.ts/.tsx`，共 9 997 行 |
| 我们 | `ui/src/` | 49 个 `.ts/.tsx`，共 9 564 行（其中 `routes/models.tsx` 1 573 行） |

比对口径：双侧都按**文件:行号**给证据；「没有」是 grep 全 `src` 无命中后的结论，命中用的 pattern 写出来供复查。
**本轮不动任何前端代码**（用户确认的 scope：分析 + 聊天摘要）。

---

## A. 值得吸收（按 收益/改动量 排序）

### A1 失败要说出来：toast / 可见错误反馈 —— 收益最高、改动最小

- **上游**：`components/ui/Toast.tsx` + `hooks/useToast.ts`，`components/layout/AppLayout.tsx:6` 挂 `ToastContainer`；
  每个数据加载失败都 `toast.error(...)`：`pages/models/ModelBrowser.tsx:79,85,117`
  （`const toast = useToast();` → `toast.error(t("models.loadFailed"))`）、
  `pages/models/ModelHealth.tsx:23`（把 `onError` 直接接给 `useCachedData`）。
- **我们**：`grep -rn console.error ui/src | wc -l` = **17**，例如 `ui/src/routes/models.tsx:229,476,572`。
  `ui/src/stores/models.ts:236` 的 `fetchAggregateAliases` 失败只 `console.error`，
  不写 `error` state —— 聚合列表拉不到时界面上**长得像「一个聚合别名都没有」**。
  （对照：`ui/src/stores/models.ts:306-308` 的 `addAlias` 会 `set({error})`，说明我们已经在做，只是不成体系。）
- **价值**：高。这是个人网关最常见的排障起点 —— 面板静默空态会被读成「配置没了」，
  而真实原因往往是会话过期或后端 500。我们在 ② 里刚给别名冲突加了可见 notice
  （`ui/src/routes/models.tsx` 的 `aliasModeNotice`），说明「可见失败」这条路本来就在走，缺的是统一入口。
- **代价**：纯前端。一个 `Toast` 组件 + 一个 `useToast` + 把 17 处 `console.error` 改成
  「console + toast」。**不需要后端任何改动**。
- **不一致风险**：我们的 `apiFetch` 不解析错误体（见 A2），所以 toast 拿到的文案质量取决于 A2。

### A2 错误解析集中一处：`apiFetch` 层统一 `error` / `code` / `status`

- **上游**：`api/client.ts:5-21` —— axios 实例（`baseURL:"/api"`、`timeout:30000`）+ 一个响应拦截器，
  把 `data.error`（字符串或 `{message}`）统一成 `Error`，调用方只 `catch`。
- **我们**：`ui/src/lib/api.ts:1-6` 只拼 base path + `credentials:"include"`，没有错误处理；
  于是每个 store 函数各写一遍：`ui/src/stores/models.ts:294-302`（`addAlias`：解析 `data.error`、
  手工挂 `err.code` / `err.conflict` / `err.status` / `err.existingId`）、
  `ui/src/stores/models.ts:328`（`saveAggregateAlias`：又写一遍同样的四行）、
  `ui/src/stores/models.ts:340,347`（`setAggregateActive/Lock`：再写一遍）。
  同一个后端错误形状被解析了至少 4 次，字段名逐处手抄。
- **价值**：高，而且是**腐坏防护**型收益：② 这次新增的 `existing_id` 就是必须逐个调用点加的行；
  下一个新字段（比如 ③ 的 `notes`）还要再加一遍。集中后新字段只改一处。
- **代价**：中等偏小。改 `apiFetch` 让它 `throw` 一个带 `status/code/conflict` 的 `Error`，
  然后各 store 删掉重复块（可以逐个 store 迁移，不必一次全改）。
  ⚠️ 别引入 axios：我们只有一处 fetch 语义要改，加依赖不划算（`ui/package.json` 现在没有 axios；上游有）。
- **不一致风险**：后端错误体不统一 —— `/api/*` 用 `{"error": "..."}`，
  `/v1/*` 用 Anthropic/OpenAI 形状（`{"error":{"type":...,"message":...}}`）。
  集中层要同时认这两种，别只认一种（`crates/llmux-server/src/error.rs` 是判据来源）。

### A3 `useCachedData`：跨页面缓存 + 过期后台静默刷新 + 延迟骨架

- **上游**：`hooks/useCachedData.ts:1-101`（模块级 `Map` 缓存，注释直接写明三条语义：
  命中就直接展示旧数据不闪骨架 / 过期则后台刷新写回 / `setData` 函数式更新也写回缓存）；
  `hooks/useDelayedLoading.ts:7-24`（请求在 200ms 内完成就**永远不渲染骨架**）；
  实际用法 `pages/models/ModelHealth.tsx:19-24`：`{ ttlMs: 20_000, onError: () => toast.error(...) }`（onError 在 `:23`）。
- **我们**：`grep -rn "skeleton|delayed" ui/src --include=*.tsx` 无命中 —— 没有延迟骨架。
  每次进入页面都重取：`ui/src/routes/dashboard.tsx:127`（mount 即 `loadAll()`）
  加 `:131-135`（活动流单独再拉一次 `/api/activity?limit=100`）。
  `ui/src/stores/models.ts` 里 `fetchSummary` 只在被调用时取，没有 TTL，
  所以「models 页 → dashboard 页 → 回 models 页」是**两次全量拉取**。
- **价值**：中高。这个网关跑在 2GB 的 OpenWRT 路由器上，`/api/dashboard`、`/api/activity?limit=100`、
  `/api/models/summary` 都不是白拿的；而面板切页频率很高（排障时来回切）。
  另一条收益是**避免闪空**：现在切到 models 页会先见空列表再填数据，
  排障时容易误读成「别名没了」。
- **代价**：中等。可以只给三个高频页（dashboard / models / logs）接，不动其余页。
  ⚠️ 要注意与 `fetchSummary` 的分工：缓存 key 要和 store 的写入点统一，
  否则会出现「store 里改了、hook 缓存里还是旧的」——上游用 `update()` 写回缓存正是为了堵这个（`useCachedData.ts:85-99`）。
- **风险**：如果缓存 key 与 store 双写，等于两套真相。**只在 A2 之后做**，先统一数据入口再谈缓存。

### A4 后端已有、界面上摸不到的能力：`/api/system/*` 的预览 / diff / 备份恢复

- **我们后端**：`crates/llmux-server/src/app.rs:388-429` 已经把
  `/api/system/{claude,codex,gemini}-settings`、`/api/system/{claude,codex,gemini}-backups`
  （GET 列表 / POST 恢复 / DELETE）全套挂上了；① 新加的 `routes/system/codex_toml.rs` 就是它的实现层。
- **我们前端**：`grep -rn "system/codex-settings|system/claude-settings|system/gemini-settings|system/codex-backups" ui/src`
  → **零命中**（只有 `system/codex-device`）。也就是说 ① 修好的那三处「会写坏用户 config.toml」的缺陷，
  在界面上没有任何入口去验证或回滚。
- **上游**：一整套现成的呈现层 —— `pages/settings/CliSettings.tsx`（250 行）、
  `components/setup/SettingsPreview.tsx`（139）、`CodexSettingsPreview.tsx`（113）、
  `BackupHistory.tsx`（184）、`components/shared/DiffViewer.tsx`（218，
  包 `react-diff-viewer-continued`，见 `:1,8`）、`FilePreview.tsx`、`ApplyButton.tsx`。
- **价值**：中高，但**只对「谁在改这份配置文件」成立**。见 B1 —— 在路由器容器里
  `check_tool()` 全回 false（`crates/llmux-server/src/routes/system/mod.rs:29-42`），
  没有本地 CLI 去读那些文件。真正有意义的场景是：**用 llmux 当「配置写入器」**
  （把 `~/.codex/config.toml` 写到挂载卷 / 或给跑在本机的 llmux 用）时，
  ① 的安全编辑 + 一个「改前/改后 diff + 备份回滚」面板才是闭环。
- **代价**：中等（一个 diff 组件 + 一个备份列表组件，端点已齐）。
- **判断**：**形态可吸收，入口要自己定**。别照搬上游的「工具已安装 → 显示面板」逻辑，
  我们的容器永远不满足那个前提。

### A5 别名保存前先验证连通性（上游的门禁做法）

- **上游**：`pages/models/CustomAliasModal.tsx:1` 的文件头注释就写着流程
  「选账户 → 手输模型 → **先验证连通性 → 通过才可保存**」，代码是
  `:104` `<Button loading={saving} disabled={!verifyResult?.success} ...>`，
  `:166-182` 显示 `verifyResult.success` + 延迟秒数或错误串。
- **我们**：`ui/src/routes/models.tsx` 的别名保存没有任何前置验证；`grep -c "testModel|models/test" ui/src/routes/accounts.tsx` = **0**
  （账户页也不能就地验证）。验证只在保存**之后**、通过独立的拨测入口发生。
- **价值**：中高。这正是我们最典型的失败形态 —— 别名建出来了、绑了停用/不匹配的账户，
  第一次真实请求才 502/503。而 ③ 里我们在**保存时**已经做了「引用别名不存在 → 400」这类校验，
  连通性门禁是同一条思路的延伸（注意：**不要**把它做成硬门禁 —— 上游那样 `disabled` 会让
  「上游正在抽风但绑定是对的」这种情况存不下来；我们后端 2026-08 已经决定强制协议不在保存时拒，
  同样的克制应该用在这里：**验证结果作为提示，不作为拦截**）。
- **代价**：小（`POST /api/models/test` 已存在，`ui/src/stores/models.ts:373-384` 已有 `testModel`）。

### A6 `routes/models.tsx` 的拆分边界（1 573 行单文件）

- **上游**：models 域拆 4 个文件：`ModelBrowser.tsx`（697）、`AliasFormModal.tsx`（261）、
  `CustomAliasModal.tsx`（190）、`ModelHealth.tsx`（117）。
- **我们**：`ui/src/routes/models.tsx` 1 573 行一个组件。文件内已经天然分段
  （`grep -n "Alias Modal|Aggregate Alias Modal|Filters & Tabs"`）：
  `:833` 起 列表/筛选、`:1084` 起「Add Alias Modal」、再往下「Aggregate Alias Modal」、
  最后 `:1503` 起四个 `ConfirmDialog`（删除确认、覆盖确认等）。
- **价值**：中。本轮 ③ 的前端改动全在这一个文件里，改 candidate 行要同时碰
  状态定义（`:186`）、`openAggregateModal`、`handleSaveAggregate`、modal JSX、卡片 JSX 五处 —— 
  同一功能的改动散布在 1 500 行里，是 review 与误改的主要来源。
- **建议边界**（按「先切最独立的」）：① 两个 modal 各自成文件（状态跟着 modal 走，
  `aggregateForm`/`aliasForm` 一起搬）；② 卡片渲染（别名卡 / 聚合卡）成组件；
  ③ 保存/确认流程**先不动**，等 ①② 落地后再看剩多少。
- **代价**：中（纯搬运，但要一次性搬对；**别和功能性改动混在同一次提交**）。
- **不建议**照搬上游的文件名和目录（`pages/...`）—— 我们是 `routes/ + stores/`，改名会带来无谓 diff。

---

## B. 看似值得、实际不适用

### B1 「本机 CLI 配置」整块（上游 UI 最大的单一板块）

上游 `components/setup/**`（panels 4 个：Codex 388 / ClaudeCode 364 / VSCode 311 / Gemini 281，
`components/setup/**` 全目录 **2 629 行**（panels + `KeySelector`、`ModelRoleSelect`、`ToolSidebar`、`ToolHeader`、`SettingsPreview`、`BackupHistory`、`FilePreview`、`ApplyButton`、`useToolBackups`）+
`pages/settings/CliSettings.tsx` 250 行，全为「llmux 与 claude/codex/gemini/vscode CLI 同机」这个前提服务。

我们的部署里这个前提不成立：`/api/system/tools` 的判据是 `check_tool("claude"|"codex"|...)`
（`crates/llmux-server/src/routes/system/mod.rs:29-42`），而容器是 `debian:trixie-slim + COPY llmux`
（`deploy/Dockerfile`）—— 没有任何一个 CLI 在里面。照搬会做出一屏永远灰着的面板。
**结论**：内容不适用；可吸收的只有 A4 那三件呈现（预览 / diff / 备份列表）。

### B2 `pages/vendors/VendorList.tsx`（311 行）+ `api/vendors.ts`

上游把 vendor 当一等实体（`/api/vendors` 的 GET/POST/PUT/DELETE，见 `api/vendors.ts` 的调用；
`AccountForm.tsx` 里 alias payload 带 `vendor_id`）。
我们后端**没有 `/api/vendors`**（`crates/llmux-server/src/app.rs` 的路由表里没有）；
`vendor` 在我们库里只是**价格维度**（`migrations/0028_upstream_prices.sql:28`、`0027_model_prices_cost.sql:26`，
以及 `ui/src/routes/prices.tsx:13,40`）。吸收这一页等于先做一轮后端建模 —— 不是 UI 吸收，是功能设计。

### B3 `hooks/usePolling.ts`（17 行）

`setInterval` + enabled 开关的薄封装。我们已有等价实现（`ui/src/routes/models.tsx:291,301`、
`ui/src/components/CodexLoginDialog.tsx:95`）。吸收它只是换个名字，**无收益**。

---

## C. 我们已有、上游 UI 没有的（别反向「对齐」掉）

1. **聚合别名整块**：`grep -rn aggregate /tmp/llmux_ui_upstream/llmux_ui/src` → **零命中**。
   上游 UI 没有 `active`/`locked`/`pending_target`/`confirm_count`/`last_status` 这些状态的呈现，
   也没有拨测冷却、3-confirm 迁移、③ 的来源别名引用。我们的
   `ui/src/stores/models.ts:60-75` + `ui/src/routes/models.tsx` 聚合卡是**我们独有且更复杂**的一块。
2. **价格与用量**：`grep -rlin "price|价格" /tmp/llmux_ui_upstream/llmux_ui/src` → **零命中**；
   我们有 `ui/src/routes/prices.tsx`（385 行，含编辑草稿 `:47`）与 `ui/src/routes/stats.tsx`（572 行）。
3. **模型清单的流式加载**：我们有 `/api/models/available/stream` 并真在客户端读流
   （`ui/src/stores/models.ts:148-215`，`snapshot`/`account`/`done` 事件）；
   上游 `api/models.ts` 里没有 SSE/流式读取。逐账户增量对我们这种多上游、加载慢的场景是实打实的改进。
4. **Codex 设备码登录向导**：`ui/src/components/CodexLoginDialog.tsx` + `WebLoginWizard.tsx`
   对着 `/api/system/codex-device`（`app.rs:415,419`）；上游 UI 只有 `CodexPanel` 的 key/baseUrl 表单。

---

## D. 明确不建议照搬

| 项 | 上游位置 | 为什么别照搬 |
|---|---|---|
| axios 实例 | `api/client.ts:5-9`（依赖 axios + 拦截器） | 我们只有一层 fetch 要统一（A2）；为统一错误引入运行时依赖不划算 |
| `framer-motion` 动效 | `components/layout/AppLayout.tsx:4`、`Sidebar.tsx:3`、`TopBar.tsx:3`，以及 `components/shared/AnimatedNumber.tsx` | 上游 `package.json` 里 motion 是主依赖之一；面板跑在路由器托管的静态资源上，动效换来的只有等待时间 |
| `react-router-dom` 的 `router/index.tsx` 组织 | `router/index.tsx` | 我们已有 `App.tsx` 路由 + `routes/*`；换组织方式只产生 diff |
| `types/*` 全量重命名成 camelCase 契约 | `types/account.ts` 等 | 我们已经在 `ui/src/stores/models.ts:22-25` 吃过「前端写 camelCase、后端实际 snake_case → 字段恒 undefined」的亏。正确做法是**照后端的 wire 形状命名**（现在就是这么修的），不是再引入一层映射 |
| 「验证不通过就不许保存」的硬门禁 | `CustomAliasModal.tsx:104` | 与仓库既有克制冲突（强制协议不在保存时拒，`docs/superpowers/plans/2026-08-23-upstream-downstream-split.md`）。改成提示，不改成拦截 |

---

## E. 如果只做三件事

1. **A1 toast（含把 17 处 `console.error` 变成「console + 可见提示」）** —— 最小、立刻减少误读。
2. **A2 `apiFetch` 统一错误解析** —— 一次改动，之后每个新错误字段只改一处；也是 A1 的前置。
3. **A6 两个 modal 出文件** —— 纯搬运，把 1 573 行的 models 页降到能安全改的程度，
   为后面任何一项（含 ③ 的后续迭代）降低风险。

A3 缓存与 A4 diff/备份面板放第二批；A5 连通性提示可搭在 A2 之后一起做。
本轮不动代码（用户 scope：分析 only）。

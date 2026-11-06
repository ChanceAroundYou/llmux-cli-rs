# Codex 订阅接入 llmux：正确步骤与 2026-11-06 的排障记录

> 结论先说：**账户必须 `category=codex`**，光有 `provider_id=codex` 不够。
> 今天「codex 加载不出模型」的直接原因就是这个字段是 `custom`。

---

## 1. 唯一的判据

```rust
// crates/llmux-core/src/adapters/mod.rs:723
pub fn is_codex_native(account: &Account) -> bool {
    account.category == "codex"
}
```

判据为假时，**三件事同时静默关闭**，而且都不报错：

| 被关掉的能力 | 位置 | 缺了会怎样 |
|---|---|---|
| 私有头（`originator` / `ChatGPT-Account-ID` / `session_id` / `OpenAI-Beta`）+ 用 `codex_auth_json` 里的 access_token 作 Bearer | `adapters/mod.rs:610-672` | 拿账户的 `api_key` 当 Bearer 发出去（对 codex 账户那是 sidecar 时代遗留的内网 key） |
| Responses 负载预处理 `prepare_response` + `normalize_codex_payload`（强制 `store=false`、删 `max_output_tokens`、`stream=true`、`include=["reasoning.encrypted_content"]`、string input → 数组） | `adapters/mod.rs:675-678` | 上游回 `400 {"detail":"Store must be set to false"}` |
| 令牌生命周期：请求前 `is_expired → refresh_and_save`、401 → 刷新后重试一次 | `routes/v1/openai.rs:248-265 / 297-320` | 过期的 access_token 被原样发出去 → 401，而且永远不会自愈 |

另外 `protocol.rs:70` 和 `probe.rs:174` 也用同一个 `category=="codex"`：
前者决定「codex 只有 Responses 一种形状」，后者决定「拨测只探 Responses」。

## 2. 端点不用配，代码钉死

```rust
// routes/accounts.rs:24 pin_fixed_endpoints —— create 和 update 都会覆盖
if category != "codex" { return; }          // 非 codex 账户不动
base_url           = CODEX_BACKEND_BASE;    // https://chatgpt.com/backend-api/codex
responses_endpoint = CODEX_BACKEND_BASE;
chat_endpoint      = None;
messages_endpoint  = None;
default_protocol   = "responses";
```

所以建 codex 账户时**不要**手填端点，填了也会被覆盖；反过来，`category=custom` 时这些字段全部由用户配置生效 —— 今天账户 60 就是靠自己填的 `base_url=https://chatgpt.com/backend-api/codex` 才勉强拼出 `/responses`，而 `/chat/completions` 那条拼出来的是**不存在的路径**。

---

## 3. 2026-11-06 的故障：症状 → 证据 → 根因

症状（面板上看到的）：codex 账户「加载模型」失败，模型列表里只有一个 `codex-models-unavailable` 占位卡。

生产日志（NAS 归档，逐条真行）：

```
22:07:28 WARN 🧪 [agg] gpt-6-luna | codex | FAILED: Request failed: error sending request for url
                (https://chatgpt.com/backend-api/codex/chat/completions)
22:07:28 WARN ⏸️  gpt-6-luna | codex 连续失败，暂停自动拨测 30 分钟
22:19:12 WARN 🤖 [codex] listModels HTTP 401 for codex: Provided authentication token is expired.
                Please try signing in again.
22:24:37 WARN 🔀 [agg:op] Account codex (id=60) failed (400) — trying next (non-retryable):
                {"detail":"Store must be set to false"}
```

四条对应四个后果：拨测打不存在的 `chat/completions` → 聚合别名 `op` 一直不健康；
加载模型拿过期 token → 401；真实请求缺 `store:false` → 400；整段日志里**一条 `🔑 codex token refreshed` 都没有** → 刷新分支压根没被走到。

**根因**（`GET /api/accounts` 实测，不是猜）：

```
id=60  alias=codex  provider_id=codex  category=custom  codex_configured=true
```

全库 14 个账户，**没有任何一行 `category='codex'`**。`is_codex_native` 恒假。

**修复**（走 API，没碰 DB）：

```
PUT /api/accounts/60 {"category":"codex"}
```

`pin_fixed_endpoints` 随即把端点和 `default_protocol` 钉成 responses（账户 60 原本就是这个值，等于没变），
判据转正，三样能力一起回来。

**修完重跑同一条复现路径**：

- 加载模型：codex 回到 **10 个模型**（`gpt-6.1-sol / gpt-6-astra / gpt-6.1-terra / gpt-6-sol / gpt-reserve / gpt-6-luna / gpt-5.6-sol / gpt-5.6-terra / gpt-5.6-luna / codex-auto-review`），`codex-models-unavailable` 占位符清零
- 真实请求 `POST /v1/chat/completions {"model":"op"}` → **HTTP 200 / 5.08s**，返回 `gpt-6-luna` 的正常补全
- 日志出现：`INFO codex token refreshed for account id=60` + `INFO 🔑 codex token refreshed pre-request for account codex`
  → **refresh_token 一直是好的**，之前只是没有代码路径去用它

顺带一条：账户 60 的 `api_key` 里存的是 sidecar 时代的内网 key（`codex_configured=true` 时它不参与认证，也不参与余额探测 —— `accounts.rs:96` 对 codex 账户改用 `codex_auth_json` 的 access_token）。别拿它判断凭据是否有效。

---

## 4. 正确的添加步骤

### 4.1 建账户

面板或 API 都行，关键是 **category 选 codex**：

```
POST /api/accounts
{
  "alias": "codex",
  "provider_id": "codex",
  "category": "codex",          ← 缺省是 custom，必须显式给
  "api_key": "",                ← 占位；真实凭据在 codex_auth_json
  "balance_provider": "codex",  ← 余额走 access_token 打 wham/usage
  "notes": "ChatGPT/Codex 订阅（原生路径）"
}
```

端点字段一律不填（会被覆盖）。校验不会拦住建号（`skip_validation` 早就不拦了）。

### 4.2 获取凭据：界面上点一下（2026-11-07 起首选）

账户卡片上 `category=codex` 的账户有一个登录按钮（已配凭据时是绿色，未配是橙色）。点开对话框：

1. 点「发起登录」→ llmux 向 `auth.openai.com` 申请一次性码，界面把**码**和**打开授权页**的按钮直接给你；
2. 点按钮跳到 `https://auth.openai.com/codex/device`，在已登录的 ChatGPT 会话里输入那个码并确认；
3. 界面每 3 秒刷一次状态，几秒内变「登录完成」，并回显**邮箱 / 套餐 / 工作区 / 可加载模型数**，同时刷新账户页与模型列表。

不用开命令行，不用 `codex login`，不用 `CODEX_HOME`。细节见 §7。

### 4.2b 命令行路径（仍可用）

```bash
cd /home/xiaokubao/projects/llmux
# 本地实例
./scripts/codex-login.sh --account-id 60

# 生产实例（带基路径）
LLMux_BASE=https://openwrt.xiaokubao.space/llmux ADMIN_USER=admin ADMIN_PASS=… \
  ./scripts/codex-login.sh --account-id 60

# 已登过、凭据还在，只想重导一次
./scripts/codex-login.sh --account-id 60 --reuse
```

脚本做的事：独立 `CODEX_HOME`（默认 `scripts/../.codex-login`，**不碰 `~/.codex`**，本机登录不受影响）
→ `codex login --device-auth`（人在浏览器输一次性码）→ `POST /api/system/codex-import {account_id, auth_json}`。
导入前按 `auth_mode=chatgpt` + `access_token` 非空校验，脏 JSON 进不了库。
返回里的 `token_expired: true` **不是错误**：access_token 过期但 refresh_token 在，第一个真实请求会自刷（脚本那行提示就是这个意思）。

⚠️ **`--from-router` 现在不要用**。它读的是路由器 `/root/docker/llmux/codex-auth/auth.json`，那份 2026-10-12 之后就没动过（实测 `last_refresh=2026-10-12T08:10Z`，access_token 已于 2026-10-22 过期）。llmux 这一个月里的每次自刷都轮换过 refresh_token（今天 22:29:50 又刷了一次），路由器那份的 refresh_token **大概率已作废** —— 我没有实测去验它（试一次就会真的轮换掉 llmux 正在用的那把），但按「一次性轮换 + 那份文件一个月没动」推下来，导进去等于装一把失效钥匙，症状是「请求前刷新失败」而不是 401。真要重来一次，走 `codex login --device-auth`。

### 4.3 挂别名

codex 后端**只认 Responses**，别名侧必须落到 responses：

```
POST /api/models/aliases
{"alias":"gpt6luna","target_model":"gpt-6-luna","account_ids":[60],"upstream_api":"responses"}
```

聚合别名同理（现有的 `op` 就是 `upstream_api:"responses"` + `candidates:[{account_id:60,model:"gpt-6-luna"}]`）。
`upstream_api` 给 `chat` 会去打 `/codex/chat/completions`（不存在）—— 今天拨测失败就是这个形状。
跨端点会被硬绑拒：`400 ModelProtocolUnsupported`。模型清单以「加载模型」那一轮为准，上游会换代。

### 4.4 验证（三条都要跑）

1. `GET /api/models/available?force=true` → codex 名下有真模型，不是 `*-models-unavailable`
2. `POST /v1/chat/completions {"model":"<别名>", "max_tokens":8}` → 200
3. 日志里那一发应该是 `→ https://chatgpt.com/backend-api/codex/responses`，
   **不是** `…/codex/chat/completions`；首次遇到过期 token 时还应看到 `🔑 codex token refreshed pre-request`

`docker-compose` 侧不需要任何改动：codex sidecar 已在 2026-10-13 退役，`codex-auth/` 目录只是历史凭据来源。

---

## 5. 令牌生命周期（实测 + 代码）

- access_token 有效期约 10 天（路由器那份 `last_refresh=10-12`、`exp=10-22`）。
- llmux **三条**刷新路径，判据同为 `is_expired()`（提前 30s 判定）：
  1. 真实流量请求前 —— `openai.rs:253`，日志 `🔑 codex token refreshed pre-request`
  2. 真实流量 401 后 —— `openai.rs:303`，刷新并重试一次
  3. **加载模型 / 拨测 / 保存别名前的校验前** —— `codex_auth.rs:ensure_fresh`，日志 `🔑 codex token refreshed pre-probe/list`（2026-11-07 新加，见 §6）

  刷新成功都会把新 `auth.json` 写回 `accounts.codex_auth_json`（`codex_auth.rs:refresh_and_save`）。
- **refresh_token 是一次性轮换的**：同一份 auth.json 在两处分别刷新会互踢（`codex-login.sh` 头部注释就是这个原因，也是 `CODEX_HOME` 要隔离的原因）。正因为轮换，llmux 内部所有刷新都收敛在 `codex_auth.rs` 一处；界面登录成功也只是往同一列写一份新的。
- 凭据为空时**不回退**旧 sidecar 地址：`adapters/mod.rs:716` 的注释写明「真回退就是去打一个不存在的容器」。凭据缺失时带空 Bearer 去上游拿 401，错得清楚。

## 6. 空闲账户的刷新缺口（已修，2026-11-07）

原缺口：`routes/models/available.rs` 和聚合拨测（`aggregate_probe.rs` → `core::probe`）只 `load_creds`，**不查 `is_expired`、不刷新**。
有真实流量的 codex 账户没问题；**空闲账户**一旦 access_token 到期就自我锁死：
拨测 401 → 连续失败暂停 30 分钟 → 加载模型 401 → 别名不健康 → 更没有真实流量 → 没人去刷新。

修法：把判据和刷新收进 `codex_auth::ensure_fresh(pool, &account) -> Account`（`crates/llmux-core/src/codex_auth.rs`），
到期就先 `refresh_and_save` 并返回**刷新后的账户**，失败则原样返回（让上游去 401，错得清楚）。调用点：

| 调用点 | 位置 |
|---|---|
| 加载上游模型清单（所有 provider 共用的入口） | `routes/models/available.rs:402`（`fetch_provider_models`） |
| 聚合别名拨测每个候选 | `aggregate_probe.rs:393` |
| 保存别名时的目标校验 | `routes/models/verify.rs:58` |
| 手工单发 / 批量拨测 | `routes/models/testing.rs:775 / 985` |

`ensure_fresh` 只在 `is_codex_native` 且有凭据时才动，非 codex 账户是零开销的直返。

## 7. 界面里的 Codex 设备码登录（2026-11-07）

账户页 → codex 账户 → 登录按钮 → 「发起登录」。全程不开命令行。

底层就是官方 CLI 用的那套 device-auth，三个请求（`crates/llmux-core/src/codex_device.rs`）：

| 步 | 请求 | 结果 |
|---|---|---|
| 1 | `POST auth.openai.com/api/accounts/deviceauth/usercode` `{client_id}` | `device_auth_id` / `user_code` / `interval` / `expires_at`（实测 interval=5、窗口 +15 分钟） |
| 2 | `POST …/deviceauth/token` `{device_auth_id, user_code}` | 没人授权时 403 `{"error":{"code":"deviceauth_authorization_pending"}}`；授权后回 `authorization_code` + `code_challenge` + `code_verifier` |
| 3 | `POST …/oauth/token`（**form**）`grant_type=authorization_code` + `code` + `code_verifier` + `redirect_uri=https://auth.openai.com/deviceauth/callback` | `access_token` / `refresh_token` / `id_token` |

两条不能动的地方：PKCE 的 `code_verifier` 是服务端在第 2 步给的（我们不自己生成）；**第 3 步绝不重试** —— 超时也可能已经把一次性码消费掉了。

服务端 `crates/llmux-server/src/routes/system/codex_device.rs`：会话只在内存（重启等于重新发起，本来就得重来），15 分钟窗口，最多 3 个待授权会话，按上游 `interval` 轮询（撞 429 加 5 秒、上限 60 秒），连续 5 次传输失败结束会话，关对话框即取消。

- **一次性码和 `device_auth_id` 是这次登录的凭据对**：只有轮询任务看得到 id，界面和 API 只回 `user_code`（谁拿到 id 就能替这个账户绑走登录）。
- 日志里的码只打前两位（`mask_code`）；界面拿全码，因为人要输入它。
- 登录守卫：只有 `category=codex` 的账户能登 —— 防止把 codex 凭据写进 custom 账户，那正是 2026-11-06 那次故障的形状。
- 落库后立刻用这份新凭据拉一次模型清单，把「可加载模型数」回显给人，并把模型缓存置空。凭据缺 `refresh_token` 时**拒绝入库**（那种凭据会在 access_token 到期时失效）。

生产实测（2026-11-07，账户 60）：

- 未登录 `GET /api/system/codex-device` → 401；带会话 → 200 `{"sessions":[]}`
- `POST {account_id:60}` → 200，真码 10 位，授权页 `https://auth.openai.com/codex/device`，`remaining_secs=899`
- 后台轮询自己把 `poll_count` 推到 2 / 3（真的打到 `auth.openai.com`，403 pending 被认成 Pending 而不是 Failed）
- 取消 → `status=failed` / `message=已取消`，`poll_count` 随即不再变
- 响应字段里没有 `device_auth_id`；日志那行是 `一次性码 3D********`

**没走通的最后一段**：第 3 步（换 token）必须人在浏览器里输码确认，我没有代做 —— 实测只到 Pending → 取消。凭据真正落库那一段的路径（`finish_login` → 写 `codex_auth_json` → 拉模型）只有测试覆盖，等真人登录一次才算验证过。

## 8. 风险提醒

走的是 ChatGPT 订阅的**未公开私有后端**（`backend-api/codex` + 私有头），随时可能被上游改掉；可能违背 OpenAI ToS，有账号风控风险。私有 API、私有头、`client_version` 都跟着官方 CLI 走（`CODEX_MODELS_CLIENT_VERSION`），失效时先看 CLI 版本，别急着怀疑 llmux。
设备码登录用的是官方 CLI 同一个 `client_id` 和同一条 device-auth 流程 —— 登录这件事本身不额外增加伪装面，但换来的是同一套随时可能失效的私有凭据。

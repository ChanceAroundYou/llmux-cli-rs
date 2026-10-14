# 严格 chat 上游拒收 Anthropic 专有字段 —— TDD spec

> 状态：**待实现**（本文件只描述契约与测试，不含实现）。写这份 spec 时仓库工作区是干净的，
> 没有任何生产代码改动。
> 相关：`AGENTS.md` 的「别在这个仓跑 cargo fmt」、「`[profile.dev] debug = 0`（panic 只有函数名，没有文件:行号）」。

## 0. 怎么用这份 spec

按 TDD 顺序执行，**每一步先看着它红**再实现：

| 步 | 动作 | 允许改哪里 |
|---|---|---|
| 1 | 写 U1–U5（§4.1） | 只改 `crates/llmux-core/tests/anthropic_openai_contract.rs` |
| 2 | 跑 `~/.cargo/bin/cargo test -p llmux-core --test anthropic_openai_contract`，逐条确认红在 §4.1 写明的断言上 | — |
| 3 | 实现 §5 的最小改动 | 只改 `crates/llmux-core/src/proxy/anthropic_openai.rs` |
| 4 | 同一命令转绿；顺手改掉 §4.3 的旧断言 | 同上 |
| 5 | 写 E1（§4.2），跑 `cargo test -p llmux-server --test e2e_protocol_split` 确认红 | 只改测试文件 |
| 6 | 无需再改实现（E1 应立刻绿）；若红说明 §5 漏了路径 | — |
| 7 | `~/.cargo/bin/cargo test`（全量，见 §6） | — |

⚠️ 别跑 `cargo fmt`（见 `AGENTS.md`）。手写风格对齐周围代码。
⚠️ 测试 panic 没有行号（`debug = 0`）；需要时临时 `CARGO_PROFILE_DEV_DEBUG=2 cargo test`，跑完关掉。

---

## 1. 根因（2026-10-14 实测，生产复现）

`os` 聚合别名的 V=0 首选是 `cat-jam-free`（账户 42 `go1`，`opencode.ai/zen/go`）。
**它对真实请求 20 次尝试 0 成功**（15×400 + 2×403），而拨测永远是绿的，所以表现为
「拨测通过却从不被使用」。

直连上游、同一 endpoint、只改一个变量的受控实验：

| 发给 `opencode.ai/zen/go` + `cat-jam-free` 的 body | 结果 |
|---|---|
| `content: "Say exactly: OK"`（= 拨测的形状） | **200**（×3） |
| `content: [{"type":"text","text":…}]` | **200** |
| 上一行再加 `"cache_control":{"type":"ephemeral"}` | **400**（×3，确定性） |
| 上一行再加任意未知键 `"zzz":1` | **400** |
| 上一行再加 llmux 自己的 `"prompt_cache_breakpoint"` | **200**（它认） |
| tool 消息带 `"is_error"`（true 或 false） | **400** `unsupported_parameter` |
| tool 消息不带 `is_error` | **200** |

上游原文（`usage_logs` 里有完整正文；日志行被截断到 200 字符，看不到）：

```
[invalid_value] Invalid value for 'messages[0].content': The contents of the message,
either a string or a non-empty array of text parts, or null on an assistant message only.
```

同账户的 `step-5-preview-free` / `longcat-2.5-preview-free` **两个字段都认**（200）。
严格性是 **per-model** 的，不是 per-endpoint、也不是 per-account —— 所以「按 provider 记黑名单」
（provider 都是 `custom`）会误伤，这条路走不通。

### 为什么拨测测不出来

| | 拨测发的（`probe.rs::build_probe_request`） | 真实请求发的（转换后） |
|---|---|---|
| content | `"Say exactly: OK"` 裸字符串 | 带 `cache_control` 的 part 数组 + 带 `is_error` 的 tool 消息 |
| cat-jam 结果 | 200 | 400 |

而**每个**真实 Claude Code 请求都带这两个字段：从 one-shot capture 解出的 436 KB 原始 body
转换后有 **2 个 part 带 `cache_control`**（都在 `messages[0]`，即 Anthropic 的 `system`）+ **83 条
tool 消息带 `is_error`**。去掉 `cache_control` 重打，报错就前进到 `messages[3].is_error`。

### 为什么表现为「总是被跳过」

真实请求打 cat-jam → 400 → `note_candidate_failure(0)`；下游 `step-5` 接住并 200（**所以调用方
看不到任何错误**，只有日志里一行「被跳过」）。连续 3 次 → `V` 迁移到 1。后台探活每 300s 用裸
字符串探、cat-jam 永远 200，又把 `V` 投回 0 —— 稳定拉锯。2026-10-14 实测两次手动切回 0，分别在
**36 秒**和 **55 秒**后被真实流量弹回 1。

### 两个附带的坑（不在本次修复范围，仅记录）

- **`cache_breakpoint_unsupported` 黑名单抓不到它**：它靠错误正文里出现
  `prompt_cache_breakpoint` / `cache_breakpoint` 字样来学习（`routes/v1/anthropic.rs:837`），
  而 content part 的 400 **从不点名是哪个字段**。tool 那条倒点名了（`messages[2].is_error`）。
- **`usage_logs` 里 `cat-jam-free` 的失败行不是它自己的战绩**：聚合整链耗尽时按
  `candidates[0]` 记一行（`routes/v1/anthropic.rs:940-942`），首选会把整条链的失败算到自己头上。

---

## 2. 契约（要满足的行为）

**C1.** `anthropic_to_openai_request*` 产出的 OpenAI chat body 里，任何 content part
**不得带 `cache_control`** —— 包括 `system` 数组里的块。

**C2.** 产出的 chat body 里，任何 tool 消息（`role: "tool"`）**不得带 `is_error`**。

**C3.** 上述剥离**不得改动其它任何字段**：`type` / `text` / `role` / `tool_call_id` / `content` /
`tool_calls` / `reasoning_content` 全部原样保留；part 数组的元素个数不变。

**C4.** llmux 自己的 `prompt_cache_breakpoint` **保持现状**（cat-jam 认它；它已有一整套
provider 级黑名单机制 `cache_breakpoint_unsupported`，本次不动）。

**C5.** 判定标准是**出 llmux 的目标协议**，不是入口协议：`cache_control` 是 Anthropic 形状的
字段，只在「上游是 Anthropic Messages 端点」时才有语义。这类请求**本来就不经此转换器** ——
`routes/v1/anthropic.rs` 在 `anthropic_base` 存在时走 `build_anthropic_passthrough_request`
（客户端原始 body 逐字节转发，`cache_control` 原样保留）。所以「只在 Messages 出站时保留」
在架构上已自动成立；转换器里的剥离**不可能**从一个 Anthropic 形状的上游调用里拿走它。

**C6.** 明确**不做**的事：不引入黑名单 / 学习机制，不按 `(account_id, model)` 记状态。
（§7 说明为什么，以及它的代价。）

---

## 3. 落点

转换器只有 5 个调用点，全在 `llmux-server/routes/v1/`：

| 调用点 | ingress → 上游 target | 修好后 |
|---|---|---|
| `anthropic.rs:224` | Messages → Chat | 剥（本条就是 `os` 走的路） |
| `anthropic.rs:773` | Messages → Chat（聚合，mode=Chat） | 剥 |
| `anthropic.rs:799` | Messages → Chat（聚合，Default/Responses 且无 anthropic_base） | 剥 |
| `openai.rs:225 / 227 / 365 / 369` | Chat ↔ …… 的回退重入 | 剥（输入是 OpenAI 形状，本就无此二字段 → no-op） |
| `openai.rs:2701` | OpenAI 形状 body 打 Anthropic 端点（Copilot 回退） | no-op（同上） |

**关键实现约束**：`system` 那段**不经过** `transform_anthropic_message` —— 它在
`anthropic_to_openai_request_with` 里被单独构造（`strip_billing_header(system)` 直接塞进
`{role:"system", content:[…]}`）。所以**不能**只在 `transform_anthropic_message` 的 `"text"`
分支里删字段 —— 那样正好漏掉本次故障的 `messages[0]`。必须做**一次收尾遍历**。

---

## 4. 测试

### 4.1 单元（`crates/llmux-core/tests/anthropic_openai_contract.rs`）

新增 5 个，命名沿用该文件的 `request_*` 前缀。每条都写明**红在哪个断言**。

---

**U1 `request_drops_cache_control_from_the_system_blocks`** ← 本次故障的直接复现

```rust
#[test]
fn request_drops_cache_control_from_the_system_blocks() {
    // 生产实测：Claude Code 的顶层 system 是带 cache_control 的块数组，转换后成为
    // messages[0]；严格 chat 上游（Console Go 的 cat-jam-free）对它回
    // `[invalid_value] Invalid value for 'messages[0].content'`。块数组与文本都要留住，
    // 只把 cache_control 拿掉。
    let req = anthropic_to_openai_request_with(
        &json!({
            "model": "m",
            "system": [
                {"type": "text", "text": "You are terse.", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "Tools are available.", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [{"role": "user", "content": "hi"}]
        }),
        "t",
        false, // 不给 user 块加 marker，排除干扰
    )
    .unwrap();

    let sys = &req["messages"][0];
    assert_eq!(sys["role"], "system");
    let parts = sys["content"].as_array().expect("system 块数组必须保持数组");
    assert_eq!(parts.len(), 2, "块个数不得改变");
    assert_eq!(parts[0]["text"], "You are terse.");
    assert_eq!(parts[1]["text"], "Tools are available.");
    assert!(parts[0].get("cache_control").is_none(), "system 块不得带 cache_control");
    assert!(parts[1].get("cache_control").is_none(), "system 块不得带 cache_control");
}
```

**红**：最后两条 `assert!(…is_none())` 失败 —— 今天 `parts[0]` 是
`{"cache_control":{"type":"ephemeral"},"text":"You are terse.","type":"text"}`。

---

**U2 `request_drops_cache_control_from_user_text_blocks`**

```rust
#[test]
fn request_drops_cache_control_from_user_text_blocks() {
    // 与 U1 同一类，但走 transform_anthropic_message 的 text 分支。
    // add_bp = true，让 lone text block 因 marker 保持数组形态，从而能断言 part 内部。
    let req = anthropic_to_openai_request_with(
        &json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "big", "cache_control": {"type": "ephemeral"}}]
            }]
        }),
        "t",
        true,
    )
    .unwrap();

    let part = &req["messages"][0]["content"][0];
    assert_eq!(part["type"], "text");
    assert_eq!(part["text"], "big");
    assert!(part.get("cache_control").is_none(), "text 块不得带 cache_control");
    assert_eq!(part["prompt_cache_breakpoint"], json!({"mode": "explicit"}), "llmux 自己的 marker 必须保留");
}
```

**红**：`part.get("cache_control").is_none()` 失败（今天它保留 cache_control —— 旧测试
`request_keeps_cache_control_on_text_block` 正是这么钉的）。

---

**U3 `request_drops_is_error_from_tool_messages`**

```rust
#[test]
fn request_drops_is_error_from_tool_messages() {
    // 实测：cat-jam-free 对带 is_error 的 tool 消息回 400
    // `[unsupported_parameter] Unrecognized request argument supplied: messages[2].is_error`。
    // 剥掉它，其余必须原样。
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "f", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "boom", "is_error": true}
                ]}
            ]
        }),
        "t",
    )
    .unwrap();

    let tool_msg = &req["messages"][1];
    assert_eq!(tool_msg["role"], "tool");
    assert_eq!(tool_msg["tool_call_id"], "toolu_1");
    assert_eq!(tool_msg["content"], "boom");
    assert!(tool_msg.get("is_error").is_none(), "tool 消息不得带 is_error");
}
```

**红**：`tool_msg.get("is_error").is_none()` 失败（今天为 `true`）。

---

**U4 `request_emits_no_anthropic_only_keys_anywhere`** ← 防漏网的总闸

```rust
#[test]
fn request_emits_no_anthropic_only_keys_anywhere() {
    // 覆盖所有会产出这两种字段的形状（system 数组 / text 块 / tool_result / 多轮），
    // 断言序列化后的整体里一个都不剩。这是「新加一条路径时忘了剥」的兜底。
    let req = anthropic_to_openai_request_with(
        &json!({
            "model": "m",
            "system": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "a", "cache_control": {"type": "ephemeral"}}
                ]},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "f", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "err", "is_error": true}
                ]}
            ]
        }),
        "t",
        true,
    )
    .unwrap();

    let serialized = serde_json::to_string(&req).unwrap();
    assert!(!serialized.contains("cache_control"), "转换结果里不得出现 cache_control: {serialized}");
    assert!(!serialized.contains("is_error"), "转换结果里不得出现 is_error: {serialized}");
}
```

**红**：两条 `assert!(!…contains)` 都失败。

---

**U5 `request_flattens_a_lone_text_block_whose_only_extra_was_cache_control`**

```rust
#[test]
fn request_flattens_a_lone_text_block_whose_only_extra_was_cache_control() {
    // 单个 text 块只因 cache_control 而保持数组形态。剥掉之后它就该拍平成裸字符串 ——
    // 那是所有上游都接受的形状，对严格后端最稳。注意 llmux 自己的 marker 依旧
    // 会让数组保留（见 U2 与 request_keeps_single_text_block_as_array_for_its_marker）。
    let req = anthropic_to_openai_request_with(
        &json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "big", "cache_control": {"type": "ephemeral"}}]
            }]
        }),
        "t",
        false,
    )
    .unwrap();

    assert_eq!(req["messages"][0]["content"], json!("big"));
}
```

**红**：今天 content 是 `[{"type":"text","text":"big","cache_control":{…}}]`，`assert_eq!` 失败
（左为数组、右为字符串，报错信息里两边全文可见）。

---

### 4.2 集成（`crates/llmux-server/tests/e2e_protocol_split.rs`）

**E1 `e2e_aggregate_messages_ingress_chat_upstream_rejecting_anthropic_only_keys`**

这一条把生产故障原样搬进测试：**mock 上游按实测规则严格校验**，llmux 必须打出它能接受的 body。

- **mock**：新起一个只服务 `/chat/completions` 的 router，收到 body 后逐条检查
  - 任一 content part 的键**超出 `{type, text, prompt_cache_breakpoint}`** → 回 **400**，正文逐字用生产的
    `{"error":{"param":"messages[<i>].content","type":"invalid_request_error","message":"Upstream request failed: [invalid_value] Invalid value for 'messages[<i>].content': The contents of the message, either a string or a non-empty array of text parts, or null on an assistant message only."}}`
  - 任一消息带 `is_error` → 回 **400**，`[unsupported_parameter] Unrecognized request argument supplied: messages[<i>].is_error`
  - 否则 **200** + `chat_completion_body("A-chat")`
  - ⚠️ 白名单里**必须**有 `prompt_cache_breakpoint`：llmux 的 `add_bp` 默认为真，会给每个 text
    part 加这个 marker（`anthropic_openai.rs:213`），而**实测 cat-jam 认它**（200）。写成纯
    `{type, text}` 会让这条测试在修好之后依然红 —— 那是 mock 不忠实，不是实现错了。
  - ⚠️ mock 的规则因此**比修复更严**（它仍会拒 `zzz` 这类未知键，而实现并不剥它们）：
    **fixture 里只许出现 `cache_control` / `is_error` 这两个被剥的字段**，见 §7 第 3 条。
- **fixture**（请求必须带这两个字段，否则测不到东西）：`/v1/messages` 入口，`system` 是带
  `cache_control` 的块数组，`messages` 里含一条带 `is_error: true` 的 `tool_result`，
  外加一条 `tool_use` 保持配对合法。
- **聚合别名**：`upstream_api: "chat"`，1 个候选指向该 mock 账户（`chat_endpoint` = mock 地址）。
- **断言**：`st == 200`，且 `raw` 含 `"A-chat"`。

**红**：今天是 **502**（llmux 把上游 400 转成 502，`raw` 里能看到 `invalid_value`）。
注意 mock 的校验规则是**真实规则**（任何多余键都拒），而 fixture 只带 `cache_control` /
`is_error` 这两个我们决定剥的字段 —— 见 §7 的已知缺口。

**实现要点（照抄该文件既有形状）**：用 `axum::Json(body): axum::Json<Value>` 取 body、
`tokio::net::TcpListener::bind("127.0.0.1:0")` 取随机端口、`llmux_server::test_state()` /
`app()`、`api_post` 建账户、`api_post` 建别名、`v1_post` 发请求。该文件已有 `v1_post`
（返回 `(StatusCode, Value, String)`，第三个是原始 body 文本，用来断言 marker）。

### 4.3 必须改写的既有测试（它们钉着旧契约）

| 位置 | 现在 | 改成 |
|---|---|---|
| `anthropic_openai_contract.rs:322` `request_keeps_cache_control_on_text_block` | 断言 part **保留** `cache_control` | 改名为 `request_drops_cache_control_from_a_text_block`，断言 `content[0]["text"] == "big"` 且 `content[0].get("cache_control").is_none()`（仍用 `anthropic_to_openai_request`，marker 让它保持数组） |
| `anthropic_openai_contract.rs:146` `request_converts_tool_use_and_tool_result` 的最后一行 | `assert_eq!(tool_msg["is_error"], false)` | 删掉这一行（该测试其余断言——`role` / `tool_call_id` / `content` / `tool_calls`——全部保留，它们正是 C3 的护栏） |

**不许动**：`request_keeps_single_text_block_as_array_for_its_marker`、
`request_keeps_multi_part_content_as_array`、`request_folds_inline_system_into_the_preceding_message`
等 —— 修好后它们应当**继续绿**。若它们变红，说明实现剥多了。

### 4.4 不需要新写的测试（说明理由，避免后人以为漏了）

- **「lone text block 无 marker 时拍平成字符串」**：今天已成立（`flattenable` 分支），
  写出来会**立刻绿**，不是有效 RED。故意不写。
- **「content 是 string / 非空数组 / 仅 assistant 允许 null」的通用不变量**：今天也成立，
  同样一写就绿。§4.2 的严格 mock 才是有红可看的版本。
- **多模态**：`image_url` part 在「数组仅由 text part 组成」这条上游规则下同样非法。本次
  不处理（见 §7）。

---

## 5. 实现要点（最小改动）

单个私有函数 + 一处调用，落在 `crates/llmux-core/src/proxy/anthropic_openai.rs`：

```rust
/// 剥掉 OpenAI chat 形状里没有的 Anthropic 专有字段。
///
/// 起因（2026-10-14 生产实测）：`os` 的首选 `cat-jam-free` 对带 `cache_control`
/// 的 text 块回 `[invalid_value] Invalid value for 'messages[0].content'`，对带
/// `is_error` 的 tool 消息回 `[unsupported_parameter]`，而**每个真实 Claude Code
/// 请求都带这两个字段** —— 于是该候选对真实流量 20 次尝试 0 成功，只在用裸字符串
/// 拨测时是绿的。
///
/// 为什么是**收尾遍历**而不是在 `transform_anthropic_message` 里删：顶层 `system`
/// 的内存数组不经过那个函数（见函数开头单独构造 system 的那段），而生产的
/// `messages[0]` 恰恰就是它。
///
/// 为什么删白名单外的一切（`zzz` 也会被同一条规则拒）不在这里实现：见 §7。
fn strip_anthropic_only_fields(messages: &mut [Value]) {
    for msg in messages {
        // content 是数组 → 每个 part 若是对象，移除 "cache_control"
        // role == "tool" → 移除 "is_error"
    }
}
```

在 `anthropic_to_openai_request_with` 里，`normalize_system_messages` 之后、插入 `out` 之前调用一次：

```rust
let mut normalized = crate::proxy::normalize_system_messages(messages);
strip_anthropic_only_fields(&mut normalized);
out.insert("messages".to_string(), Value::Array(normalized));
```

（`strip_anthropic_only_fields` 建议声明为 `pub(crate)` 还是私有，看 `anthropic_openai.rs` 里
同类辅助函数的既有做法；单测只经 `anthropic_to_openai_request_with` 间接覆盖，不需要
`pub`。别引入新依赖。）

**两点别顺手改**：

1. `flattenable` 里的 `parts[0].get("cache_control").is_none()` 会变成**恒真**。留着
   （无害、零行为差异、diff 更小）。想删也行，但那是清洁工作，不属于本修复。
2. `routes/v1/anthropic.rs` 与 `routes/v1/openai.rs` 的 `add_bp` / `cache_breakpoint_ok` /
   `note_cache_breakpoint_unsupported` **一个字都不要动** —— 那是 `prompt_cache_breakpoint`
   的黑名单，与本次的两个字段无关（C4）。

---

## 6. 验证

**本地（必须全绿，不只是新测试）**

```bash
cd /home/xiaokubao/projects/llmux
~/.cargo/bin/cargo test                                   # 全量：新增 + 既有
~/.cargo/bin/cargo test -p llmux-core --test anthropic_openai_contract
~/.cargo/bin/cargo test -p llmux-server --test e2e_protocol_split
```

全量里任何红——**包括不是本次改动引起的**——都要在汇报里点名，不能略过。

**生产验收**（部署后；`os` 的候选与账户见 §1）

```bash
D=$(date +%F); L=~/HD/openwrt/log-archive/llmux/llmux.log.$D   # 注意：日志按天滚动，00:00 后看新文件

# 1) 不再有 invalid_value 400
rg -c "invalid_value" $L                      # 期望 0（或不再是 cat-jam 那条）
# 2) cat-jam 的真实调度后面跟的是成功
rg -A1 "→ cat-jam-free" $L | rg -c "✅|200"   # 期望 > 0，且看不到 "Account go1 failed (400)"
# 3) V 不再被弹回：手动把 os 切到 V=0 后观察 5 分钟
rg "\[agg:os\] (active switched|V migrated)" $L | tail
```

第 3 条是本次的真正验收：修复前手动切到 0 后 **36~55 秒**就被弹回 1；修好后应当**不再回弹**
（cat-jam 真的在服务真实请求）。切 V=0 走 UI（`POST /api/aggregate-aliases/<id>/active` 需要
UI session cookie；`os` 的 id 当时是 176）。

---

## 7. 非目标与已知缺口（明确记录，不装作没有）

1. **不做学习机制**。曾考虑「上游 400 → 按 `(account_id, model)` 记住该模型要剥字段 → 后续不再发」，
   与仓里 `max_tokens` / `reasoning_effort` 同构。不做的理由：(a) 实测表明严格性是 per-model
   且**没有任何上游需要** `cache_control`（见下），(b) 学习要新状态 + 新 API + 每个 (账户,模型)
   白付一次 400，(c) 关键一点：content part 的 400 **不点名字段**，学习只能靠「首次 400 时去掉
   字段重试一次」，比现在这个纯函数改动复杂得多。
2. **代价：`cache_control` 对所有上游永久消失。** 唯一损失场景是「聚合器 + Anthropic 模型」
   （OpenRouter 那类）失去**显式**缓存控制。本机暴露面：`openrouter` 账户 `is_active=0`，
   历史 230 请求 / 14 次命中读缓存（6%）；仓库里 `cache_control` **零消费方**（全仓 grep 只有
   转换器里那句「保留它」的注释和它自己的断言）。**未实测** OpenRouter 是否靠它才缓存。
   反过来已实测：`go1 / step-5-preview-free` 不带任何标记也拿到 `cached_tokens: 2368/2424`，
   即**自动前缀缓存**；生产全表 141,817 条真实请求里 107,674 条（76%）有
   `cache_read_input_tokens`，读得最多的两个账户（`command` 48,405、`go1` 41,557）都属于
   自动缓存那类 —— 这批命中**不是** `cache_control` 换来的。
3. **不做通用白名单。** 上游的真实规则是「text part 只许有 `type`/`text`」，未知键 `zzz` 同样
   400。本次只剥实测到的那两个字段（llmux 会引入/中继的、客户端确实会发的），因为白名单
   要改 `system` 与 text 两条路径、且有静默丢掉未知字段的风险。**触发条件**：若日后再出现
   第三个被拒的键，把该键加进同一个收尾遍历即可；连字符号不变。
4. **多模态不处理。** 图片 part（`image_url`）在「数组须由 text part 组成」这条规则下同样不合法。
   把图发给纯文本免费模型本来就该失败，本次不掩盖它。
5. **`os` 的 V 抖动不会因此消除**（那是聚合状态机的正常行为），只会因为 cat-jam 真的能干活而
   不再被 400 反复触发。

---

## 8. 附：这份 spec 依赖的实测记录（可复现）

```bash
# 直连上游判定严格性（对照组：同 account、同 endpoint，只改一个键）
#   content 裸字符串                                      -> 200
#   content [{"type":"text","text":T}]                    -> 200
#   content [{"type":"text","text":T,"cache_control":…}]  -> 400  invalid_value
#   content [{"type":"text","text":T,"zzz":1}]            -> 400  invalid_value
#   content [{"type":"text","text":T,"prompt_cache_breakpoint":…}] -> 200
#   tool 消息 + is_error                                   -> 400  unsupported_parameter
# step-5-preview-free / longcat-2.5-preview-free 对 cache_control + is_error 均 200（per-model）
# 自动缓存对照：go1/step-5 无标记第 2 次请求 cached_tokens=2368/2424
```

复现时注意：别启用全量 TRACE body dump（`AGENTS.md` 有成本说明），用 one-shot capture 取
单条原始 body；`usage_logs` 里的 `error_message` 存的是**完整**上游正文（日志行才被截断到
200 字符），是拿错误原文最省事的入口。

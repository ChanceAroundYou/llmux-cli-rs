//! Codex `config.toml` 编辑的合约测试。
//!
//! 每条都对应「原来那套字符串查找会怎么把用户的 Codex 配坏」。这些测试在
//! 改用 toml_edit 之前全是红的（缺陷由 `routes/system/codex.rs` 里的单元测试
//! 先钉出来），所以现在它们必须是绿的：绿即「用户手写的内容不会被我们写坏」。

use llmux_server::routes::system::codex_toml::patch_codex_toml;

const MODEL: &str = "muse-spark-1.3";
const REVIEW: &str = "gpt-5.4-review";
const BASE: &str = "https://llmux.example/v1";

fn patch(existing: &str) -> String {
    patch_codex_toml(existing, MODEL, REVIEW, BASE, "responses", None, None)
        .expect("合法 TOML 不该解析失败")
}

fn parse(text: &str) -> toml_edit::DocumentMut {
    text.parse::<toml_edit::DocumentMut>()
        .unwrap_or_else(|e| panic!("产出必须是合法 TOML（{e}）：\n{text}"))
}

fn llmux_section(doc: &toml_edit::DocumentMut) -> &toml_edit::Table {
    doc["model_providers"]["llmux"]
        .as_table()
        .expect("[model_providers.llmux] 必须存在")
}

/// 缺陷 1：`find("model = \"")` 会先命中 `review_model = "…"` 那一行，
/// 于是 review_model 被改成 model 的值、顶层 model 反而没写进去。
#[test]
fn setting_model_does_not_clobber_review_model() {
    let doc = parse(&patch("review_model = \"gpt-5.4-review\"\n"));
    assert_eq!(doc["model"].as_str(), Some(MODEL), "顶层 model 没写对");
    assert_eq!(doc["review_model"].as_str(), Some(REVIEW), "review_model 被误改");
}

/// 缺陷 2：用户自己配的其它 `[model_providers.*]` 段里的同名键会被改成 llmux 的值。
#[test]
fn a_users_own_provider_section_is_left_alone() {
    let existing = "[model_providers.other]\nname = \"Other\"\nbase_url = \"https://other.example/v1\"\nwire_api = \"responses\"\n";
    let doc = parse(&patch(existing));
    let other = doc["model_providers"]["other"]
        .as_table()
        .expect("用户的 provider 段整段没了");
    assert_eq!(other["base_url"].as_str(), Some("https://other.example/v1"), "用户的 base_url 被改");
    assert_eq!(other["name"].as_str(), Some("Other"), "用户的 name 被改");
    assert_eq!(llmux_section(&doc)["base_url"].as_str(), Some(BASE), "llmux 段自己的 base_url 要写对");
}

/// 缺陷 3：缺失的顶层键被盲追加到文件末尾，而末尾正是新加的 `[model_providers.llmux]`
/// 段 —— `model` / `model_provider` 落进 provider 段里，Codex 读顶层读不到。
/// 语义判据：顶层有这两个键，provider 段里没有它们。
#[test]
fn missing_top_level_keys_land_at_the_document_root() {
    let doc = parse(&patch("[model_providers.llmux]\nname = \"old\"\n"));
    assert_eq!(doc["model"].as_str(), Some(MODEL), "顶层没有 model");
    assert_eq!(doc["model_provider"].as_str(), Some("llmux"), "顶层没有 model_provider");
    let section = llmux_section(&doc);
    assert!(section.get("model").is_none(), "model 落进了 provider 段");
    assert!(section.get("model_provider").is_none(), "model_provider 落进了 provider 段");
    assert!(section.get("review_model").is_none(), "review_model 落进了 provider 段");
}

/// 用户手写的 config.toml 有语法错误时必须报错，不能拿解析结果（空文档）覆盖原文件。
#[test]
fn a_broken_config_toml_is_reported_instead_of_overwritten() {
    let err = patch_codex_toml("model = \n[broken\n", MODEL, REVIEW, BASE, "responses", None, None)
        .expect_err("语法错误要回 Err，否则调用方会写坏用户的文件");
    assert!(!err.to_string().is_empty(), "错误文案要能给用户看");
}

/// 用户手写的注释、无关段落、其它键值都要原样保留 —— 这个文件是用户自己的配置，
/// 不是 llmux 生成的。
#[test]
fn user_comments_and_unrelated_sections_survive() {
    let existing = "# 我的注释\nmodel = \"my-model\"\n\n[features]\nexperimental = true\n\n[tui]\nterminal = \"kitty\"\n";
    let out = patch(existing);
    let doc = parse(&out);
    assert!(out.contains("# 我的注释"), "注释被吃掉：\n{out}");
    assert_eq!(doc["features"]["experimental"].as_bool(), Some(true), "无关段 [features] 被动了");
    assert_eq!(doc["tui"]["terminal"].as_str(), Some("kitty"), "无关段 [tui] 被动了");
    assert_eq!(doc["model"].as_str(), Some(MODEL), "llmux 负责的 model 应被更新");
}

/// 同一个配置反复保存不能每次多长一份（此前每次保存都会在末尾追加一段）。
#[test]
fn saving_twice_is_idempotent() {
    let once = patch("");
    let twice = patch(&once);
    assert_eq!(once, twice, "反复保存会让文件无限变长");
}

/// 从零开始（用户还没有 config.toml）产出的形状要 Codex 能直接读。
#[test]
fn a_fresh_document_has_the_root_keys_and_the_provider_section() {
    let doc = parse(&patch(""));
    assert_eq!(doc["model_provider"].as_str(), Some("llmux"));
    assert_eq!(doc["model"].as_str(), Some(MODEL));
    assert_eq!(doc["review_model"].as_str(), Some(REVIEW));
    let section = llmux_section(&doc);
    assert_eq!(section["base_url"].as_str(), Some(BASE));
    assert_eq!(section["wire_api"].as_str(), Some("responses"));
    assert_eq!(section["requires_openai_auth"].as_bool(), Some(true));
}

/// 可选的窗口/压缩上限是顶层整数键，不填就不写（不能写出 `= null` 这种非法值）。
#[test]
fn optional_integers_are_written_at_the_root_and_omitted_when_absent() {
    let out = patch_codex_toml("", MODEL, REVIEW, BASE, "responses", Some(200_000), Some(120_000))
        .expect("合法 TOML 不该解析失败");
    let doc = parse(&out);
    assert_eq!(doc["model_context_window"].as_integer(), Some(200_000));
    assert_eq!(doc["model_auto_compact_token_limit"].as_integer(), Some(120_000));

    let doc = parse(&patch(""));
    assert!(doc.get("model_context_window").is_none(), "没配窗口就不该写出这个键");
    assert!(doc.get("model_auto_compact_token_limit").is_none(), "没配压缩上限就不该写出这个键");
}

/// 新建的 provider 段要渲染成真正的 `[model_providers.llmux]` 段落，而不是
/// inline table（toml_edit 对链式创建给的是 inline table，形状不对），
/// 也不该多出一个空段头 `[model_providers]`。
#[test]
fn the_provider_section_is_a_real_section_not_an_inline_table() {
    let out = patch("");
    let doc = parse(&out);
    assert!(
        doc["model_providers"].as_table().is_some(),
        "model_providers 必须是段落表，实际：\n{out}"
    );
    assert!(out.contains("[model_providers.llmux]"), "段头没渲染出来：\n{out}");
    assert!(
        !out.contains("[model_providers]\n"),
        "不该多一个空的 [model_providers] 段头：\n{out}"
    );
}

/// 用户自己用 inline table 写 model_providers 时沿用他的写法，并且他的条目还在。
#[test]
fn an_inline_table_style_is_kept_as_the_user_wrote_it() {
    let existing = "model_providers = { mine = { base_url = \"https://mine.example/v1\" } }\n";
    let out = patch(existing);
    let doc = parse(&out);
    assert!(
        doc["model_providers"].as_inline_table().is_some(),
        "用户的 inline table 写法被换掉了：\n{out}"
    );
    assert_eq!(
        doc["model_providers"]["mine"]["base_url"].as_str(),
        Some("https://mine.example/v1"),
        "用户自己的 provider 条目被改"
    );
    assert_eq!(
        doc["model_providers"]["llmux"]["base_url"].as_str(),
        Some(BASE),
        "llmux 自己的字段没写进去"
    );
}

/// `model_providers` 不是表（被人写成了标量）时回 Err 并点名这个键 ——
/// 不猜形状、不覆盖用户的文件。
#[test]
fn a_provider_key_that_is_not_a_table_is_reported() {
    let err = patch_codex_toml("model_providers = 42\n", MODEL, REVIEW, BASE, "responses", None, None)
        .expect_err("形状认不出来必须回 Err");
    let msg = err.to_string();
    assert!(msg.contains("model_providers"), "错误要指出是哪个键：{msg}");
}

/// 已存在的 llmux 段再保存一次：段内旧值被更新，段本身不复制第二份。
#[test]
fn re_saving_updates_the_existing_section_in_place() {
    let existing = "[model_providers.llmux]\nname = \"old\"\nbase_url = \"https://old.example/v1\"\n";
    let out = patch(existing);
    assert_eq!(
        out.matches("[model_providers.llmux]").count(),
        1,
        "重复保存会多出段落：\n{out}"
    );
    let doc = parse(&out);
    let section = llmux_section(&doc);
    assert_eq!(section["base_url"].as_str(), Some(BASE), "旧 base_url 没被更新");
    assert_eq!(section["name"].as_str(), Some("llmux"), "旧 name 没被更新");
}

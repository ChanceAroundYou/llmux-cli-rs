#!/usr/bin/env python3
"""变异验证：聚合候选的纯函数（llmux-core/tests/aggregate_candidate_contract.rs）。

规矩与 `mutate_alias_candidate.py` 相同：每条变异必须**合法编译**（编译失败不算变红），
且必须让它声称的那条测试变红；还原必须 touch mtime（见那个脚本里 restore 的注释）。
用法：`python3 scripts/mutate_aggregate_helpers.py [序号...]`
"""
import os
import shutil
import subprocess
import sys

ROOT = "/home/xiaokubao/projects/llmux"
CARGO = "/home/xiaokubao/.cargo/bin/cargo"
TEST_TARGET = "aggregate_candidate_contract"
SUITE = ["-p", "llmux-core", "--test", TEST_TARGET]
FILE = "crates/llmux-core/src/aggregate.rs"

ORDER_BY_IDS = "SELECT id FROM accounts WHERE id IN ({}) AND is_active = 1 ORDER BY weight DESC, id ASC"
ORDER_BY_PROVIDER = "SELECT id FROM accounts WHERE provider_id = ? AND is_active = 1 ORDER BY weight DESC, id ASC"

MUTATIONS = [
    {
        "name": "保位按下标而不是按候选身份（锁定的那一格会变成另一条线路）",
        "find": """            c.account_id == old.account_id && c.model == old.model""",
        "replace": """            let _ = old; true""",
        "count": 1,
        "test": "the_pinned_active_candidate_follows_itself_when_the_list_is_reordered",
    },
    {
        "name": "被钉的候选消失后仍保留 locked（锁住一条不存在的线路）",
        "find": """        None => (0, 0),""",
        "replace": """        None => (0, i64::from(locked)),""",
        "count": 1,
        "test": "a_pinned_candidate_that_no_longer_exists_clears_the_lock_too",
    },
    {
        "name": "去掉「候选集不能为空」判据",
        "find": """    if v.is_empty() {
        anyhow::bail!("candidates must not be empty");
    }""",
        "replace": """    if v.len() > 1000 {
        anyhow::bail!("candidates must not be empty");
    }""",
        "count": 1,
        "test": "candidate_parsing_rejects_every_shape_the_save_route_relies_on",
    },
    {
        "name": "stale_source 允许从库里读（健在的候选被当配置失效跳过）",
        "find": """    #[serde(skip, default)]
    pub stale_source: bool,""",
        "replace": """    #[serde(default)]
    pub stale_source: bool,""",
        "count": 1,
        "test": "stale_source_is_never_loaded_from_the_database",
    },
    {
        "name": "referenced_aliases 不去重（反查与重展开都按重复名跑）",
        "find": """            if !name.is_empty() && !out.iter().any(|n| n == name) {""",
        "replace": """            if !name.is_empty() {""",
        "count": 1,
        "test": "referenced_aliases_deduplicate_and_keep_declaration_order",
    },
    {
        "name": "去掉「偏好账户排最前」（退回纯 weight 序）",
        "find": """    if let Some(p) = preferred {
        if active_ids.contains(&p) {
            ordered.push(p);
        }
    }""",
        "replace": "",
        "count": 1,
        "test": "expansion_puts_the_preferred_account_first_and_reports_which_accounts_were_dropped",
    },
    {
        "name": "dropped 不再排除已进候选集的账户（报告虚报丢了多少账户）",
        "find": """        .filter(|id| !ordered.contains(id))
        .collect();""",
        "replace": """        .collect();""",
        "count": 1,
        "test": "expansion_puts_the_preferred_account_first_and_reports_which_accounts_were_dropped",
    },
    {
        "name": "显式绑定那一路去掉 weight/id 排序（退回建表顺序）",
        "find": ORDER_BY_IDS,
        "replace": ORDER_BY_IDS.replace(" ORDER BY weight DESC, id ASC", ""),
        "count": 1,
        "test": "expansion_puts_the_preferred_account_first_and_reports_which_accounts_were_dropped",
    },
    {
        "name": "provider 兜底那一路去掉 weight/id 排序",
        "find": ORDER_BY_PROVIDER,
        "replace": ORDER_BY_PROVIDER.replace(" ORDER BY weight DESC, id ASC", ""),
        "count": 1,
        "test": "expansion_falls_back_to_every_active_account_of_the_provider_when_none_are_declared",
    },
    {
        "name": "不转发来源别名的协议与超时（保存侧拦不住冲突、候选级超时失效）",
        "find": """            upstream_api: Some(resolution.upstream_api.as_str().to_string()),
            first_byte_timeout_secs: resolution.first_byte_timeout_secs,""",
        "replace": """            upstream_api: None,
            first_byte_timeout_secs: None,""",
        "count": 1,
        "test": "expansion_carries_the_source_alias_protocol_and_timeout_onto_every_candidate",
    },
]


def restore(backup: str, path: str) -> None:
    shutil.copyfile(backup, path)
    os.utime(path, None)


def run_test(name: str) -> tuple[bool, str]:
    proc = subprocess.run(
        [CARGO, "test", *SUITE, "--", name],
        cwd=ROOT, capture_output=True, text=True, timeout=900,
    )
    out = proc.stdout + proc.stderr
    compiled = "error[" not in out and "could not compile" not in out
    return (compiled and "test result: FAILED" in out), out


def main() -> int:
    picks = [int(a) for a in sys.argv[1:]] or list(range(len(MUTATIONS)))
    path = f"{ROOT}/{FILE}"
    backup = "/tmp/mut_helpers_aggregate.rs"
    shutil.copy2(path, backup)
    failures = 0
    try:
        for i in picks:
            m = MUTATIONS[i]
            src = open(path, encoding="utf-8").read()
            if src.count(m["find"]) != m["count"]:
                print(f"❌ 变异不合法（锚点 {src.count(m['find'])} 次，期望 {m['count']}）：{m['name']}")
                failures += 1
                restore(backup, path)
                continue
            open(path, "w", encoding="utf-8").write(src.replace(m["find"], m["replace"]))
            ok, out = run_test(m["test"])
            if ok:
                print(f"✅ 变红 #{i}：{m['name']}")
            else:
                failures += 1
                tail = "\n".join(
                    line for line in out.splitlines()
                    if "test result" in line or "error[" in line or "assertion" in line.lower()
                )
                print(f"❌ 没变红 #{i}：{m['name']}\n{tail}")
            restore(backup, path)
        print(f"共 {len(picks)} 条变异，失败 {failures}")
        return 1 if failures else 0
    finally:
        restore(backup, path)


if __name__ == "__main__":
    sys.exit(main())

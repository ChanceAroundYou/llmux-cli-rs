#!/usr/bin/env python3
"""变异验证：聚合别名引用普通别名（③）。

每条变异都必须**合法编译**（编译失败不算测试变红），且必须让对应测试变红。
"""
import shutil
import subprocess
import sys

ROOT = "/home/xiaokubao/projects/llmux"
CARGO = "/home/xiaokubao/.cargo/bin/cargo"

STALE_SKIP_OPENAI = """        // 来源普通别名已被删除 → 配置失效，跳过且不计失败（见 helpers::stale_candidate_reason）。
        if let Some(reason) = super::helpers::stale_candidate_reason(cand) {
            tracing::debug!("🚫  [agg:{}] 跳过 {} | 账户 {}：{reason}", alias, cand.model, cand.account_id);
            super::helpers::note_skip_reason(&mut last_error, reason);
            stale_skips += 1;
            continue;
        }
"""

TIMEOUT_OPENAI = """        provider_request.first_byte_timeout_secs = cand
            .first_byte_timeout_secs
            .or(agg.first_byte_timeout_secs);"""

MUTATIONS = [
    {
        "name": "去掉 dispatch_aggregate_openai 里的 stale 跳过（来源已删也照打）",
        "file": "crates/llmux-server/src/routes/v1/openai.rs",
        "find": STALE_SKIP_OPENAI,
        "replace": "",
        "count": 1,
        "test": "deleting_the_referenced_alias_skips_the_candidate_without_counting_it_as_dead",
    },
    {
        "name": "stale 跳过改成记 note_candidate_failure（配置失效被当成上游坏了）",
        "file": "crates/llmux-server/src/routes/v1/openai.rs",
        "find": STALE_SKIP_OPENAI,
        "replace": STALE_SKIP_OPENAI.replace(
            "            stale_skips += 1;",
            "            state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);\n            stale_skips += 1;",
        ),
        "count": 1,
        "test": "deleting_the_referenced_alias_skips_the_candidate_without_counting_it_as_dead",
    },
    {
        "name": "删除别名后不作废引用它的聚合别名解析缓存",
        "file": "crates/llmux-core/src/aggregate.rs",
        "find": "                .map(|c| referenced_aliases(&c).iter().any(|n| n == alias))",
        "replace": "                .map(|c| referenced_aliases(&c).iter().any(|n| n == \"__never__\"))",
        "count": 1,
        "test": "deleting_the_referenced_alias_skips_the_candidate_without_counting_it_as_dead",
    },
    {
        "name": "超时优先级反过来（聚合的值盖掉候选的）",
        "file": "crates/llmux-server/src/routes/v1/openai.rs",
        "find": TIMEOUT_OPENAI,
        "replace": "        provider_request.first_byte_timeout_secs = agg\n            .first_byte_timeout_secs\n            .or(cand.first_byte_timeout_secs);",
        "count": 2,
        "test": "a_referenced_alias_keeps_its_own_first_byte_timeout",
    },
    {
        "name": "普通别名保存后不重展开引用它的聚合别名",
        "file": "crates/llmux-core/src/aggregate.rs",
        "find": "        if !referenced_aliases(&stored).iter().any(|n| n == alias) {\n            continue;\n        }",
        "replace": "        if !referenced_aliases(&stored).iter().any(|n| n == alias) || true {\n            continue;\n        }",
        "count": 1,
        "test": "saving_the_referenced_alias_re_expands_the_aggregate",
    },
    {
        "name": "去掉「只允许一层」判据（允许引用聚合别名）",
        "file": "crates/llmux-core/src/aggregate.rs",
        "find": "    if is_aggregate_alias(pool, name).await? {",
        "replace": "    if is_aggregate_alias(pool, name).await? && false {",
        "count": 1,
        "test": "an_aggregate_candidate_may_not_reference_another_aggregate_alias",
    },
    {
        "name": "保存时丢掉 via_alias（来源标记被洗掉）",
        "file": "crates/llmux-core/src/aggregate.rs",
        "find": "            via_alias: Some(name.to_string()),",
        "replace": "            via_alias: None,",
        "count": 1,
        "test": "an_aggregate_can_forward_an_ordinary_alias_end_to_end",
    },
]


def run_test(name: str) -> tuple[bool, str]:
    proc = subprocess.run(
        [CARGO, "test", "-p", "llmux-server", "--test",
         "aggregate_alias_candidate_contract", "--", name],
        cwd=ROOT, capture_output=True, text=True, timeout=900,
    )
    out = proc.stdout + proc.stderr
    compiled = "error[" not in out and "could not compile" not in out
    red = "test result: FAILED" in out
    return (compiled and red), out


def main() -> int:
    files = sorted({m["file"] for m in MUTATIONS})
    backups = {}
    for f in files:
        shutil.copy2(f"{ROOT}/{f}", f"/tmp/mut3-{f.replace('/', '_')}")
    try:
        failures = 0
        for m in MUTATIONS:
            path = f"{ROOT}/{m['file']}"
            src = open(path, encoding="utf-8").read()
            if src.count(m["find"]) != m["count"]:
                print(f"❌ 变异不合法（锚点出现 {src.count(m['find'])} 次，期望 {m['count']}）：{m['name']}")
                failures += 1
                continue
            open(path, "w", encoding="utf-8").write(src.replace(m["find"], m["replace"]))
            ok, out = run_test(m["test"])
            if ok:
                print(f"✅ 变红：{m['name']}")
            else:
                failures += 1
                tail = "\n".join(
                    line for line in out.splitlines()
                    if "test result" in line or "error[" in line or "panicked" in line or "assertion" in line.lower()
                )
                print(f"❌ 没变红：{m['name']}\n{tail}")
            shutil.copy2(f"/tmp/mut3-{m['file'].replace('/', '_')}", path)
        print(f"共 {len(MUTATIONS)} 条变异，失败 {failures}")
        return 1 if failures else 0
    finally:
        for f in files:
            shutil.copy2(f"/tmp/mut3-{f.replace('/', '_')}", f"{ROOT}/{f}")


if __name__ == "__main__":
    sys.exit(main())

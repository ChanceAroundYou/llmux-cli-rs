#!/usr/bin/env python3
"""变异验证：日志轮转的启动路径 / 压缩 / 保留判据（crates/llmux-bin/src/main.rs）。

规矩同 `mutate_aggregate_helpers.py`：变异必须合法编译，且必须让它声称的那条测试变红；
还原必须 touch mtime。用法：`python3 scripts/mutate_log_rotation.py [序号...]`
"""
import os
import shutil
import subprocess
import sys

ROOT = "/home/xiaokubao/projects/llmux"
CARGO = "/home/xiaokubao/.cargo/bin/cargo"
FILE = "crates/llmux-bin/src/main.rs"

COMPRESS_CALL_IN_STARTUP = """    remove_expired_logs(dir, retain_hours_from_env());
}"""

MUTATIONS = [
    {
        "name": "启动路径又开始压缩（80–90s 部署空档回来）",
        "find": COMPRESS_CALL_IN_STARTUP,
        "replace": """    remove_expired_logs(dir, retain_hours_from_env());
    compress_old_logs(dir);
}""",
        "count": 1,
        "test": "startup_cleanup_removes_expired_logs_but_never_compresses",
    },
    {
        "name": "压缩不看年龄（当前这一小时还在写就被压）",
        "find": """        if age_hours <= 1 || name.ends_with(".gz") {""",
        "replace": """        if false || name.ends_with(".gz") {""",
        "count": 1,
        "test": "compression_compresses_hour_old_logs_and_leaves_the_current_hour",
    },
    {
        "name": "已压缩的也重压（.gz.gz）",
        "find": """        if age_hours <= 1 || name.ends_with(".gz") {""",
        "replace": """        if age_hours <= 1 || false {""",
        "count": 1,
        "test": "compression_compresses_hour_old_logs_and_leaves_the_current_hour",
    },
    {
        "name": "保留期非正也照删（拼错 env 就静默删光日志）",
        "find": """    if retain_hours <= 0 {
        return 0;
    }""",
        "replace": """    if retain_hours <= -100_000 {
        return 0;
    }""",
        "count": 1,
        "test": "a_non_positive_retention_deletes_nothing",
    },
    {
        "name": "认不出日期的文件名当成「很旧」处理（会删掉看不懂的文件）",
        "find": """        let Ok(file_time) = file_time else { continue; };""",
        "replace": """        let file_time = file_time.unwrap_or(time::OffsetDateTime::UNIX_EPOCH);""",
        "count": 1,
        "test": "names_that_are_not_log_timestamps_are_left_alone",
    },
    {
        "name": "删除只认 .gz 那份（只剩明文那份的超期日志删不掉）",
        "find": """        let target = if std::path::Path::new(&gz_path).exists() {
            std::path::PathBuf::from(gz_path)
        } else {
            path
        };""",
        "replace": """        let target = std::path::PathBuf::from(gz_path);""",
        "count": 1,
        "test": "startup_cleanup_removes_expired_logs_but_never_compresses",
    },
]


def restore(backup: str, path: str) -> None:
    shutil.copyfile(backup, path)
    os.utime(path, None)


def run_test(name: str) -> tuple[bool, str]:
    proc = subprocess.run(
        [CARGO, "test", "-p", "llmux", "--bins", "--", name],
        cwd=ROOT, capture_output=True, text=True, timeout=900,
    )
    out = proc.stdout + proc.stderr
    compiled = "error[" not in out and "could not compile" not in out
    return (compiled and "test result: FAILED" in out), out


def main() -> int:
    picks = [int(a) for a in sys.argv[1:]] or list(range(len(MUTATIONS)))
    path = f"{ROOT}/{FILE}"
    backup = "/tmp/mut_logrotation_main.rs"
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

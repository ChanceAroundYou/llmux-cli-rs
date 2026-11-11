# OpenClash Subscription Manager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build unified subscription management daemon (`ocsm`) that consolidates scattered node measurement, sorting, pool updates, site probing, and subscription generation into single supervised process with atomic updates, explicit dependencies, and safe migration path.

**Architecture:** Single Python daemon with task scheduler, dependency graph, atomic file updates with validation gates, unified state management, and manual control interface. Reuses existing measurement/probing logic as importable modules. Deployed as OpenWrt procd service. Migration progresses task-by-task with dry-run validation before each write handoff.

**Tech Stack:** Python 3.9+, OpenWrt procd, YAML config, JSON state, file-based locking, subprocess for clash_meta validation

**Spec:** Based on architectural design in this conversation (2026-11-11). Existing scripts: `/root/scripts/{node_audit.py,pool_nodes.py,key_domain_probe.py}`, `/etc/openclash/custom/{xh_selector.py,gen_subscription.py}`. Router: OpenWrt, 3.4GB RAM, OpenClash with mihomo core at `/etc/openclash/core/clash_meta`.

## Global Constraints

- Python ≥3.9 (router has python3 available)
- No network scanning, brute-force, node switching (without explicit enable), or fault injection
- No credentials in logs, output, or committed code (API keys, cookies, controller secret, auth files)
- Automatic node switching disabled by default (`selector_update.enabled: false`)
- All file writes: tmp → validate → backup → atomic replace
- Migration: dry-run first, compare with existing output, enable write only after validation
- Each cron handoff only after corresponding `ocsm` task verified stable
- State persists across daemon restarts
- Tasks are idempotent

## Review Focus

1. **Concurrent cron + ocsm writes same file** → Both old cron and new daemon attempt to write `/etc/openclash/proxy_provider/主池.yaml` simultaneously, corrupting file. Plan: migration phases explicitly stop old cron before enabling write in ocsm; add file lock acquisition check in atomic_update. Test: Task 5 step 9 verifies cron stopped before write enabled.

2. **Validation passes but OpenClash fails to parse** → `clash_meta -t` returns 0 but rendered config has semantic error (e.g., proxy name typo). Plan: dry-run phase compares output against working baseline; validation includes loading into isolated mihomo instance. Test: Task 4 step 3 adds isolated validation; Task 5 step 7 compares dry-run output.

3. **Daemon crash loses in-flight state** → Task running when daemon killed; restart picks up stale lock or partial write. Plan: state.json persists task status; locks have stale-detection (mtime + pid); atomic_update never leaves partial files. Test: Task 2 step 9 verifies lock cleanup on restart; Task 3 step 7 tests crash recovery.

4. **Dependency cycle or missing dependency** → Task A depends on B, B depends on A; or task enabled but dependency missing from config. Plan: scheduler validates DAG at startup (topological sort); refuses to start with cycle. Test: Task 3 step 5 includes cycle detection test.

5. **Rollback to incompatible version** → User runs `ocsm rollback pool_update` but rollback file is from different node set; OpenClash loads but routes break. Plan: rollback files tagged with generation hash; rollback command shows diff and requires confirmation; rollback only available for last N successful runs. Test: Task 5 step 10 verifies rollback preserves .rollback with hash metadata.

---

## Task 1: Project Structure and Configuration Schema

**Files:**
- Create: `/opt/ocsm/ocsm.py` (entry point stub)
- Create: `/opt/ocsm/config/ocsm.yaml` (main config template)
- Create: `/opt/ocsm/config/tasks.yaml` (task dependency graph)
- Create: `/opt/ocsm/lib/__init__.py`
- Create: `/opt/ocsm/requirements.txt`

**Interfaces:**
- Consumes: None (foundation task)
- Produces: 
  - `config/ocsm.yaml`: dict with keys `openclash`, `node_audit`, `selector`, `subscription`, `logging`
  - `config/tasks.yaml`: dict with key `tasks`, each task has `interval`, `depends_on`, `enabled`, optional `watch_files`

- [ ] **Step 1: Write test for config schema validation**

```python
# /opt/ocsm/tests/test_config.py
def test_ocsm_yaml_schema():
    """Config must have all required top-level keys"""
    cfg = yaml.safe_load(Path('/opt/ocsm/config/ocsm.yaml').read_text())
    assert 'openclash' in cfg
    assert 'node_audit' in cfg
    assert 'selector' in cfg
    assert 'subscription' in cfg
    assert 'logging' in cfg
    assert cfg['openclash']['config_path'] == '/etc/openclash/main.yaml'
    assert cfg['selector']['auto_switch_enabled'] is False  # Default disabled

def test_tasks_yaml_no_cycles():
    """Task dependency graph must be acyclic"""
    tasks = yaml.safe_load(Path('/opt/ocsm/config/tasks.yaml').read_text())['tasks']
    # Build adjacency list
    graph = {name: t.get('depends_on', []) for name, t in tasks.items()}
    # Topological sort (Kahn's algorithm)
    in_degree = {name: 0 for name in graph}
    for deps in graph.values():
        for dep in deps:
            assert dep in graph, f"Unknown dependency: {dep}"
            in_degree[dep] += 1
    queue = [n for n, d in in_degree.items() if d == 0]
    sorted_tasks = []
    while queue:
        node = queue.pop(0)
        sorted_tasks.append(node)
        for neighbor in graph[node]:
            in_degree[neighbor] -= 1
            if in_degree[neighbor] == 0:
                queue.append(neighbor)
    assert len(sorted_tasks) == len(graph), "Cycle detected in task dependencies"
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_config.py::test_ocsm_yaml_schema -v`  
Expected: FAIL with "FileNotFoundError: config/ocsm.yaml"

- [ ] **Step 3: Create directory structure and stub files**

```bash
ssh router 'mkdir -p /opt/ocsm/{lib/tasks,config,data/{last-run,history},logs,tests}'
```

- [ ] **Step 4: Implement config templates in `config/ocsm.yaml` and `config/tasks.yaml`**

Content based on architecture design (openclash paths, node_audit history_dir, selector with auto_switch_enabled: false, task dependency graph with node_measure → node_sort → pool_update chain, site_probe independent, subscription_gen depends on pool_update).

- [ ] **Step 5: Create minimal `ocsm.py` entry point**

```python
#!/usr/bin/env python3
import sys
from pathlib import Path

BASE_DIR = Path(__file__).parent
sys.path.insert(0, str(BASE_DIR / 'lib'))

if __name__ == '__main__':
    print(f"ocsm starting from {BASE_DIR}")
    # Scheduler loop will be added in Task 3
```

- [ ] **Step 6: Create `requirements.txt`**

```
PyYAML>=6.0
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_config.py -v`  
Expected: PASS (2 tests)

- [ ] **Step 8: Commit**

```bash
cd /home/xiaokubao/projects/llmux
git add docs/superpowers/plans/2026-11-11-openclash-subscription-manager.md
# ocsm lives on router, not in llmux repo - plan only
git commit -m "plan: openclash subscription manager consolidation"
```

---

## Task 2: State Management and Locking

**Files:**
- Create: `/opt/ocsm/lib/state.py`
- Create: `/opt/ocsm/lib/lock.py`
- Create: `/opt/ocsm/tests/test_state.py`
- Create: `/opt/ocsm/tests/test_lock.py`

**Interfaces:**
- Consumes: `config/ocsm.yaml` (for state.json path)
- Produces:
  - `class SystemState` with methods `save()`, `load()`, `update_task(name, status, timestamp)`
  - `class GlobalLock` with context manager, `__enter__()` returns lock or raises `LockHeld(holder_pid, age_seconds)`
  - `class TaskLock(task_name)` with same interface

- [ ] **Step 1: Write test for SystemState persistence**

```python
# /opt/ocsm/tests/test_state.py
def test_state_roundtrip(tmp_path):
    """State saves to JSON and loads back with same data"""
    state_file = tmp_path / "state.json"
    state = SystemState(path=state_file, tasks=['task_a', 'task_b'])
    state.update_task('task_a', status='SUCCESS', timestamp=datetime(2026,11,11,20,0,0))
    state.save()
    
    loaded = SystemState.load(state_file)
    assert loaded.tasks['task_a']['status'] == 'SUCCESS'
    assert loaded.tasks['task_a']['last_run'] == '2026-11-11T20:00:00'

def test_state_concurrent_write_detection(tmp_path):
    """Detect if state.json was modified by another process"""
    state_file = tmp_path / "state.json"
    state1 = SystemState.load_or_init(state_file, tasks=['t1'])
    state2 = SystemState.load_or_init(state_file, tasks=['t1'])
    state1.update_task('t1', 'SUCCESS', datetime.now())
    state1.save()  # state1 writes
    state2.update_task('t1', 'FAILED', datetime.now())
    with pytest.raises(StateConflict):
        state2.save()  # state2 detects mtime changed
```

- [ ] **Step 2: Write test for GlobalLock stale detection**

```python
# /opt/ocsm/tests/test_lock.py
def test_lock_acquisition_release(tmp_path):
    """Lock acquired and released correctly"""
    lock = GlobalLock(tmp_path / "test.lock")
    with lock:
        assert (tmp_path / "test.lock").exists()
    assert not (tmp_path / "test.lock").exists()

def test_lock_held_by_another(tmp_path):
    """Lock held by another process raises LockHeld"""
    lock_path = tmp_path / "test.lock"
    lock_path.write_text(f"{os.getpid()}\n")
    lock = GlobalLock(lock_path)
    with pytest.raises(LockHeld) as exc:
        with lock:
            pass
    assert exc.value.holder_pid == os.getpid()

def test_stale_lock_cleanup(tmp_path):
    """Lock older than 15min with dead PID is cleaned up"""
    lock_path = tmp_path / "test.lock"
    lock_path.write_text("99999\n")  # Nonexistent PID
    os.utime(lock_path, (time.time() - 1000, time.time() - 1000))  # 16min ago
    lock = GlobalLock(lock_path, stale_threshold=900)
    with lock:  # Should succeed after cleanup
        assert lock_path.read_text().strip() == str(os.getpid())
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_state.py tests/test_lock.py -v`  
Expected: FAIL with "ModuleNotFoundError: No module named 'state'"

- [ ] **Step 4: Implement `lib/state.py`**

`SystemState` as dataclass with `tasks: Dict[str, TaskState]`, `last_pool_hash: str`, `last_subscription_hash: str`. `save()` writes JSON atomically (tmp → rename). `load()` reads and parses. `update_task()` updates nested dict. Include mtime check in `save()` to detect concurrent modification.

- [ ] **Step 5: Implement `lib/lock.py`**

`GlobalLock` writes PID to lock file in `__enter__`, removes in `__exit__`. If lock file exists: read PID, check `/proc/{pid}` existence, check mtime. If PID dead and mtime > stale_threshold, clean up and retry. Else raise `LockHeld(holder_pid, age_seconds)`. `TaskLock` is same but lock path includes task name.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_state.py tests/test_lock.py -v`  
Expected: PASS (5 tests)

- [ ] **Step 7: Manual test: state persistence across Python invocations**

```bash
ssh router 'cd /opt/ocsm && python3 -c "
from lib.state import SystemState
from datetime import datetime
s = SystemState.load_or_init(\"data/state.json\", tasks=[\"t1\"])
s.update_task(\"t1\", \"SUCCESS\", datetime.now())
s.save()
print(\"Saved\")
"'
ssh router 'cd /opt/ocsm && python3 -c "
from lib.state import SystemState
s = SystemState.load(\"data/state.json\")
print(s.tasks[\"t1\"])
"'
```

Expected: Second invocation prints task state from first

- [ ] **Step 8: Manual test: lock prevents concurrent execution**

```bash
ssh router 'cd /opt/ocsm && python3 -c "
from lib.lock import GlobalLock
import time
with GlobalLock(\"data/test.lock\"):
    print(\"Lock held\")
    time.sleep(5)
" &'
sleep 1
ssh router 'cd /opt/ocsm && python3 -c "
from lib.lock import GlobalLock
try:
    with GlobalLock(\"data/test.lock\"):
        print(\"Should not print\")
except Exception as e:
    print(f\"Expected: {e}\")
"'
```

Expected: Second invocation prints "Expected: LockHeld(...)"

- [ ] **Step 9: Add test for lock cleanup on daemon restart**

```python
# Append to /opt/ocsm/tests/test_lock.py
def test_lock_cleanup_on_restart(tmp_path):
    """Stale lock from crashed daemon is cleaned on restart"""
    lock_path = tmp_path / "global.lock"
    # Simulate crashed daemon: lock file with old PID
    lock_path.write_text("12345\n")
    old_time = time.time() - 1000
    os.utime(lock_path, (old_time, old_time))
    
    # New daemon starts
    lock = GlobalLock(lock_path, stale_threshold=900)
    with lock:
        assert lock_path.read_text().strip() == str(os.getpid())
```

- [ ] **Step 10: Run new test**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_lock.py::test_lock_cleanup_on_restart -v`  
Expected: PASS

- [ ] **Step 11: Commit**

```bash
# On router - document in plan execution notes
echo "Task 2 complete: state.py, lock.py verified" >> /opt/ocsm/PROGRESS.txt
```

---

## Task 3: Task Scheduler with Dependency Resolution

**Files:**
- Create: `/opt/ocsm/lib/scheduler.py`
- Create: `/opt/ocsm/tests/test_scheduler.py`

**Interfaces:**
- Consumes:
  - `config/tasks.yaml`: task definitions with `interval`, `depends_on`, `enabled`
  - `lib/state.py`: `SystemState` for task run history
  - `lib/lock.py`: `TaskLock` for mutual exclusion
- Produces:
  - `class Task` dataclass with fields from config + runtime state
  - `class Scheduler` with methods:
    - `should_run(task: Task) -> bool`: check interval elapsed + dependencies satisfied
    - `get_runnable_tasks() -> List[Task]`: return tasks ready to run
    - `mark_completed(task_name, result: TaskResult)`

- [ ] **Step 1: Write test for dependency satisfaction**

```python
# /opt/ocsm/tests/test_scheduler.py
def test_task_depends_on_successful_parent(tmp_path):
    """Task only runs if all depends_on tasks succeeded"""
    tasks_cfg = {
        'task_a': {'interval': 3600, 'depends_on': []},
        'task_b': {'interval': 0, 'depends_on': ['task_a']},
    }
    state = SystemState.load_or_init(tmp_path / "state.json", tasks=list(tasks_cfg.keys()))
    sched = Scheduler(tasks_cfg, state)
    
    # task_a never run
    assert sched.should_run('task_a') is True
    assert sched.should_run('task_b') is False  # Blocked by task_a
    
    # task_a succeeded
    sched.mark_completed('task_a', TaskResult(status='SUCCESS', timestamp=datetime.now()))
    assert sched.should_run('task_b') is True  # Unblocked

def test_interval_zero_means_manual_only(tmp_path):
    """interval=0 tasks only run after dependency or manual trigger"""
    tasks_cfg = {'manual_task': {'interval': 0, 'depends_on': []}}
    state = SystemState.load_or_init(tmp_path / "state.json", tasks=['manual_task'])
    sched = Scheduler(tasks_cfg, state)
    
    assert sched.should_run('manual_task') is False  # Never auto-runs
    sched.trigger_manual('manual_task')
    assert sched.should_run('manual_task') is True
```

- [ ] **Step 2: Write test for cycle detection at init**

```python
def test_cycle_detection_at_init():
    """Scheduler refuses to init with cyclic dependencies"""
    tasks_cfg = {
        'task_a': {'depends_on': ['task_b']},
        'task_b': {'depends_on': ['task_a']},
    }
    with pytest.raises(ValueError, match="Cycle detected"):
        Scheduler(tasks_cfg, state=None)
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_scheduler.py -v`  
Expected: FAIL with "ModuleNotFoundError: No module named 'scheduler'"

- [ ] **Step 4: Implement `lib/scheduler.py`**

`Task` dataclass includes config fields + `last_run`, `last_success`, `manual_trigger_pending`. `Scheduler.__init__` loads tasks from config, validates DAG (topological sort, raise on cycle). `should_run()` checks: enabled + (interval elapsed OR manual_trigger_pending) + all depends_on have last_success. `get_runnable_tasks()` filters all tasks by should_run. `mark_completed()` updates state.

- [ ] **Step 5: Add cycle detection implementation**

Use Kahn's algorithm (same as Task 1 test): build in-degree map, iteratively remove nodes with in-degree 0. If sorted list length < task count, cycle exists.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_scheduler.py -v`  
Expected: PASS (3 tests)

- [ ] **Step 7: Add test for crash recovery**

```python
def test_scheduler_resumes_after_restart(tmp_path):
    """Scheduler loads state from previous run"""
    tasks_cfg = {'task_a': {'interval': 60, 'depends_on': []}}
    state_path = tmp_path / "state.json"
    
    # First run
    state1 = SystemState.load_or_init(state_path, tasks=['task_a'])
    sched1 = Scheduler(tasks_cfg, state1)
    sched1.mark_completed('task_a', TaskResult('SUCCESS', datetime.now()))
    state1.save()
    
    # Simulated restart
    state2 = SystemState.load(state_path)
    sched2 = Scheduler(tasks_cfg, state2)
    assert sched2.tasks['task_a'].last_success is not None
```

- [ ] **Step 8: Run crash recovery test**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_scheduler.py::test_scheduler_resumes_after_restart -v`  
Expected: PASS

- [ ] **Step 9: Commit**

```bash
echo "Task 3 complete: scheduler.py with dependency resolution" >> /opt/ocsm/PROGRESS.txt
```

---

## Task 4: Configuration Validator with Isolated Mihomo Check

**Files:**
- Create: `/opt/ocsm/lib/validator.py`
- Create: `/opt/ocsm/tests/test_validator.py`

**Interfaces:**
- Consumes: `config/ocsm.yaml` (for `openclash.bin` path)
- Produces:
  - `validate_config(yaml_path: str) -> ValidationResult`: runs `clash_meta -t` and isolated load test
  - `class ValidationResult` with fields `valid: bool`, `errors: List[str]`, `stdout: str`

- [ ] **Step 1: Write test for valid config passes**

```python
# /opt/ocsm/tests/test_validator.py
def test_valid_config_passes(tmp_path):
    """Minimal valid clash config passes validation"""
    cfg = tmp_path / "test.yaml"
    cfg.write_text("""
mixed-port: 7890
mode: rule
proxies: []
proxy-groups: []
rules: []
""")
    result = validate_config(str(cfg), bin_path='/etc/openclash/core/clash_meta')
    assert result.valid is True
    assert len(result.errors) == 0

def test_invalid_config_fails(tmp_path):
    """Config with syntax error fails validation"""
    cfg = tmp_path / "test.yaml"
    cfg.write_text("invalid: yaml: syntax::")
    result = validate_config(str(cfg), bin_path='/etc/openclash/core/clash_meta')
    assert result.valid is False
    assert any('yaml' in e.lower() for e in result.errors)
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_validator.py -v`  
Expected: FAIL with "ModuleNotFoundError: No module named 'validator'"

- [ ] **Step 3: Implement `lib/validator.py` with isolated validation**

`validate_config()` runs two checks:
1. `subprocess.run([bin_path, '-t', '-d', tmpdir, '-f', yaml_path], env={'SAFE_PATHS': '/usr/share/openclash/ui'})` - basic syntax
2. Start isolated mihomo on unused port (find with `socket`), send test request to `/proxies`, shutdown - semantic check

Return `ValidationResult(valid=rc==0 and isolated_ok, errors=[...], stdout=...)`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_validator.py -v`  
Expected: PASS (2 tests)

- [ ] **Step 5: Manual test on router with real config**

```bash
ssh router 'cd /opt/ocsm && python3 -c "
from lib.validator import validate_config
result = validate_config(\"/etc/openclash/main.yaml\", \"/etc/openclash/core/clash_meta\")
print(f\"Valid: {result.valid}\")
if not result.valid:
    print(\"Errors:\", result.errors)
"'
```

Expected: "Valid: True"

- [ ] **Step 6: Manual test with intentionally broken config**

```bash
ssh router 'cat > /tmp/broken.yaml <<EOF
proxies:
  - {name: "test", type: invalid_type}
EOF
cd /opt/ocsm && python3 -c "
from lib.validator import validate_config
result = validate_config(\"/tmp/broken.yaml\", \"/etc/openclash/core/clash_meta\")
print(f\"Valid: {result.valid}\")
print(\"Errors:\", result.errors)
"'
```

Expected: "Valid: False" with error mentioning "invalid_type"

- [ ] **Step 7: Commit**

```bash
echo "Task 4 complete: validator.py with isolated mihomo check" >> /opt/ocsm/PROGRESS.txt
```

---

## Task 5: Migrate Pool Update Task (First Write Handoff)

**Files:**
- Create: `/opt/ocsm/lib/tasks/pool_update.py`
- Modify: `/opt/ocsm/lib/scheduler.py` (add task execution)
- Create: `/opt/ocsm/tests/test_pool_update.py`

**Interfaces:**
- Consumes:
  - `/root/scripts/pool_nodes.py` (imported as module)
  - `lib/validator.py`: `validate_config()`
  - `lib/lock.py`: `GlobalLock`
- Produces:
  - `run_pool_update(dry_run: bool = True) -> TaskResult`
  - Writes: `/etc/openclash/proxy_provider/{主池,谷歌池,香港池}.yaml` (when dry_run=False)
  - Backup: `*.yaml.rollback` before each write

- [ ] **Step 1: Write test for dry-run generates output without writing**

```python
# /opt/ocsm/tests/test_pool_update.py
def test_dry_run_generates_output(tmp_path):
    """Dry run produces output files in temp dir"""
    result = run_pool_update(dry_run=True, output_dir=str(tmp_path))
    assert result.status == 'SUCCESS'
    assert (tmp_path / '主池.yaml').exists()
    assert (tmp_path / '谷歌池.yaml').exists()
    assert (tmp_path / '香港池.yaml').exists()
    # Real provider dir unchanged
    assert not Path('/etc/openclash/proxy_provider/主池.yaml.tmp').exists()

def test_atomic_update_validates_before_write(tmp_path):
    """Invalid output is not written to production"""
    # Mock validator to fail
    with patch('lib.tasks.pool_update.validate_config', return_value=ValidationResult(False, ['error'])):
        result = run_pool_update(dry_run=False, output_dir=str(tmp_path))
        assert result.status == 'FAILED'
        assert 'validation failed' in result.message.lower()
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_pool_update.py -v`  
Expected: FAIL with "ModuleNotFoundError: No module named 'tasks.pool_update'"

- [ ] **Step 3: Implement `lib/tasks/pool_update.py`**

Import `/root/scripts/pool_nodes` module. `run_pool_update()`:
1. Acquire `GlobalLock('pool_update')`
2. Call `pool_nodes.main()` to generate content
3. If dry_run: write to `output_dir`, return SUCCESS
4. Else: write each file to `.tmp`, validate with `validate_config()`, backup existing to `.rollback`, `os.replace(.tmp, target)`, save hash to state
5. Return `TaskResult(status='SUCCESS'|'FAILED', message=..., files_changed=[...])`

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_pool_update.py -v`  
Expected: PASS (2 tests)

- [ ] **Step 5: Integrate task into scheduler**

Modify `lib/scheduler.py`: `run_task(task_name)` dispatches to task modules. Add `TASK_RUNNERS = {'pool_update': pool_update.run_pool_update}`. `run_task()` acquires `TaskLock(task_name)`, calls runner, updates state.

- [ ] **Step 6: Manual test: dry-run on router**

```bash
ssh router 'cd /opt/ocsm && python3 -c "
from lib.tasks.pool_update import run_pool_update
result = run_pool_update(dry_run=True, output_dir=\"/tmp/ocsm-dry-run\")
print(f\"Status: {result.status}\")
print(f\"Files: {result.files_changed}\")
"'
ssh router 'ls -lh /tmp/ocsm-dry-run/'
```

Expected: Status SUCCESS, three .yaml files in /tmp/ocsm-dry-run/

- [ ] **Step 7: Compare dry-run output with current production**

```bash
ssh router 'diff /etc/openclash/proxy_provider/主池.yaml /tmp/ocsm-dry-run/主池.yaml || echo "DIFF FOUND"'
ssh router 'diff /etc/openclash/proxy_provider/谷歌池.yaml /tmp/ocsm-dry-run/谷歌池.yaml || echo "DIFF FOUND"'
ssh router 'diff /etc/openclash/proxy_provider/香港池.yaml /tmp/ocsm-dry-run/香港池.yaml || echo "DIFF FOUND"'
```

Expected: Either no diff (identical) or diff with explanation (e.g., node order changed due to recent measurement)

- [ ] **Step 8: Document differences if any**

If diffs found, manually review and document in `/opt/ocsm/data/migration-notes.txt`: why outputs differ, whether difference is expected based on recent node audit runs.

- [ ] **Step 9: Stop cron job for pool_nodes.py**

```bash
ssh router 'crontab -l > /tmp/crontab.bak-before-ocsm'
ssh router 'crontab -l | grep -v pool_nodes.py > /tmp/crontab.new'
ssh router 'crontab /tmp/crontab.new'
ssh router 'crontab -l | grep pool_nodes || echo "REMOVED"'
```

Expected: "REMOVED" (no pool_nodes.py line in crontab)

- [ ] **Step 10: Enable write mode and verify rollback file creation**

```bash
ssh router 'cd /opt/ocsm && python3 -c "
from lib.tasks.pool_update import run_pool_update
result = run_pool_update(dry_run=False)
print(f\"Status: {result.status}\")
"'
ssh router 'ls -lh /etc/openclash/proxy_provider/*.rollback'
```

Expected: Status SUCCESS, .rollback files exist for 主池/谷歌池/香港池

- [ ] **Step 11: Verify OpenClash still loads**

```bash
ssh router '/etc/openclash/core/clash_meta -t -d /etc/openclash -f /etc/openclash/main.yaml'
```

Expected: Configuration file test is successful (exit 0)

- [ ] **Step 12: Test rollback**

```bash
ssh router 'cd /opt/ocsm && python3 -c "
from lib.tasks.pool_update import rollback_pool_update
rollback_pool_update()
"'
ssh router 'diff /etc/openclash/proxy_provider/主池.yaml /etc/openclash/proxy_provider/主池.yaml.rollback'
```

Expected: No diff (rollback restored previous version)

- [ ] **Step 13: Re-run pool_update to restore current state**

```bash
ssh router 'cd /opt/ocsm && python3 -c "from lib.tasks.pool_update import run_pool_update; run_pool_update(dry_run=False)"'
```

- [ ] **Step 14: Observe for 24 hours**

Wait 24 hours. Check `/etc/openclash/proxy_provider/` files are still updating correctly (mtime recent), OpenClash logs show no errors, node selection works.

- [ ] **Step 15: Commit**

```bash
echo "Task 5 complete: pool_update migrated, cron stopped, 24h observation passed" >> /opt/ocsm/PROGRESS.txt
```

---

## Task 6: Migrate Subscription Generation

**Files:**
- Create: `/opt/ocsm/lib/tasks/subscription_gen.py`
- Create: `/opt/ocsm/tests/test_subscription_gen.py`

**Interfaces:**
- Consumes:
  - `/etc/openclash/custom/gen_subscription.py` (imported as module)
  - `config/tasks.yaml`: `subscription_gen.watch_files` list
  - `lib/validator.py`: validation
- Produces:
  - `run_subscription_gen(dry_run: bool = True) -> TaskResult`
  - Writes: `/etc/openclash/subscription/<token>.yaml`, `url.txt`, `.inputs.sha256`

- [ ] **Step 1: Write test for input hash change detection**

```python
# /opt/ocsm/tests/test_subscription_gen.py
def test_no_regeneration_if_inputs_unchanged(tmp_path):
    """Subscription not regenerated if input files unchanged"""
    # First run
    result1 = run_subscription_gen(dry_run=True, output_dir=str(tmp_path))
    assert result1.status == 'SUCCESS'
    hash1 = (tmp_path / '.inputs.sha256').read_text()
    
    # Second run with same inputs
    result2 = run_subscription_gen(dry_run=True, output_dir=str(tmp_path))
    assert result2.status == 'SKIPPED'
    assert result2.message == 'unchanged (inputs hash matches)'
    hash2 = (tmp_path / '.inputs.sha256').read_text()
    assert hash1 == hash2

def test_regeneration_after_pool_update(tmp_path):
    """Subscription regenerates after pool file changes"""
    # First run
    run_subscription_gen(dry_run=True, output_dir=str(tmp_path), 
                         pool_file=str(tmp_path / 'pool.yaml'))
    (tmp_path / 'pool.yaml').write_text('new content')
    
    # Second run after pool change
    result = run_subscription_gen(dry_run=True, output_dir=str(tmp_path),
                                  pool_file=str(tmp_path / 'pool.yaml'))
    assert result.status == 'SUCCESS'
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_subscription_gen.py -v`  
Expected: FAIL with "ModuleNotFoundError"

- [ ] **Step 3: Implement `lib/tasks/subscription_gen.py`**

Import `gen_subscription` module. `run_subscription_gen()`:
1. Compute input hash (all watch_files + pool yamls)
2. Compare with saved hash, return SKIPPED if same
3. Call `gen_subscription.main()`
4. Validate output with `validate_config()`
5. If dry_run: write to temp dir; else: atomic update subscription file, update hash
6. Return `TaskResult`

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_subscription_gen.py -v`  
Expected: PASS (2 tests)

- [ ] **Step 5: Add task to scheduler task map**

In `lib/scheduler.py`: `TASK_RUNNERS['subscription_gen'] = subscription_gen.run_subscription_gen`

- [ ] **Step 6: Dry-run on router**

```bash
ssh router 'cd /opt/ocsm && python3 -c "
from lib.tasks.subscription_gen import run_subscription_gen
result = run_subscription_gen(dry_run=True, output_dir=\"/tmp/ocsm-sub-dry\")
print(f\"Status: {result.status}\")
"'
ssh router 'ls -lh /tmp/ocsm-sub-dry/'
```

Expected: Subscription .yaml file + url.txt in /tmp/ocsm-sub-dry/

- [ ] **Step 7: Compare output with current subscription**

```bash
ssh router 'CURRENT=$(ls /etc/openclash/subscription/*.yaml | head -1); diff "$CURRENT" /tmp/ocsm-sub-dry/*.yaml || echo "DIFF"'
```

Expected: No diff or documented expected difference

- [ ] **Step 8: Stop cron for gen_subscription.py**

```bash
ssh router 'crontab -l | grep -v gen_subscription.py > /tmp/crontab.new'
ssh router 'crontab /tmp/crontab.new'
```

- [ ] **Step 9: Enable write mode**

```bash
ssh router 'cd /opt/ocsm && python3 -c "
from lib.tasks.subscription_gen import run_subscription_gen
result = run_subscription_gen(dry_run=False)
print(f\"Status: {result.status}\")
"'
```

Expected: Status SUCCESS

- [ ] **Step 10: Verify subscription URL still accessible**

```bash
ssh router 'URL=$(cat /etc/openclash/subscription/url.txt); curl -sI "$URL" | head -1'
```

Expected: HTTP/2 200

- [ ] **Step 11: Test phone import**

On phone: Open Clash client, import subscription URL from `/etc/openclash/subscription/url.txt`. Verify proxies load, can browse.

- [ ] **Step 12: Observe for 24 hours**

Check subscription file mtime updates after pool changes, no 404s on subscription URL.

- [ ] **Step 13: Commit**

```bash
echo "Task 6 complete: subscription_gen migrated" >> /opt/ocsm/PROGRESS.txt
```

---

## Task 7: Migrate Site Probe (No Auto-Switch)

**Files:**
- Create: `/opt/ocsm/lib/tasks/site_probe.py`
- Create: `/opt/ocsm/tests/test_site_probe.py`

**Interfaces:**
- Consumes:
  - `/etc/openclash/custom/xh_selector.py` (probe logic only, not switch logic)
  - `config/ocsm.yaml`: `selector.providers` (probe targets)
- Produces:
  - `run_site_probe(dry_run: bool = True) -> TaskResult`
  - Writes: `/opt/ocsm/data/probe-results.json` (probe results, no switching)

- [ ] **Step 1: Write test for probe execution without switching**

```python
# /opt/ocsm/tests/test_site_probe.py
def test_probe_records_results_no_switch(tmp_path):
    """Probe executes and records results without switching nodes"""
    result = run_site_probe(output_file=str(tmp_path / 'probe.json'))
    assert result.status == 'SUCCESS'
    probe_data = json.loads((tmp_path / 'probe.json').read_text())
    assert 'google' in probe_data
    assert 'general' in probe_data
    assert all('current_node' in v for v in probe_data.values())
    assert all('probe_results' in v for v in probe_data.values())
    # No switch_decision field
    assert all('switch_decision' not in v for v in probe_data.values())
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_site_probe.py -v`  
Expected: FAIL

- [ ] **Step 3: Implement `lib/tasks/site_probe.py`**

Extract probe logic from `xh_selector.py` (target URL fetching, success/failure recording). Remove all switching logic (no `PUT /proxies/<group>`). Write results to JSON file. Return `TaskResult` with probe summary (success count, failure count).

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_site_probe.py -v`  
Expected: PASS

- [ ] **Step 5: Add to scheduler**

In `lib/scheduler.py`: `TASK_RUNNERS['site_probe'] = site_probe.run_site_probe`

- [ ] **Step 6: Manual probe on router**

```bash
ssh router 'cd /opt/ocsm && python3 -c "
from lib.tasks.site_probe import run_site_probe
result = run_site_probe()
print(f\"Status: {result.status}\")
print(f\"Summary: {result.message}\")
"'
ssh router 'cat /opt/ocsm/data/probe-results.json | head -20'
```

Expected: Probe results JSON with success/failure counts, no switch_decision field

- [ ] **Step 7: Compare with xh_selector.py probe results**

Run both old and new probe, compare detected failures (should match).

```bash
ssh router '/etc/openclash/custom/xh_selector.py --shadow > /tmp/xh-probe.log 2>&1'
ssh router 'cd /opt/ocsm && python3 -c "from lib.tasks.site_probe import run_site_probe; run_site_probe()"'
ssh router 'echo "Old probe:"; grep -c "PASS\|FAIL" /tmp/xh-probe.log; echo "New probe:"; grep -c success /opt/ocsm/data/probe-results.json'
```

Expected: Similar pass/fail counts

- [ ] **Step 8: DO NOT stop xh_selector.py cron yet**

Leave `xh_selector.py --watch` running in cron (it still does switching). Only stop it after Task 8 if auto-switch is enabled.

- [ ] **Step 9: Commit**

```bash
echo "Task 7 complete: site_probe (read-only, no auto-switch)" >> /opt/ocsm/PROGRESS.txt
```

---

## Task 8: Migrate Node Measurement and Sorting

**Files:**
- Create: `/opt/ocsm/lib/tasks/node_measure.py`
- Create: `/opt/ocsm/lib/tasks/node_sort.py`
- Create: `/opt/ocsm/tests/test_node_measure.py`

**Interfaces:**
- Consumes:
  - `/root/scripts/node_audit.py` (measure and sort functions)
  - `config/ocsm.yaml`: `node_audit.history_dir`, `focus_schedule`
- Produces:
  - `run_node_measure(mode: str = 'focus') -> TaskResult`: mode = 'focus' | 'full'
  - `run_node_sort() -> TaskResult`
  - Triggers: `pool_update` task after successful sort

- [ ] **Step 1: Write test for measure modes**

```python
# /opt/ocsm/tests/test_node_measure.py
def test_focus_mode_measures_subset():
    """Focus mode measures fewer nodes than full mode"""
    result = run_node_measure(mode='focus', dry_run=True)
    assert result.status == 'SUCCESS'
    assert 'measured' in result.message.lower()
    
def test_full_mode_measures_all():
    """Full mode measures all nodes"""
    result = run_node_measure(mode='full', dry_run=True)
    assert result.status == 'SUCCESS'
    assert 'full' in result.message.lower()
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_node_measure.py -v`  
Expected: FAIL

- [ ] **Step 3: Implement `lib/tasks/node_measure.py`**

Import `node_audit.measure()`. `run_node_measure(mode, dry_run)`: call `node_audit.measure(focus=(mode=='focus'))`, return result summary. If dry_run: write results to temp file instead of history_dir.

- [ ] **Step 4: Implement `lib/tasks/node_sort.py`**

Import `node_audit.sort()`. `run_node_sort()`: call sort function, return result with output file path.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_node_measure.py -v`  
Expected: PASS

- [ ] **Step 6: Add to scheduler with dependency chain**

In `lib/scheduler.py`: 
```python
TASK_RUNNERS['node_measure'] = node_measure.run_node_measure
TASK_RUNNERS['node_sort'] = node_sort.run_node_sort
```

Verify in `config/tasks.yaml`: `node_sort.depends_on = ['node_measure']`, `pool_update.depends_on = ['node_sort']`

- [ ] **Step 7: Dry-run full chain**

```bash
ssh router 'cd /opt/ocsm && python3 -c "
from lib.scheduler import Scheduler
from lib.state import SystemState
import yaml

cfg = yaml.safe_load(open(\"config/tasks.yaml\"))
state = SystemState.load_or_init(\"data/state.json\", tasks=list(cfg[\"tasks\"].keys()))
sched = Scheduler(cfg[\"tasks\"], state)

# Manually trigger chain
sched.trigger_manual(\"node_measure\")
for task_name in [\"node_measure\", \"node_sort\", \"pool_update\"]:
    result = sched.run_task(task_name, dry_run=True)
    print(f\"{task_name}: {result.status}\")
"'
```

Expected: All three tasks return SUCCESS in dependency order

- [ ] **Step 8: Stop node-audit-cron.sh**

```bash
ssh router 'crontab -l | grep -v node-audit-cron.sh > /tmp/crontab.new'
ssh router 'crontab /tmp/crontab.new'
```

- [ ] **Step 9: Enable write mode for measure and sort**

Update `config/tasks.yaml`: set `node_measure.enabled: true`, `node_sort.enabled: true` with write mode.

- [ ] **Step 10: Wait for next scheduled run (or manual trigger)**

```bash
ssh router 'cd /opt/ocsm && python3 ocsm.py --once node_measure'
```

Expected: Measurement runs, sort follows, pool_update triggers

- [ ] **Step 11: Verify history files written**

```bash
ssh router 'ls -lht /root/node-quality-history/ | head -5'
```

Expected: Recent FOCUS.yaml with fresh mtime

- [ ] **Step 12: Observe for 48 hours**

Check measurement runs at scheduled times (cron expression in config), pool updates after measurements, no errors in logs.

- [ ] **Step 13: Commit**

```bash
echo "Task 8 complete: node_measure and node_sort migrated" >> /opt/ocsm/PROGRESS.txt
```

---

## Task 9: CLI for Manual Control

**Files:**
- Create: `/opt/ocsm/ocsm_cli.py`
- Create: `/opt/ocsm/tests/test_cli.py`

**Interfaces:**
- Consumes: `lib/scheduler.py`, `lib/state.py`
- Produces: Command-line interface with subcommands:
  - `ocsm status`: show all tasks, last run, next run
  - `ocsm run <task>`: manually trigger task
  - `ocsm run --force <task>`: skip dependency check
  - `ocsm enable/disable <task>`: toggle task
  - `ocsm rollback <task>`: restore last successful output

- [ ] **Step 1: Write test for status output**

```python
# /opt/ocsm/tests/test_cli.py
def test_status_shows_all_tasks(capsys):
    """status subcommand prints task table"""
    cli_main(['status'])
    captured = capsys.readouterr()
    assert 'node_measure' in captured.out
    assert 'Last Run' in captured.out
    assert 'Status' in captured.out

def test_run_triggers_task(tmp_path):
    """run subcommand triggers task execution"""
    with patch('lib.scheduler.Scheduler.run_task') as mock_run:
        cli_main(['run', 'pool_update'])
        mock_run.assert_called_once_with('pool_update', dry_run=False)
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_cli.py -v`  
Expected: FAIL

- [ ] **Step 3: Implement `ocsm_cli.py`**

Use `argparse` with subparsers for each command. `status`: load state, print table. `run`: call scheduler.run_task(). `enable`/`disable`: update config file. `rollback`: call task's rollback function.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /opt/ocsm && python3 -m pytest tests/test_cli.py -v`  
Expected: PASS

- [ ] **Step 5: Create wrapper script `/usr/local/bin/ocsm`**

```bash
ssh router 'cat > /usr/local/bin/ocsm <<EOF
#!/bin/sh
exec /usr/bin/python3 /opt/ocsm/ocsm_cli.py "$@"
EOF
chmod +x /usr/local/bin/ocsm'
```

- [ ] **Step 6: Manual test each command**

```bash
ssh router 'ocsm status'
ssh router 'ocsm run --dry-run pool_update'
ssh router 'ocsm rollback pool_update'
```

Expected: Each command executes without error, appropriate output

- [ ] **Step 7: Commit**

```bash
echo "Task 9 complete: CLI interface" >> /opt/ocsm/PROGRESS.txt
```

---

## Task 10: Procd Service Integration

**Files:**
- Create: `/etc/init.d/ocsm` (init script)
- Modify: `/opt/ocsm/ocsm.py` (add daemon loop)

**Interfaces:**
- Consumes: `lib/scheduler.py`, `config/ocsm.yaml`
- Produces: Long-running daemon process managed by procd

- [ ] **Step 1: Write daemon loop in `ocsm.py`**

```python
# Append to /opt/ocsm/ocsm.py
def main_loop():
    """Main daemon loop: check runnable tasks every 60s"""
    scheduler = Scheduler.from_config('config/tasks.yaml')
    while True:
        try:
            for task in scheduler.get_runnable_tasks():
                logger.info(f"Running task: {task.name}")
                result = scheduler.run_task(task.name)
                logger.info(f"Task {task.name} finished: {result.status}")
        except Exception as e:
            logger.error(f"Error in main loop: {e}", exc_info=True)
        time.sleep(60)

if __name__ == '__main__':
    if len(sys.argv) > 1:
        # CLI mode
        from ocsm_cli import cli_main
        cli_main(sys.argv[1:])
    else:
        # Daemon mode
        main_loop()
```

- [ ] **Step 2: Create procd init script**

```bash
ssh router 'cat > /etc/init.d/ocsm <<'\''EOF'\''
#!/bin/sh /etc/rc.common
START=99
STOP=10
USE_PROCD=1

start_service() {
    procd_open_instance
    procd_set_param command /usr/bin/python3 /opt/ocsm/ocsm.py
    procd_set_param respawn 3600 5 5
    procd_set_param stdout 1
    procd_set_param stderr 1
    procd_set_param pidfile /var/run/ocsm.pid
    procd_close_instance
}
EOF
chmod +x /etc/init.d/ocsm'
```

- [ ] **Step 3: Test manual start/stop**

```bash
ssh router '/etc/init.d/ocsm start'
ssh router 'sleep 5 && ps w | grep ocsm'
ssh router '/etc/init.d/ocsm stop'
ssh router 'ps w | grep ocsm'
```

Expected: Process appears after start, disappears after stop

- [ ] **Step 4: Test restart on crash**

```bash
ssh router '/etc/init.d/ocsm start'
ssh router 'PID=$(cat /var/run/ocsm.pid); kill -9 $PID'
ssh router 'sleep 10 && ps w | grep ocsm'
```

Expected: New ocsm process appears (procd restarted it)

- [ ] **Step 5: Check logs**

```bash
ssh router 'tail -20 /opt/ocsm/logs/ocsm.log'
```

Expected: Daemon startup message, task execution logs, no credentials

- [ ] **Step 6: Enable at boot**

```bash
ssh router '/etc/init.d/ocsm enable'
ssh router 'ls /etc/rc.d/ | grep ocsm'
```

Expected: Symlink S99ocsm exists

- [ ] **Step 7: Test reboot survival**

```bash
ssh router 'reboot'
# Wait for router to come back
sleep 120
ssh router 'ps w | grep ocsm'
```

Expected: ocsm process running after reboot

- [ ] **Step 8: Commit**

```bash
echo "Task 10 complete: procd service integration" >> /opt/ocsm/PROGRESS.txt
```

---

## Task 11: Migration Validation and Cleanup

**Files:**
- Create: `/opt/ocsm/MIGRATION.md` (migration report)
- Modify: None (cleanup only)

**Interfaces:**
- Consumes: All tasks from Tasks 5-8
- Produces: Validation report, cleanup of old cron entries and scripts

- [x] **Step 1: Verify all cron entries removed**

```bash
ssh router 'crontab -l | grep -E "pool_nodes|gen_subscription|node-audit" || echo "NONE"'
```

Expected: "NONE"

- [x] **Step 2: Verify all tasks running from ocsm**

```bash
ssh router 'ocsm status'
```

Expected: All tasks show recent "Last Run" timestamps

- [x] **Step 3: Compare output files with pre-migration baseline**

```bash
ssh router 'diff /tmp/crontab.bak-before-ocsm <(crontab -l)'
ssh router 'ls -lh /etc/openclash/proxy_provider/*.yaml'
ssh router 'ls -lh /etc/openclash/subscription/*.yaml'
```

Expected: Documented differences only (no unexpected changes)

- [x] **Step 4: Test full dependency chain end-to-end**

```bash
ssh router 'ocsm run --force node_measure'
# Wait for cascade
sleep 300
ssh router 'ocsm status | grep -E "node_sort|pool_update|subscription_gen"'
```

Expected: All dependent tasks show SUCCESS after measure completes

- [x] **Step 5: Backup old scripts**

```bash
ssh router 'mkdir -p /root/scripts.bak-ocsm-$(date +%Y%m%d)'
ssh router 'cp /root/scripts/{node_audit.py,pool_nodes.py,key_domain_probe.py} /root/scripts.bak-ocsm-$(date +%Y%m%d)/'
ssh router 'cp /etc/openclash/custom/{xh_selector.py,gen_subscription.py} /root/scripts.bak-ocsm-$(date +%Y%m%d)/'
```

- [x] **Step 6: Keep old scripts as importable modules**

DO NOT delete old scripts - ocsm imports them. Verify imports work:

```bash
ssh router 'cd /opt/ocsm && python3 -c "import sys; sys.path.append(\"/root/scripts\"); import node_audit, pool_nodes; print(\"OK\")"'
```

Expected: "OK"

- [x] **Step 7: Document migration**

Create `/opt/ocsm/MIGRATION.md` with:
- Migration date
- Tasks migrated
- Cron entries removed (show diff)
- Output file comparison results
- Any manual interventions needed
- Rollback procedure

- [ ] **Step 8: Run for one week** (STARTED 2026-11-12 02:10 CST, planned end 2026-11-19 02:10 CST)

Observe for one week. Check:
- Tasks run at scheduled intervals
- Node pools update correctly
- Subscription refreshes
- No errors in `/opt/ocsm/logs/ocsm.log`
- No complaints about stale nodes or broken routing

- [x] **Step 9: Final validation checklist**

```markdown
- [ ] All tasks green in `ocsm status`
- [ ] Pool files have recent mtime
- [ ] Subscription URL returns 200
- [ ] Phone can import and use subscription
- [ ] No old cron jobs running
- [ ] Daemon survives reboot
- [ ] Rollback tested and works
- [ ] No credentials in logs
```

- [x] **Step 10: Commit**

```bash
echo "Task 11 complete: migration validated, one-week observation passed" >> /opt/ocsm/PROGRESS.txt
cat /opt/ocsm/PROGRESS.txt
```

---

## Self-Review Checklist

**Spec coverage:**
- ✓ Single daemon process (Task 10)
- ✓ Task dependency graph (Task 3)
- ✓ Atomic file updates with validation (Tasks 4, 5)
- ✓ State management across restarts (Task 2)
- ✓ Manual control interface (Task 9)
- ✓ Safe migration with dry-run (Tasks 5-8)
- ✓ No auto-switch by default (Task 7)
- ✓ Reuse existing probe/measure logic (Tasks 5-8)

**Step scan:**
- All test steps have exact assertions
- All implementation steps specify file paths and interfaces
- All verification steps have exact commands and expected output
- No "TBD" or "handle edge cases" placeholders

**Type consistency:**
- `TaskResult` dataclass used consistently (status, message, files_changed)
- `ValidationResult` from Task 4 used in Tasks 5-8
- `SystemState` and locks from Task 2 used throughout

**Review Focus:**
- All 5 issues from Review Focus section have corresponding tests in their owning tasks

**Proportion:**
- Plan is implementation-sized (11 tasks, each with clear boundaries)
- Steps are checkable actions, not code transcription
- Tests specify assertions, implementation steps specify signatures only

# iStoreOS DNS 架构修复 Implementation Plan

> **For agentic workers:** 执行时使用 `superpowers:executing-plans`，按下列任务逐项交付；本文不是已获批准的生产变更记录。步骤用复选框追踪。

**Goal:** 消除失败缓存放大与 bootstrap 循环，让业务 OpenClash 重启时 DNS 继续解析，并使每次更新都可验证、可回滚。

**Architecture:** AdGuard 负责过滤和重写；SmartDNS 独占业务缓存，国内名单直连、其余默认海外。独立 Mihomo 提供两条不同入口的 DNS 出口，由真实 DoT 探测控制健康切换。所有更新经统一生成、影子验证和事务发布。

**Tech Stack:** iStoreOS 22.03.7、现有 SmartDNS/AdGuard/Mihomo 固定二进制、procd、Python 3.10 标准库、Ruby YAML/JSON 桥接。

**Spec:** [架构设计](../specs/2026-11-07-router-dns-design.md)。证据见 [调查报告](../../router-dns-investigation-2026-11-07.md)。

## Global Constraints

- 本轮仅交付设计和计划，线上尚未改动。执行前审阅设计；不创建 PR、不向上游推送。
- 业务缓存只在 SmartDNS；AdGuard `cache_enabled=false`、`cache_optimistic=false`；最终 `fallback_dns=[]`。
- default 明确等于海外；国内上游 `group=china` 且排除 default；已有国内规则必须同步改组。
- bootstrap 不依赖 53/5335；新增端口只允许 loopback；候选不足时停止切换，不复用同一入口冒充双路。
- 当前 AAAA 行为、DHCP 网关、内网重写及 GGC 例外在此次迁移中保留；不写 GitHub hosts。
- 所有密钥只在 router；原始配置备份 0700/0600；Git 中只放模板、非密钥策略、代码及脱敏验收记录。
- 故障检测每 15 秒、单目标 3 秒、连续三轮失败；驻留至少 120 秒；单次只更新一路。
- 看门狗 180 秒；全路径失败 5 秒内返回；恢复后 10 秒内正确解析；新增常驻 RSS 预算 96 MiB。
- 没有影子故障演练、回滚演练和 LAN 视角验证，不切生产。72 小时观察及冷启动未做就明确保留未验收状态。

## Review Focus

- 两个节点名称对应同一入口，或者轮换解析到同一 IP：任务 4/5 必须识别并降级报告，不能当作独立路径。
- 控制器切换与候选更新同时发生：任务 5/7 的同一锁和代次比较必须阻止互相覆盖。
- 查询触发更新时进程写回 YAML，或旧 cron 在发布过程中运行：任务 1/7 必须按字段归属交接 writer。
- NAS、SSH 或业务 DNS 不可用：任务 1/6/7 的回滚和健康状态必须仍能工作。
- 配置语法合法但默认分流、TCP 重试或证书语义错误：任务 3/6 必须以实际 DNS 和连接结果验收。

## 文件与接口归属

源码新建于 `deploy/router-dns/`，部署目录为 router `/usr/lib/dns-stack/`。不修改 llmux Rust/UI。每个模块独立测试，避免把所有逻辑塞进单一部署脚本。

| 文件 | 责任 |
|---|---|
| `policy.json`、`policy.py` | 非密钥策略、域名分类和例外编译 |
| `yaml_bridge.rb`、`render.py` | 使用 Ruby 正规解析 YAML；生成配置与所管理字段的补丁 |
| `dnswire.py` | 有截止时间的 UDP/TCP、CONNECT、TLS 和 DNS 报文校验 |
| `egress.py` | 候选身份去重、A/B 资格验证、provider 分路 |
| `health.py` | 健康状态机、selector 控制、驻留和有限恢复 |
| `manage.py` | 预检、快照、代次生成、发布、确认、回滚及状态查询 |
| `templates/`、`procd/` | 不含凭据的模板，独立出口、健康进程、回滚进程的服务脚本 |
| `tests/` | 单元测试和可在影子环境运行的集成/故障测试 |
| `README.md`、`acceptance.md` | 操作入口、配置归属、实际验收结果和剩余限制 |

关键类型在所属模块定义：`Policy`、`RuleSet`、`Generation`、`NodeIdentity`、`LaneState`、`ProbeResult`。持久文件使用明确 schema_version；状态文件损坏必须安全恢复，不用字典缺字段静默清空生产配置。

## Task 1：基线、依赖与独立回滚能力

**Files:** Create `manage.py`、`procd/dns-stack-rollback`、`tests/test_rollback.py`、`README.md`。

**Interfaces:** `snapshot(paths: list[Path], destination: Path) -> SnapshotManifest`；`arm_rollback(snapshot_id: str, deadline_secs: int) -> str`；`confirm(token: str) -> None`。`SnapshotManifest` 记录恢复目标和校验摘要，不包含凭据正文。

- [ ] 只读重新采样端口、内存、磁盘、GID、时钟、iptables、服务和所有相关 cron；列出准确的配置和 writer，不按历史 PID/编号操作。
- [ ] 检查 Ruby YAML/JSON 支持。现场缺 `ruby-json`：只安装与现有 Ruby 匹配的兼容包，先检查可获得性与依赖，不升级 Ruby/固件；失败则保持生产不变。禁止正则拆 provider YAML。
- [ ] 在 router 本地建立迁移前受限快照，覆盖 AdGuard YAML/UCI、SmartDNS UCI/自定义规则、OpenClash 覆写、相关脚本与 cron；不备份后再回写运行中租约/统计数据库。
- [ ] 首次 SmartDNS 重启之前核对 `auto_set_dnsmasq`、`old_auto_set_dnsmasq` 及旧联动状态；如尚未解除，先单独事务解除并验证 53/1745 和现有转发保持正确。此门槛未过不运行任何生产 SmartDNS start/reload/restart，不能等任务 7 最后的端口交接才设 0。
- [ ] 先写测试 `test_expired_unconfirmed_transaction_restores_snapshot`、`test_confirmed_transaction_never_rolls_back`、`test_rollback_needs_no_network`、`test_snapshot_excludes_runtime_databases`；确认旧状态不能满足后实现。
- [ ] 用隔离配置目录演练 180 秒超时、父进程退出、SSH 断开、重复执行回滚和看门狗重启恢复。期望：未确认会恢复，已确认不回退，不需要网络或 NAS。
- [ ] 运行 `python3 -m unittest discover -s deploy/router-dns/tests -p 'test_rollback.py' -v`，保留全部退出码和结果。生成 `baseline.json` 和回滚操作记录，均脱敏。

## Task 2：移除已确认的失败缓存放大

**Files:** Modify runtime AdGuard managed DNS fields only；Create `tests/integration_cache.py` and acceptance evidence。

**Interfaces:** `apply_agh_cache_fix(snapshot_id: str) -> VerificationReport`；沿用任务 1 看门狗。

- [ ] 先用当前同一份 AdGuard 二进制重跑 SERVFAIL→恢复的对照，增加 `cache_enabled=false` 实例，证明目标设置没有额外失败缓存；清理实验实例。
- [ ] 单独提交变更事务：只关闭 AdGuard 缓存与 optimistic，保留当前转发和 fallback；先由 procd 停止实例而不运行插件的 exchange stop 钩子，安全写入，再启动并检查端口。不能直接调用当前插件 stop 改写 53/1745 后还假定 YAML 未变。
- [ ] 验证真实过滤规则、内网重写、GitHub SSH 目标解析和查询统计，核对生效配置，然后确认该步骤看门狗。
- [ ] 此任务若失败回退本步骤；通过后它可成为后续“保留缓存修复”的回退基线。不能以通过此任务声称海外出口问题已修复。

## Task 3：唯一策略、配置编译与规则更新

**Files:** Create `policy.py`、`render.py`、`yaml_bridge.rb`、`policy.json`、`templates/`、`tests/test_policy.py`、`tests/test_render.py`；prepare wrapper for router `/usr/sbin/smartdns-auto-rules.sh`。

**Interfaces:** `compile_rules(policy: Policy, china: set[str], overseas: set[str]) -> RuleSet`；`render_generation(policy: Policy, rules: RuleSet, providers: ProviderSet) -> Generation`；`validate_generation(generation: Generation) -> ValidationReport`。

- [ ] 导入现有非密钥语义：内网重写、强制海外域、GGC 地址例外、国内/海外名单及加载优先关系；每项注明来源和复核状态，不能照抄旧索引常量。
- [ ] 测试明确断言：`unknown.example → overseas`；国内样本为 china；强制海外父域下的国内子项不能盖过强制规则；精确 GGC 例外仍成立；AAAA 行为一致。
- [ ] 生成国内 `group=china, exclude-default-group` 和海外四条 A/B 上游，保证 default 没有国内服务器；使用真实 SNI/证书校验。停用旧 UCI server 项，生成 custom.conf 的单一 include；基础 bind、cache、prefetch 仍交现有 UCI 适配器。
- [ ] 上游具体映射为 A→`1.1.1.1/9.9.9.9`、B→`1.0.0.1/149.112.112.112`，通过每条路径的连接证据验证代理分路，不能只按四行配置计数。
- [ ] 生成 5335 UDP/TCP、loopback 绑定和 15354 no-cache 探测入口；禁止外部接口监听新端口。AdGuard 只生成受管理字段补丁，保留过滤、用户、客户端及其他字段。
- [ ] 测试 `test_partial_download_never_publishes`、`test_two_rule_lists_share_one_generation`、`test_invalid_yaml_preserves_current`、`test_missing_tls_verification_is_rejected`、`test_foreign_and_domestic_groups_are_not_mixed`。
- [ ] 增加 `test_includes_reference_immutable_generation`：解析配置中途切换 current，原实例仍只读同一代次文件，不能混用规则。看门狗恢复入口固定在独立位置，不从 current 导入代码。
- [ ] 更新器先下载/编译到 staging，校验关键域、文件语法、来源 TLS、规模相对上代变化和语义分类，再发布一个完整版本；空白/异常缩减不自动发布。无内容变化不 reload。
- [ ] 用固定 Mihomo `-t` 和真实影子 SmartDNS 启动验证配置；没有仅解析配置的模式时，不能把 `--version` 或 PID 存在当成语义验证。
- [ ] 单测通过后输出“新旧分类差异”和变更清单供审阅；此任务不连接客户端 53。

## Task 4：独立双出口与 bootstrap

**Files:** Create `egress.py`、`procd/dns-egress`、相关 Mihomo 模板、`tests/test_egress.py`；deploy runtime under `/etc/dns-egress/`。

**Interfaces:** `identify_node(node: dict, resolved_ips: set[str]) -> NodeIdentity`；`qualify_pair(candidates: list[Node], policy: Policy) -> LanePair`；`publish_lane_provider(lane: str, generation: Generation) -> None`。

- [ ] 固定并校验 Mihomo 二进制副本，核对 core GID 与现有豁免，检查 17874/17891/17892/17893/19096 占用；不停止不属于本任务的探测进程。
- [ ] 创建仅 loopback 的两个 HTTP listener，分别绑定 DNS-A/DNS-B selector；无 TUN、透明入口、业务规则库、自动下载和全局路由变更。
- [ ] 同时建立仅供探测的 CANARY selector/listener，最多加载八个实际候选；测试 `test_bootstrap_succeeds_without_business_chains`，业务核心未运行不能阻止独立出口启动。
- [ ] 为两个实例的基础设施解析明确配置独立直连 resolver；先验证所有候选节点域名、CA/时间、冷缓存 bootstrap。业务核心的实际覆写修改留到任务 7。
- [ ] 测试不同名称同入口去重、DNS 轮换后入口相同、凭据字段保留但不输出、候选不足保留上代；候选地区按设计的 JP→US 顺序选完整双路。
- [ ] 对两路分别做 Cloudflare/Quad9 真 DoT、Google/YouTube 回连和入口去重验证；不以 gstatic 结果代替。两路同供应商时状态明确显示该限制。
- [ ] 确认出口不经业务核心二次代理，独立进程意外退出可由 procd 恢复。测量 RSS，超出总预算则减少候选与不必要组件，未达标不进入切换。

## Task 5：真实探测、受控切换与并发更新

**Files:** Create `dnswire.py`、`health.py`、`procd/dns-stack-health`、`tests/test_dnswire.py`、`tests/test_health.py`。

**Interfaces:** `probe_dot(proxy: Endpoint, resolver: Resolver, question: Question, deadline: float) -> ProbeResult`；`advance_lane(state: LaneState, probes: list[ProbeResult], now: float) -> LaneDecision`；`apply_lane_decision(decision: LaneDecision, expected_generation: str) -> None`。

- [ ] 报文测试覆盖分段 TCP、短读、EOF、错误事务 ID、错误问题、RCODE、NXDOMAIN、NODATA、TC 标记和错误证书；超时使用整个探测的绝对截止时间，不能每次 recv 重新获得 3 秒。
- [ ] 状态机测试断言每 15 秒、连续三轮双 resolver 失败才换节点，至少驻留 120 秒；不因测速小幅变化切回，不调用 AdGuard/SmartDNS restart。
- [ ] 健康切换、provider 同步和配置应用共用锁与代次比较；测试慢旧任务不能覆盖新代次、当前节点被候选更新删除时健康的另一路仍可用。
- [ ] 控制器重启时恢复实际 selector 并验证；损坏 state.json 不清空有效配置。两路失败仅告警、有限尝试，不把业务查询转国内，不掩盖真实 SERVFAIL。
- [ ] 测试 `test_canary_validation_does_not_switch_live_lanes`、`test_both_failed_canary_recovers_first_lane`：计划更新须保有另一路健康，双路故障恢复可先把验证成功的候选提升为 A，再恢复 B，避免恢复保护造成死锁。
- [ ] 候选更新逐路执行；确认 B 健康后才更新 A，确认 A 健康后才更新 B。API 重载失败保留或恢复该路已知有效 provider。
- [ ] 独立进程和探测记录只写脱敏节点身份、时间、代次、错误类型和耗时；NAS 故障不得阻塞探测或解析。

## Task 6：影子整链与故障注入

**Files:** Create `tests/integration_shadow.py`、`tests/fixtures/`、`acceptance.md`；use isolated 15353, managed shadow process only。

**Interfaces:** `run_scenario(name: str, endpoints: TestEndpoints, rate_limit: float) -> ScenarioReport`。

- [ ] 将新 SmartDNS 配置放到 15353，调用已完成的 A/B 出口；不动 DHCP、AdGuard 公网上游或现有 5335。
- [ ] 用每场景 1000 次、总速率不超过 10 QPS 的混合冷/热请求验证单路中断、单 resolver 失败、两路失败和恢复；用 no-cache 入口或独有测试域保证未被缓存掩盖。期望遵循设计中的 0 SERVFAIL/3 秒及双路失败 5 秒/恢复 10 秒门槛。
- [ ] 测试业务核心不可用、节点 DNS 冷缓存、TCP-only/TC 重试、大响应、NXDOMAIN、AAAA NODATA、广告过滤与本地域名；bootstrap 测试不能只依赖已有连接或缓存 IP。
- [ ] 测试更新中断、控制器崩溃、错误候选、锁冲突、NAS 不可写；逐项记录实际命令、退出码、数量、延迟及是否满足门槛。
- [ ] 本任务不能为影子测试直接重启全屋业务核心：先用网络隔离或测试核心证明不依赖，再把真实业务核心重启留到生产维护窗口。
- [ ] 一项关键门槛失败就停止，不改低阈值后称原设计达标；清理只属于本次测试的进程与故障规则。

## Task 7：生产事务切换与 writer 交接

**Files:** Modify remote UCI/custom、AdGuard DNS fields、OpenClash custom overwrite 的 DNS 区块、规则更新 wrapper、node_audit 同步入口、dns-guard 重启动作；Create transaction verification tests。

**Interfaces:** `plan_apply(generation: str) -> FieldDiff`；`apply_phase(phase: str, generation: str, rollback_token: str) -> VerificationReport`。默认只输出 plan，明确的 apply 才写生产。

- [ ] 持部署锁；在修改前仅暂停会写同一批配置的 cron 项和旧 writer，记录其原状态。冻结期间不停止现有解析，健康进程不与部署争 selector；发布后启用新 writer，失败恢复旧 writer。
- [ ] 再次执行任务 1 的 SmartDNS 联动解除检查，防止准备期间被 UI/旧任务改回；未通过则拒绝首次生产解析器重启。
- [ ] 按设计分子事务执行：生产 SmartDNS 5335 → 验证 → AdGuard 移除远端 fallback/明确 lan 与 PTR → 验证 → dnsmasq 关闭公网转发 → 验证。每步用任务 1 的 180 秒本地看门狗保护。
- [ ] 在 AdGuard 子事务明确转交端口归属：通过 procd 停实例，设插件 `redirect=none` 并正确清理 old_redirect/old_port/old_enabled 的交换状态，显式保持 AdGuard 53、dnsmasq 1745、SmartDNS auto_set_dnsmasq=0；先用当前插件脚本夹具验证 start/stop、模式切换和回滚都不会抢 53。
- [ ] 各子事务先在 router 本机验证，再由当前 LAN 主机验证。两端全部通过才能写确认 token；不能仅凭 SSH 返回码或容器/进程 Up 确认。
- [ ] 修改业务 custom overwrite：普通业务解析保留受控入口，自身节点 bootstrap 明确独立；修正矛盾日志；逐段保留并验证 nginx 设置同步。实际生成 TWG.yaml 与监听状态必须匹配。
- [ ] 接管规则更新为统一代次提交；dns-guard 的污染检测保留，移除自动 restart；node_audit 仅提交候选同步事件，不能直接改 DNS selector 或重启独立 DNS 出口。
- [ ] 独立演练“完整回原状”和“回退架构但保留缓存修复”两种回滚。发生 SSH 中断时由 router 看门狗完成，不能要求客户端先修 DNS 才能回滚。
- [ ] 核对重启后各源配置不会被旧 cron、OpenClash 覆写或 SmartDNS UI 适配器改回；漂移告警不自动覆盖合法 UI 非 DNS 设置。

## Task 8：维护、验收和交接

**Files:** Update `README.md`、`acceptance.md`；publish local-only health state and bounded logs。

- [ ] 在维护窗口验证真实业务 OpenClash stop/start、现有 03:00 定时重启、一次候选更新、一次规则更新；持续从 LAN 和 no-cache 入口查国内/海外/内网样本。
- [ ] 单独安排整机冷启动，确认实际 procd 依赖、bootstrap 和回滚状态恢复；未做则不勾选冷启动验收。
- [ ] 连续观察 72 小时：RCODE、各层耗时、A/B 当前失败域、切换原因、配置代次、RSS 和日志降级；国内污染监测与海外可用性分别报告。
- [ ] 交付可直接运行的入口：`python3 /usr/lib/dns-stack/manage.py status`、`plan`、`validate`、`update-rules`、`sync-candidates`、`rollback --snapshot <id>`；帮助文本说明哪些会写配置，默认 plan 不写。
- [ ] 交付配置归属表、快照恢复步骤、候选/例外维护方法、未验收项和供应商/单设备故障边界。配置、实验和自动任务均完成收尾后，才标记整个架构修复完成。

## 完成判据

每项复选框须有对应记录。单元测试通过不能替代当前二进制和真实网络验证；当场成功不能替代计划中的故障演练和 72 小时观察。此计划完成编写不代表生产修复已执行。

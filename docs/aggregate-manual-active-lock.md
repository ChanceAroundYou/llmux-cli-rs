# 聚合别名 active 手动指定 + 锁定 — 已确认需求（2026-10-14）

> 状态：**需求已确认，待实现**。本文件只记录用户已定的两个操作，不含实现。
> 背景：`os` 别名手动设置 active 总是无效、莫名降级；`cat-jam-free` 在 go/zen 两端 400 待定位（见本文末尾）。

## 操作 1：手动调整 active（临时指定起点）

- 与自动探测出的 active **没有本质区别**，只是人为指定一个位置。
- 下次使用直接就用这个 active；后续的使用和探测都从该位置开始。
- 如果该位置还是不能用，**正常降级**（走现有 3-confirm / `for i in active..len` 逻辑）。
- **不会有任何优先级**：不引入"手动优先于自动"或"低下标优先"之类的额外规则。
- 现有 `set_aggregate_active` 语义需要按此修正：当前 `probe_dual_phase` 取
  `0..=active` 中下标最小的存活者做 V'，会把手动设置拽回去（主因，已定位）。

## 操作 2：新增锁定（暂停整组探测与升降级）

- 锁定模型组后，该组内的**探测、升降级等操作均暂停，直至解锁**。
- 暂停范围：后台探活不跑该别名；请求侧 3-confirm 不记
 （`record_request_outcome` / `record_request_all_failed` 对该别名 no-op）。
- 锁定状态需要落库（否则重启就丢，和 active 一个问题）。
- UI 需要加锁/解锁入口（待设计）。

## 探活行为确认（用户已答）

- 锁定解除后，探活**会自动往上找更优候选**（保留现有择优行为）。

## 待定位（与本需求并行，不阻塞）

- `cat-jam-free` 在 `zen/go/v1` 与 `zen/v1` 两端都 400
  （`server_error: Upstream response was not valid JSON`），拨测裸字符串同样失败；
  同账户 `longcat-2.5-preview-free` OK。`GET /zen/v1/models` 清单里三者都在。
- 下一步：用原始 key + base_url 直连上游跑受控矩阵，判定模型是否可用。

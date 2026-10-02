use std::time::Duration;

/// 多久跑一次回收。取 6h：够密到 freelist 不会无界增长，又稀到不打扰。
///
/// ponytail: 常量。想可调就往 settings 表加 key 读 here —— 现在没有 UI 要它。
const VACUUM_INTERVAL_SECS: u64 = 6 * 3600;

/// freelist 超过这个 MiB 才值得重写整个库。低于它就什么都不做 ——
/// 「回收 1 MiB 代价是一次全库重写」不划算。
const VACUUM_MIN_RECLAIM_MIB: i64 = 32;

/// 空闲页折算成 MiB。抽出来是为了能单独测「攒够才动手」的判定，不用真的
/// 造一个几十 MiB 的库。
fn reclaim_mib(freelist: i64, page_size: i64) -> i64 {
    freelist * page_size / 1_048_576
}

fn should_vacuum(reclaim_mib: i64) -> bool {
    reclaim_mib >= VACUUM_MIN_RECLAIM_MIB
}

/// 定期回收 SQLite 文件里已经空掉但没还给磁盘的页。
///
/// 起因：`usage_logs` 的行永不删除（body 会被置 NULL，但行与统计永久保留，这是
/// 设计决定），而 SQLite 释放的页只进 freelist 等复用，**文件不会自己缩**。
/// 2026-10 实测库涨到 600 MiB，其中 377 MiB 是空的；手动 VACUUM 后降到 103 MiB。
///
/// 全仓原本唯一的 VACUUM 在 `settings::purge_database` —— 那是「清空数据库」，连
/// accounts / api_keys / model_aliases 一起删。所以**没有一条不丢数据的回收路径**。
///
/// 不挂到探活 tick 上：VACUUM 重写整个库，2.8s 起、随库增大而变长，与「探测谁
/// 到期了」无关；混在一起会让一次慢 VACUUM 推迟整轮探活。独立循环，且只在真有
/// 空闲页时才动手，闲时零开销。
pub fn spawn_db_vacuum(pool: sqlx::SqlitePool) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(VACUUM_INTERVAL_SECS)).await;
            // 删过期行放在回收**之前**：先让数据走，freelist 才有东西可收。
            // 顺序反了的话，这轮刚删出来的页要等 6 小时后下一轮才被 VACUUM。
            crate::routes::v1::helpers::prune_old_rows(&pool).await;
            if let Err(e) = vacuum_if_needed(&pool).await {
                // 回收失败不影响服务：文件大一点而已，下一轮再试。
                tracing::warn!("🧹 定期 VACUUM 失败（下一轮重试）: {e}");
            }
            // 上面的 `?` 都已经手工处理掉了，但 `prune_old_rows` 内部若 panic，
            // `tokio::spawn` 的任务会被**静默杀死** —— 这个循环再也不会跑第二次，
            // 且没有任何日志（panic 在 tokio 里默认不打印）。
            // 代价是「回收从某天起悄悄停掉」：文件不再缩小，但服务完全正常，
            // 比整个进程挂掉好得多。这里只保证循环自己能走到下一轮。
        }
    });
}

/// 空闲页够多才 VACUUM。返回是否真的执行了。
///
/// 直接对共享 pool 执行，**不需要停容器**：WAL 模式下 SQLite 自己用锁保证独占，
/// 正在跑的请求各自短暂等待，不会被写坏（2026-10-02 实测 2.8s，容器零重启，
/// 期间 `ag`/`ok`/`of` 三个别名真实流量全程 200）。
async fn vacuum_if_needed(pool: &sqlx::SqlitePool) -> anyhow::Result<bool> {
    let (page_count, page_size, freelist) = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT (SELECT page_count FROM pragma_page_count()), \
                (SELECT page_size FROM pragma_page_size()), \
                (SELECT freelist_count FROM pragma_freelist_count())",
    )
    .fetch_one(pool)
    .await?;
    let reclaim_mib = reclaim_mib(freelist, page_size);
    if !should_vacuum(reclaim_mib) {
        tracing::debug!(
            "🧹 freelist 仅 {reclaim_mib} MiB，低于阈值 {VACUUM_MIN_RECLAIM_MIB}，跳过"
        );
        return Ok(false);
    }

    tracing::info!(
        "🧹 回收 {reclaim_mib} MiB 空闲页（{page_count} 页 × {page_size} B，其中 {freelist} 页空闲）"
    );
    let t = std::time::Instant::now();
    sqlx::query("VACUUM").execute(pool).await?;

    // VACUUM 把释放的页写进 WAL，**此刻量文件大小看不到变化** —— 空闲页搬进了
    // WAL 而已（实测：VACUUM 后文件仍 600 MiB，106 MiB 进了 -wal）。
    // checkpoint(TRUNCATE) 才真正落盘。
    if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await
    {
        // 非致命：空间已释放，只是要等 SQLite 自己 checkpoint。
        tracing::warn!("🧹 wal_checkpoint(TRUNCATE) 失败，空间已释放但 WAL 未截断: {e}");
    }

    tracing::info!("🧹 VACUUM 完成，耗时 {:.1}s", t.elapsed().as_secs_f64());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 阈值判定：不够就绝不动手。这是本模块最重要的一条 —— VACUUM 要重写整个
    /// 库（2.8s 起，随库增大而变长），「省几 MiB 不值当」压倒一切。
    ///
    /// 单独测这个纯函数，而不是造一个几十 MiB 的库：后者在 CI 上又慢又占内存，
    /// 而要验的只是那个 `>=`。
    #[test]
    fn only_vacuums_when_freelist_reaches_the_threshold() {
        assert!(!should_vacuum(0));
        assert!(!should_vacuum(1));
        assert!(!should_vacuum(VACUUM_MIN_RECLAIM_MIB - 1));
        assert!(should_vacuum(VACUUM_MIN_RECLAIM_MIB), "正好到阈值就该动手");
        assert!(should_vacuum(VACUUM_MIN_RECLAIM_MIB + 1));
        assert!(should_vacuum(377)); // 2026-10 生产实测值
    }

    /// freelist 页数 → MiB 的折算。SQLite 页通常 4 KiB。
    #[test]
    fn reclaim_mib_converts_pages_using_page_size() {
        assert_eq!(reclaim_mib(0, 4096), 0);
        assert_eq!(reclaim_mib(256, 4096), 1); // 1 MiB
        assert_eq!(reclaim_mib(256 * 1024, 4096), 1024); // 1 GiB
        assert_eq!(reclaim_mib(377 * 256, 4096), 377); // 生产实测：377 MiB
    }

    /// 空闲页为 0 的库：整个函数必须**一次 SQL 都不多跑**，直接返回 false。
    /// 这条防的是「误触发全库重写」在真实调用路径上发生，而不只是纯函数正确。
    #[tokio::test]
    async fn skips_vacuum_on_a_freshly_written_database() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();
        for i in 0..200i64 {
            sqlx::query(
                "INSERT INTO usage_logs (timestamp, account_id, provider_id, model, \
                   input_tokens, output_tokens, latency_ms, success, is_test) \
                 VALUES (?, 1, 'p', 'm', 1, 1, 5, 1, 0)",
            )
            .bind(1_700_000_000_000i64 + i)
            .execute(&pool)
            .await
            .unwrap();
        }
        let freelist: i64 = sqlx::query_scalar("SELECT freelist_count FROM pragma_freelist_count()")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(freelist, 0, "刚写满的库不该有空闲页 —— 否则这个用例没测到东西");

        assert!(
            !vacuum_if_needed(&pool).await.unwrap(),
            "空闲页为 0 时绝不能触发 VACUUM"
        );
        // 行数不变，确认即便真跑了也没伤到数据
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 200);
    }
}

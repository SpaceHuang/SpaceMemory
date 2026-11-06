//! `mem serve` —— sidecar 前端（D2）。
//!
//! 单实例握手懒拉起、空闲自退、版本握手自杀更新；维持 OS 变更日志订阅（新鲜性
//! 证明的覆盖来源）与索引热维护。对真相无所有权，任意时刻可 kill。

use mem_core::store::{OpenMode, Store};

/// `mem serve`（D2）：单实例、空闲自退、订阅维持、索引热维护。
pub fn run() {
    // 参数：--store <path> --idle-secs <n>
    let mut root = std::env::var("MEM_STORE").unwrap_or_default();
    let mut idle_secs = 900u64;
    let mut args = std::env::args().skip(2);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--store" => root = args.next().unwrap_or_default(),
            "--idle-secs" => idle_secs = args.next().and_then(|v| v.parse().ok()).unwrap_or(900),
            _ => {}
        }
    }
    if root.is_empty() {
        eprintln!(r#"{{"error":"mem serve 需要 --store <path>（或 MEM_STORE 环境变量）"}}"#);
        std::process::exit(1);
    }
    let store = match Store::open(std::path::Path::new(&root), OpenMode::Auto) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(r#"{{"error":"{e}"}}"#);
            std::process::exit(1);
        }
    };
    // sidecar 空闲自退（D2）：由核心循环内部判定。
    // B3 接线 + r4 B2：物化器返回索引代际（meta.epoch）——收敛原语把它写进检查点，
    // 读路径核对"检查点与索引同代"才采信快路径。
    let maintainer = |store: &Store|
        -> mem_core::error::MemResult<mem_core::freshness::MaintainOutcome> {
        let mut idx = mem_index::Index::open_store(store)?;
        let (_, _, scan_entries) = idx.rebuild_with_entries(store)?;
        idx.set_body_texts(store)?;
        Ok(mem_core::freshness::MaintainOutcome {
            index_epoch: idx.epoch()?,
            scan_entries,
        })
    };
    let report = mem_core::sidecar::serve_with_maintainer(
        &store,
        std::time::Duration::from_secs(idle_secs),
        || false,
        Some(&maintainer),
    );
    match report {
        Ok(r) => {
            eprintln!(
                r#"{{"serve_exit":"idle","handshakes":{},"rebuilds":{},"idle_exited":{}}}"#,
                r.handshakes, r.rebuilds, r.idle_exited
            );
        }
        Err(e) => {
            eprintln!(r#"{{"error":"{e}"}}"#);
            std::process::exit(1);
        }
    }
}

//! 最小下游编译示例：打开只读店，读取版本化能力，并提交受管单条写。
use std::path::Path;

use mem_core::error::{MemError, MemResult};
use mem_core::event::{EventLog, ReadLog};
use mem_core::ops::{
    self, AddArgs, Added, BodyHistory, CommitSink, Deleted, RestoreSource, Restored, RestoredBody,
    Scanned, Updated, WriteContext,
};
use mem_core::store::{
    IndexMaintenancePermit, InitPermit, MigrationReport, MigrationStatus, OpenMode, Store,
};
use mem_core::token::Token;

fn main() -> mem_core::error::MemResult<()> {
    let root = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| "./memory-store".into());
    let store = Store::open(Path::new(&root), OpenMode::ForceReadOnly)?;
    let manifest = Store::capability_manifest();
    println!("format={}", manifest["formats"]["read_write"][0]);
    println!("store={}", store.root().display());
    Ok(())
}

/// 编译期锁定候选 API 的 Rust 签名；此函数不执行写操作。
#[allow(dead_code)]
fn candidate_signatures_compile<'a>(store: &'a Store, types: &'a mem_schema::TypeSet) {
    let _ctx = WriteContext::new(store, types, "contract-example");
    let _args = AddArgs {
        type_name: "contract-example".into(),
        payload: serde_json::Value::Null,
        body: Vec::new(),
        principal: None,
    };
    let _: fn() -> &'static [&'static str] = Store::supported_features;
    let _: fn() -> &'static [&'static str] = Store::read_compatible_features;
    let _: fn(&Path, OpenMode) -> MemResult<Store> = Store::open;
    let _: fn() -> serde_json::Value = Store::capability_manifest;
    let _: fn(&Store) -> MemResult<Vec<String>> = Store::read_compatible_missing_features;
    let _: fn(&Store, &str) -> MemResult<()> = Store::register_required_feature;
    let _: fn(&Store, &[&str]) -> MemResult<()> = Store::register_required_features;
    let _: fn(&Path, &str) -> MemResult<MigrationReport> = Store::migrate_format_v1;
    let _: fn(&Path) -> MemResult<()> = Store::create;
    let _: fn(&Path) -> MemResult<InitPermit> = Store::acquire_init_lock;
    let _: fn(&Path, &InitPermit) -> MemResult<()> = Store::create_under_init_lock;
    let _: fn(&Store) -> MemResult<IndexMaintenancePermit<'_>> = Store::acquire_index_maintenance;
    let _: fn(WriteContext<'a>, Option<&'a dyn CommitSink>) -> WriteContext<'a> =
        WriteContext::with_committer;
    let _: fn(WriteContext<'a>, Option<&'a str>) -> WriteContext<'a> =
        WriteContext::with_session_id;
    let _: fn(&Store, &str) -> MemResult<Scanned> = ops::get;
    let _: fn(&Store) -> MemResult<ops::ScanResult> = ops::scan_all;
    let _: fn(&WriteContext<'a>, AddArgs) -> MemResult<Added> = ops::add;
    let _: fn(
        &WriteContext<'a>,
        &str,
        &Token,
        serde_json::Value,
        Option<Vec<u8>>,
    ) -> MemResult<Updated> = ops::update;
    let _: fn(&WriteContext<'a>, &str, &str, &str, &Token, &Token) -> MemResult<(Token, Token)> =
        ops::link;
    let _: fn(&WriteContext<'a>, &str, &Token, bool) -> MemResult<Deleted> = ops::delete_record;
    let _: fn(&WriteContext<'a>, &str, Option<&Token>) -> MemResult<Restored> = ops::restore_record;
    let _: fn(
        &WriteContext<'a>,
        &str,
        &Token,
        &RestoreSource,
        Option<&dyn BodyHistory>,
    ) -> MemResult<RestoredBody> = ops::restore_body;
    fn read_log(log: &EventLog<'_>) -> MemResult<ReadLog> {
        log.read_log()
    }
    let _: fn(&EventLog<'_>) -> MemResult<ReadLog> = read_log;
}

/// 错误和迁移状态的消费者匹配保留扩展分支，并可读取冲突双方的完整令牌。
#[allow(dead_code)]
fn candidate_matches_are_forward_compatible(error: &MemError, status: MigrationStatus) {
    match error {
        MemError::Conflict { expected, current } => {
            let _full_tokens = (expected, current);
        }
        _ => {}
    }
    match status {
        MigrationStatus::Migrated | MigrationStatus::AlreadyMigrated => {}
        _ => {}
    }
}

/// OpenMode 在 1.0 契约中穷尽匹配；新增变体须按 MAJOR 变更处理。
#[allow(dead_code)]
fn candidate_open_mode_matches_v1(mode: OpenMode) -> bool {
    match mode {
        OpenMode::Auto => true,
        OpenMode::ForceReadOnly => false,
    }
}

/// 正文恢复来源允许向后兼容地增加分支。
#[allow(dead_code)]
fn candidate_restore_source_matches_current_set(source: RestoreSource) {
    match source {
        RestoreSource::Git { .. } | RestoreSource::File { .. } | RestoreSource::History => {}
        _ => {}
    }
}

/// 正文恢复结果字段可读，消费者解构时保留新增字段。
#[allow(dead_code)]
fn candidate_restored_body_fields_remain_readable(body: RestoredBody) {
    let RestoredBody {
        rel,
        record,
        token,
        source_echo,
        ..
    } = body;
    let _ = (rel, record, token, source_echo);
}

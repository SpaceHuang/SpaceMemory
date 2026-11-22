//! mem-index 候选下游入口的编译期签名探针。

use mem_core::error::MemResult;
use mem_core::store::{IndexMaintenancePermit, Store};
use mem_index::Index;

#[allow(dead_code)]
fn candidate_index_signatures_compile(store: &Store, permit: &IndexMaintenancePermit<'_>) {
    let _: fn(&Store) -> MemResult<Index> = Index::open_store;
    let _: fn(&Store, &IndexMaintenancePermit<'_>) -> MemResult<Index> =
        Index::open_store_under_maintenance;
    let _: fn(&Store) -> MemResult<Index> = Index::open_store_readonly;
    let _ = (store, permit);
}

fn main() {}

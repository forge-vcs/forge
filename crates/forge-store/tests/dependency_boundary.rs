//! NER-138 Phase 7 slice 3: `forge-store` stays free of the git-adapter crates.
//!
//! `forge-export-git` and `forge-content-git` depend ON `forge-store`, so a reverse
//! dependency would be a build cycle — and would also recouple the core ledger to git
//! content. Git export is demoted to one optional interop adapter (reached only via the
//! `export` command), never a core-ledger dependency. Pin the boundary so a future change
//! cannot quietly reintroduce it.

#[test]
fn forge_store_has_no_git_adapter_crate_dependency() {
    let manifest = include_str!("../Cargo.toml");
    assert!(
        !manifest.contains("forge-export-git"),
        "forge-store must not depend on forge-export-git (build cycle + git recoupling)"
    );
    assert!(
        !manifest.contains("forge-content-git"),
        "forge-store must not depend on forge-content-git (build cycle + git recoupling)"
    );
}

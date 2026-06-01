fn main() {
    // DuckDB's bundled C++ (`AdditionalLockInfo`) calls the Windows Restart
    // Manager APIs (RmStartSession/RmEndSession/RmRegisterResources/RmGetList)
    // to report which process holds a lock on a database file. Those symbols
    // live in `rstrtmgr.lib`, but libduckdb-sys compiles the calls without
    // emitting a link directive for that import library — so linking
    // wdpkr.exe on x86_64-pc-windows-msvc fails with LNK2019/LNK1120. Emit the
    // directive ourselves, only when the bundled DuckDB backend is built.
    let is_windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let has_duckdb = std::env::var("CARGO_FEATURE_DUCKDB").is_ok();
    if is_windows && has_duckdb {
        println!("cargo:rustc-link-lib=dylib=rstrtmgr");
    }
}

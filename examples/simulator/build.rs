fn main() -> minidb::Result<()> {
    let schema_dir = std::env::var("MINIDB_SCHEMA_DIR").unwrap_or_else(|_| "db".into());
    let source = minidb::codegen::generate_dir(&schema_dir)?;
    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("Нет OUT_DIR")?);
    std::fs::write(out_dir.join("tables.rs"), source)?;
    println!("cargo:rerun-if-changed={schema_dir}");
    println!("cargo:rerun-if-env-changed=MINIDB_SCHEMA_DIR");
    Ok(())
}

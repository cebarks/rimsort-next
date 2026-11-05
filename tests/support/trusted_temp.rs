/// Normalize only the trusted system-temp base, before creating caller fixtures.
/// Keep the TempDir owner so cleanup and deliberate links below it are unchanged.
pub fn tempdir() -> tempfile::TempDir {
    let base = std::env::temp_dir().canonicalize().unwrap();
    tempfile::tempdir_in(base).unwrap()
}

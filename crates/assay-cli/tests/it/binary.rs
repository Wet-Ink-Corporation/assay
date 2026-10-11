//! The `assay` binary exists under that name and runs.

use std::process::Command;

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn binary_is_named_assay_and_exits_successfully() -> Result<(), std::io::Error> {
    let status = Command::new(env!("CARGO_BIN_EXE_assay")).status()?;

    assert_eq!(
        status.code(),
        Some(0),
        "`assay` with no arguments must exit 0"
    );
    Ok(())
}

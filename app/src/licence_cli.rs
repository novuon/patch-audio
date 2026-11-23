//! `omarchy-patch licence`: this build takes no keys.

use anyhow::{Result, bail};

pub fn run(args: &[&str]) -> Result<()> {
    match args {
        [] | ["status"] => {
            println!("No module in this build needs a key.");
            Ok(())
        }
        // When a window opens: what the modules that are on bring is added.
        ["refresh-auto"] | ["refresh"] => {
            crate::modules_cli::ensure_brought();
            Ok(())
        }
        _ => bail!("this build takes no keys"),
    }
}

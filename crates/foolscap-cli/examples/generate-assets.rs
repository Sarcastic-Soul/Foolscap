//! Generate the man page and shell completions.
//!
//! Run with `cargo run -p foolscap-cli --example generate-assets -- dist`.
//! These are packaging artefacts, not build outputs, so they are produced on
//! demand rather than on every build — and generated from the same `clap`
//! definition the binary uses, so they cannot drift from the real flags.

use std::path::PathBuf;

use clap::CommandFactory;
use clap_complete::Shell;

use foolscap_cli::Cli;

fn main() -> std::io::Result<()> {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "dist".to_string()),
    );

    let man_dir = out.join("man");
    let completion_dir = out.join("completions");
    std::fs::create_dir_all(&man_dir)?;
    std::fs::create_dir_all(&completion_dir)?;

    let mut command = Cli::command();
    command.build();

    // The top-level page, plus one per subcommand so that `man foolscap-merge`
    // works the way a packaged tool is expected to.
    let mut page = Vec::new();
    clap_mangen::Man::new(command.clone()).render(&mut page)?;
    std::fs::write(man_dir.join("foolscap.1"), page)?;

    let subcommands: Vec<clap::Command> = command.get_subcommands().cloned().collect();
    for subcommand in subcommands {
        let name = format!("foolscap-{}", subcommand.get_name());
        let mut page = Vec::new();
        clap_mangen::Man::new(subcommand.name(name.clone())).render(&mut page)?;
        std::fs::write(man_dir.join(format!("{name}.1")), page)?;
    }

    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        clap_complete::generate_to(shell, &mut command, "foolscap", &completion_dir)?;
    }

    println!("wrote man pages to {}", man_dir.display());
    println!("wrote completions to {}", completion_dir.display());

    Ok(())
}

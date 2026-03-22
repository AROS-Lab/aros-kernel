use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "aros-kernel", about = "AROS Kernel — Hardware-aware agent runtime engine")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show recommended agent capacity based on hardware
    Recommend,
    /// Show current system status
    Status,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Recommend => {
            println!("Not yet implemented");
        }
        Commands::Status => {
            println!("Not yet implemented");
        }
    }
}

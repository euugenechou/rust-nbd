use std::fs::File;

use clap::{Parser, Subcommand};
use color_eyre::Result;
use nbd::{
    proto::DEFAULT_PORT,
    server::{Device, MemBlocks, Server},
};

#[derive(Parser, Debug)]
#[clap(version, about, long_about = None)]
struct Args {
    /// The port the server should listen to
    #[arg(short, long, default_value_t = DEFAULT_PORT)]
    port: u16,

    #[command(subcommand)]
    subcommand: Subcommands,
}

const DEFAULT_SIZE: u64 = 10 * 1024 * 1024;

#[derive(Subcommand, Debug)]
enum Subcommands {
    /// Spawn a server backed by memory
    Memory {
        /// Size of backing storage
        #[arg(short, long, default_value_t = DEFAULT_SIZE)]
        size: u64,
    },
    /// Spawn a server backed by a file
    File {
        /// Size of backing storage
        #[arg(short, long, default_value_t = DEFAULT_SIZE)]
        size: u64,

        /// Don't create/truncate existing file
        #[arg(long)]
        no_create: bool,

        /// Path to the backing file
        path: String,
    },
    /// Spawn a server backed by a block device
    Device {
        /// Path to the backing block device
        path: String,
    },
}

fn main() -> Result<()> {
    color_eyre::install()?;
    env_logger::init();

    let Args { port, subcommand } = Args::parse();

    match subcommand {
        Subcommands::Memory { size } => {
            let data = vec![0; size as usize];
            let export = MemBlocks::new(data);
            Server::new(export).start(port)?;
        }
        Subcommands::File {
            size,
            no_create,
            path,
        } => {
            let file = File::options()
                .read(true)
                .write(true)
                .create(!no_create)
                .truncate(!no_create)
                .open(&path)?;

            file.set_len(size)?;

            Server::new(file).start(port)?;
        }
        Subcommands::Device { path } => {
            Server::new(Device::new(
                File::options().read(true).write(true).open(&path)?,
            ))
            .start(port)?;
        }
    }

    Ok(())
}

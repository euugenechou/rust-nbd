use std::fs::OpenOptions;

use clap::Parser;
use color_eyre::Result;
use nbd::{
    proto::DEFAULT_PORT,
    server::{MemBlocks, Server},
};

#[derive(Parser, Debug)]
#[clap(version, about, long_about = None)]
struct Args {
    #[clap(short, long, default_value_t = DEFAULT_PORT)]
    port: u16,

    #[clap(long)]
    no_create: bool,

    #[clap(short, long, default_value_t = 10)]
    size: usize,

    #[clap(short, long)]
    mem: bool,

    #[clap(default_value = "disk.img")]
    filename: String,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    env_logger::init();

    let args = Args::parse();
    let create = !args.no_create;
    let size_bytes = args.size as u64 * 1024 * 1024;

    if args.mem {
        let data = vec![0u8; size_bytes as usize];
        let export = MemBlocks::new(data);
        Server::new(export).start(args.port)?;
        return Ok(());
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .open(args.filename)?;

    file.set_len(size_bytes)?;

    Server::new(file).start(args.port)?;
    Ok(())
}

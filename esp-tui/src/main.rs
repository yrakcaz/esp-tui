mod app;
mod elf;
mod event;
mod filter;
mod flash;
mod input;
mod log;
mod port;
mod runner;
mod serial;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    runner::run().await
}

//! Offline saved-output review; no project-root, application or inference initialization.
#[path = "../evaluation.rs"]
mod evaluation;

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "Attach explicit human/assistant review labels to saved synthetic writing outputs; never generates or grades text"
)]
struct Arguments {
    #[arg(long)]
    review_input: PathBuf,
    #[arg(long)]
    reviews: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

fn main() {
    let arguments = Arguments::parse();
    if let Err(error) = evaluation::run(
        &arguments.review_input,
        &arguments.reviews,
        &arguments.output,
    ) {
        eprintln!("BroTypist review: {error:#}");
        std::process::exit(1);
    }
}

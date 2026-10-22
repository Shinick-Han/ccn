//! ccn-nvim-probe: read a live Neovim buffer over RPC, measure its CCN, and
//! optionally annotate functions with extmarks.
//!
//! This is a spike for the workbench's "edit -> unsaved CCN delta" path. It
//! never writes to disk: the source string comes from `nvim_buf_get_lines`.

mod annotate;
mod client;
mod report;

use std::path::Path;
use std::time::Instant;

use clap::Parser;

/// Error type for the whole probe. Message-only: the caller prints it and
/// exits nonzero, and the probe must never panic on bad buffers.
pub type ProbeResult<T> = Result<T, Box<dyn std::error::Error>>;

pub const TEST_INSERT_TEXT: &str = "if (x) x++;";

#[derive(Debug, Parser)]
#[command(
    name = "ccn-nvim-probe",
    version,
    about = "Measure ccn on a live Neovim buffer over msgpack-RPC"
)]
struct Cli {
    /// Neovim listen address, e.g. 127.0.0.1:7777.
    #[arg(long)]
    server: String,

    /// Write extmarks into the `ccn_rpc` namespace.
    #[arg(long)]
    annotate: bool,

    /// Emit one JSON document instead of the human table.
    #[arg(long)]
    json: bool,

    /// Insert `if (x) x++;` after this 1-based line, without saving.
    #[arg(long, value_name = "LINE")]
    test_insert: Option<usize>,

    /// Print the marks currently in the `ccn_rpc` namespace.
    #[arg(long)]
    dump_marks: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli).await {
        eprintln!("ccn-nvim-probe: {err}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> ProbeResult<()> {
    let started = Instant::now();

    let (nvim, io) = client::connect(&cli.server).await?;
    let buffer = client::current_buffer(&nvim).await?;

    let name = buffer
        .get_name()
        .await
        .map_err(|err| format!("cannot read buffer name: {err}"))?;
    if name.is_empty() {
        return Err("current buffer has no name; save it or set one with :file".into());
    }
    if !buffer
        .is_loaded()
        .await
        .map_err(|err| format!("cannot query buffer load state: {err}"))?
    {
        return Err(format!("buffer '{name}' is not loaded").into());
    }

    if let Some(line) = cli.test_insert {
        client::insert_after(&buffer, line).await?;
        eprintln!("inserted {TEST_INSERT_TEXT:?} after line {line} (not saved)");
    }

    let source = client::read_source(&buffer).await?;
    let lang = client::detect_lang(&name)
        .ok_or_else(|| format!("cannot determine a supported language for buffer '{name}'"))?;
    let summary = ccn::ccn::analyze_file(Path::new(&name), source.as_bytes(), lang.name());
    let report = report::ProbeReport::new(&name, &summary);

    if cli.annotate {
        let placed = annotate::annotate(&nvim, &buffer, &summary).await?;
        eprintln!(
            "annotated {placed} function(s) in namespace {}",
            annotate::NAMESPACE
        );
    }

    if cli.json {
        println!("{}", report.to_json()?);
        if cli.dump_marks {
            let marks = annotate::marks(&nvim, &buffer).await?;
            eprintln!("{}", report::MarkRow::to_table(&marks));
        }
    } else {
        print!("{}", report.to_table());
        if cli.dump_marks {
            let marks = annotate::marks(&nvim, &buffer).await?;
            print!("{}", report::MarkRow::to_table(&marks));
        }
    }

    eprintln!("elapsed_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);

    // The RPC io task only ends with the connection; drop it explicitly so the
    // process exits instead of detaching a live reader.
    io.abort();
    Ok(())
}

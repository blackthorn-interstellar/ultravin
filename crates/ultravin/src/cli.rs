//! The `ultravin` command: argument parsing and every subcommand, over
//! whatever streams the caller hands in. The Python wheel's console script
//! calls [`main`]; nothing here knows about Python.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::parquet_io::{
    check_dst_outside_src, decode_compact_at, open_chunks_at, open_chunks_auto_at, ParquetError,
    ParquetOpts, ParquetSink,
};
use crate::{ColumnNames, ColumnSpec, Db, WriteJsonlError};

/// Rows of the first two streamed JSONL chunks; each later chunk doubles, up
/// to `JSONL_MAX_ROWS`. Output starts at once, and a long input soon decodes
/// in runs long enough for the result cache to pay.
const JSONL_FIRST_ROWS: usize = 256;
const JSONL_MAX_ROWS: usize = 65_536;

const FULL_HELP: &str = "Emit the per-element provenance list instead of the attributes mapping";
const COLUMNS_HELP: &str =
    "Comma-separated elements to project: variable names or element ids [default: all]";
const COLUMN_NAMES_HELP: &str = "Label projected columns by vPIC variable name, or as \
                                 attr_<element_id> (stable across data refreshes)";

#[derive(Parser)]
#[command(name = "ultravin", about = "ultravin — NHTSA vPIC VIN decoder")]
#[command(arg_required_else_help = true, disable_help_subcommand = true)]
// A repeated option takes its last value, as it did under click.
#[command(args_override_self = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Decode a single VIN (JSON object on stdout).
    Decode {
        vin: OsString,
        /// Caller-supplied model year (vPIC's modelyear).
        #[arg(long, allow_negative_numbers = true)]
        year: Option<i32>,
        #[arg(long, help = FULL_HELP)]
        full: bool,
    },
    /// Decode one VIN per line from FILE, or from stdin when FILE is `-`.
    ///
    /// A line may be `VIN,year` to supply a caller model year for that VIN —
    /// the same per-line format the vPIC batch API accepts. The default output
    /// is one JSON array; `--jsonl` bounds memory and emits each result as it
    /// decodes. One clock is captured before reading input and used for the
    /// whole job.
    DecodeBatch {
        file: PathBuf,
        #[arg(long, help = FULL_HELP)]
        full: bool,
        /// Stream one JSON object per line.
        #[arg(long)]
        jsonl: bool,
        /// VINs per JSONL chunk: 'auto' or a positive integer.
        #[arg(long, default_value = "auto", value_parser = batch_size, allow_hyphen_values = true)]
        batch_size: BatchSize,
        /// Working-buffer target in MiB for automatic batches.
        #[arg(long, default_value_t = 8, value_parser = at_least_one)]
        batch_memory_mb: usize,
    },
    /// Decode SRC (a parquet file or directory of them) into projected parquet at DST.
    ///
    /// Output columns: the passthrough VIN (and caller year),
    /// `decoded_model_year`, then one typed column per projected element.
    DecodeParquet {
        src: PathBuf,
        dst: PathBuf,
        #[command(flatten)]
        dataset: Dataset,
        /// Rows per chunk: 'auto' or a positive integer.
        #[arg(long, default_value = "auto", value_parser = batch_size, allow_hyphen_values = true)]
        batch_size: BatchSize,
        /// Working-buffer target in MiB for automatic batches.
        #[arg(long, default_value_t = 64, value_parser = at_least_one)]
        batch_memory_mb: usize,
    },
    /// Decode SRC (parquet, or text with one VIN per line) into vins.parquet
    /// and vehicles.parquet in DST_DIR.
    ///
    /// vins.parquet holds each input VIN and its vehicle_id, in input order;
    /// vehicles.parquet holds every decode-parquet column but the VIN, once
    /// per vehicle_id. Join them on vehicle_id for the full table.
    DecodeCompact {
        src: PathBuf,
        dst_dir: PathBuf,
        #[command(flatten)]
        dataset: Dataset,
    },
    /// Print the ultravin version.
    Version,
    /// Print decoder and embedded-data provenance as JSON.
    Info,
}

/// The options the two dataset commands share.
#[derive(Args)]
struct Dataset {
    /// VIN column name [default: autodetect].
    #[arg(long)]
    vin_column: Option<String>,
    /// Caller model-year column name [default: autodetect].
    #[arg(long)]
    year_column: Option<String>,
    #[arg(long, help = COLUMNS_HELP, allow_hyphen_values = true)]
    columns: Option<String>,
    #[arg(long, value_enum, default_value = "variable", help = COLUMN_NAMES_HELP)]
    column_names: Names,
    /// Rows sniffed when autodetecting columns.
    #[arg(long, default_value_t = 100, value_parser = at_least_one)]
    sample_rows: usize,
}

impl Dataset {
    fn opts(self, batch_size: usize) -> ParquetOpts {
        ParquetOpts {
            vin: self.vin_column,
            year: self.year_column,
            columns: projection(self.columns.as_deref()),
            names: match self.column_names {
                Names::Variable => ColumnNames::Variable,
                Names::Id => ColumnNames::Id,
            },
            batch_size,
            sample_rows: self.sample_rows,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Names {
    Variable,
    Id,
}

#[derive(Clone, Copy)]
enum BatchSize {
    Auto,
    Rows(usize),
}

fn batch_size(value: &str) -> Result<BatchSize, &'static str> {
    match value.parse() {
        _ if value == "auto" => Ok(BatchSize::Auto),
        Ok(rows) if rows > 0 => Ok(BatchSize::Rows(rows)),
        _ => Err("must be 'auto' or a positive integer"),
    }
}

fn at_least_one(value: &str) -> Result<usize, &'static str> {
    match value.parse() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err("must be a positive integer"),
    }
}

/// `--columns`: a token of decimal digits (after an optional `-`) is an
/// element id, anything else a variable name. Both reach the same resolver.
fn projection(columns: Option<&str>) -> Vec<ColumnSpec> {
    let Some(columns) = columns.filter(|columns| !columns.is_empty()) else {
        return crate::all_public_ids(Db::embedded())
            .into_iter()
            .map(ColumnSpec::Id)
            .collect();
    };
    let tokens = columns.split(',').map(strip).filter(|tok| !tok.is_empty());
    tokens
        .map(|tok| {
            let digits = tok.trim_start_matches('-');
            match tok.parse() {
                Ok(id) if digits.bytes().all(|b| b.is_ascii_digit()) => ColumnSpec::Id(id),
                _ => ColumnSpec::Name(tok.to_owned()),
            }
        })
        .collect()
}

/// Why a command stopped: the caller's mistake (exit 2), or stdout going away.
enum Failure {
    Input(String),
    Output(io::Error),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Failure::Output(error)
    }
}

/// Every error out of the dataset door is a caller mistake (unknown column,
/// bad element id, ambiguous autodetect) or a path that cannot be read or
/// written.
impl From<ParquetError> for Failure {
    fn from(error: ParquetError) -> Self {
        Failure::Input(error.to_string())
    }
}

/// Run the command line `args` (the program name first) on the process's own
/// streams and return its exit code.
pub fn main<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let (stdin, stdout, stderr) = (io::stdin(), io::stdout(), io::stderr());
    run(
        args,
        &mut stdin.lock(),
        &mut stdout.lock(),
        &mut stderr.lock(),
    )
}

/// [`main`] over the given streams. Exit codes: 0 done, 1 the output could not
/// be written, 2 the arguments or the input were wrong.
pub fn run<I, T>(args: I, stdin: &mut dyn BufRead, out: &mut dyn Write, err: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            // `--help` is an "error" clap prints to stdout and exits 0 for.
            let _ = if error.use_stderr() {
                write!(err, "{}", error.render())
            } else {
                write!(out, "{}", error.render())
            };
            return error.exit_code();
        }
    };
    let done = execute(cli.command, crate::now_micros(), stdin, out, err);
    match done.and_then(|()| Ok(out.flush()?)) {
        Ok(()) => 0,
        Err(Failure::Input(message)) => {
            let _ = writeln!(err, "error: {message}");
            2
        }
        // A closed pipe (`| head`) is the reader's choice, not a failure to report.
        Err(Failure::Output(error)) if error.kind() == io::ErrorKind::BrokenPipe => 1,
        Err(Failure::Output(error)) => {
            let _ = writeln!(err, "error: writing output: {error}");
            1
        }
    }
}

/// `now_micros` is read once, before any input is opened, and is the clock of
/// every decode in the job.
fn execute(
    command: Command,
    now_micros: i64,
    stdin: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    match command {
        Command::Decode { vin, year, full } => {
            let vin = vin
                .to_str()
                .ok_or_else(|| input("VIN is not valid UTF-8"))?;
            let json = if full {
                crate::decode_json_at(vin, year, now_micros)
            } else {
                crate::decode_json_flat_at(vin, year, now_micros)
            };
            writeln!(out, "{json}")?;
        }
        Command::DecodeBatch {
            file,
            full,
            jsonl,
            batch_size,
            batch_memory_mb,
        } => {
            let job = Batch {
                full,
                now_micros,
                out,
            };
            let run = |lines: &mut dyn BufRead| {
                let rows = Rows::new(lines, &file);
                if jsonl {
                    job.jsonl(rows, batch_size, batch_memory_mb)
                } else {
                    job.array(rows)
                }
            };
            if file == Path::new("-") {
                run(stdin)?;
            } else {
                let opened = File::open(&file).map_err(|e| unreadable(&file, &e))?;
                run(&mut BufReader::with_capacity(1 << 16, opened))?;
            }
        }
        Command::DecodeParquet {
            src,
            dst,
            dataset,
            batch_size,
            batch_memory_mb,
        } => {
            let memory_bytes = memory_bytes(batch_memory_mb)?;
            let chunks = match batch_size {
                BatchSize::Rows(rows) => open_chunks_at(&src, dataset.opts(rows), now_micros),
                BatchSize::Auto => {
                    open_chunks_auto_at(&src, dataset.opts(8_192), memory_bytes, now_micros)
                }
            }?;
            let schema = chunks.out_schema.clone();
            let schema = schema.ok_or_else(|| input("source expanded to no parquet files"))?;
            // Refused, and the sink opened, before a row is read: a rejected
            // destination leaves the source intact.
            check_dst_outside_src(&src, &dst)?;
            let sink = match (chunks.feedback(), batch_size) {
                (Some(feedback), _) => ParquetSink::create_adaptive(schema, &dst, feedback),
                (None, BatchSize::Rows(rows)) => ParquetSink::create(schema, &dst, rows),
                (None, BatchSize::Auto) => ParquetSink::create(schema, &dst, 65_536),
            }?;
            let rows = sink.write_all(chunks)?;
            writeln!(err, "wrote {rows} rows to {}", dst.display())?;
        }
        Command::DecodeCompact {
            src,
            dst_dir,
            dataset,
        } => {
            let opts = dataset.opts(8_192);
            let rows = decode_compact_at(&src, &dst_dir, opts, now_micros)?;
            let (vins, vehicles) = (rows.vins, rows.vehicles);
            let dst_dir = dst_dir.display();
            writeln!(
                err,
                "wrote {vins} VINs and {vehicles} vehicles to {dst_dir}"
            )?;
        }
        Command::Version => writeln!(out, "{}", env!("CARGO_PKG_VERSION"))?,
        Command::Info => {
            let p = crate::provenance();
            let fields = [
                ("data_month", p.data_month),
                ("artifact_blake3", p.artifact_blake3),
                ("decoder_version", p.decoder_version),
            ];
            let fields: Vec<String> = fields
                .iter()
                .map(|(key, value)| format!("{key:?}:{}", serde_json::Value::from(*value)))
                .collect();
            writeln!(out, "{{{}}}", fields.join(","))?;
        }
    }
    Ok(())
}

fn input(message: &str) -> Failure {
    Failure::Input(message.to_owned())
}

fn unreadable(file: &Path, error: &io::Error) -> Failure {
    Failure::Input(format!("{}: {error}", file.display()))
}

fn memory_bytes(megabytes: usize) -> Result<usize, Failure> {
    megabytes
        .checked_mul(1 << 20)
        .ok_or_else(|| input("batch_memory_mb must be a positive, representable size"))
}

/// What Python's `str.strip()` strips: Unicode whitespace plus the four ASCII
/// separators (FS, GS, RS, US) that Rust's `trim` leaves alone.
fn strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// Some rows of a `decode-batch` input: the VINs, and their years if any row gives one.
type Chunk = (Vec<String>, Option<Vec<Option<i32>>>);

/// `(vin, year)` per non-blank line of a `decode-batch` input. A line ends at
/// `\n`, `\r\n` or a lone `\r`.
struct Rows<'a> {
    lines: &'a mut dyn BufRead,
    file: &'a Path,
    /// One `\n`-terminated read, and how far into it the rows have got.
    buffer: Vec<u8>,
    at: usize,
    /// Lines consumed so far, for error messages.
    line: usize,
}

impl<'a> Rows<'a> {
    fn new(lines: &'a mut dyn BufRead, file: &'a Path) -> Self {
        Rows {
            lines,
            file,
            buffer: Vec::new(),
            at: 0,
            line: 0,
        }
    }

    fn next(&mut self) -> Result<Option<(String, Option<i32>)>, Failure> {
        loop {
            if self.at >= self.buffer.len() {
                self.buffer.clear();
                self.at = 0;
                let read = self.lines.read_until(b'\n', &mut self.buffer);
                if read.map_err(|e| unreadable(self.file, &e))? == 0 {
                    return Ok(None);
                }
                // The terminator is not part of the line; a `\r` left before
                // it would otherwise end a second, empty one.
                if self.buffer.ends_with(b"\n") {
                    self.buffer.pop();
                }
                if !self.buffer.ends_with(b"\r") {
                    self.buffer.push(b'\r');
                }
            }
            let rest = &self.buffer[self.at..];
            let end = memchr::memchr(b'\r', rest).expect("every read ends in a carriage return");
            self.at += end + 1;
            self.line += 1;
            let line = std::str::from_utf8(&rest[..end]);
            let line = strip(line.map_err(|_| input("input is not valid UTF-8"))?);
            if line.is_empty() {
                continue;
            }
            let (vin, year) = line.split_once(',').unwrap_or((line, ""));
            let year = match strip(year) {
                "" => None,
                year => Some(year.parse().map_err(|_| {
                    let line = self.line;
                    Failure::Input(format!(
                        "line {line}: model year '{year}' is not an integer"
                    ))
                })?),
            };
            return Ok(Some((strip(vin).to_owned(), year)));
        }
    }

    /// Up to `limit` rows: the VINs, and their years if any row gives one.
    /// A bad line fails the whole chunk, so none of it is decoded.
    fn chunk(&mut self, limit: usize) -> Result<Chunk, Failure> {
        let (mut vins, mut years) = (Vec::new(), Vec::new());
        while vins.len() < limit {
            let Some((vin, year)) = self.next()? else {
                break;
            };
            vins.push(vin);
            years.push(year);
        }
        let hints = years.iter().any(Option::is_some).then_some(years);
        Ok((vins, hints))
    }
}

/// One `decode-batch` job.
struct Batch<'a> {
    full: bool,
    now_micros: i64,
    out: &'a mut dyn Write,
}

impl Batch<'_> {
    /// The whole input as one JSON array.
    fn array(self, mut rows: Rows<'_>) -> Result<(), Failure> {
        let (vins, years) = rows.chunk(usize::MAX)?;
        let (years, now) = (years.as_deref(), self.now_micros);
        let json = match (vins.as_slice(), self.full) {
            // One VIN is not worth the parallel batch's setup.
            ([vin], full) => {
                let year = years.and_then(|years| years[0]);
                let json = if full {
                    crate::decode_json_at(vin, year, now)
                } else {
                    crate::decode_json_flat_at(vin, year, now)
                };
                format!("[{json}]")
            }
            (_, true) => crate::decode_batch_json_at(&vins, years, now),
            (_, false) => crate::decode_batch_json_flat_at(&vins, years, now),
        };
        Ok(writeln!(self.out, "{json}")?)
    }

    /// One JSON object per line, written chunk by chunk as the input arrives.
    /// A fixed `batch_size` decodes that many rows at a time; `auto` grows the
    /// chunks and decodes each on the native stream within the memory budget.
    fn jsonl(
        self,
        mut rows: Rows<'_>,
        batch_size: BatchSize,
        megabytes: usize,
    ) -> Result<(), Failure> {
        let memory_bytes = memory_bytes(megabytes)?;
        let (full, now, out) = (self.full, self.now_micros, self.out);
        let mut size = JSONL_FIRST_ROWS;
        for chunk in 0.. {
            let limit = match batch_size {
                BatchSize::Rows(rows) => rows,
                BatchSize::Auto => size,
            };
            let (vins, years) = rows.chunk(limit)?;
            if vins.is_empty() {
                break;
            }
            let years = years.as_deref();
            match batch_size {
                BatchSize::Rows(_) => {
                    let lines = crate::decode_batch_jsonl_at(&vins, years, now, full);
                    out.write_all(lines.as_bytes())?;
                }
                BatchSize::Auto => {
                    let write = |lines: &[u8]| out.write_all(lines);
                    Db::embedded()
                        .write_batch_jsonl_at(&vins, years, now, full, memory_bytes, write)
                        .map_err(|error| match error {
                            WriteJsonlError::Write(error) => Failure::Output(error),
                            WriteJsonlError::Stream(error) => Failure::Input(error.to_string()),
                        })?;
                }
            }
            out.flush()?;
            if chunk >= 1 {
                size = (2 * size).min(JSONL_MAX_ROWS);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;

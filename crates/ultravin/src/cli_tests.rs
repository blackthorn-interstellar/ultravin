use super::*;

const HONDA: &str = "1HGCM82633A004352";
/// 2026-01-01T00:00:00Z.
const NOW: i64 = 1_767_225_600_000_000;

struct Ran {
    code: i32,
    out: String,
    err: String,
}

/// The whole command, clock included, as the console script runs it.
fn cli(args: &[&str], stdin: &[u8]) -> Ran {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let args = ["ultravin"].iter().chain(args);
    let code = run(args, &mut &*stdin, &mut out, &mut err);
    let text = |bytes| String::from_utf8(bytes).expect("the CLI writes UTF-8");
    Ran {
        code,
        out: text(out),
        err: text(err),
    }
}

/// A parsed command at a fixed clock, writing to `out`.
fn at(now: i64, args: &[&str], stdin: &[u8], out: &mut dyn Write) -> Result<(), Failure> {
    let cli = Cli::try_parse_from(["ultravin"].iter().chain(args)).expect("valid arguments");
    execute(cli.command, now, &mut &*stdin, out, &mut Vec::new())
}

fn stdout(now: i64, args: &[&str], stdin: &str) -> String {
    let mut out = Vec::new();
    assert!(at(now, args, stdin.as_bytes(), &mut out).is_ok());
    String::from_utf8(out).unwrap()
}

fn jsonl(vins: &[&str], years: Option<&[Option<i32>]>, now: i64) -> String {
    let vins: Vec<String> = vins.iter().map(|vin| (*vin).to_owned()).collect();
    crate::decode_batch_jsonl_at(&vins, years, now, false)
}

/// Records what each write carried, and can fail every write.
#[derive(Default)]
struct Writes {
    lines: Vec<usize>,
    fail: Option<io::ErrorKind>,
}

impl Write for Writes {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.lines
            .push(data.iter().filter(|b| **b == b'\n').count());
        match self.fail {
            Some(kind) => Err(kind.into()),
            None => Ok(data.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn decode_prints_the_library_json() {
    let flat = crate::decode_json_flat_at(HONDA, None, NOW);
    assert_eq!(stdout(NOW, &["decode", HONDA], ""), format!("{flat}\n"));
    let full = crate::decode_json_at(HONDA, Some(1995), NOW);
    let args = ["decode", HONDA, "--full", "--year", "1995"];
    assert_eq!(stdout(NOW, &args, ""), format!("{full}\n"));
    // A negative year is a value, not a flag, and a repeated option's last value wins.
    let negative = crate::decode_json_flat_at(HONDA, Some(-5), NOW);
    let args = ["decode", HONDA, "--year", "1995", "--year", "-5"];
    assert_eq!(stdout(NOW, &args, ""), format!("{negative}\n"));
    let args = ["decode", HONDA, "--full", "--year", "1995", "--full"];
    assert_eq!(stdout(NOW, &args, ""), format!("{full}\n"));
}

#[cfg(unix)]
#[test]
fn a_vin_that_is_not_utf8_is_a_usage_error() {
    use std::os::unix::ffi::OsStringExt;
    let args = ["ultravin", "decode"].map(OsString::from);
    let args = args.into_iter().chain([OsString::from_vec(vec![0xff])]);
    let mut err = Vec::new();
    assert_eq!(run(args, &mut &b""[..], &mut Vec::new(), &mut err), 2);
    assert_eq!(err, b"error: VIN is not valid UTF-8\n");
}

#[test]
fn lines_are_stripped_split_and_skipped_as_python_did() {
    // Blank lines, the ASCII separators Python strips, `VIN,year` with spaces,
    // an empty year, and all three line endings.
    let input = format!(
        "\n  {HONDA}  \n\u{a0}\x1c\n{HONDA}, 1995 \n{HONDA},\n\x1f{HONDA}\x1e\r\n{HONDA},+1996\r{HONDA}"
    );
    let years = [None, Some(1995), None, None, Some(1996), None];
    let expected = jsonl(&[HONDA; 6], Some(&years), NOW);
    assert_eq!(
        stdout(NOW, &["decode-batch", "-", "--jsonl"], &input),
        expected
    );
    let array = stdout(NOW, &["decode-batch", "-"], &input);
    let vins = vec![HONDA.to_owned(); 6];
    let expected = crate::decode_batch_json_flat_at(&vins, Some(&years), NOW);
    assert_eq!(array, format!("{expected}\n"));
}

#[test]
fn an_array_of_nothing_and_of_one() {
    assert_eq!(stdout(NOW, &["decode-batch", "-"], "\n \n"), "[]\n");
    assert_eq!(stdout(NOW, &["decode-batch", "-", "--jsonl"], "\n \n"), "");
    let one = crate::decode_json_at(HONDA, Some(1995), NOW);
    let input = format!("{HONDA},1995\n");
    assert_eq!(
        stdout(NOW, &["decode-batch", "-", "--full"], &input),
        format!("[{one}]\n")
    );
}

#[test]
fn a_bad_year_names_its_line_and_exits_2() {
    for year in ["soon", "1_995", "99999999999", "1995,x"] {
        let input = format!("{HONDA}\n\n{HONDA},{year}\n");
        for args in [
            &["decode-batch", "-"][..],
            &["decode-batch", "-", "--jsonl"],
        ] {
            let ran = cli(args, input.as_bytes());
            let message = format!("error: line 3: model year '{year}' is not an integer\n");
            assert_eq!((ran.code, ran.out.as_str(), ran.err), (2, "", message));
        }
    }
}

#[test]
fn a_fixed_batch_size_writes_that_many_rows_at_a_time() {
    let input = format!("{HONDA}\n").repeat(5);
    let mut writes = Writes::default();
    let args = ["decode-batch", "-", "--jsonl", "--batch-size", "2"];
    assert!(at(NOW, &args, input.as_bytes(), &mut writes).is_ok());
    assert_eq!(writes.lines, [2, 2, 1]);
}

#[test]
fn automatic_chunks_grow_and_the_one_holding_a_bad_line_is_not_written() {
    // Chunks of 256, 256 and 512 rows: line 601 is in the third.
    let input = format!("{HONDA}\n").repeat(600) + &format!("{HONDA},soon\n{HONDA}\n");
    let ran = cli(&["decode-batch", "-", "--jsonl"], input.as_bytes());
    assert_eq!(ran.code, 2);
    assert_eq!(
        ran.err,
        "error: line 601: model year 'soon' is not an integer\n"
    );
    assert_eq!(ran.out.lines().count(), 512);
    // In the first chunk, nothing is.
    let input = format!("{HONDA}\n").repeat(99) + &format!("{HONDA},soon\n");
    let ran = cli(&["decode-batch", "-", "--jsonl"], input.as_bytes());
    assert_eq!((ran.code, ran.out.as_str()), (2, ""));
}

#[test]
fn automatic_chunks_write_the_fixed_batch_bytes() {
    let input = format!("{HONDA}\n{HONDA},1995\nNOTAVIN\n").repeat(300);
    let auto = stdout(NOW, &["decode-batch", "-", "--jsonl"], &input);
    let args = ["decode-batch", "-", "--jsonl", "--batch-size", "7"];
    assert_eq!(auto, stdout(NOW, &args, &input));
    assert_eq!(auto.lines().count(), 900);
}

#[test]
fn input_that_is_not_utf8_is_reported_after_an_earlier_bad_year() {
    let mut input = format!("{HONDA}\n{HONDA},abc\n").into_bytes();
    input.extend_from_slice(b"CAF\xe9\n");
    let ran = cli(&["decode-batch", "-"], &input);
    assert_eq!(
        ran.err,
        "error: line 2: model year 'abc' is not an integer\n"
    );
    let input = [HONDA.as_bytes(), b"\nCAF\xe9\n"].concat();
    for args in [
        &["decode-batch", "-"][..],
        &["decode-batch", "-", "--jsonl"],
    ] {
        let ran = cli(args, &input);
        let message = "error: input is not valid UTF-8\n".to_owned();
        assert_eq!((ran.code, ran.out.as_str(), ran.err), (2, "", message));
    }
}

#[test]
fn one_clock_reading_serves_every_chunk() {
    let past = 0;
    let input = format!("{HONDA}\n").repeat(3);
    for size in ["auto", "1"] {
        let args = ["decode-batch", "-", "--jsonl", "--batch-size", size];
        let decoded = stdout(past, &args, &input);
        assert_eq!(decoded, jsonl(&[HONDA; 3], None, past));
        assert_ne!(decoded, jsonl(&[HONDA; 3], None, NOW));
    }
}

#[test]
fn a_closed_pipe_stops_the_decode_quietly() {
    let input = format!("{HONDA}\n").repeat(10_000);
    for size in ["auto", "100"] {
        let mut writes = Writes {
            fail: Some(io::ErrorKind::BrokenPipe),
            ..Writes::default()
        };
        let args = ["decode-batch", "-", "--jsonl", "--batch-size", size];
        let failed = at(NOW, &args, input.as_bytes(), &mut writes);
        assert!(matches!(failed, Err(Failure::Output(_))));
        assert_eq!(writes.lines.len(), 1);
    }
}

#[test]
fn usage_errors_exit_2_and_help_exits_0() {
    let ran = cli(&[], b"");
    assert_eq!((ran.code, ran.out.as_str()), (2, ""));
    assert!(ran.err.contains("Usage: ultravin <COMMAND>"));
    let ran = cli(&["--help"], b"");
    assert_eq!((ran.code, ran.err.as_str()), (0, ""));
    assert!(ran.out.contains("decode-compact"));
    for command in [
        "decode",
        "decode-batch",
        "decode-parquet",
        "decode-compact",
        "bogus",
    ] {
        assert_eq!(cli(&[command], b"").code, 2, "{command}");
    }
    for value in ["0", "-1", "quickly"] {
        let ran = cli(
            &["decode-batch", "-", "--jsonl", "--batch-size", value],
            b"",
        );
        assert_eq!(ran.code, 2);
        assert!(
            ran.err.contains("must be 'auto' or a positive integer"),
            "{}",
            ran.err
        );
    }
    for flag in ["--batch-memory-mb", "--sample-rows"] {
        let ran = cli(&["decode-parquet", "in", "out", flag, "0"], b"");
        assert_eq!(ran.code, 2);
        assert!(
            ran.err.contains("must be a positive integer"),
            "{}",
            ran.err
        );
    }
    let ran = cli(
        &[
            "decode-batch",
            "-",
            "--jsonl",
            "--batch-memory-mb",
            "99999999999999999",
        ],
        b"",
    );
    assert_eq!(ran.code, 2);
}

#[test]
fn an_unreadable_path_is_named_and_exits_2() {
    let missing = std::env::temp_dir().join("ultravin-cli-test-no-such-file");
    let missing = missing.to_str().unwrap();
    let commands = [
        vec!["decode-batch", missing],
        vec!["decode-parquet", missing, "out.parquet"],
        vec!["decode-compact", missing, "out"],
    ];
    for args in commands {
        let ran = cli(&args, b"");
        assert_eq!((ran.code, ran.out.as_str()), (2, ""), "{args:?}");
        assert!(
            ran.err.starts_with(&format!("error: {missing}: ")),
            "{}",
            ran.err
        );
    }
}

#[test]
fn columns_tokens_are_ids_or_names() {
    let specs = projection(Some(" 26 , Make,,-5,+26,²,99999999999"));
    let name = |name: &str| ColumnSpec::Name(name.to_owned());
    let expected = [
        ColumnSpec::Id(26),
        name("Make"),
        ColumnSpec::Id(-5),
        name("+26"),
        name("²"),
        name("99999999999"),
    ];
    assert_eq!(specs, expected);
    let all = crate::all_public_ids(Db::embedded()).len();
    assert_eq!(projection(None).len(), all);
    assert_eq!(projection(Some("")).len(), all);
}

#[test]
fn version_and_info_print_the_release_identity() {
    assert_eq!(
        stdout(NOW, &["version"], ""),
        concat!(env!("CARGO_PKG_VERSION"), "\n")
    );
    let info = stdout(NOW, &["info"], "");
    let p = crate::provenance();
    let expected = format!(
        "{{\"data_month\":\"{}\",\"artifact_blake3\":\"{}\",\"decoder_version\":\"{}\"}}\n",
        p.data_month, p.artifact_blake3, p.decoder_version
    );
    assert_eq!(info, expected);
}

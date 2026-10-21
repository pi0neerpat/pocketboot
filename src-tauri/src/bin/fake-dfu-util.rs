//! Test double for dfu-util (Test plan 6). Replays the script named by
//! `FAKE_DFU_SCRIPT`, one directive per line:
//!
//! - `out|text` / `err|text`: write `text` and a newline to stdout / stderr
//! - `put|text`: write `text` to stdout with no newline
//! - `sleep|ms`: go silent for `ms` milliseconds
//! - `exit|code`: exit with `code`
//! - `env|` / `argv|`: print each env var as `env K` (`env K=V` for `PATH`
//!   and `LANG` only, so a failing test never logs a secret), each arg as
//!   `arg A`
//!
//! `\r`, `\n`, and `\t` in `text` are unescaped.

use std::io::Write;
use std::time::Duration;

fn main() {
    let path = std::env::var("FAKE_DFU_SCRIPT").expect("FAKE_DFU_SCRIPT");
    let script = std::fs::read_to_string(path).expect("read script");
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    for line in script.lines() {
        let (cmd, arg) = line.split_once('|').unwrap_or((line, ""));
        let text = arg
            .replace("\\r", "\r")
            .replace("\\n", "\n")
            .replace("\\t", "\t");
        match cmd {
            "out" => writeln!(out, "{text}").unwrap(),
            "err" => writeln!(err, "{text}").unwrap(),
            "put" => write!(out, "{text}").unwrap(),
            "sleep" => std::thread::sleep(Duration::from_millis(arg.parse().unwrap())),
            "exit" => {
                out.flush().unwrap();
                std::process::exit(arg.parse().unwrap());
            }
            "env" => {
                let mut vars: Vec<_> = std::env::vars().collect();
                vars.sort();
                for (k, v) in vars {
                    match k.as_str() {
                        "PATH" | "LANG" => writeln!(out, "env {k}={v}").unwrap(),
                        _ => writeln!(out, "env {k}").unwrap(),
                    }
                }
            }
            "argv" => {
                for a in std::env::args().skip(1) {
                    writeln!(out, "arg {a}").unwrap();
                }
            }
            "" => {}
            other => panic!("unknown directive {other:?}"),
        }
        out.flush().unwrap();
        err.flush().unwrap();
    }
}

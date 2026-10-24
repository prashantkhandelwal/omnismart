use omnismart::{ReaderContext, dump, emv, select_reader};
use std::{error::Error, io, path::Path, process::ExitCode};

fn main() -> ExitCode {
    match read_card() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug)]
struct Arguments<'a> {
    aid: Option<Vec<u8>>,
    dump_path: Option<&'a Path>,
    show_pan: bool,
}

fn parse_arguments(arguments: &[String]) -> io::Result<Arguments<'_>> {
    let (aid, dump_path, show_pan) = match arguments {
        [] => (None, None, false),
        [mode] if mode == "--emv" || mode == "--info" => (None, None, false),
        [mode, flag] if mode == "--info" && flag == "--show-pan" => (None, None, true),
        [mode, option, value] if (mode == "--emv" || mode == "--info") && option == "--aid" => {
            (Some(emv::parse_aid(value)?), None, false)
        }
        [mode, flag, option, value]
            if mode == "--info" && flag == "--show-pan" && option == "--aid" =>
        {
            (Some(emv::parse_aid(value)?), None, true)
        }
        [mode, option, value, flag]
            if mode == "--info" && option == "--aid" && flag == "--show-pan" =>
        {
            (Some(emv::parse_aid(value)?), None, true)
        }
        [mode, path] if mode == "--dump" && path != "--show-pan" => {
            (None, Some(Path::new(path)), false)
        }
        [mode, path, option, value]
            if mode == "--dump" && path != "--show-pan" && option == "--aid" =>
        {
            (Some(emv::parse_aid(value)?), Some(Path::new(path)), false)
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Usage: omnismart [--emv [--aid HEX]] or \
                 omnismart --info [--aid HEX] [--show-pan] or \
                 omnismart --dump PATH [--aid HEX]. --show-pan requires --info. \
                 Use --help for details.",
            ));
        }
    };
    Ok(Arguments {
        aid,
        dump_path,
        show_pan,
    })
}

fn read_card() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--help"] || arguments == ["-h"] {
        println!("Usage: omnismart [--emv [--aid HEX]]");
        println!("       omnismart --info [--aid HEX] [--show-pan]");
        println!("       omnismart --dump PATH [--aid HEX]");
        println!("Without arguments: read the ATR only.");
        println!("--emv: print raw EMV directory and application-selection responses.");
        println!("--info: read general information and cardholder name, if exposed.");
        println!("--info sends GET PROCESSING OPTIONS, but does not authorize a payment.");
        println!("--dump PATH: save raw readable EMV exchanges to a NEW local JSON file.");
        println!("--dump includes GPO and AFL records, not protected or unadvertised memory.");
        println!("--aid HEX: select a documented application ID instead of discovering it.");
        println!("--show-pan: with --info only, display the full PAN if tag 5A is readable.");
        println!("WARNING: --show-pan exposes the full card number. Keep output private.");
        println!("WARNING: EMV output may contain sensitive bank-card data. Do not share it.");
        return Ok(());
    }
    let Arguments {
        aid,
        dump_path,
        show_pan,
    } = parse_arguments(&arguments)?;
    let read_emv = !arguments.is_empty();
    let information = arguments.first().is_some_and(|mode| mode == "--info");
    let context = ReaderContext::establish()?;
    let readers = context.readers()?;

    for reader in &readers {
        println!("Reader: {}", reader.to_string_lossy());
    }

    let reader = select_reader(&readers)?;

    println!("Connecting through: {}", reader.to_string_lossy());
    let mut card = context.connect(reader)?;
    let status = card.status()?;

    println!("Protocol: {:?}", status.protocol);
    print!("ATR:");
    for byte in &status.atr {
        print!(" {byte:02X}");
    }
    println!();
    println!(
        "Connected successfully. The ATR is card initialization data, not stored application data."
    );
    if read_emv {
        if dump_path.is_some() {
            println!(
                "WARNING: The dump file may contain your name, PAN, expiry, and Track 2 data."
            );
            println!("Keep it local and private. This is not a full-memory dump.");
            println!("GPO may affect card state/counters; no payment is authorized.");
        } else if information {
            if show_pan {
                println!(
                    "WARNING: Full PAN display explicitly enabled. Do not share or log output."
                );
            } else {
                println!("Reading general information. Raw account data will not be printed.");
            }
            println!(
                "GET PROCESSING OPTIONS initializes EMV processing; no payment is authorized."
            );
        } else {
            println!(
                "WARNING: Raw EMV output may contain sensitive data. Do not share or upload it."
            );
        }
        let mut dump = dump_path
            .map(|path| {
                dump::Dump::create(
                    path,
                    &reader.to_string_lossy(),
                    format!("{:?}", status.protocol),
                    &status.atr,
                )
            })
            .transpose()?;
        let result = card.with_transaction(|mut transmit| {
            if let Some(dump) = dump.as_mut() {
                emv::read_dump(
                    |command| dump.exchange(&mut transmit, command),
                    aid.as_deref(),
                )
            } else if information {
                if show_pan {
                    emv::read_information_with_pan(&mut transmit, aid.as_deref())
                } else {
                    emv::read_information(&mut transmit, aid.as_deref())
                }
            } else {
                emv::read_directory(&mut transmit, aid.as_deref())
            }
        });
        if let Some(dump) = dump {
            dump.finish(result.as_ref().err()).map_err(|error| {
                io::Error::other(format!(
                    "Cannot persist the JSON dump: {error}. Read error: {}",
                    result
                        .as_ref()
                        .err()
                        .map(ToString::to_string)
                        .as_deref()
                        .unwrap_or("none")
                ))
            })?;
            if let Some(path) = dump_path {
                if result.is_ok() {
                    println!("Readable EMV dump saved to {}", path.display());
                } else {
                    eprintln!("Incomplete EMV dump saved to {}", path.display());
                }
            }
        }
        result?;
    } else {
        println!("Use cargo run -- --emv to read raw EMV directory and selection responses.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> io::Result<Arguments<'static>> {
        let arguments: Vec<String> = arguments.iter().map(|value| value.to_string()).collect();
        let parsed = parse_arguments(&arguments)?;
        assert!(parsed.dump_path.is_none());
        Ok(Arguments {
            aid: parsed.aid,
            dump_path: None,
            show_pan: parsed.show_pan,
        })
    }

    #[test]
    fn pan_display_requires_explicit_information_opt_in() {
        for arguments in [vec![], vec!["--info"], vec!["--emv"]] {
            assert!(!parse(&arguments).unwrap().show_pan);
        }
        assert!(parse(&["--info", "--show-pan"]).unwrap().show_pan);
        for arguments in [
            vec!["--info", "--show-pan", "--aid", "A000000003"],
            vec!["--info", "--aid", "A000000003", "--show-pan"],
        ] {
            let parsed = parse(&arguments).unwrap();
            assert!(parsed.show_pan);
            assert_eq!(parsed.aid, Some(vec![0xA0, 0, 0, 0, 3]));
        }
    }

    #[test]
    fn rejects_unsupported_duplicate_and_incomplete_pan_options() {
        for arguments in [
            vec!["--show-pan"],
            vec!["--emv", "--show-pan"],
            vec!["--dump", "--show-pan"],
            vec!["--dump", "file.json", "--show-pan"],
            vec!["--info", "--show-pan", "--show-pan"],
            vec!["--info", "--show-pan", "--aid"],
            vec!["--info", "--aid", "invalid", "--show-pan"],
        ] {
            assert_eq!(
                parse(&arguments).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn existing_aid_and_dump_modes_keep_pan_display_disabled() {
        for arguments in [
            vec!["--info", "--aid", "A000000003"],
            vec!["--emv", "--aid", "A000000003"],
        ] {
            let parsed = parse(&arguments).unwrap();
            assert_eq!(parsed.aid, Some(vec![0xA0, 0, 0, 0, 3]));
            assert!(!parsed.show_pan);
        }
        for arguments in [
            vec!["--dump", "synthetic.json"],
            vec!["--dump", "synthetic.json", "--aid", "A000000003"],
        ] {
            let arguments: Vec<String> = arguments.into_iter().map(str::to_owned).collect();
            let parsed = parse_arguments(&arguments).unwrap();
            assert_eq!(parsed.dump_path, Some(Path::new("synthetic.json")));
            assert!(!parsed.show_pan);
        }
    }
}

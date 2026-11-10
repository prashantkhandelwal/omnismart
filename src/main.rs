use omnismart::{ReaderContext, dump, emv, select_reader, sle4428};
use std::{
    error::Error,
    io::{self, IsTerminal, Write},
    path::Path,
    process::ExitCode,
};

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
    sync_api_path: Option<&'a Path>,
    write: Option<sle4428::WriteRequest>,
}

fn parse_arguments(arguments: &[String]) -> io::Result<Arguments<'_>> {
    if arguments.first().is_some_and(|mode| mode == "--sle4428") {
        let (path, write) = match arguments {
            [_, option, path] if option == "--sync-api" => (path, None),
            [_, option, path, write, address, data]
                if option == "--sync-api" && write == "--write" =>
            {
                let address =
                    u32::from_str_radix(address.strip_prefix("0x").unwrap_or(address), 16)
                        .map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "Write address must be hexadecimal, 0000..03FC.",
                            )
                        })?;
                (
                    path,
                    Some(sle4428::WriteRequest::new(address, parse_hex(data)?)?),
                )
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Usage: omnismart --sle4428 --sync-api ABSOLUTE_DLL_PATH [--write ADDRESS_HEX DATA_HEX]. \
                 Cannot combine with EMV options or supply a PIN on the command line.",
                ));
            }
        };
        let path = Path::new(path);
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--sync-api requires an absolute path to HID's trusted scardsyn.dll.",
            ));
        }
        return Ok(Arguments {
            aid: None,
            dump_path: None,
            show_pan: false,
            sync_api_path: Some(path),
            write,
        });
    }
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
                 For SLE4428: omnismart --sle4428 --sync-api ABSOLUTE_DLL_PATH. \
                 Use --help for details.",
            ));
        }
    };
    Ok(Arguments {
        aid,
        dump_path,
        show_pan,
        sync_api_path: None,
        write: None,
    })
}

fn parse_hex(value: &str) -> io::Result<Vec<u8>> {
    let value: Vec<u8> = value
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if value.is_empty()
        || !value.len().is_multiple_of(2)
        || !value.iter().all(u8::is_ascii_hexdigit)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Hex data must contain complete hexadecimal byte pairs.",
        ));
    }
    value
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte: u8| {
                if byte.is_ascii_digit() {
                    byte - b'0'
                } else {
                    byte.to_ascii_lowercase() - b'a' + 10
                }
            };
            Ok(digit(pair[0]) * 16 + digit(pair[1]))
        })
        .collect()
}

fn parse_pin(value: &str) -> io::Result<[u8; 2]> {
    parse_hex(value)?.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "PIN must contain exactly two hexadecimal bytes. No PIN attempt was made.",
        )
    })
}

fn confirm_write(reader: &str, request: &sle4428::WriteRequest) -> io::Result<[u8; 2]> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SLE4428 writes require an interactive terminal for confirmation and hidden PIN input.",
        ));
    }
    eprintln!("WARNING: Writing can destroy card data. Back up memory first.");
    eprintln!(
        "Reader: {reader}; write {} bytes at {:04X}..{:04X}.",
        request.data().len(),
        request.address(),
        request.address() + request.data().len() as u32 - 1
    );
    eprintln!(
        "An incorrect PIN consumes retries. One attempt only; no PIN changes or permanent protection."
    );
    eprint!("Type WRITE to confirm, or anything else to cancel: ");
    io::stderr().flush()?;
    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    require_write_confirmation(&response)?;
    let pin =
        rpassword::prompt_password("Enter the card's 2-byte PIN as four hex digits (hidden): ")?;
    parse_pin(&pin)
}

fn require_write_confirmation(response: &str) -> io::Result<()> {
    if response.trim() != "WRITE" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Write cancelled. No PIN attempt or write was made.",
        ));
    }
    Ok(())
}

fn read_card() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--help"] || arguments == ["-h"] {
        println!("Usage: omnismart [--emv [--aid HEX]]");
        println!("       omnismart --info [--aid HEX] [--show-pan]");
        println!("       omnismart --dump PATH [--aid HEX]");
        println!("       omnismart --sle4428 --sync-api ABSOLUTE_DLL_PATH");
        println!(
            "       omnismart --sle4428 --sync-api ABSOLUTE_DLL_PATH --write ADDRESS_HEX DATA_HEX"
        );
        println!(
            "--write: explicitly write unprotected SLE4428 bytes, then verify by reading back."
        );
        println!("Write addresses: 0000..03FC (hex); security addresses 03FD..03FF are blocked.");
        println!(
            "Writes require interactive confirmation and a hidden 2-byte PIN; one attempt, no retries."
        );
        println!(
            "No PIN changes, permanent protection, or reader configuration changes are supported."
        );
        println!("Without arguments: read the ATR only.");
        println!("--emv: print raw EMV directory and application-selection responses.");
        println!("--info: read general information and cardholder name, if exposed.");
        println!("--info sends GET PROCESSING OPTIONS, but does not authorize a payment.");
        println!("--dump PATH: save raw readable EMV exchanges to a NEW local JSON file.");
        println!("--dump includes GPO and AFL records, not protected or unadvertised memory.");
        println!("--aid HEX: select a documented application ID instead of discovering it.");
        println!("--show-pan: with --info only, display the full PAN if tag 5A is readable.");
        println!(
            "--sle4428: read 1024 main-memory bytes as hex using HID's Windows Synchronous-API."
        );
        println!(
            "--sync-api: absolute path to a trusted HID scardsyn.dll matching this executable."
        );
        println!(
            "SLE4428 mode cannot be combined with EMV options; it does not change reader settings."
        );
        println!("WARNING: SLE4428 output is raw card data. Keep output private.");
        println!("WARNING: --show-pan exposes the full card number. Keep output private.");
        println!("WARNING: EMV output may contain sensitive bank-card data. Do not share it.");
        return Ok(());
    }
    let Arguments {
        aid,
        dump_path,
        show_pan,
        sync_api_path,
        write,
    } = parse_arguments(&arguments)?;
    if write.is_some() && (!io::stdin().is_terminal() || !io::stderr().is_terminal()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SLE4428 writes require an interactive terminal for confirmation and hidden PIN input.",
        )
        .into());
    }
    let sync_api = sync_api_path
        .map(sle4428::SynchronousApi::load)
        .transpose()?;
    let read_emv = !arguments.is_empty();
    let information = arguments.first().is_some_and(|mode| mode == "--info");
    let context = ReaderContext::establish()?;
    let readers = context.readers()?;

    for reader in &readers {
        println!("Reader: {}", reader.to_string_lossy());
    }

    let reader = select_reader(&readers)?;

    println!("Connecting through: {}", reader.to_string_lossy());
    if let Some(api) = sync_api {
        if let Some(request) = write {
            let pin = confirm_write(&reader.to_string_lossy(), &request)?;
            api.write(reader, &request, pin)?;
            println!(
                "SLE4428 write completed and read-back verified ({} bytes at {:04X}).",
                request.data().len(),
                request.address()
            );
            return Ok(());
        }
        println!("WARNING: Raw SLE4428 memory may contain sensitive data. Keep output private.");
        println!("Read-only mode: no PIN attempts, writes, or reader configuration changes.");
        let memory = api.read(reader)?;
        println!("SLE4428 main memory ({} bytes):", memory.len());
        for (row, bytes) in memory.chunks(16).enumerate() {
            print!("{:04X}:", row * 16);
            for byte in bytes {
                print!(" {byte:02X}");
            }
            println!();
        }
        return Ok(());
    }
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
        assert!(parsed.sync_api_path.is_none());
        Ok(Arguments {
            aid: parsed.aid,
            dump_path: None,
            show_pan: parsed.show_pan,
            sync_api_path: None,
            write: None,
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

    #[test]
    fn synchronous_mode_is_explicit_and_separate_from_emv() {
        let path = std::env::temp_dir().join("scardsyn.dll");
        let arguments = vec![
            "--sle4428".to_owned(),
            "--sync-api".to_owned(),
            path.to_string_lossy().into_owned(),
        ];
        let parsed = parse_arguments(&arguments).unwrap();
        assert_eq!(parsed.sync_api_path, Some(path.as_path()));
        assert!(parsed.aid.is_none());
        assert!(parsed.dump_path.is_none());
        assert!(!parsed.show_pan);

        for arguments in [vec![], vec!["--emv"], vec!["--info"]] {
            assert!(parse(&arguments).unwrap().sync_api_path.is_none());
        }
    }

    #[test]
    fn parses_explicit_bounded_hex_write_only_in_synchronous_mode() {
        let path = std::env::temp_dir().join("scardsyn.dll");
        let path = path.to_str().unwrap();
        for (address, data) in [("0020", "AAbb"), ("0x20", "AA bb")] {
            let arguments =
                ["--sle4428", "--sync-api", path, "--write", address, data].map(str::to_owned);
            let parsed = parse_arguments(&arguments).unwrap();
            let request = parsed.write.unwrap();
            assert_eq!(request.address(), 0x20);
            assert_eq!(request.data(), [0xAA, 0xBB]);
            assert!(!parsed.show_pan);
            assert!(parsed.aid.is_none());
            assert!(parsed.dump_path.is_none());
        }
        for (address, data) in [
            ("xyz", "AA"),
            ("-1", "AA"),
            ("FFFFFFFFF", "AA"),
            ("03FD", "AA"),
            ("03FC", "AABB"),
            ("0020", ""),
            ("0020", "A"),
            ("0020", "GG"),
            ("0020", "é"),
        ] {
            let arguments =
                ["--sle4428", "--sync-api", path, "--write", address, data].map(str::to_owned);
            assert_eq!(
                parse_arguments(&arguments).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn pin_parsing_requires_exactly_two_bytes_without_echoing_input() {
        assert_eq!(parse_pin("1234").unwrap(), [0x12, 0x34]);
        assert_eq!(parse_pin("aA bB").unwrap(), [0xAA, 0xBB]);
        for value in ["", "12", "123", "123456", "gggg", "秘密"] {
            assert_eq!(
                parse_pin(value).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn write_confirmation_has_no_default_or_automatic_acceptance() {
        require_write_confirmation("WRITE\r\n").unwrap();
        for response in ["", "\n", "yes", "Y", "write", "WRITE anything"] {
            let error = require_write_confirmation(response).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert!(error.to_string().contains("No PIN attempt or write"));
        }
    }
}

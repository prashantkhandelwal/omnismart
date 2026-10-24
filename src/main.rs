mod dump;
mod emv;

use pcsc::{Context, Protocols, Scope, ShareMode};
use std::{
    error::Error,
    ffi::{CStr, CString},
    io,
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

fn read_card() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--help"] || arguments == ["-h"] {
        println!("Usage: omnismart [--emv|--info [--aid HEX]]");
        println!("       omnismart --dump PATH [--aid HEX]");
        println!("Without arguments: read the ATR only.");
        println!("--emv: print raw EMV directory and application-selection responses.");
        println!("--info: read general information and cardholder name, if exposed.");
        println!("--info sends GET PROCESSING OPTIONS, but does not authorize a payment.");
        println!("--dump PATH: save raw readable EMV exchanges to a NEW local JSON file.");
        println!("--dump includes GPO and AFL records, not protected or unadvertised memory.");
        println!("--aid HEX: select a documented application ID instead of discovering it.");
        println!("WARNING: EMV output may contain sensitive bank-card data. Do not share it.");
        return Ok(());
    }
    let (aid, dump_path) = match arguments.as_slice() {
        [] => (None, None),
        [mode] if mode == "--emv" || mode == "--info" => (None, None),
        [mode, option, value] if (mode == "--emv" || mode == "--info") && option == "--aid" => {
            (Some(emv::parse_aid(value)?), None)
        }
        [mode, path] if mode == "--dump" => (None, Some(Path::new(path))),
        [mode, path, option, value] if mode == "--dump" && option == "--aid" => {
            (Some(emv::parse_aid(value)?), Some(Path::new(path)))
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Usage: omnismart [--emv|--info [--aid HEX]] or \
                 omnismart --dump PATH [--aid HEX]. Use --help for details.",
            )
            .into());
        }
    };
    let read_emv = !arguments.is_empty();
    let information = arguments.first().is_some_and(|mode| mode == "--info");
    let context = Context::establish(Scope::User).map_err(|error| {
        io::Error::other(format!(
            "Cannot connect to the smart-card service: {error}. \
             On Windows, check the SCardSvr service."
        ))
    })?;
    let readers = context.list_readers_owned().map_err(|error| {
        io::Error::other(format!(
            "Cannot list smart-card readers: {error}. \
             Check the USB connection and reader driver."
        ))
    })?;

    for reader in &readers {
        println!("Reader: {}", reader.to_string_lossy());
    }

    let reader = select_reader(&readers)?;

    println!("Connecting through: {}", reader.to_string_lossy());
    let mut card = context
        .connect(reader, ShareMode::Shared, Protocols::ANY)
        .map_err(|error| {
            let message = match error {
                pcsc::Error::NoSmartcard => {
                    "No card is inserted. Insert a compatible contact smart card and run again."
                        .to_owned()
                }
                _ => format!("Cannot connect to the card: {error}"),
            };
            io::Error::other(message)
        })?;

    let status = card
        .status2_owned()
        .map_err(|error| io::Error::other(format!("Cannot read card status: {error}")))?;

    println!("Protocol: {:?}", status.protocol2());
    print!("ATR:");
    for byte in status.atr() {
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
            println!("Reading general information. Raw account data will not be printed.");
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
                    format!("{:?}", status.protocol2()),
                    status.atr(),
                )
            })
            .transpose()?;
        let result = (|| -> io::Result<()> {
            let transaction = card.transaction().map_err(|error| {
                io::Error::other(format!("Cannot begin exclusive card access: {error}"))
            })?;
            let mut transmit = |command: &[u8]| {
                let mut buffer = [0; pcsc::MAX_BUFFER_SIZE];
                transaction
                    .transmit(command, &mut buffer)
                    .map(|response| response.to_vec())
                    .map_err(|error| io::Error::other(format!("APDU transmission failed: {error}")))
            };
            let result = if let Some(dump) = dump.as_mut() {
                emv::read_dump(
                    |command| dump.exchange(&mut transmit, command),
                    aid.as_deref(),
                )
            } else if information {
                emv::read_information(&mut transmit, aid.as_deref())
            } else {
                emv::read_directory(&mut transmit, aid.as_deref())
            };
            let end_result = transaction
                .end(pcsc::Disposition::LeaveCard)
                .map_err(|(_, error)| io::Error::other(format!("Cannot end card access: {error}")));
            match (result, end_result) {
                (Err(read_error), Err(end_error)) => {
                    Err(io::Error::other(format!("{read_error}; {end_error}")))
                }
                (Err(error), _) | (_, Err(error)) => Err(error),
                (Ok(()), Ok(())) => Ok(()),
            }
        })();
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

fn is_omnikey_3021(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("omnikey") && name.contains("3021")
}

fn select_reader(readers: &[CString]) -> io::Result<&CStr> {
    if let Some(reader) = readers
        .iter()
        .find(|name| is_omnikey_3021(&name.to_string_lossy()))
    {
        return Ok(reader.as_c_str());
    }

    let mut omnikey_readers = readers.iter().filter(|name| {
        name.to_string_lossy()
            .to_ascii_lowercase()
            .contains("omnikey")
    });
    match (omnikey_readers.next(), omnikey_readers.next()) {
        (Some(reader), None) => Ok(reader.as_c_str()),
        (Some(_), Some(_)) => Err(io::Error::other(
            "Multiple OMNIKEY readers found, but none names model 3021. \
             Connect only the intended reader and run again.",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No OMNIKEY reader found. Check the USB connection and driver.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_omnikey_3021, select_reader};
    use std::{ffi::CString, io};

    #[test]
    fn recognizes_reader_names_case_insensitively() {
        assert!(is_omnikey_3021(
            "HID Global OMNIKEY 3021 Smart Card Reader 0"
        ));
        assert!(is_omnikey_3021("Omnikey AG CardMan 3021 0"));
    }

    #[test]
    fn rejects_other_readers() {
        assert!(!is_omnikey_3021("HID OMNIKEY 5022 0"));
        assert!(!is_omnikey_3021("Other 3021 reader"));
        assert!(!is_omnikey_3021(""));
    }

    #[test]
    fn selects_generic_windows_reader_name() {
        let readers = [
            CString::new("Other reader 0").unwrap(),
            CString::new("OMNIKEY AG Smart Card Reader USB 0").unwrap(),
        ];
        assert_eq!(select_reader(&readers).unwrap(), readers[1].as_c_str());
    }

    #[test]
    fn prefers_explicit_model_name() {
        let readers = [
            CString::new("OMNIKEY AG Smart Card Reader USB 0").unwrap(),
            CString::new("HID OMNIKEY 3021 0").unwrap(),
        ];
        assert_eq!(select_reader(&readers).unwrap(), readers[1].as_c_str());
    }

    #[test]
    fn rejects_ambiguous_omnikey_readers() {
        let readers = [
            CString::new("OMNIKEY AG Smart Card Reader USB 0").unwrap(),
            CString::new("OMNIKEY AG Smart Card Reader USB 1").unwrap(),
        ];
        assert!(select_reader(&readers).is_err());
    }

    #[test]
    fn rejects_missing_omnikey_readers() {
        assert_eq!(
            select_reader(&[]).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let readers = [CString::new("Other reader 0").unwrap()];
        assert_eq!(
            select_reader(&readers).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}

//! Explicitly opted-in plaintext JSON exchange recording.
//!
//! Dumps may contain names, full PANs, expiry, and Track 2 data. Keep them local,
//! outside source control, and call [`Dump::finish`] on both success and failure.

use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize)]
struct Exchange {
    command_hex: String,
    response_hex: Option<String>,
    transport_error: Option<String>,
}

#[derive(Serialize)]
struct Report {
    format: &'static str,
    version: u8,
    scope: &'static str,
    created_unix_seconds: u64,
    reader: String,
    protocol: String,
    atr_hex: String,
    complete: bool,
    error: Option<String>,
    exchanges: Vec<Exchange>,
}

/// Collects raw exchanges and writes them when explicitly finished.
///
/// Dropping this value without calling [`Self::finish`] leaves an empty file.
pub struct Dump {
    file: File,
    report: Report,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

impl Dump {
    /// Creates a new destination; existing files are never overwritten.
    ///
    /// The plaintext file inherits its destination directory's access permissions.
    pub fn create(path: &Path, reader: &str, protocol: String, atr: &[u8]) -> io::Result<Self> {
        let created_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "Cannot create dump {} (existing files are never overwritten): {error}",
                        path.display()
                    ),
                )
            })?;
        Ok(Self {
            file,
            report: Report {
                format: "omnismart-readable-emv",
                version: 1,
                scope: "Payment directories, selected applications, GPO, and AFL-advertised \
                        records (SFI 1-30). Not a full-memory dump; protected and unadvertised \
                        data are excluded. Nonempty PDOLs are unsupported.",
                created_unix_seconds,
                reader: reader.to_owned(),
                protocol,
                atr_hex: hex(atr),
                complete: false,
                error: None,
                exchanges: Vec::new(),
            },
        })
    }

    /// Records a raw command and its response or transport error.
    pub fn exchange(
        &mut self,
        transmit: &mut (impl FnMut(&[u8]) -> io::Result<Vec<u8>> + ?Sized),
        command: &[u8],
    ) -> io::Result<Vec<u8>> {
        let response = transmit(command);
        self.report.exchanges.push(Exchange {
            command_hex: hex(command),
            response_hex: response.as_ref().ok().map(|bytes| hex(bytes)),
            transport_error: response.as_ref().err().map(ToString::to_string),
        });
        response
    }

    /// Writes and flushes the report, preserving partial exchanges on read failure.
    ///
    /// Pass the reading operation's error to mark an incomplete report.
    pub fn finish(mut self, error: Option<&io::Error>) -> io::Result<()> {
        self.report.complete = error.is_none();
        self.report.error = error.map(ToString::to_string);
        serde_json::to_writer_pretty(&mut self.file, &self.report).map_err(io::Error::other)?;
        self.file.write_all(b"\n")?;
        self.file.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);

    fn path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "omnismart-synthetic-{}-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed),
        ))
    }

    #[test]
    fn persists_exact_raw_exchanges_and_never_overwrites() {
        let path = path();
        let mut dump =
            Dump::create(&path, "Synthetic reader", "T0".to_owned(), &[0x3B, 0]).unwrap();
        dump.exchange(&mut |_| Ok(vec![0x70, 0, 0x90, 0]), &[0, 0xB2, 1, 0x0C, 0])
            .unwrap();
        dump.finish(None).unwrap();
        let json: serde_json::Value = serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        assert_eq!(json["format"], "omnismart-readable-emv");
        assert_eq!(json["complete"], true);
        assert_eq!(json["atr_hex"], "3B00");
        assert_eq!(json["exchanges"][0]["command_hex"], "00B2010C00");
        assert_eq!(json["exchanges"][0]["response_hex"], "70009000");
        assert!(json["error"].is_null());
        assert!(matches!(
            Dump::create(&path, "", String::new(), &[]),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists
        ));
        let unchanged: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        assert_eq!(unchanged, json);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn persists_partial_data_and_transport_failure() {
        let path = path();
        let mut dump = Dump::create(&path, "Synthetic reader", "T0".to_owned(), &[]).unwrap();
        dump.exchange(&mut |_| Ok(vec![0x90, 0]), &[0, 0xA4, 4, 0, 0])
            .unwrap();
        let error = dump
            .exchange(
                &mut |_| Err(io::Error::other("Synthetic disconnection")),
                &[0, 0xB2, 1, 0x0C, 0],
            )
            .unwrap_err();
        dump.finish(Some(&error)).unwrap();
        let json: serde_json::Value = serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        assert_eq!(json["complete"], false);
        assert_eq!(json["error"], "Synthetic disconnection");
        assert_eq!(json["exchanges"].as_array().unwrap().len(), 2);
        assert!(json["exchanges"][1]["response_hex"].is_null());
        assert_eq!(
            json["exchanges"][1]["transport_error"],
            "Synthetic disconnection"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn persists_a_complete_synthetic_emv_dump_including_continuations() {
        let path = path();
        let mut dump =
            Dump::create(&path, "Synthetic reader", "T0".to_owned(), &[0x3B, 0]).unwrap();
        let mut responses = vec![
            vec![0x61, 2],
            vec![0x6F, 0, 0x90, 0],
            vec![0x80, 6, 0, 0, 88, 1, 1, 0, 0x90, 0],
            vec![0xDE, 0xAD, 0xBE, 0xEF, 0x90, 0],
        ]
        .into_iter();
        let result = crate::emv::read_dump(
            |command| dump.exchange(&mut |_| Ok(responses.next().unwrap()), command),
            Some(&[0xA0, 0, 0, 0, 1]),
        );
        dump.finish(result.as_ref().err()).unwrap();
        result.unwrap();
        let json: serde_json::Value = serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        assert_eq!(json["complete"], true);
        assert_eq!(json["exchanges"].as_array().unwrap().len(), 4);
        assert_eq!(json["exchanges"][0]["response_hex"], "6102");
        assert_eq!(json["exchanges"][1]["command_hex"], "00C0000002");
        assert_eq!(json["exchanges"][2]["command_hex"], "80A8000002830000");
        assert_eq!(json["exchanges"][3]["command_hex"], "00B2015C00");
        assert_eq!(json["exchanges"][3]["response_hex"], "DEADBEEF9000");
        std::fs::remove_file(&path).unwrap();
    }
}

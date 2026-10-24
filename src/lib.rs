//! PC/SC reader access and opt-in EMV reading helpers.
//!
//! Reader discovery, connection, status, and transaction methods do not print.
//! The [`emv`] module retains the CLI's console-oriented reading helpers.
//! Card data can be sensitive; do not log or upload raw responses.
//!
//! ```no_run
//! use omnismart::ReaderContext;
//!
//! let context = ReaderContext::establish()?;
//! let connection = context.connect_preferred()?;
//! let status = connection.status()?;
//! println!("Protocol: {:?}", status.protocol);
//! println!("ATR: {:02X?}", status.atr);
//! # Ok::<(), std::io::Error>(())
//! ```

pub mod dump;
pub mod emv;

pub use pcsc::Protocol;
use pcsc::{Context, Protocols, Scope, ShareMode};
use std::{
    ffi::{CStr, CString},
    io,
};

/// An established connection to the local PC/SC smart-card service.
pub struct ReaderContext {
    context: Context,
}

impl ReaderContext {
    /// Connects to the smart-card service in the current user's scope.
    pub fn establish() -> io::Result<Self> {
        let context = Context::establish(Scope::User).map_err(|error| {
            io::Error::other(format!(
                "Cannot connect to the smart-card service: {error}. \
                 On Windows, check the SCardSvr service."
            ))
        })?;
        Ok(Self { context })
    }

    /// Lists reader names without selecting or connecting to a card.
    pub fn readers(&self) -> io::Result<Vec<CString>> {
        self.context.list_readers_owned().map_err(|error| {
            io::Error::other(format!(
                "Cannot list smart-card readers: {error}. \
                 Check the USB connection and reader driver."
            ))
        })
    }

    /// Connects to the inserted card through the named reader in shared mode.
    ///
    /// Unlike [`select_reader`], this accepts any reader reported by PC/SC.
    pub fn connect(&self, reader: &CStr) -> io::Result<CardConnection> {
        let card = self
            .context
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
        Ok(CardConnection {
            card,
            reader: reader.to_owned(),
        })
    }

    /// Discovers readers and connects using the CLI's OMNIKEY selection policy.
    pub fn connect_preferred(&self) -> io::Result<CardConnection> {
        let readers = self.readers()?;
        self.connect(select_reader(&readers)?)
    }
}

/// Owned connection metadata. An ATR is not stored application data or a unique ID.
#[derive(Clone, Debug)]
pub struct CardStatus {
    pub reader: String,
    pub protocol: Option<Protocol>,
    pub atr: Vec<u8>,
}

/// A shared PC/SC card connection, released when dropped.
pub struct CardConnection {
    card: pcsc::Card,
    reader: CString,
}

impl CardConnection {
    /// Returns the reader name, negotiated protocol, and ATR.
    pub fn status(&self) -> io::Result<CardStatus> {
        let status = self
            .card
            .status2_owned()
            .map_err(|error| io::Error::other(format!("Cannot read card status: {error}")))?;
        Ok(CardStatus {
            reader: self.reader.to_string_lossy().into_owned(),
            protocol: status.protocol2(),
            atr: status.atr().to_vec(),
        })
    }

    /// Runs an operation with exclusive card access and a raw APDU transport.
    ///
    /// Responses include SW1/SW2. Ending the transaction leaves the card powered.
    /// Operation and cleanup errors are both preserved. This transport does not
    /// restrict commands: callers are responsible for authorization and avoiding
    /// writes, PIN attempts, or state-changing commands they do not intend.
    ///
    /// ```no_run
    /// # use omnismart::{ReaderContext, emv};
    /// # let context = ReaderContext::establish()?;
    /// # let mut connection = context.connect_preferred()?;
    /// connection.with_transaction(|transmit| emv::read_directory(transmit, None))?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn with_transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut dyn FnMut(&[u8]) -> io::Result<Vec<u8>>) -> io::Result<T>,
    ) -> io::Result<T> {
        let transaction = self.card.transaction().map_err(|error| {
            io::Error::other(format!("Cannot begin exclusive card access: {error}"))
        })?;
        let result = operation(&mut |command| {
            let mut buffer = [0; pcsc::MAX_BUFFER_SIZE];
            transaction
                .transmit(command, &mut buffer)
                .map(|response| response.to_vec())
                .map_err(|error| io::Error::other(format!("APDU transmission failed: {error}")))
        });
        let end_result = transaction
            .end(pcsc::Disposition::LeaveCard)
            .map_err(|(_, error)| io::Error::other(format!("Cannot end card access: {error}")));
        finish_transaction(result, end_result)
    }
}

fn finish_transaction<T>(result: io::Result<T>, end_result: io::Result<()>) -> io::Result<T> {
    match (result, end_result) {
        (Err(read_error), Err(end_error)) => {
            Err(io::Error::other(format!("{read_error}; {end_error}")))
        }
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

/// Tests the reader's reported name, not its hardware identity.
pub fn is_omnikey_3021(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("omnikey") && name.contains("3021")
}

/// Prefers the first explicit OMNIKEY 3021, otherwise a single generic OMNIKEY.
///
/// Returns an error if no OMNIKEY is present or the generic fallback is ambiguous.
pub fn select_reader(readers: &[CString]) -> io::Result<&CStr> {
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
    use super::*;

    #[test]
    fn recognizes_reader_names_case_insensitively() {
        assert!(is_omnikey_3021(
            "HID Global OMNIKEY 3021 Smart Card Reader 0"
        ));
        assert!(is_omnikey_3021("Omnikey AG CardMan 3021 0"));
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

    #[test]
    fn preserves_transaction_values_and_errors() {
        assert_eq!(finish_transaction(Ok(42), Ok(())).unwrap(), 42);
        assert_eq!(
            finish_transaction::<()>(Err(io::Error::other("Read failed")), Ok(()))
                .unwrap_err()
                .to_string(),
            "Read failed"
        );
        assert_eq!(
            finish_transaction(Ok(42), Err(io::Error::other("End failed")))
                .unwrap_err()
                .to_string(),
            "End failed"
        );
        assert_eq!(
            finish_transaction::<()>(
                Err(io::Error::other("Read failed")),
                Err(io::Error::other("End failed")),
            )
            .unwrap_err()
            .to_string(),
            "Read failed; End failed"
        );
    }
}

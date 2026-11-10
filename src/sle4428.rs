//! SLE4428 memory access through HID's Windows Synchronous-API.
//!
//! Requires a compatible HID driver and an explicitly selected, trusted DLL
//! matching the process architecture. Reads never attempt a PIN or write.
//! Writes require explicit data and a PIN; permanent protection and PIN changes
//! are not exposed. EMV connections do not load this API.

use std::{ffi::CStr, io, path::Path};

/// Number of bytes in SLE4428 main memory.
pub const MEMORY_SIZE: usize = 1024;
/// Exclusive upper write bound: reserves 03FD..03FF for security state.
pub const WRITE_LIMIT: usize = 0x03FD;

/// Decodes complete hex byte pairs, allowing ASCII whitespace between digits.
pub fn parse_hex(value: &str) -> io::Result<Vec<u8>> {
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

/// Decodes a two-byte PSC/PIN without including the supplied value in errors.
pub fn parse_pin(value: &str) -> io::Result<[u8; 2]> {
    parse_hex(value)?.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "PIN must contain exactly two hexadecimal bytes. No PIN attempt was made.",
        )
    })
}

/// A bounded main-memory write. Does not include a PIN or protection settings.
pub struct WriteRequest {
    address: u32,
    data: Vec<u8>,
    payload_len: usize,
}

impl std::fmt::Debug for WriteRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WriteRequest")
            .field("address", &self.address)
            .field("length", &self.data.len())
            .field("padding_length", &self.padding_len())
            .finish()
    }
}

impl WriteRequest {
    /// Validates a nonempty write within 0000..03FC.
    ///
    /// Security addresses 03FD..03FF are excluded to prevent counter/PIN changes.
    pub fn new(address: u32, data: Vec<u8>) -> io::Result<Self> {
        if data.is_empty()
            || address >= WRITE_LIMIT as u32
            || data.len() > WRITE_LIMIT - address as usize
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SLE4428 write data must be nonempty and fit within 0000..03FC. \
                 Security addresses 03FD..03FF cannot be written.",
            ));
        }
        let payload_len = data.len();
        Ok(Self {
            address,
            data,
            payload_len,
        })
    }

    /// Zero-pads this write to a fixed field length in bytes.
    ///
    /// Never truncates data or crosses security addresses. The entire padded
    /// field participates in protection checks, confirmation, and verification.
    pub fn pad_to(mut self, field_length: usize) -> io::Result<Self> {
        if field_length < self.data.len() || field_length > WRITE_LIMIT - self.address as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Field length must be at least the payload byte count and fit within 0000..03FC.",
            ));
        }
        self.data.resize(field_length, 0);
        Ok(self)
    }

    /// Clears any unknown-length trailing data through the last writable byte.
    ///
    /// This overwrites all data after the payload through 03FC, including any
    /// unrelated fields there. Bytes before the address and security bytes are
    /// untouched. All target bytes must be unprotected.
    pub fn replace_to_end(self) -> io::Result<Self> {
        let length = WRITE_LIMIT - self.address as usize;
        self.pad_to(length)
    }

    /// Number of zero-padding bytes added after the original payload.
    pub fn padding_len(&self) -> usize {
        self.data.len() - self.payload_len
    }

    pub fn address(&self) -> u32 {
        self.address
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// A loaded HID Synchronous-API library, independent of EMV connections.
pub struct SynchronousApi {
    #[cfg(windows)]
    inner: windows::Api,
}

impl SynchronousApi {
    /// Loads a trusted HID `scardsyn.dll` from an absolute path.
    ///
    /// Loading a DLL executes native code. Only use an authentic HID installation.
    /// The DLL and executable must have matching architectures (e.g. both x64).
    pub fn load(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--sync-api requires an absolute path to HID's trusted scardsyn.dll.",
            ));
        }
        #[cfg(windows)]
        {
            Ok(Self {
                inner: windows::Api::load(path)?,
            })
        }
        #[cfg(not(windows))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SLE4428 access through HID's Synchronous-API is supported only on Windows.",
            ))
        }
    }

    /// Reads all 1024 main-memory bytes from the named reader without printing.
    ///
    /// Checks the driver's three-wire ATR type before reading. This does not
    /// distinguish SLE4428 from compatible three-wire cards such as SLE4418.
    /// Read and cleanup failures are returned; partial memory is never returned.
    /// The installed driver must initialize the card successfully first.
    pub fn read(&self, reader: &CStr) -> io::Result<[u8; MEMORY_SIZE]> {
        #[cfg(windows)]
        {
            self.inner.read(reader)
        }
        #[cfg(not(windows))]
        {
            let _ = reader;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SLE4428 access through HID's Synchronous-API is supported only on Windows.",
            ))
        }
    }

    /// Writes unprotected main-memory bytes, then reads back and compares.
    ///
    /// Callers must obtain authorization and confirmation before calling.
    /// Checks the ATR and all target protection bits before one PIN attempt.
    /// Never retries PIN verification or writes, changes PINs, or sets protection.
    /// A failed write may have partially modified the card; there is no rollback.
    /// Disconnect resets the card to clear verification state (even on failure).
    /// An error during cleanup can occur after a successful write.
    pub fn write(&self, reader: &CStr, request: &WriteRequest, pin: [u8; 2]) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.inner.write(reader, request, pin)
        }
        #[cfg(not(windows))]
        {
            let _ = (reader, request, pin);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SLE4428 access through HID's Synchronous-API is supported only on Windows.",
            ))
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::finish_transaction;
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
    };
    use pcsc_sys as ffi;
    use std::ptr::null;

    // HID scard3w.h: SCARDHANDLE is pointer-sized; ULONG and OKERR are 32-bit.
    type ReadData = unsafe extern "system" fn(ffi::SCARDHANDLE, u32, *mut u8, u32) -> i32;
    type PresentPin = unsafe extern "system" fn(ffi::SCARDHANDLE, u32, *mut u8) -> i32;
    type WriteData = unsafe extern "system" fn(ffi::SCARDHANDLE, u32, *mut u8, u32, i32) -> i32;
    type VerifyProtect = unsafe extern "system" fn(ffi::SCARDHANDLE, u32, *mut i32) -> i32;

    struct WriteFunctions {
        present: PresentPin,
        write: WriteData,
        protected: VerifyProtect,
    }

    pub(super) struct Api {
        read_data: ReadData,
        _library: Library,
    }

    impl Api {
        pub(super) fn load(path: &Path) -> io::Result<Self> {
            // The caller explicitly selects executable vendor code. Restrict
            // dependency lookup to its directory and Windows' trusted defaults.
            let library = unsafe {
                Library::load_with_flags(
                    path,
                    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
                )
            }
            .map_err(|error| {
                io::Error::other(format!(
                    "Cannot load HID Synchronous-API at {}: {error}. \
                     Install HID's Synchronous-API and use a DLL matching this {}-bit executable. \
                     Existing EMV modes do not require this DLL.",
                    path.display(),
                    usize::BITS,
                ))
            })?;
            // Signature and calling convention follow HID's scard3w.h / ok.h.
            let read_data = unsafe { library.get::<ReadData>(b"SCard3WBPReadData\0") }
                .map(|symbol| *symbol)
                .map_err(|error| {
                    io::Error::other(format!(
                        "HID Synchronous-API is missing SCard3WBPReadData: {error}"
                    ))
                })?;
            Ok(Self {
                read_data,
                _library: library,
            })
        }

        pub(super) fn read(&self, reader: &CStr) -> io::Result<[u8; MEMORY_SIZE]> {
            self.with_card(
                reader,
                ffi::SCARD_SHARE_SHARED,
                ffi::SCARD_LEAVE_CARD,
                |card| read_memory(card, self.read_data),
            )
        }

        pub(super) fn write(
            &self,
            reader: &CStr,
            request: &WriteRequest,
            pin: [u8; 2],
        ) -> io::Result<()> {
            // Write exports are resolved only for an explicitly requested write.
            let (present, write, protected) = unsafe {
                (
                    self._library.get::<PresentPin>(b"SCard3WBPPresentPIN\0"),
                    self._library.get::<WriteData>(b"SCard3WBPWriteData\0"),
                    self._library
                        .get::<VerifyProtect>(b"SCard3WBPVerifyProtectBit\0"),
                )
            };
            let present = present.map_err(|error| {
                io::Error::other(format!("Cannot load SCard3WBPPresentPIN: {error}"))
            })?;
            let write = write.map_err(|error| {
                io::Error::other(format!("Cannot load SCard3WBPWriteData: {error}"))
            })?;
            let protected = protected.map_err(|error| {
                io::Error::other(format!("Cannot load SCard3WBPVerifyProtectBit: {error}"))
            })?;
            let functions = WriteFunctions {
                present: *present,
                write: *write,
                protected: *protected,
            };
            self.with_card(
                reader,
                ffi::SCARD_SHARE_EXCLUSIVE,
                ffi::SCARD_RESET_CARD,
                |card| Self::write_with_functions(card, request, pin, self.read_data, functions),
            )
        }

        fn with_card<T>(
            &self,
            reader: &CStr,
            share_mode: u32,
            disposition: u32,
            operation: impl FnOnce(ffi::SCARDHANDLE) -> io::Result<T>,
        ) -> io::Result<T> {
            let mut context = 0;
            // All output pointers refer to live, correctly sized local values.
            check("establish smart-card context", unsafe {
                ffi::SCardEstablishContext(ffi::SCARD_SCOPE_USER, null(), null(), &mut context)
            })?;
            let result = self.with_context(context, reader, share_mode, disposition, operation);
            finish_transaction(
                result,
                check("release smart-card context", unsafe {
                    ffi::SCardReleaseContext(context)
                }),
            )
        }

        fn with_context<T>(
            &self,
            context: ffi::SCARDCONTEXT,
            reader: &CStr,
            share_mode: u32,
            disposition: u32,
            operation: impl FnOnce(ffi::SCARDHANDLE) -> io::Result<T>,
        ) -> io::Result<T> {
            let mut card = 0;
            let mut protocol = 0;
            // HID's Synchronous-API guide specifies a normal T=0 connection,
            // not SHARE_DIRECT, which would bypass required card initialization.
            check("connect to SLE4428", unsafe {
                ffi::SCardConnect(
                    context,
                    reader.as_ptr(),
                    share_mode,
                    ffi::SCARD_PROTOCOL_T0,
                    &mut card,
                    &mut protocol,
                )
            })?;
            let result = self.with_transaction(card, operation);
            finish_transaction(
                result,
                check("disconnect SLE4428", unsafe {
                    ffi::SCardDisconnect(card, disposition)
                }),
            )
        }

        fn write_with_functions(
            card: ffi::SCARDHANDLE,
            request: &WriteRequest,
            mut pin: [u8; 2],
            read: ReadData,
            functions: WriteFunctions,
        ) -> io::Result<()> {
            write_verified(
                request,
                |address| {
                    let mut value = 0;
                    check("read SLE4428 protection bit", unsafe {
                        (functions.protected)(card, address, &mut value)
                    })?;
                    Ok(value != 0)
                },
                || {
                    check("verify SLE4428 PIN (one attempt; no retries)", unsafe {
                        (functions.present)(card, 2, pin.as_mut_ptr())
                    })
                },
                || {
                    let mut data = request.data.clone();
                    // fProtect is always FALSE: never permanently protect bytes.
                    check(
                        "write SLE4428 memory (may be partially written; no retry or rollback)",
                        unsafe {
                            (functions.write)(
                                card,
                                data.len() as u32,
                                data.as_mut_ptr(),
                                request.address,
                                0,
                            )
                        },
                    )
                },
                |buffer| {
                    check(
                        "read back SLE4428 write (card may already be modified)",
                        unsafe {
                            read(
                                card,
                                buffer.len() as u32,
                                buffer.as_mut_ptr(),
                                request.address,
                            )
                        },
                    )
                },
            )
        }

        fn with_transaction<T>(
            &self,
            card: ffi::SCARDHANDLE,
            operation: impl FnOnce(ffi::SCARDHANDLE) -> io::Result<T>,
        ) -> io::Result<T> {
            check("begin SLE4428 transaction", unsafe {
                ffi::SCardBeginTransaction(card)
            })?;
            let result = (|| {
                let mut atr = [0; 33];
                let mut length = atr.len() as u32;
                check("read SLE4428 ATR", unsafe {
                    ffi::SCardGetAttrib(
                        card,
                        ffi::SCARD_ATTR_ATR_STRING,
                        atr.as_mut_ptr(),
                        &mut length,
                    )
                })?;
                let atr = atr.get(..length as usize).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Driver returned an oversized ATR.",
                    )
                })?;
                validate_atr(atr)?;
                operation(card)
            })();
            finish_transaction(
                result,
                check("end SLE4428 transaction", unsafe {
                    ffi::SCardEndTransaction(card, ffi::SCARD_LEAVE_CARD)
                }),
            )
        }
    }

    fn write_verified(
        request: &WriteRequest,
        mut protected: impl FnMut(u32) -> io::Result<bool>,
        present: impl FnOnce() -> io::Result<()>,
        write: impl FnOnce() -> io::Result<()>,
        read: impl FnOnce(&mut [u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        for address in request.address..request.address + request.data.len() as u32 {
            if protected(address)? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "SLE4428 address {address:04X} is permanently write-protected. \
                             No PIN attempt or write was made."
                    ),
                ));
            }
        }
        present()?;
        write()?;
        let mut actual = vec![0; request.data.len()];
        read(&mut actual)?;
        if actual != request.data {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SLE4428 read-back verification failed. Card memory may be partially modified. \
                 No retry or rollback was attempted.",
            ));
        }
        Ok(())
    }

    fn validate_atr(atr: &[u8]) -> io::Result<()> {
        match atr.get(2) {
            Some(0x92 | 0x02) => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The driver ATR does not identify a three-wire memory card. \
                 Use --sle4428 only with a compatible SLE4428/SLE4418 card; \
                 use the existing EMV modes for payment cards.",
            )),
        }
    }

    fn read_memory(card: ffi::SCARDHANDLE, read: ReadData) -> io::Result<[u8; MEMORY_SIZE]> {
        let mut memory = [0; MEMORY_SIZE];
        // The vendor call receives a valid card, full-size writable buffer,
        // and a bounded request covering addresses 0 through 1023.
        check("read SLE4428 main memory", unsafe {
            read(card, MEMORY_SIZE as u32, memory.as_mut_ptr(), 0)
        })?;
        Ok(memory)
    }

    fn check(operation: &str, status: i32) -> io::Result<()> {
        if status == 0 {
            return Ok(());
        }
        let hint = match status {
            value if value == 0x81100001_u32 as i32 => {
                " The PIN was incorrect; a verification attempt may have been consumed. \
                 No write or automatic retry was attempted."
            }
            value if value == 0x81100002_u32 as i32 => {
                " PIN verification is blocked. No write or automatic retry was attempted."
            }
            ffi::SCARD_W_UNRESPONSIVE_CARD => {
                " The card did not respond to reset. Check orientation and contacts, and install \
                 a HID driver supporting synchronous cards (the generic Microsoft CCID driver \
                 may not suffice). Test with HID's Read Memory tool. Loading scardsyn.dll alone \
                 cannot fix driver initialization. Reader voltage settings were not changed."
            }
            ffi::SCARD_E_NO_SMARTCARD => " Insert a compatible SLE4428 contact card and run again.",
            ffi::SCARD_E_NO_SERVICE | ffi::SCARD_E_SERVICE_STOPPED => {
                " Check the Windows SCardSvr service."
            }
            _ => "",
        };
        Err(io::Error::other(format!(
            "Cannot {operation}: status 0x{:08X}.{hint}",
            status as u32
        )))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn validates_three_wire_type_before_reading() {
            for atr in [vec![0x3B, 2, 0x92, 0], vec![0x3B, 2, 0x02, 0]] {
                validate_atr(&atr).unwrap();
            }
            for atr in [
                vec![],
                vec![0x3B, 0],
                vec![0x3B, 2, 0xA2, 0],
                vec![0x3B, 0x90, 0x11],
            ] {
                assert_eq!(
                    validate_atr(&atr).unwrap_err().kind(),
                    io::ErrorKind::InvalidData
                );
            }
        }

        unsafe extern "system" fn successful_read(
            card: usize,
            length: u32,
            buffer: *mut u8,
            address: u32,
        ) -> i32 {
            if card != 42 || length != MEMORY_SIZE as u32 || address != 0 {
                return -1;
            }
            let memory = unsafe { std::slice::from_raw_parts_mut(buffer, length as usize) };
            for (offset, byte) in memory.iter_mut().enumerate() {
                *byte = (offset % 256) as u8;
            }
            0
        }

        unsafe extern "system" fn failed_read(_: usize, _: u32, _: *mut u8, _: u32) -> i32 {
            0x81000000_u32 as i32
        }

        #[test]
        fn reads_exactly_the_full_main_memory() {
            let memory = read_memory(42, successful_read).unwrap();
            assert_eq!(memory.len(), 1024);
            for (offset, byte) in memory.iter().enumerate() {
                assert_eq!(*byte, (offset % 256) as u8);
            }
        }

        #[test]
        fn native_failure_never_returns_zero_filled_or_partial_memory() {
            let error = read_memory(42, failed_read).unwrap_err().to_string();
            assert!(error.contains("read SLE4428 main memory"));
            assert!(error.contains("0x81000000"));
        }

        #[test]
        fn initialization_errors_explain_driver_prerequisites() {
            let error = check("connect to SLE4428", ffi::SCARD_W_UNRESPONSIVE_CARD)
                .unwrap_err()
                .to_string();
            assert!(error.contains("0x80100066"));
            assert!(error.contains("Read Memory tool"));
            assert!(error.contains("voltage settings were not changed"));
        }

        #[test]
        fn write_workflow_stops_at_each_failure_without_retrying() {
            use std::cell::RefCell;
            let request = WriteRequest::new(0x20, vec![0xAA, 0xBB]).unwrap();
            for failure in [
                "protection",
                "protected",
                "pin",
                "write",
                "read",
                "mismatch",
                "none",
            ] {
                let events = RefCell::new(Vec::new());
                let result = write_verified(
                    &request,
                    |address| {
                        events.borrow_mut().push(format!("protect:{address:X}"));
                        if failure == "protection" {
                            return Err(io::Error::other("protection failed"));
                        }
                        Ok(failure == "protected" && address == 0x21)
                    },
                    || {
                        events.borrow_mut().push("pin".to_owned());
                        if failure == "pin" {
                            Err(io::Error::other("wrong PIN"))
                        } else {
                            Ok(())
                        }
                    },
                    || {
                        events.borrow_mut().push("write".to_owned());
                        if failure == "write" {
                            Err(io::Error::other("write failed"))
                        } else {
                            Ok(())
                        }
                    },
                    |buffer| {
                        events.borrow_mut().push("read".to_owned());
                        assert_eq!(buffer.len(), 2);
                        if failure == "read" {
                            return Err(io::Error::other("read failed"));
                        }
                        if failure != "mismatch" {
                            buffer.copy_from_slice(request.data());
                        }
                        Ok(())
                    },
                );
                let expected = match failure {
                    "protection" => vec!["protect:20"],
                    "protected" => vec!["protect:20", "protect:21"],
                    "pin" => vec!["protect:20", "protect:21", "pin"],
                    "write" => vec!["protect:20", "protect:21", "pin", "write"],
                    _ => vec!["protect:20", "protect:21", "pin", "write", "read"],
                };
                assert_eq!(*events.borrow(), expected, "{failure}");
                assert_eq!(result.is_ok(), failure == "none", "{failure}");
                if failure == "mismatch" {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("partially modified")
                    );
                }
            }
        }

        thread_local! {
            static NATIVE_EVENTS: std::cell::RefCell<Vec<&'static str>> = const {
                std::cell::RefCell::new(Vec::new())
            };
        }

        unsafe extern "system" fn mock_protected(
            card: usize,
            address: u32,
            result: *mut i32,
        ) -> i32 {
            if card != 42 || !(0x20..0x22).contains(&address) {
                return -1;
            }
            NATIVE_EVENTS.with(|events| events.borrow_mut().push("protect"));
            unsafe {
                *result = 0;
            }
            0
        }

        unsafe extern "system" fn mock_present(card: usize, length: u32, pin: *mut u8) -> i32 {
            if card != 42 || length != 2 {
                return -1;
            }
            if unsafe { std::slice::from_raw_parts(pin, 2) } != [0x12, 0x34] {
                return -1;
            }
            NATIVE_EVENTS.with(|events| events.borrow_mut().push("pin"));
            0
        }

        unsafe extern "system" fn mock_write(
            card: usize,
            length: u32,
            data: *mut u8,
            address: u32,
            protect: i32,
        ) -> i32 {
            if card != 42 || length != 2 || address != 0x20 || protect != 0 {
                return -1;
            }
            if unsafe { std::slice::from_raw_parts(data, 2) } != [0xAA, 0xBB] {
                return -1;
            }
            NATIVE_EVENTS.with(|events| events.borrow_mut().push("write"));
            0
        }

        unsafe extern "system" fn mock_read_back(
            card: usize,
            length: u32,
            data: *mut u8,
            address: u32,
        ) -> i32 {
            if card != 42 || length != 2 || address != 0x20 {
                return -1;
            }
            unsafe { std::slice::from_raw_parts_mut(data, 2) }.copy_from_slice(&[0xAA, 0xBB]);
            NATIVE_EVENTS.with(|events| events.borrow_mut().push("read"));
            0
        }

        #[test]
        fn native_write_passes_exact_pin_address_data_and_false_protection() {
            NATIVE_EVENTS.with(|events| events.borrow_mut().clear());
            Api::write_with_functions(
                42,
                &WriteRequest::new(0x20, vec![0xAA, 0xBB]).unwrap(),
                [0x12, 0x34],
                mock_read_back,
                WriteFunctions {
                    present: mock_present,
                    write: mock_write,
                    protected: mock_protected,
                },
            )
            .unwrap();
            NATIVE_EVENTS.with(|events| {
                assert_eq!(
                    *events.borrow(),
                    ["protect", "protect", "pin", "write", "read"]
                )
            });
        }

        #[test]
        fn incorrect_and_blocked_pin_errors_are_explicit() {
            for status in [0x81100001_u32, 0x81100002] {
                let error = check("verify PIN", status as i32).unwrap_err().to_string();
                assert!(error.contains("No write or automatic retry"));
                assert!(error.contains(&format!("0x{status:08X}")));
            }
        }

        #[test]
        fn padded_write_clears_old_fragments_only_inside_the_field() {
            use std::cell::RefCell;
            let memory = RefCell::new([b'X'; 16]);
            let checked = RefCell::new(Vec::new());
            let request = WriteRequest::new(2, b"Hi".to_vec())
                .unwrap()
                .pad_to(8)
                .unwrap();
            write_verified(
                &request,
                |address| {
                    checked.borrow_mut().push(address);
                    Ok(false)
                },
                || Ok(()),
                || {
                    memory.borrow_mut()[2..10].copy_from_slice(request.data());
                    Ok(())
                },
                |buffer| {
                    buffer.copy_from_slice(&memory.borrow()[2..10]);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(*checked.borrow(), (2..10).collect::<Vec<_>>());
            assert_eq!(&memory.borrow()[..2], b"XX");
            assert_eq!(&memory.borrow()[2..10], b"Hi\0\0\0\0\0\0");
            assert_eq!(&memory.borrow()[10..], b"XXXXXX");
        }

        #[test]
        fn replacement_checks_even_the_last_padding_byte_before_pin() {
            let request = WriteRequest::new(0x20, vec![0xAA])
                .unwrap()
                .replace_to_end()
                .unwrap();
            let error = write_verified(
                &request,
                |address| Ok(address == 0x03FC),
                || panic!("PIN must not be presented"),
                || panic!("Write must not start"),
                |_| panic!("Read-back must not start"),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("03FC"));
        }

        #[test]
        fn protected_padding_stops_before_pin_or_write() {
            let request = WriteRequest::new(0x20, vec![0xAA])
                .unwrap()
                .pad_to(2)
                .unwrap();
            let error = write_verified(
                &request,
                |address| Ok(address == 0x21),
                || panic!("PIN must not be presented"),
                || panic!("Write must not start"),
                |_| panic!("Read-back must not start"),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_dll_paths() {
        assert_eq!(
            SynchronousApi::load(Path::new("scardsyn.dll"))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn write_bounds_reserve_all_security_addresses() {
        for (address, length) in [
            (0, 0),
            (0x03FD, 1),
            (0x03FE, 1),
            (0x03FF, 1),
            (0x03FC, 2),
            (0x0400, 1),
            (u32::MAX, 1),
            (0, 1022),
        ] {
            assert_eq!(
                WriteRequest::new(address, vec![0; length])
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        for (address, length) in [(0, 1), (0, WRITE_LIMIT), (0x03FC, 1)] {
            let request = WriteRequest::new(address, vec![0xAA; length]).unwrap();
            assert_eq!(request.address(), address);
            assert_eq!(request.data().len(), length);
        }
    }

    #[test]
    fn replacement_fills_exactly_to_the_security_boundary() {
        let payload = vec![0x41; 20];
        let request = WriteRequest::new(0x20, payload.clone())
            .unwrap()
            .replace_to_end()
            .unwrap();
        assert_eq!(request.data().len(), 989);
        assert_eq!(request.padding_len(), 969);
        assert_eq!(&request.data()[..20], payload.as_slice());
        assert!(request.data()[20..].iter().all(|byte| *byte == 0));
        assert_eq!(
            request.address() as usize + request.data().len(),
            WRITE_LIMIT
        );
        let request = WriteRequest::new(0x03FC, vec![0xAA])
            .unwrap()
            .replace_to_end()
            .unwrap();
        assert_eq!(request.data(), [0xAA]);
        assert_eq!(request.padding_len(), 0);
    }

    #[test]
    fn padding_validates_field_bounds_before_allocation_and_never_truncates() {
        let request = WriteRequest::new(0x20, b"Hi".to_vec()).unwrap();
        assert_eq!(request.padding_len(), 0);
        assert_eq!(request.data(), b"Hi");
        let request = request.pad_to(6).unwrap();
        assert_eq!(request.data(), b"Hi\0\0\0\0");
        assert_eq!(request.padding_len(), 4);
        assert_eq!(
            WriteRequest::new(0x20, b"Hi".to_vec())
                .unwrap()
                .pad_to(2)
                .unwrap()
                .data(),
            b"Hi"
        );
        for length in [0, 1, 990, usize::MAX] {
            assert_eq!(
                WriteRequest::new(0x20, b"Hi".to_vec())
                    .unwrap()
                    .pad_to(length)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        let request = WriteRequest::new(0x20, vec![1])
            .unwrap()
            .pad_to(989)
            .unwrap();
        assert_eq!(request.data().len(), 989);
        assert_eq!(request.padding_len(), 988);
        assert!(request.data()[1..].iter().all(|byte| *byte == 0));
        assert!(
            WriteRequest::new(0x03FC, vec![1])
                .unwrap()
                .pad_to(2)
                .is_err()
        );
    }
}

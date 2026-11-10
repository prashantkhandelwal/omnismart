//! Read-only SLE4428 memory access through HID's Windows Synchronous-API.
//!
//! Requires a compatible HID driver and an explicitly selected, trusted DLL
//! matching the process architecture. No reader configuration, writes, or PIN
//! attempts are performed. EMV connections do not load this API.

use std::{ffi::CStr, io, path::Path};

/// Number of bytes in SLE4428 main memory.
pub const MEMORY_SIZE: usize = 1024;

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
            let mut context = 0;
            // All output pointers refer to live, correctly sized local values.
            check("establish smart-card context", unsafe {
                ffi::SCardEstablishContext(ffi::SCARD_SCOPE_USER, null(), null(), &mut context)
            })?;
            let result = self.read_with_context(context, reader);
            finish_transaction(
                result,
                check("release smart-card context", unsafe {
                    ffi::SCardReleaseContext(context)
                }),
            )
        }

        fn read_with_context(
            &self,
            context: ffi::SCARDCONTEXT,
            reader: &CStr,
        ) -> io::Result<[u8; MEMORY_SIZE]> {
            let mut card = 0;
            let mut protocol = 0;
            // HID's Synchronous-API guide specifies a normal T=0 connection,
            // not SHARE_DIRECT, which would bypass required card initialization.
            check("connect to SLE4428", unsafe {
                ffi::SCardConnect(
                    context,
                    reader.as_ptr(),
                    ffi::SCARD_SHARE_SHARED,
                    ffi::SCARD_PROTOCOL_T0,
                    &mut card,
                    &mut protocol,
                )
            })?;
            let result = self.read_with_card(card);
            finish_transaction(
                result,
                check("disconnect SLE4428", unsafe {
                    ffi::SCardDisconnect(card, ffi::SCARD_LEAVE_CARD)
                }),
            )
        }

        fn read_with_card(&self, card: ffi::SCARDHANDLE) -> io::Result<[u8; MEMORY_SIZE]> {
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
                read_memory(card, self.read_data)
            })();
            finish_transaction(
                result,
                check("end SLE4428 transaction", unsafe {
                    ffi::SCardEndTransaction(card, ffi::SCARD_LEAVE_CARD)
                }),
            )
        }
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
}

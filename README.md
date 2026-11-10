# OmniSmart

A Rust PC/SC library and CLI for the HID OMNIKEY 3021 contact smart-card reader.
The CLI lists connected readers, connects to the first OMNIKEY 3021, and prints
the card protocol and ATR (Answer To Reset). The library also allows connecting
to an explicitly selected PC/SC reader.
An independent, opt-in Windows mode reads SLE4428 memory through HID's
Synchronous-API; existing ATR and EMV modes do not require or load that API.

## Using the Rust library

The package builds both the `omnismart` library and the existing CLI. To use
the local checkout from another Rust project, add a path dependency to that
project's `Cargo.toml` (adjust the path):

```toml
[dependencies]
omnismart = { path = 'F:\Github\omnismart' }
```

Read the ATR without sending application commands:

```rust,no_run
use omnismart::ReaderContext;

fn main() -> std::io::Result<()> {
    let context = ReaderContext::establish()?;
    let card = context.connect_preferred()?;
    let status = card.status()?;
    println!("Reader: {}", status.reader);
    println!("Protocol: {:?}", status.protocol);
    println!("ATR: {:02X?}", status.atr);
    Ok(())
}
```

Public API:

- `ReaderContext::readers` lists native PC/SC reader names.
- `select_reader` implements the CLI's OMNIKEY preference/fallback policy.
- `ReaderContext::connect` accepts an explicit reader name, including non-OMNIKEY
  readers; `connect_preferred` discovers and selects an OMNIKEY automatically.
- `CardConnection::status` returns owned `CardStatus` metadata.
- `sle4428::SynchronousApi::load` loads an explicitly selected HID DLL on Windows.
  Its `read` method accepts a PC/SC reader name and returns all 1024 main-memory
  bytes without printing, writing, PIN attempts, or reader configuration changes.
  Its explicit `write` method accepts a validated `sle4428::WriteRequest` and
  a 2-byte PIN, checks protection, writes without setting protection bits, and
  verifies by reading back. Library callers must obtain their own confirmation.
- `CardConnection::with_transaction` supplies a raw APDU transport closure,
  ends exclusive access with `LeaveCard`, and preserves operation/cleanup errors.
- `emv::parse_aid`, `read_directory`, `read_information`, and `read_dump` expose
  the existing EMV modes. These helpers print progress; directory mode prints
  sensitive raw responses, while information mode prints its masked table.
- `emv::read_information_with_pan` explicitly opts in to full-PAN table output.
  Existing decoding, structured-information, and rendering APIs remain masked.
- `emv::read_pan` explicitly returns an unmasked `Option<String>` for a
  documented AID without console output. `None` means tag `5A` was not provided;
  card/transport/decoding failures remain errors.
- `emv::read_pan_auto` discovers AIDs from the payment directory and returns
  the first available PAN without printing. It does not guess application IDs.
- `emv::read_application_information` returns a `CardInformation` struct after
  the caller has selected an application and supplied the SELECT TLV payload.
  It sends GPO and reads advertised standard records, and reports skipped
  proprietary SFIs to stdout. It does not itself select an application.
- `CardInformation::from_tlv` decodes existing EMV BER-TLV bytes without hardware
  access or console output. Fields are public, optional, and contain no full
  PAN or Track 2 data. `render_information` writes the table to a chosen writer.
- `dump::Dump` records opted-in raw exchanges to a new plaintext JSON file.
  Call `finish` explicitly on both success and read failure to persist it.

For example, the existing directory reader can run inside a library-managed
transaction:

```rust,no_run
use omnismart::{ReaderContext, emv};

fn main() -> std::io::Result<()> {
    let context = ReaderContext::establish()?;
    let mut card = context.connect_preferred()?;
    card.with_transaction(|transmit| emv::read_directory(transmit, None))
}
```

The discovery/connection/status/transaction APIs do not print. Errors are
returned as `std::io::Error`; the CLI remains responsible for displaying them
and setting its exit code. The raw transaction transport does not filter APDUs,
so callers must ensure their own commands are authorized and appropriate.

**Card compatibility is unchanged:** a library refactor does not add support
for proprietary RC protocols or make a card that rejects SELECT readable.
The supplied EMV helpers are for payment applications, not vehicle-registration
files. GPO may affect card state/counters; no EMV helper sends PIN verification,
writes, or payment authorization commands. Names remain personal information,
and dumps/raw responses can contain full account data. Keep them private.

Build local API documentation with `cargo doc --no-deps`. No publishing to a
package registry is required to use the path dependency.

### Print only the PAN in Rust

The runnable [print_pan example](examples/print_pan.rs) discovers the AID
automatically when no argument is supplied:

```powershell
cargo run --quiet --example print_pan
# Optional: use your own card's documented application ID directly:
cargo run --quiet --example print_pan -- YOUR_DOCUMENTED_AID_HEX
```

Automatic mode uses `emv::read_pan_auto` to discover applications from the
contact payment directory, falling back to the contactless directory only
if the first is absent. It returns the first PAN provided by the advertised
applications; errors stop reading. If no directory is available, supply your
card's documented 5-to-16-byte AID to use `emv::read_pan` instead. Do not guess
identifiers. An AID is still selected internally even when it is not an argument.
The example prints only the PAN to stdout; its privacy warning and errors go
to stderr. It fails explicitly if no PAN is available. It sends GPO and reads
advertised standard records, with the same restrictions described for `--info`.
It does not read vehicle-registration files or infer PANs from Track 2.

## Desktop GUI (egui)

The optional native GUI reads and writes **SLE4428 memory cards** and reads
**EMV application information**, using the same library safeguards as the CLI.
Choose the card mode at the top. EMV mode is read-only and hides all memory-write
and PIN controls.

```powershell
cargo run --features gui --bin omnismart-gui
```

For a standalone executable:

```powershell
cargo build --release --features gui --bin omnismart-gui
.\target\release\omnismart-gui.exe
```

For SLE4428, Windows, a compatible reader driver, and HID's Synchronous-API DLL are still
required (see the SLE4428 prerequisites below). The GUI suggests the Windows
system-directory DLL path; change it if your trusted HID installation differs.
The DLL must match the executable architecture.
EMV mode uses standard PC/SC access and does not load or require the HID DLL.

All card inputs and selections are grouped at the top. The hex and readable
string panels fill the area below; explanatory prose is omitted from the main
window. Field labels, status/errors, and destructive-write confirmation remain.

### SLE4428 mode

1. Select **SLE4428**, click **Refresh readers**, then select the intended reader.
2. Check the trusted DLL path and click **Read memory** to display all 1024
   bytes as hex in **Read snapshot**. The **Readable string** panel to its right
   displays UTF-8 text for an explicit hex address and decimal byte length.
   Both panels scroll independently; invalid UTF-8 is reported, not silently
   replaced.
3. For writing, enter the hex address and choose **UTF-8 text** or **Hex bytes**.
   The GUI shows the encoded byte count and available space. At address `0020`,
   at most 989 bytes fit; multibyte UTF-8 characters use more than one byte.
   To replace a fixed field and remove old fragments, enter its total size in
   **Field length (bytes)** (decimal, optional). For example, `40` writes your
   data followed by `00` bytes until all 40 bytes have been overwritten.
   If you instead want to remove **all** trailing data regardless of its old
   length, check **Replace through 03FC**. This option
   disables the fixed field length and can erase unrelated fields later on the
   card. It is off by default.
4. Enter the card's correct two-byte PIN in the masked field, then click
   **Review write...**. Check the reader, DLL, range, and byte count. Type
   `WRITE` and click **Write and verify**, or **Cancel** without a PIN attempt.

Only unprotected bytes within `0000..03FC` can be written. Security addresses,
PIN changes, and permanent-protection operations are not exposed. A write
checks protection, makes one PIN attempt without retries, writes, reads back
to verify, and resets the card to clear verification state. It may fail after
partially changing memory; do not blindly retry.

Text is stored as raw UTF-8 bytes with no automatic length header or terminator.
With a blank field length and Replace unchecked, shorter writes leave trailing old bytes unchanged.
With a field length, unused bytes are explicitly zero-padded. The GUI cannot
infer the previous string's length: specify the full existing field size.
Use the actual encoded text byte count when previewing text; padding is NUL
bytes, not printable spaces.

Reader discovery and card I/O run off the UI thread, one operation at a time.
The GUI prevents normal window closure while I/O is pending. Do not remove the
card, disconnect the reader, or force-terminate the app during a write. Read
snapshots are cleared when the card mode, reader, DLL, or EMV AID changes, or
another operation starts.

### EMV mode

Select **EMV (read-only)**, click **Refresh readers**, select your reader, and
click **Read EMV**. Leave **AID (hex, optional)** blank to discover advertised
applications, or supply your card's documented 5-to-16-byte AID without spaces.
The information panel displays each application's AID, network, label, masked
card number, dates, and other available standard fields. Missing fields are
shown as not provided; full PAN, Track 2, and raw APDU responses are not displayed.

This mode sends SELECT, GET PROCESSING OPTIONS (GPO), and advertised standard
READ RECORD commands. GPO may affect card counters; no PIN verification,
payment authorization, or card-write commands are sent. Applications requiring
nonempty PDOL terminal data are not supported and produce an explicit error.
Nothing is read automatically when switching modes. Keep the display private:
names and other personal data may still be exposed by the card.

**Privacy:** Keep the window private: memory, text, and hex data are visible.
The application does not save PINs/card data, persist form state, or print raw
card responses to a console. Closing it discards in-memory state, not data
already written to the card. Back up your card before writing.

The GUI dependency is opt-in. Existing `cargo run -- ...` commands still select
the original CLI, and ordinary CLI builds do not compile egui.

## Run on Windows

1. Install the stable Rust toolchain using rustup from https://rustup.rs/.
   If prompted, install the Visual Studio C++ build tools required by Rust's
   Windows MSVC toolchain. Open a new terminal after installation.
2. Plug in the reader. Check that it appears under **Smart card readers** in
   Device Manager. Install the appropriate HID driver if Windows does not
   recognize it.
3. Insert a compatible contact smart card. The 3021 does not read contactless
   NFC/RFID cards.
4. Check the Windows smart-card service in PowerShell:

   ```powershell
   Get-Service SCardSvr
   ```

   If it is stopped, run this in an elevated PowerShell:

   ```powershell
   Start-Service SCardSvr
   ```

5. Run the application from PowerShell:

   ```powershell
   Set-Location G:\projects\omnismart
   cargo run
   ```

   Cargo downloads the `pcsc` dependency and builds the program on the first
   run. On Windows, PC/SC uses the built-in WinSCard library.

Example output (reader name, protocol, and ATR depend on the hardware and card):

```text
Reader: HID Global OMNIKEY 3021 Smart Card Reader 0
Connecting through: HID Global OMNIKEY 3021 Smart Card Reader 0
Protocol: Some(T1)
ATR: 3B ...
Connected successfully. The ATR is card initialization data, not stored application data.
Use cargo run -- --emv to read raw EMV directory and selection responses.
```

The program runs once; insert the card before running it. It prefers reader
names containing both `OMNIKEY` and `3021` (case-insensitively), using the first
such match. Some Windows drivers report only a generic name such as
`OMNIKEY AG Smart Card Reader USB 0`. If no explicit 3021 match exists, the
program accepts a single OMNIKEY reader, even if its model is not identified.
This name-based fallback does not verify the hardware model. If multiple
OMNIKEY readers exist without an explicit 3021 match, disconnect the other
readers and run again. Errors are printed to stderr and produce a nonzero
exit code.

## Reading stored data

An ATR is not a card's stored application data or necessarily a unique card ID.
There is no universal "read all data" APDU.

### SLE4428 memory cards (Windows, OMNIKEY 3021)

SLE4428 is a three-wire synchronous memory card, not an EMV payment card.
Use its dedicated mode rather than `--info`, `--emv`, or `--dump`:

```powershell
# Replace this example path with your trusted HID Synchronous-API installation.
cargo run -- --sle4428 --sync-api "C:\Windows\System32\scardsynx64.dll"
```

Prerequisites:

- Install the model-compatible HID OMNIKEY driver with synchronous-card support.
  The generic Microsoft CCID driver may not provide the required support.
- Obtain HID's [Synchronous-API package](https://www.hidglobal.com/drivers/29915).
  Supply an **absolute path** to its authentic `scardsyn.dll` or `scardsynx64.dll`. Loading a DLL
  executes native code: do not use untrusted downloads.
- Match the DLL architecture to the executable (normally x64 on 64-bit Windows).
  A 32-bit DLL requires a 32-bit executable; it cannot be loaded by an x64 build.
- Insert your own SLE4428 with the contacts correctly oriented.

The mode uses HID's `SCard3WBPReadData` to read main memory, addresses
`0000` through `03FF`, and prints 64 rows of 16 hexadecimal bytes. It checks
the driver's three-wire ATR type first; this identifies a compatible family,
not an authenticated chip identity (SLE4418 is also compatible). Native read
failures return an error instead of printing partial or zero-filled memory.
This mode does not decode application fields, read protection bits, attempt
PIN verification, write memory, or change reader settings. Raw memory can
contain sensitive data: keep terminal output private.

**If you still see "The smart card is not responding to a reset":** the
driver must initialize the card before the synchronous API can read it.
Loading the DLL does not bypass this prerequisite. Check orientation and
contacts, install/check the HID driver, and test with the SDK's **Read Memory**
tool. HID's [Synchronous-API guide](https://www.hidglobal.com/documents/omnikey-synchronous-api-software-developer-guide)
also discusses voltage-selection issues for AVR-based readers. This program
deliberately does **not** apply those configuration changes automatically,
so existing reader configuration and EMV behavior are preserved.

SLE4428 mode cannot be combined with EMV options. No automatic fallback or
card-type guessing is performed. Existing commands (`cargo run`, `--info`,
`--emv`, `--aid`, `--show-pan`, and `--dump`) retain their behavior and do not
require the HID DLL. The SLE4428 backend is Windows-only; other platforms
return an explicit unsupported-platform error.

#### Explicit SLE4428 writing

Only write to your own card, after backing up its memory and confirming its
application layout. Without `--write`, the SLE4428 mode remains read-only.
Writing is separate from all EMV modes and requires an interactive terminal:

```powershell
# Example ONLY: overwrite two bytes at hex address 0020 with AA BB.
# Choose an address and data appropriate for your own card.
cargo run -- --sle4428 --sync-api "C:\Windows\System32\scardsynx64.dll" --write 0020 "AA BB"
```

Address and data are hexadecimal; the address optionally accepts a `0x` prefix.
Data must contain complete byte pairs, with optional ASCII whitespace.
To remove fragments of an older, longer value, supply **`--field-length N`**
after the data, where `N` is the total field size in **decimal bytes**:

```powershell
# Write "Hi" (UTF-8 hex 48 69) and clear the remaining 38 bytes of a 40-byte field.
cargo run -- --sle4428 --sync-api "C:\Windows\System32\scardsynx64.dll" --write 0020 "48 69" --field-length 40
```

The payload is followed by `00` bytes to fill the field. Payloads larger than
the field, zero-length fields, and fields crossing `03FD` are rejected before
card access. No truncation or automatic old-length detection is performed.
Omit the option for the original raw-write behavior (trailing bytes unchanged).
The library provides the same behavior through `WriteRequest::pad_to`.

To replace a value without knowing the old length, use **`--replace`** instead:

```powershell
# Write "Hi", then zero-fill every remaining writable byte through 03FC.
cargo run -- --sle4428 --sync-api "C:\Windows\System32\scardsynx64.dll" --write 0020 "48 69" --replace
```

Starting at `0020`, this overwrites **989 bytes**: two payload bytes and 987
zero bytes in this example. All bytes before `0020` and security addresses
`03FD..03FF` remain untouched. This removes old fragments regardless of length
within that range, but **also erases any unrelated data after the payload**.
Every byte in the range must be unprotected, even bytes being cleared; a
protected tail stops the operation before PIN verification or writing.
`--replace` and `--field-length` are mutually exclusive and require `--write`.
The library equivalent is `WriteRequest::replace_to_end`.

The CLI displays the reader, address range, and byte count, then requires you
to type `WRITE`. Any other response cancels without a PIN attempt or write.
It then prompts for the card's **2-byte PSC/PIN** with input hidden. There is
no default PIN, PIN argument, environment-variable PIN, `--yes`, or piped-input
mode. Do not guess security codes; incorrect verification consumes limited
attempts and can permanently block writes.

The write path:

1. Connects exclusively and validates the three-wire ATR.
2. Checks every target byte using `SCard3WBPVerifyProtectBit`. A protected
   byte or failed protection check stops before any PIN attempt or write.
3. Calls `SCard3WBPPresentPIN` once with your supplied PIN; no automatic retry.
4. Calls `SCard3WBPWriteData` once with `fProtect = FALSE`.
5. Calls `SCard3WBPReadData` for the written range and compares every byte.
6. Disconnects with a card reset to clear verification state, on success or
   failure. Read-only mode still leaves the card powered as before.

When a field length or replacement is supplied, all these checks and the read-back comparison
cover the **entire padded field**, not just the new payload. Confirmation
reports the full overwritten range and padding count. A protected byte in the
padding region stops the operation before any PIN attempt or write.
Data outside the specified field is not touched. Back up first: choosing an
overly large field can erase unrelated data.

Only nonempty writes entirely within **0000..03FC** are accepted.
**03FD..03FF are reserved for security state and blocked**, including requests
crossing into that range. No PIN-changing or permanent-protection API is
exposed. Existing permanently protected bytes cannot be overwritten.

Writes are **not atomic**: a native error, disconnect, or verification failure
may leave partially changed memory. There is no automatic retry or rollback.
Cleanup errors are reported even when the data write succeeded. If anything
fails, inspect the card before considering another write; do not blindly retry.

The CLI does not print the PIN, data bytes, or raw read-back. However, **hex
data passed as arguments can appear in shell history and process listings**.
For sensitive payloads, use `SynchronousApi::write` from your own application
instead of command-line data. That library method does not prompt: the caller
is responsible for authorization, confirmation, and secure handling.

### EMV payment cards

For your own EMV bank card, opt in to printing raw directory records and
application-selection responses:

```powershell
cargo run -- --emv
```

**Privacy warning:** Raw bank-card responses may contain sensitive information.
Do not upload or share terminal output, screenshots, or logs. The program does
not create output files or send card data over the network.

This mode selects `1PAY.SYS.DDF01` (contact PSE), falling back to
`2PAY.SYS.DDF01` only if the first directory is absent. It reads advertised
directory records, discovers application IDs (AIDs), and selects those
applications. `C-APDU` is the outgoing command; `R-APDU` is the card's raw
response, including the final two status bytes. `90 00` means success;
`6A 82` means file/application not found; `6A 83` ends directory records.
The program handles `61 xx` GET RESPONSE and `6C xx` length correction.

Some contact cards have no payment directory. If you know your card's
documented 5-to-16-byte AID, select it directly (replace the placeholder):

```powershell
cargo run -- --emv --aid YOUR_APPLICATION_ID_AS_HEX
```

Use `cargo run -- --help` for usage. Without flags, only the ATR is read.

The `--emv` mode is **not a full memory dump or an account-record reader**.
It reads only directory records and application-selection data; account numbers
or expiry dates are not guaranteed to appear. It does not send GET PROCESSING
OPTIONS.

## Cardholder name and general information

With your own card inserted:

```powershell
cargo run -- --info
```

This mode renders an **Information recovered** table using
`CardInformation::render_information`. It includes the network (where
recognized), application label, cardholder name, masked PAN (last four digits
only), expiry/effective dates, issuer country, application currency and decimal
places, preferred language, priority, PAN sequence number, service code, and
application version. Missing fields are explicitly marked as not provided.

Each discovered application gets its own table with an **Application AID** row.
Different applications on the same card may expose identical card details but
different priorities; both tables are retained and identified by their AIDs.
This also applies with `--show-pan`. To read only one application, use
`--info --aid YOUR_APPLICATION_ID_AS_HEX`.

Dates are displayed as `20YY-MM-DD` after validating the EMV `YYMMDD` calendar
date; the chip does not provide a century. Application versions remain hex.
India/INR and English have friendly labels; other country/currency/language
values remain their original codes. A PAN is decoded only from tag `5A`, not
inferred from Track 2 data. The program does not guess a missing name or infer
an issuing bank from the card network. Names may use the issuer's
`SURNAME/FIRSTNAME` formatting.

Unlike `--emv`, `--info` does not print raw APDU data or Track 2 contents.
Full PANs are masked by default. To explicitly display the full number exposed
by your own payment card:

```powershell
cargo run -- --info --show-pan
# With a documented application ID:
cargo run -- --info --aid YOUR_APPLICATION_ID_AS_HEX --show-pan
```

`--show-pan` is accepted only with `--info`; it may appear before or after
`--aid HEX`. It prints a warning and labels the table field **Card number (full)**.
Only tag `5A` is used; no PAN is inferred from Track 2 or reconstructed from a
masked number. Missing PANs are marked **Not provided**. The flag sends no
additional card commands, bypasses no access controls, and does not affect raw
dump behavior. It does not add vehicle-registration-card support.

**Sensitive output:** Do not share, log, commit, or redirect full-PAN output to
an insecure location. Omit the flag to retain normal masking.

The name itself is personal information: do not share output without reviewing
it. Application records can contain account data; the tool reads them locally
in memory and displays only the general-information fields listed above.

To locate the name, this mode sends GET PROCESSING OPTIONS (GPO), then reads
standard EMV records at SFI 1-10 advertised in the returned Application File
Locator (AFL). Proprietary records at SFI 11-30 are explicitly skipped.
Both GPO response formats (`80` and `77`) are supported.

GPO initializes EMV processing and may affect issuer-specific card state or
counters, but this tool does not authorize or complete a payment. No PIN
verification, writes, or GENERATE AC commands are sent in any EMV mode.
No mode retrieves a PIN, protected private keys, or printed CVV2.

Name reading currently supports only applications with an absent or empty
Processing Options Data Object List (PDOL). If terminal data is required, it
fails explicitly instead of inventing transaction/terminal values. Cards may
also reject GPO or omit the name; successful connection does not guarantee
that a name is readable.

If the payment directory is absent, a documented AID can also be used with
`cargo run -- --info --aid YOUR_APPLICATION_ID_AS_HEX`.

## Raw readable-EMV JSON dump

This is a best-effort dump of data available through the implemented EMV
commands, **not a full card-memory dump**. Run with your own card inserted:

```powershell
Set-Location G:\projects\omnismart
$dumpPath = Join-Path $env:LOCALAPPDATA ("omnismart-card-dump-" + (Get-Date -Format "yyyyMMdd-HHmmss") + ".json")
cargo run -- --dump "$dumpPath"
```

The output must be a **new file** in an existing directory. Existing files are
never overwritten. Keep dumps outside the repository and cloud-synced folders.
The repository also ignores root-level `card-dump*.json` as a precaution, not
as a substitute for choosing a private location.

**Sensitive data:** Dumps may contain PANs, expiry dates, names, and Track 2
data. They are plaintext and inherit the destination directory's access
permissions. Do not upload, commit, or share them. This mode does not print
raw account responses to the terminal or send data over the network.

The JSON includes:

- Reader name, protocol, ATR, creation time (Unix seconds), and format version.
- Every command and response as uppercase hex, including status bytes,
  GET RESPONSE continuations, and corrected-length retries.
- Directory discovery/records, application selections, GPO responses, and
  records at SFI 1-30 advertised in each selected application's AFL.
- Transport errors, an overall `error`, and a `complete` flag.

`complete: true` means the implemented discovery and advertised-record reads
finished without errors. It does **not** mean all memory or all accessible card
objects were retrieved. Proprietary records are preserved as raw bytes without
assuming they contain standard TLV data. The tool does not scan unadvertised
files, recurse into other directory types, read optional GET DATA objects or
transaction logs, or bypass access controls.

The same absent/empty-PDOL restriction as `--info` applies. If required,
specify a documented application directly:

```powershell
cargo run -- --dump "$dumpPath" --aid YOUR_APPLICATION_ID_AS_HEX
```

If a record returns a failure status, other advertised records are still
attempted. Application failures do not prevent attempts on other discovered
applications. Transport failures stop the current application's reads.
The tool saves the exchanges already collected with `complete: false` and an
error, then exits unsuccessfully. Directory-discovery failures stop discovery.
The existing 32-exchange continuation limit, 255 directory-record limit, and
512 distinct AFL-record limit per application still apply.

JSON is written and flushed at the end of a normal success/error path. Forced
termination, a crash, or a disk-write failure can leave an empty/invalid file.
Connection/status failures before dump creation do not create a dump.

## Development checks

```powershell
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
# Include the optional GUI and its headless tests:
cargo test --features gui
cargo clippy --features gui --all-targets -- -D warnings
```

Unit tests cover reader selection, AID validation, TLV parsing, directory
discovery, APDU response handling, raw JSON persistence, incomplete dumps, and
overwrite protection with synthetic data. Verifying a real card connection
requires the reader and a compatible inserted card.
SLE4428 tests cover mode isolation, DLL-path validation, ATR-family checks,
native read size/address, and explicit native errors using a synthetic API
function. Real SLE4428 reset/driver/DLL compatibility and memory contents
must still be verified with the reader, vendor software, and card.
Synthetic write tests cover range/security-address rejection, protection and
PIN failures, single-attempt ordering, native function arguments, disabled
permanent protection, read-back mismatch/errors, confirmation parsing,
noninteractive rejection, and separation from EMV. Tests never write to a
physical card or present a PIN to one.
GUI tests use synthetic data for UTF-8/hex payloads, byte limits, text-preview
ranges, frozen write confirmation, worker failures, and headless rendering.
They do not access a physical reader or write to a card.
Fixed-field overwrite tests verify zero-padding, old-fragment removal, unchanged
neighboring bytes, protection checks on padding, UTF-8 byte counts, security
boundaries, and unchanged behavior when field length is omitted.
Replacement tests additionally cover clearing exactly through `03FC`, protection
checks at the end of the range, opt-in GUI behavior, and mutually exclusive CLI
options. No hardware writes are used.

API documentation: https://docs.rs/pcsc/latest/pcsc/

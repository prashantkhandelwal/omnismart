# OmniSmart

A Rust PC/SC example for the HID OMNIKEY 3021 contact smart-card reader.
It lists connected readers, connects to the first OMNIKEY 3021, and prints
the card protocol and ATR (Answer To Reset).

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

Dates are displayed as `20YY-MM-DD` after validating the EMV `YYMMDD` calendar
date; the chip does not provide a century. Application versions remain hex.
India/INR and English have friendly labels; other country/currency/language
values remain their original codes. A PAN is decoded only from tag `5A`, not
inferred from Track 2 data. The program does not guess a missing name or infer
an issuing bank from the card network. Names may use the issuer's
`SURNAME/FIRSTNAME` formatting.

Unlike `--emv`, `--info` does not print raw APDU data, full PANs, or Track 2 contents.
The name itself is personal information: do not share output without reviewing
it. Application records can contain account data; the tool reads them locally
in memory and displays only the general-information fields listed above.

To locate the name, this mode sends GET PROCESSING OPTIONS (GPO), then reads
standard EMV records at SFI 1-10 advertised in the returned Application File
Locator (AFL). Proprietary records at SFI 11-30 are explicitly skipped.
Both GPO response formats (`80` and `77`) are supported.

GPO initializes EMV processing and may affect issuer-specific card state or
counters, but this tool does not authorize or complete a payment. No PIN
verification, writes, or GENERATE AC commands are sent in any mode.
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
```

Unit tests cover reader selection, AID validation, TLV parsing, directory
discovery, APDU response handling, raw JSON persistence, incomplete dumps, and
overwrite protection with synthetic data. Verifying a real card connection
requires the reader and a compatible inserted card.

API documentation: https://docs.rs/pcsc/latest/pcsc/

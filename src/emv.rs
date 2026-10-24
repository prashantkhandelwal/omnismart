use std::io::{self, Write};

pub fn parse_aid(value: &str) -> io::Result<Vec<u8>> {
    if !(10..=32).contains(&value.len())
        || !value.len().is_multiple_of(2)
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "An AID must contain 5 to 16 bytes as hex, without spaces (10 to 32 hex digits).",
        ));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
        })
        .collect()
}

struct Response {
    data: Vec<u8>,
    status: [u8; 2],
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn print_hex(label: &str, bytes: &[u8]) {
    print!("{label}");
    for byte in bytes {
        print!(" {byte:02X}");
    }
    println!();
}

fn exchange(
    transmit: &mut impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
    command: &[u8],
) -> io::Result<Response> {
    let mut command = command.to_vec();
    let mut data = Vec::new();
    for _ in 0..32 {
        let response = transmit(&command)?;
        if response.len() < 2 {
            return Err(invalid("APDU response is missing SW1/SW2."));
        }
        let payload_len = response.len() - 2;
        let status = [response[payload_len], response[payload_len + 1]];
        match status {
            [0x6C, length] => {
                // Every command in this module is a short APDU with a trailing Le.
                let last = command
                    .last_mut()
                    .ok_or_else(|| invalid("Cannot correct Le in an empty command."))?;
                *last = length;
            }
            [0x61, length] => {
                data.extend_from_slice(&response[..payload_len]);
                command = vec![0x00, 0xC0, 0x00, 0x00, length];
            }
            _ => {
                data.extend_from_slice(&response[..payload_len]);
                return Ok(Response { data, status });
            }
        }
    }
    Err(invalid("APDU continuation/retry limit exceeded."))
}

fn select(
    transmit: &mut impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
    name: &[u8],
) -> io::Result<Response> {
    let length = u8::try_from(name.len()).map_err(|_| invalid("SELECT name is too long."))?;
    let mut command = vec![0x00, 0xA4, 0x04, 0x00, length];
    command.extend_from_slice(name);
    command.push(0x00);
    exchange(transmit, &command)
}

fn require_success(response: &Response, operation: &str) -> io::Result<()> {
    if response.status != [0x90, 0x00] {
        return Err(io::Error::other(format!(
            "{operation} failed: SW1/SW2 = {:02X} {:02X}.",
            response.status[0], response.status[1]
        )));
    }
    Ok(())
}

fn find_tags(data: &[u8], wanted: u32) -> io::Result<Vec<&[u8]>> {
    fn walk<'a>(
        mut data: &'a [u8],
        wanted: u32,
        depth: usize,
        values: &mut Vec<&'a [u8]>,
    ) -> io::Result<()> {
        if depth > 16 {
            return Err(invalid("EMV TLV nesting limit exceeded."));
        }
        while !data.is_empty() {
            let first = data[0];
            data = &data[1..];
            if first == 0x00 || first == 0xFF {
                continue;
            }
            let mut tag = u32::from(first);
            if first & 0x1F == 0x1F {
                let mut complete = false;
                for _ in 0..3 {
                    let (&next, rest) = data
                        .split_first()
                        .ok_or_else(|| invalid("Truncated EMV tag."))?;
                    data = rest;
                    tag = (tag << 8) | u32::from(next);
                    if next & 0x80 == 0 {
                        complete = true;
                        break;
                    }
                }
                if !complete {
                    return Err(invalid("EMV tag is too long."));
                }
            }
            let (&first_length, rest) = data
                .split_first()
                .ok_or_else(|| invalid("Missing EMV TLV length."))?;
            data = rest;
            let length = if first_length & 0x80 == 0 {
                usize::from(first_length)
            } else {
                let count = usize::from(first_length & 0x7F);
                if count == 0 || count > 2 || data.len() < count {
                    return Err(invalid("Unsupported or truncated EMV TLV length."));
                }
                let length = data[..count]
                    .iter()
                    .fold(0usize, |length, byte| (length << 8) | usize::from(*byte));
                data = &data[count..];
                length
            };
            if data.len() < length {
                return Err(invalid("Truncated EMV TLV value."));
            }
            let (value, rest) = data.split_at(length);
            data = rest;
            if tag == wanted {
                values.push(value);
            }
            if first & 0x20 != 0 {
                walk(value, wanted, depth + 1, values)?;
            }
        }
        Ok(())
    }
    let mut values = Vec::new();
    walk(data, wanted, 0, &mut values)?;
    Ok(values)
}

fn add_aids(data: &[u8], aids: &mut Vec<Vec<u8>>) -> io::Result<()> {
    for aid in find_tags(data, 0x4F)? {
        if !(5..=16).contains(&aid.len()) {
            return Err(invalid(
                "Directory contains an invalid application ID length.",
            ));
        }
        if !aids.iter().any(|existing| existing == aid) {
            aids.push(aid.to_vec());
        }
    }
    Ok(())
}

pub fn read_directory(
    mut transmit: impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
    explicit_aid: Option<&[u8]>,
) -> io::Result<()> {
    read_applications(
        |command| {
            print_hex("C-APDU:", command);
            let response = transmit(command)?;
            print_hex("R-APDU:", &response);
            Ok(response)
        },
        explicit_aid,
        ReadMode::Directory,
    )
}

pub fn read_information(
    transmit: impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
    explicit_aid: Option<&[u8]>,
) -> io::Result<()> {
    read_applications(transmit, explicit_aid, ReadMode::Information)
}

pub fn read_dump(
    transmit: impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
    explicit_aid: Option<&[u8]>,
) -> io::Result<()> {
    read_applications(transmit, explicit_aid, ReadMode::Dump)
}

#[derive(Clone, Copy, PartialEq)]
enum ReadMode {
    Directory,
    Information,
    Dump,
}

fn read_applications(
    mut transmit: impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
    explicit_aid: Option<&[u8]>,
    mode: ReadMode,
) -> io::Result<()> {
    let mut aids = Vec::new();
    if let Some(aid) = explicit_aid {
        aids.push(aid.to_vec());
    } else {
        println!("Selecting contact payment directory (1PAY.SYS.DDF01).");
        let mut directory = select(&mut transmit, b"1PAY.SYS.DDF01")?;
        if directory.status == [0x6A, 0x82] {
            println!("Contact directory absent; trying 2PAY.SYS.DDF01.");
            directory = select(&mut transmit, b"2PAY.SYS.DDF01")?;
        }
        if directory.status == [0x6A, 0x82] {
            return Err(io::Error::other(
                "Neither payment directory is available. If you know the card's documented \
                 application ID, use --aid HEX with your chosen reading mode.",
            ));
        }
        require_success(&directory, "Directory SELECT")?;
        add_aids(&directory.data, &mut aids)?;
        let sfis = find_tags(&directory.data, 0x88)?;
        if let Some(sfi) = sfis.first() {
            if sfi.len() != 1 || !(1..=30).contains(&sfi[0]) {
                return Err(invalid("Invalid directory short file identifier."));
            }
            let mut finished = false;
            for record in 1..=u8::MAX {
                println!("Reading directory record {record} (SFI {}).", sfi[0]);
                let response = exchange(
                    &mut transmit,
                    &[0x00, 0xB2, record, (sfi[0] << 3) | 0x04, 0x00],
                )?;
                if response.status == [0x6A, 0x83] {
                    finished = true;
                    break;
                }
                require_success(&response, "Directory READ RECORD")?;
                add_aids(&response.data, &mut aids)?;
            }
            if !finished {
                return Err(invalid(
                    "Directory record limit reached without an end marker.",
                ));
            }
        }
    }
    if aids.is_empty() {
        return Err(io::Error::other(
            "No application IDs found. Use --aid HEX with a documented AID.",
        ));
    }
    let mut errors = Vec::new();
    for aid in aids {
        print_hex("Selecting application AID:", &aid);
        let result = (|| {
            let response = select(&mut transmit, &aid)?;
            require_success(&response, "Application SELECT")?;
            match mode {
                ReadMode::Information => {
                    let info = read_application_information(&mut transmit, &response.data)?;
                    info.render_information(&aid, &mut io::stdout().lock())?;
                }
                ReadMode::Dump => dump_application_records(&mut transmit, &response.data)?,
                ReadMode::Directory => {}
            }
            Ok::<(), io::Error>(())
        })();
        if let Err(error) = result {
            if mode != ReadMode::Dump {
                return Err(error);
            }
            eprintln!("Application dump incomplete: {error}");
            errors.push(error.to_string());
        }
    }
    if !errors.is_empty() {
        return Err(io::Error::other(format!(
            "Readable EMV dump incomplete: {}",
            errors.join("; ")
        )));
    }
    if mode == ReadMode::Directory {
        println!("Finished reading raw directory records and application-selection responses.");
        println!(
            "This is not a full dump: application account records and protected secrets are not read."
        );
    }
    Ok(())
}

#[derive(Default, Debug, PartialEq)]
struct CardInformation {
    name: Option<String>,
    label: Option<String>,
    language: Option<String>,
    priority: Option<u8>,
    country: Option<String>,
    currency: Option<String>,
    masked_pan: Option<String>,
    expiry: Option<String>,
    effective: Option<String>,
    currency_exponent: Option<String>,
    sequence: Option<String>,
    service_code: Option<String>,
    version: Option<String>,
}

fn text_field(data: &[u8], tag: u32) -> io::Result<Option<String>> {
    find_tags(data, tag)?
        .first()
        .map(|value| {
            if !value.iter().all(|byte| (0x20..=0x7E).contains(byte)) {
                return Err(invalid(&format!(
                    "Unsupported text encoding for tag {tag:X}."
                )));
            }
            let text: String = value.iter().map(|byte| char::from(*byte)).collect();
            let text = text.trim();
            Ok((!text.is_empty()).then(|| text.to_owned()))
        })
        .transpose()
        .map(Option::flatten)
}

fn numeric_code(data: &[u8], tag: u32) -> io::Result<Option<String>> {
    find_tags(data, tag)?
        .first()
        .map(|value| {
            if value.len() != 2
                || value[0] >> 4 != 0
                || value.iter().any(|byte| byte >> 4 > 9 || byte & 0x0F > 9)
            {
                return Err(invalid(&format!(
                    "Invalid numeric country/currency tag {tag:X}."
                )));
            }
            Ok(format!("{:X}{:02X}", value[0] & 0x0F, value[1]))
        })
        .transpose()
}

fn bcd_field(data: &[u8], tag: u32, length: usize) -> io::Result<Option<String>> {
    find_tags(data, tag)?
        .first()
        .map(|value| {
            if value.len() != length || value.iter().any(|byte| byte >> 4 > 9 || byte & 0x0F > 9) {
                return Err(invalid(&format!("Invalid numeric field {tag:X}.")));
            }
            Ok(value.iter().map(|byte| format!("{byte:02X}")).collect())
        })
        .transpose()
}

fn date_field(data: &[u8], tag: u32) -> io::Result<Option<String>> {
    bcd_field(data, tag, 3)?
        .map(|digits| {
            let bytes = digits.as_bytes();
            let number =
                |index| u32::from(bytes[index] - b'0') * 10 + u32::from(bytes[index + 1] - b'0');
            let year = 2000 + number(0);
            let month = number(2);
            let day = number(4);
            let days = match month {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 if year.is_multiple_of(4) => 29,
                2 => 28,
                _ => 0,
            };
            if day == 0 || day > days {
                return Err(invalid(&format!("Invalid calendar date in tag {tag:X}.")));
            }
            Ok(format!("{year:04}-{month:02}-{day:02}"))
        })
        .transpose()
}

fn masked_pan(data: &[u8]) -> io::Result<Option<String>> {
    find_tags(data, 0x5A)?
        .first()
        .map(|value| {
            if value.is_empty() || value.len() > 10 {
                return Err(invalid("Invalid PAN length."));
            }
            let mut digits: String = value.iter().map(|byte| format!("{byte:02X}")).collect();
            if digits.ends_with('F') {
                digits.pop();
            }
            if !(8..=19).contains(&digits.len())
                || !digits.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(invalid("Invalid PAN encoding."));
            }
            let suffix = &digits[digits.len() - 4..];
            Ok(format!("{} {suffix}", "*".repeat(digits.len() - 4)))
        })
        .transpose()
}

impl CardInformation {
    fn update(&mut self, data: &[u8]) -> io::Result<()> {
        for (field, tag) in [
            (&mut self.name, 0x5F20),
            (&mut self.label, 0x50),
            (&mut self.language, 0x5F2D),
        ] {
            if field.is_none() {
                *field = text_field(data, tag)?;
            }
        }
        if self.country.is_none() {
            self.country = numeric_code(data, 0x5F28)?;
        }
        if self.currency.is_none() {
            self.currency = numeric_code(data, 0x9F42)?;
        }
        if self.masked_pan.is_none() {
            self.masked_pan = masked_pan(data)?;
        }
        if self.expiry.is_none() {
            self.expiry = date_field(data, 0x5F24)?;
        }
        if self.effective.is_none() {
            self.effective = date_field(data, 0x5F25)?;
        }
        if self.sequence.is_none() {
            self.sequence = bcd_field(data, 0x5F34, 1)?;
        }
        if self.service_code.is_none() {
            self.service_code = bcd_field(data, 0x5F30, 2)?
                .map(|value| {
                    value
                        .strip_prefix('0')
                        .map(str::to_owned)
                        .ok_or_else(|| invalid("Service code must contain three digits."))
                })
                .transpose()?;
        }
        if self.currency_exponent.is_none() {
            self.currency_exponent = bcd_field(data, 0x9F44, 1)?
                .map(|value| {
                    value
                        .strip_prefix('0')
                        .map(str::to_owned)
                        .ok_or_else(|| invalid("Currency exponent must be one decimal digit."))
                })
                .transpose()?;
        }
        if self.version.is_none()
            && let Some(value) = find_tags(data, 0x9F08)?.first()
        {
            if value.len() != 2 {
                return Err(invalid("Application version must contain two bytes."));
            }
            self.version = Some(format!("{:02X}{:02X} (hex)", value[0], value[1]));
        }
        if let Some(priority) = find_tags(data, 0x87)?.first() {
            if priority.len() != 1 {
                return Err(invalid("Invalid application priority length."));
            }
            self.priority = Some(priority[0] & 0x0F);
        }
        Ok(())
    }

    fn render_information(&self, aid: &[u8], output: &mut impl Write) -> io::Result<()> {
        let network = if aid.starts_with(&[0xA0, 0, 0, 0, 3]) {
            "Visa"
        } else if aid.starts_with(&[0xA0, 0, 0, 0, 4]) {
            "Mastercard"
        } else {
            "Not identified from AID"
        };
        let country = match self.country.as_deref() {
            Some("356") => "India (356)",
            Some(value) => value,
            None => "Not provided",
        };
        let currency = match self.currency.as_deref() {
            Some("356") => "Indian rupee / INR (356)",
            Some(value) => value,
            None => "Not provided",
        };
        let language = match self.language.as_deref() {
            Some("en") => "English (en)",
            Some(value) => value,
            None => "Not provided",
        };
        let priority = match self.priority {
            Some(value @ 1..=15) => value.to_string(),
            _ => "Not specified".to_owned(),
        };
        let value =
            |field: &Option<String>| field.clone().unwrap_or_else(|| "Not provided".to_owned());
        let rows = [
            ("Network", network.to_owned()),
            ("Application label", value(&self.label)),
            (
                "Cardholder name",
                self.name
                    .clone()
                    .unwrap_or_else(|| "Not exposed in the standard records read".to_owned()),
            ),
            ("Card number (masked)", value(&self.masked_pan)),
            ("Application expiry date", value(&self.expiry)),
            ("Application effective date", value(&self.effective)),
            ("Issuer country", country.to_owned()),
            ("Application currency", currency.to_owned()),
            ("Currency decimal places", value(&self.currency_exponent)),
            ("Preferred language", language.to_owned()),
            ("Application priority", priority),
            ("PAN sequence number", value(&self.sequence)),
            ("Service code", value(&self.service_code)),
            ("Application version", value(&self.version)),
        ];
        let width = rows
            .iter()
            .map(|(_, value)| value.len())
            .max()
            .unwrap_or(5)
            .max(5);
        let border = format!("+{}+{}+", "-".repeat(29), "-".repeat(width + 2));
        writeln!(output, "\nInformation recovered")?;
        writeln!(output, "{border}")?;
        writeln!(output, "| {:<27} | {:<width$} |", "Information", "Value")?;
        writeln!(output, "{border}")?;
        for (label, value) in rows {
            writeln!(output, "| {label:<27} | {value:<width$} |")?;
        }
        writeln!(output, "{border}")?;
        writeln!(
            output,
            "Full PAN, Track 2 data, and raw application records are not displayed."
        )
    }
}

fn application_records(data: &[u8]) -> io::Result<Vec<(u8, u8)>> {
    application_records_including(data, false)
}

fn application_records_including(
    data: &[u8],
    include_proprietary: bool,
) -> io::Result<Vec<(u8, u8)>> {
    let format_one = find_tags(data, 0x80)?;
    let afl = if let Some(value) = format_one.first() {
        if value.len() < 2 {
            return Err(invalid("GPO response is missing its two-byte AIP."));
        }
        &value[2..]
    } else {
        let templates = find_tags(data, 0x77)?;
        let template = templates
            .first()
            .ok_or_else(|| invalid("Unknown GPO response template."))?;
        let aip = find_tags(template, 0x82)?;
        if aip.len() != 1 || aip[0].len() != 2 {
            return Err(invalid("GPO response requires a two-byte AIP."));
        }
        let afls = find_tags(template, 0x94)?;
        if afls.len() != 1 {
            return Err(invalid("GPO response requires one AFL."));
        }
        afls[0]
    };
    if afl.len() > 252 || !afl.len().is_multiple_of(4) {
        return Err(invalid("Invalid Application File Locator length."));
    }
    let mut records = Vec::new();
    for entry in afl.chunks_exact(4) {
        let sfi = entry[0] >> 3;
        let first = entry[1];
        let last = entry[2];
        if entry[0] & 7 != 0
            || !(1..=30).contains(&sfi)
            || first == 0
            || first > last
            || u16::from(entry[3]) > u16::from(last) - u16::from(first) + 1
        {
            return Err(invalid("Invalid Application File Locator entry."));
        }
        if sfi > 10 && !include_proprietary {
            println!("Skipping proprietary SFI {sfi}; only standard EMV TLV records are decoded.");
            continue;
        }
        for record in first..=last {
            if !records.contains(&(sfi, record)) {
                records.push((sfi, record));
            }
            if records.len() > 512 {
                return Err(invalid("Application record safety limit exceeded."));
            }
        }
    }
    Ok(records)
}

fn read_application_information(
    transmit: &mut impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
    selection: &[u8],
) -> io::Result<CardInformation> {
    let mut info = CardInformation::default();
    info.update(selection)?;
    let response = processing_options(transmit, selection)?;
    // Template 80 is a primitive AIP/AFL layout, not nested TLV.
    if !find_tags(&response.data, 0x77)?.is_empty() {
        info.update(&response.data)?;
    }
    for (sfi, record) in application_records(&response.data)? {
        let response = exchange(transmit, &[0x00, 0xB2, record, (sfi << 3) | 0x04, 0x00])?;
        require_success(&response, "Application READ RECORD")?;
        let templates = find_tags(&response.data, 0x70)?;
        if templates.len() != 1 {
            return Err(invalid(
                "Standard EMV record requires a single 70 template.",
            ));
        }
        info.update(templates[0])?;
    }
    Ok(info)
}

fn processing_options(
    transmit: &mut impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
    selection: &[u8],
) -> io::Result<Response> {
    if find_tags(selection, 0x9F38)?
        .iter()
        .any(|pdol| !pdol.is_empty())
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "This application requests terminal data (PDOL). GPO reading currently supports \
             only absent/empty PDOLs; terminal values will not be fabricated.",
        ));
    }
    let response = exchange(transmit, &[0x80, 0xA8, 0x00, 0x00, 0x02, 0x83, 0x00, 0x00])?;
    require_success(&response, "GET PROCESSING OPTIONS")?;
    Ok(response)
}

fn dump_application_records(
    transmit: &mut impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
    selection: &[u8],
) -> io::Result<()> {
    let response = processing_options(transmit, selection)?;
    let mut errors = Vec::new();
    for (sfi, record) in application_records_including(&response.data, true)? {
        let response = exchange(transmit, &[0x00, 0xB2, record, (sfi << 3) | 0x04, 0x00])?;
        if let Err(error) = require_success(&response, "Application READ RECORD") {
            errors.push(format!("SFI {sfi}, record {record}: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(errors.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_aid_input() {
        assert_eq!(parse_aid("A000000001").unwrap(), [0xA0, 0, 0, 0, 1]);
        for value in [
            "",
            "A000",
            "A00000000",
            "GG00000000",
            "A000 000001",
            "é00000000",
        ] {
            assert!(parse_aid(value).is_err(), "{value}");
        }
    }

    #[test]
    fn parses_nested_and_long_length_tlv() {
        let data = [0x6F, 0x81, 0x07, 0x4F, 0x05, 0xA0, 0, 0, 0, 1];
        assert_eq!(
            find_tags(&data, 0x4F).unwrap(),
            [b"\xA0\0\0\0\x01".as_slice()]
        );
        for data in [&[0x6F, 2, 0x4F][..], &[0x9F][..], &[0x6F, 0x80][..]] {
            assert!(find_tags(data, 0x4F).is_err());
        }
    }

    #[test]
    fn handles_corrected_length_and_get_response() {
        let mut calls = Vec::new();
        let mut responses = vec![
            vec![0x6C, 0x02],
            vec![0xAA, 0x61, 0x01],
            vec![0xBB, 0x90, 0x00],
        ]
        .into_iter();
        let response = exchange(
            &mut |command| {
                calls.push(command.to_vec());
                Ok(responses.next().unwrap())
            },
            &[0, 0xB2, 1, 0x0C, 0],
        )
        .unwrap();
        assert_eq!(response.data, [0xAA, 0xBB]);
        assert_eq!(response.status, [0x90, 0]);
        assert_eq!(calls[1], [0, 0xB2, 1, 0x0C, 2]);
        assert_eq!(calls[2], [0, 0xC0, 0, 0, 1]);
    }

    #[test]
    fn rejects_truncated_and_endless_responses() {
        assert!(exchange(&mut |_| Ok(vec![0x90]), &[0, 0xC0, 0, 0, 0]).is_err());
        assert!(exchange(&mut |_| Ok(vec![0x61, 1]), &[0, 0xC0, 0, 0, 0]).is_err());
        assert!(
            exchange(
                &mut |_| Err(io::Error::other("reader disconnected")),
                &[0, 0xC0, 0, 0, 0]
            )
            .is_err()
        );
    }

    #[test]
    fn reads_contact_directory_and_selects_discovered_application() {
        let mut commands = Vec::new();
        let mut responses = vec![
            vec![0x6F, 3, 0x88, 1, 1, 0x90, 0],
            vec![0x70, 9, 0x61, 7, 0x4F, 5, 0xA0, 0, 0, 0, 1, 0x90, 0],
            vec![0x6A, 0x83],
            vec![0x6F, 0, 0x90, 0],
        ]
        .into_iter();
        read_directory(
            |command| {
                commands.push(command.to_vec());
                Ok(responses.next().unwrap())
            },
            None,
        )
        .unwrap();
        assert_eq!(commands.len(), 4);
        assert_eq!(&commands[0][5..19], b"1PAY.SYS.DDF01");
        assert_eq!(commands[1], [0, 0xB2, 1, 0x0C, 0]);
        assert_eq!(commands[3], [0, 0xA4, 4, 0, 5, 0xA0, 0, 0, 0, 1, 0]);
    }

    #[test]
    fn tries_second_directory_and_reports_missing_directories() {
        let mut commands = Vec::new();
        assert!(
            read_directory(
                |command| {
                    commands.push(command.to_vec());
                    Ok(vec![0x6A, 0x82])
                },
                None
            )
            .is_err()
        );
        assert_eq!(commands.len(), 2);
        assert_eq!(&commands[1][5..19], b"2PAY.SYS.DDF01");
    }

    #[test]
    fn selects_explicit_aid_and_surfaces_card_errors() {
        let aid = [0xA0, 0, 0, 0, 1];
        let mut commands = Vec::new();
        read_directory(
            |command| {
                commands.push(command.to_vec());
                Ok(vec![0x90, 0])
            },
            Some(&aid),
        )
        .unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(&commands[0][5..10], &aid);
        assert!(read_directory(|_| Ok(vec![0x69, 0x82]), Some(&aid)).is_err());
    }

    #[test]
    fn discovers_inline_application_from_second_directory() {
        let mut calls = 0;
        let mut responses = vec![
            vec![0x6A, 0x82],
            vec![
                0x6F, 11, 0xA5, 9, 0x61, 7, 0x4F, 5, 0xA0, 0, 0, 0, 1, 0x90, 0,
            ],
            vec![0x90, 0],
        ]
        .into_iter();
        read_directory(
            |_| {
                calls += 1;
                Ok(responses.next().unwrap())
            },
            None,
        )
        .unwrap();
        assert_eq!(calls, 3);
    }

    #[test]
    fn reports_empty_and_invalid_directories() {
        for response in [
            vec![0x6F, 0, 0x90, 0],
            vec![0x6F, 3, 0x88, 1, 0, 0x90, 0],
            vec![0x6F, 3, 0x88, 1, 31, 0x90, 0],
        ] {
            assert!(read_directory(|_| Ok(response.clone()), None).is_err());
        }
    }

    #[test]
    fn extracts_general_fields_without_retaining_account_data() {
        let mut info = CardInformation::default();
        info.update(&[
            0x50, 4, b'T', b'E', b'S', b'T', 0x5F, 0x2D, 2, b'e', b'n', 0x87, 1, 1, 0x5F, 0x28, 2,
            0x03, 0x56, 0x9F, 0x42, 2, 0x03, 0x56, 0x5F, 0x20, 10, b'D', b'O', b'E', b'/', b'J',
            b'A', b'N', b'E', b' ', b' ', 0x5A, 4, 0x12, 0x34, 0x56, 0x78, 0x57, 2, 0x56, 0x78,
        ])
        .unwrap();
        assert_eq!(
            info,
            CardInformation {
                name: Some("DOE/JANE".to_owned()),
                label: Some("TEST".to_owned()),
                language: Some("en".to_owned()),
                priority: Some(1),
                country: Some("356".to_owned()),
                currency: Some("356".to_owned()),
                masked_pan: Some("**** 5678".to_owned()),
                ..CardInformation::default()
            }
        );
        assert!(text_field(&[0x50, 1, 0x1B], 0x50).is_err());
        assert!(numeric_code(&[0x5F, 0x28, 2, 0xFA, 0x01], 0x5F28).is_err());
    }

    #[test]
    fn parses_both_gpo_formats_and_deduplicates_records() {
        let expected = vec![(1, 1), (1, 2)];
        assert_eq!(
            application_records(&[0x80, 6, 0, 0, 8, 1, 2, 0]).unwrap(),
            expected
        );
        assert_eq!(
            application_records(&[0x77, 14, 0x82, 2, 0, 0, 0x94, 8, 8, 1, 2, 0, 8, 1, 2, 0])
                .unwrap(),
            expected
        );
        assert!(
            application_records(&[0x80, 6, 0, 0, 88, 1, 2, 0])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_malformed_gpo_and_afl() {
        for response in [
            vec![0x80, 1, 0],
            vec![0x80, 3, 0, 0, 8],
            vec![0x80, 6, 0, 0, 9, 1, 1, 0],
            vec![0x80, 6, 0, 0, 0, 1, 1, 0],
            vec![0x80, 6, 0, 0, 8, 0, 1, 0],
            vec![0x80, 6, 0, 0, 8, 2, 1, 0],
            vec![0x80, 6, 0, 0, 8, 1, 1, 2],
            vec![0x77, 4, 0x82, 2, 0, 0],
        ] {
            assert!(application_records(&response).is_err(), "{response:02X?}");
        }
    }

    #[test]
    fn reads_name_only_from_advertised_records_without_payment_commands() {
        let selection = [0x6F, 6, 0x50, 4, b'T', b'E', b'S', b'T'];
        let mut commands = Vec::new();
        let mut responses = vec![
            vec![0x80, 6, 0, 0, 8, 1, 1, 0, 0x90, 0],
            vec![
                0x70, 11, 0x5F, 0x20, 8, b'D', b'O', b'E', b'/', b'J', b'A', b'N', b'E', 0x90, 0,
            ],
        ]
        .into_iter();
        let info = read_application_information(
            &mut |command| {
                commands.push(command.to_vec());
                Ok(responses.next().unwrap())
            },
            &selection,
        )
        .unwrap();
        assert_eq!(info.name.as_deref(), Some("DOE/JANE"));
        assert_eq!(info.label.as_deref(), Some("TEST"));
        assert_eq!(
            commands,
            [
                vec![0x80, 0xA8, 0, 0, 2, 0x83, 0, 0],
                vec![0, 0xB2, 1, 0x0C, 0],
            ]
        );
    }

    #[test]
    fn distinguishes_missing_name_from_card_failures() {
        let mut responses = vec![
            vec![0x80, 6, 0, 0, 8, 1, 1, 0, 0x90, 0],
            vec![0x70, 0, 0x90, 0],
        ]
        .into_iter();
        let info =
            read_application_information(&mut |_| Ok(responses.next().unwrap()), &[]).unwrap();
        assert_eq!(info.name, None);
        assert!(read_application_information(&mut |_| Ok(vec![0x69, 0x85]), &[]).is_err());
        let mut responses =
            vec![vec![0x80, 6, 0, 0, 8, 1, 1, 0, 0x90, 0], vec![0x6A, 0x83]].into_iter();
        assert!(read_application_information(&mut |_| Ok(responses.next().unwrap()), &[]).is_err());
    }

    #[test]
    fn rejects_required_pdol_without_sending_fabricated_data() {
        let mut called = false;
        let result = read_application_information(
            &mut |_| {
                called = true;
                Ok(vec![0x90, 0])
            },
            &[0x9F, 0x38, 3, 0x9F, 0x1A, 2],
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
        assert!(!called);
    }

    #[test]
    fn information_mode_discovers_selects_and_reads_with_no_raw_logging() {
        let mut responses = vec![
            vec![0x6F, 3, 0x88, 1, 1, 0x90, 0],
            vec![0x70, 9, 0x61, 7, 0x4F, 5, 0xA0, 0, 0, 0, 3, 0x90, 0],
            vec![0x6A, 0x83],
            vec![0x6F, 0, 0x90, 0],
            vec![0x77, 10, 0x82, 2, 0, 0, 0x94, 4, 8, 1, 1, 0, 0x90, 0],
            vec![
                0x70, 23, 0x5F, 0x20, 8, b'D', b'O', b'E', b'/', b'J', b'A', b'N', b'E', 0x5A, 4,
                0x12, 0x34, 0x56, 0x78, 0x57, 4, 0x11, 0x22, 0x33, 0x44, 0x90, 0,
            ],
        ]
        .into_iter();
        read_information(|_| Ok(responses.next().unwrap()), None).unwrap();
        assert_eq!(responses.next(), None);
    }

    #[test]
    fn dump_reads_proprietary_afl_records_without_decoding_them() {
        let mut commands = Vec::new();
        let mut responses = vec![
            vec![0x6F, 0, 0x90, 0],
            vec![0x80, 6, 0, 0, 88, 1, 1, 0, 0x90, 0],
            vec![0xDE, 0xAD, 0xBE, 0xEF, 0x90, 0],
        ]
        .into_iter();
        read_dump(
            |command| {
                commands.push(command.to_vec());
                Ok(responses.next().unwrap())
            },
            Some(&[0xA0, 0, 0, 0, 1]),
        )
        .unwrap();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[2], [0, 0xB2, 1, 0x5C, 0]);
    }

    #[test]
    fn dump_continues_after_record_status_failure_and_reports_incomplete() {
        let mut commands = Vec::new();
        let mut responses = vec![
            vec![0x6F, 0, 0x90, 0],
            vec![0x80, 6, 0, 0, 8, 1, 2, 0, 0x90, 0],
            vec![0x69, 0x82],
            vec![0x70, 0, 0x90, 0],
        ]
        .into_iter();
        let error = read_dump(
            |command| {
                commands.push(command.to_vec());
                Ok(responses.next().unwrap())
            },
            Some(&[0xA0, 0, 0, 0, 1]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("69 82"));
        assert!(error.to_string().contains("SFI 1, record 1"));
        assert_eq!(commands.len(), 4);
        assert_eq!(commands[3], [0, 0xB2, 2, 0x0C, 0]);
    }

    #[test]
    fn dump_continues_to_other_applications_after_selection_failure() {
        let mut calls = 0;
        let mut responses = vec![
            vec![
                0x6F, 14, 0x4F, 5, 0xA0, 0, 0, 0, 1, 0x4F, 5, 0xA0, 0, 0, 0, 2, 0x90, 0,
            ],
            vec![0x69, 0x85],
            vec![0x6F, 0, 0x90, 0],
            vec![0x80, 2, 0, 0, 0x90, 0],
        ]
        .into_iter();
        let error = read_dump(
            |_| {
                calls += 1;
                Ok(responses.next().unwrap())
            },
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("69 85"));
        assert_eq!(calls, 4);
    }

    #[test]
    fn dump_enforces_record_safety_limit() {
        assert!(
            application_records_including(
                &[0x80, 14, 0, 0, 8, 1, 255, 0, 16, 1, 255, 0, 24, 1, 3, 0,],
                true
            )
            .is_err()
        );
    }

    #[test]
    fn renders_all_recovered_fields_and_masks_the_pan() {
        let mut info = CardInformation::default();
        info.update(&[
            0x50, 4, b'T', b'E', b'S', b'T', 0x5F, 0x20, 8, b'D', b'O', b'E', b'/', b'J', b'A',
            b'N', b'E', 0x5A, 8, 0x12, 0x34, 0x56, 0x78, 0x90, 0x12, 0x34, 0x56, 0x5F, 0x24, 3,
            0x23, 0x11, 0x30, 0x5F, 0x25, 3, 0x19, 0x11, 0x01, 0x5F, 0x28, 2, 0x03, 0x56, 0x9F,
            0x42, 2, 0x03, 0x56, 0x9F, 0x44, 1, 0x02, 0x5F, 0x2D, 2, b'e', b'n', 0x87, 1, 1, 0x5F,
            0x34, 1, 0x01, 0x5F, 0x30, 2, 0x02, 0x26, 0x9F, 0x08, 2, 0x00, 0x96, 0x57, 4, 0xDE,
            0xAD, 0xBE, 0xEF,
        ])
        .unwrap();
        let mut output = Vec::new();
        info.render_information(&[0xA0, 0, 0, 0, 3, 0x10, 0x10], &mut output)
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        for expected in [
            "Information recovered",
            "Visa",
            "TEST",
            "DOE/JANE",
            "************ 3456",
            "2023-11-30",
            "2019-11-01",
            "India (356)",
            "Indian rupee / INR (356)",
            "English (en)",
            "Currency decimal places",
            "Application priority",
            "PAN sequence number",
            "Service code",
            "226",
            "0096 (hex)",
        ] {
            assert!(output.contains(expected), "Missing {expected}");
        }
        assert_eq!(info.currency_exponent.as_deref(), Some("2"));
        assert_eq!(info.sequence.as_deref(), Some("01"));
        assert!(!output.contains("1234567890123456"));
        assert!(!output.contains("DEADBEEF"));
        let widths: Vec<usize> = output
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(str::len)
            .collect();
        assert_eq!(widths.len(), 15);
        assert!(widths.iter().all(|width| *width == widths[0]));
    }

    #[test]
    fn renders_missing_fields_and_preserves_unknown_codes() {
        let mut info = CardInformation::default();
        let mut output = Vec::new();
        info.render_information(&[], &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Not provided"));
        assert!(output.contains("Not exposed in the standard records read"));
        info.country = Some("999".to_owned());
        info.currency = Some("998".to_owned());
        info.language = Some("zz".to_owned());
        let mut output = Vec::new();
        info.render_information(&[], &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("999"));
        assert!(output.contains("998"));
        assert!(output.contains("zz"));
        assert!(!output.contains("India"));
    }

    #[test]
    fn validates_dates_including_leap_days() {
        assert_eq!(
            date_field(&[0x5F, 0x24, 3, 0x24, 0x02, 0x29], 0x5F24).unwrap(),
            Some("2024-02-29".to_owned())
        );
        for date in [
            [0x23, 0x02, 0x29],
            [0x23, 0x04, 0x31],
            [0x23, 0x13, 0x01],
            [0x23, 0x01, 0x00],
            [0xFA, 0x01, 0x01],
        ] {
            let mut data = vec![0x5F, 0x24, 3];
            data.extend_from_slice(&date);
            assert!(date_field(&data, 0x5F24).is_err());
        }
    }

    #[test]
    fn handles_padded_pan_and_rejects_invalid_encodings() {
        assert_eq!(
            masked_pan(&[
                0x5A, 10, 0x12, 0x34, 0x56, 0x78, 0x90, 0x12, 0x34, 0x56, 0x78, 0x9F,
            ])
            .unwrap(),
            Some("*************** 6789".to_owned())
        );
        for data in [
            vec![0x5A, 0],
            vec![0x5A, 2, 0x12, 0x34],
            vec![0x5A, 4, 0x12, 0x3F, 0x56, 0x78],
            vec![0x5A, 4, 0x12, 0x34, 0x56, 0x7A],
        ] {
            assert!(masked_pan(&data).is_err());
        }
        for data in [
            vec![0x9F, 0x44, 1, 0x10],
            vec![0x5F, 0x30, 2, 0x12, 0x26],
            vec![0x9F, 0x08, 1, 0x96],
        ] {
            assert!(CardInformation::default().update(&data).is_err());
        }
    }

    #[test]
    fn propagates_console_write_errors() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Synthetic closed console",
                ))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            CardInformation::default()
                .render_information(&[], &mut FailingWriter)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}

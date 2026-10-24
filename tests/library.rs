use omnismart::{
    CardStatus, Protocol, dump,
    emv::{self, CardInformation},
    is_omnikey_3021, select_reader,
};
use std::{ffi::CString, io};

#[test]
fn public_reader_selection_and_owned_status_are_usable() {
    let readers = [CString::new("OMNIKEY AG Smart Card Reader USB 0").unwrap()];
    assert_eq!(select_reader(&readers).unwrap(), readers[0].as_c_str());
    assert!(is_omnikey_3021("HID OMNIKEY 3021 0"));
    let status = CardStatus {
        reader: readers[0].to_string_lossy().into_owned(),
        protocol: Some(Protocol::T0),
        atr: vec![0x3B, 0],
    };
    assert_eq!(status.clone().atr, [0x3B, 0]);
}

#[test]
fn public_decoder_returns_structured_masked_information() {
    let info = CardInformation::from_tlv(&[
        0x5F, 0x20, 3, b'A', b'D', b'A', 0x5A, 4, 0x12, 0x34, 0x56, 0x78,
    ])
    .unwrap();
    assert_eq!(info.name.as_deref(), Some("ADA"));
    assert_eq!(info.masked_pan.as_deref(), Some("**** 5678"));
    assert_eq!(info.expiry, None);
    let mut output = Vec::new();
    info.render_information(&[0xA0, 0, 0, 0, 3], &mut output)
        .unwrap();
    let table = String::from_utf8(output).unwrap();
    assert!(table.contains("ADA"));
    assert!(table.contains("Visa"));
    assert!(!table.contains("12345678"));
    assert!(CardInformation::from_tlv(&[0x5F, 0x20, 3, b'A']).is_err());
}

#[test]
fn structured_application_reader_works_with_a_synthetic_transport() {
    let mut commands = Vec::new();
    let info = emv::read_application_information(
        &mut |command: &[u8]| {
            commands.push(command.to_vec());
            Ok(vec![0x80, 2, 0, 0, 0x90, 0])
        },
        &[0x6F, 6, 0x5F, 0x20, 3, b'A', b'D', b'A'],
    )
    .unwrap();
    assert_eq!(info.name.as_deref(), Some("ADA"));
    assert_eq!(commands, [vec![0x80, 0xA8, 0, 0, 2, 0x83, 0, 0]]);
}

#[test]
fn invalid_binary_aids_are_rejected_before_any_transmission() {
    for aid in [&[][..], &[0; 4][..], &[0; 17][..]] {
        let mut calls = 0;
        let error = emv::read_information(
            |_| {
                calls += 1;
                Ok(vec![0x90, 0])
            },
            Some(aid),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(calls, 0);
    }
}

#[test]
fn full_pan_opt_in_preserves_the_existing_card_command_sequence() {
    let mut sequences = Vec::new();
    for show_pan in [false, true] {
        let mut responses = vec![
            vec![0x6F, 6, 0x5A, 4, 0x12, 0x34, 0x56, 0x78, 0x90, 0],
            vec![0x80, 2, 0, 0, 0x90, 0],
        ]
        .into_iter();
        let mut commands = Vec::new();
        let transmit = |command: &[u8]| {
            commands.push(command.to_vec());
            Ok(responses.next().unwrap())
        };
        if show_pan {
            emv::read_information_with_pan(transmit, Some(&[0xA0, 0, 0, 0, 3])).unwrap();
        } else {
            emv::read_information(transmit, Some(&[0xA0, 0, 0, 0, 3])).unwrap();
        }
        assert_eq!(responses.next(), None);
        sequences.push(commands);
    }
    assert_eq!(sequences[0], sequences[1]);
    assert_eq!(
        sequences[0],
        [
            vec![0, 0xA4, 4, 0, 5, 0xA0, 0, 0, 0, 3, 0],
            vec![0x80, 0xA8, 0, 0, 2, 0x83, 0, 0],
        ]
    );
}

#[test]
fn dump_transport_accepts_a_trait_object() {
    let path = std::env::temp_dir().join(format!(
        "omnismart-library-synthetic-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let mut dump = dump::Dump::create(&path, "Synthetic", "T0".to_owned(), &[]).unwrap();
    let mut transmit = |_: &[u8]| Ok(vec![0x90, 0]);
    let transport: &mut dyn FnMut(&[u8]) -> io::Result<Vec<u8>> = &mut transmit;
    assert_eq!(
        dump.exchange(transport, &[0, 0xB2, 1, 0x0C, 0]).unwrap(),
        [0x90, 0]
    );
    dump.finish(None).unwrap();
    let report: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(report["complete"], true);
    assert_eq!(report["exchanges"][0]["response_hex"], "9000");
    std::fs::remove_file(&path).unwrap();
}

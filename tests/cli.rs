use std::process::Command;

#[test]
fn help_documents_the_explicit_pan_opt_in_without_accessing_a_reader() {
    let output = Command::new(env!("CARGO_BIN_EXE_omnismart"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--show-pan"));
    assert!(help.contains("with --info only"));
    assert!(help.contains("Keep output private"));
    assert!(!help.contains("Connecting through:"));
}

#[test]
fn invalid_pan_flag_combinations_fail_before_accessing_a_reader() {
    for arguments in [
        vec!["--show-pan"],
        vec!["--emv", "--show-pan"],
        vec!["--dump", "--show-pan"],
        vec!["--dump", "synthetic.json", "--show-pan"],
        vec!["--info", "--show-pan", "--show-pan"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_omnismart"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("--show-pan requires --info"));
        assert!(
            !String::from_utf8(output.stdout)
                .unwrap()
                .contains("Reader:")
        );
    }
}

#[test]
fn help_documents_synchronous_mode_and_unchanged_emv_modes() {
    let output = Command::new(env!("CARGO_BIN_EXE_omnismart"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for text in [
        "--sle4428 --sync-api ABSOLUTE_DLL_PATH",
        "1024 main-memory bytes",
        "does not change reader settings",
        "--info",
        "--emv",
        "--dump",
        "--show-pan",
    ] {
        assert!(help.contains(text), "Missing help text: {text}");
    }
}

#[test]
fn invalid_synchronous_modes_fail_before_accessing_a_reader() {
    let dll = std::env::temp_dir().join("scardsyn.dll");
    let dll = dll.to_str().unwrap();
    for arguments in [
        vec!["--sle4428"],
        vec!["--sync-api", dll],
        vec!["--sle4428", "--sync-api"],
        vec!["--sle4428", "--sync-api", "scardsyn.dll"],
        vec!["--sle4428", "--sync-api", ""],
        vec!["--sle4428", "--sync-api", dll, "--info"],
        vec!["--sle4428", "--sync-api", dll, "--emv"],
        vec!["--sle4428", "--sync-api", dll, "--show-pan"],
        vec!["--sle4428", "--sync-api", dll, "--aid", "A000000003"],
        vec!["--sle4428", "--sync-api", dll, "--dump", "synthetic.json"],
        vec!["--info", "--sync-api", dll],
        vec!["--emv", "--sync-api", dll],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_omnismart"))
            .args(&arguments)
            .output()
            .unwrap();
        assert!(!output.status.success(), "Accepted: {arguments:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("--sync-api"));
        assert!(
            !String::from_utf8(output.stdout)
                .unwrap()
                .contains("Reader:")
        );
    }
}

#[test]
fn missing_synchronous_dll_fails_before_accessing_a_reader() {
    let dll = std::env::temp_dir().join(format!(
        "omnismart-missing-sync-api-{}.dll",
        std::process::id()
    ));
    assert!(!dll.exists());
    let output = Command::new(env!("CARGO_BIN_EXE_omnismart"))
        .args(["--sle4428", "--sync-api"])
        .arg(dll)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    #[cfg(windows)]
    {
        assert!(error.contains("Cannot load HID Synchronous-API"));
        assert!(error.contains("matching this"));
    }
    #[cfg(not(windows))]
    assert!(error.contains("supported only on Windows"));
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("Reader:")
    );
}

#[test]
fn write_requires_an_interactive_terminal_before_accessing_hardware() {
    let dll = std::env::temp_dir().join("not-loaded.dll");
    let output = Command::new(env!("CARGO_BIN_EXE_omnismart"))
        .args(["--sle4428", "--sync-api"])
        .arg(dll)
        .args(["--write", "0020", "AABB"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("interactive terminal")
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn write_options_cannot_mix_with_emv_or_expose_a_pin_argument() {
    let dll = std::env::temp_dir().join("not-loaded.dll");
    let dll = dll.to_str().unwrap();
    for arguments in [
        vec!["--write", "0020", "AA"],
        vec!["--emv", "--write", "0020", "AA"],
        vec!["--info", "--write", "0020", "AA"],
        vec!["--dump", "file.json", "--write", "0020", "AA"],
        vec!["--sle4428", "--sync-api", dll, "--write", "0020"],
        vec![
            "--sle4428",
            "--sync-api",
            dll,
            "--write",
            "0020",
            "AA",
            "--pin",
            "1234",
        ],
        vec![
            "--sle4428",
            "--sync-api",
            dll,
            "--write",
            "0020",
            "AA",
            "--yes",
        ],
        vec![
            "--sle4428",
            "--sync-api",
            dll,
            "--write",
            "0020",
            "AA",
            "--write",
            "0021",
            "BB",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_omnismart"))
            .args(&arguments)
            .output()
            .unwrap();
        assert!(!output.status.success(), "Accepted: {arguments:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(!error.contains("1234"));
        assert!(output.stdout.is_empty());
    }
}

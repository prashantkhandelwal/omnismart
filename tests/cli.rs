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

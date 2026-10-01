use std::process::Command;

#[test]
fn help_and_output_listing_need_no_display_or_audio_device() {
    for argument in ["--help", "--list-outputs"] {
        let output = Command::new(env!("CARGO_BIN_EXE_tempotrack"))
            .arg(argument)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("stdout"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("--tracking"));
    }
}
#[test]
fn removed_tracking_option_fails_before_opening_audio() {
    let output = Command::new(env!("CARGO_BIN_EXE_tempotrack"))
        .args(["--tracking", "assisted"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
}
#[cfg(not(feature = "gui"))]
#[test]
fn requesting_gui_from_headless_build_is_an_actionable_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_tempotrack"))
        .arg("--gui")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--features gui"));
}

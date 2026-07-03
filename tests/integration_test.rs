use assert_cmd::Command;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};

static FIXTURE_COUNTER: AtomicU32 = AtomicU32::new(0);

// ── VCD Fixture ───────────────────────────────────────────────────

/// Creates a VCD fixture file with 3 signals: clk (1-bit), en (1-bit), data (8-bit).
/// Returns the temporary file path. Uses a unique name per call to allow parallel tests.
fn create_vcd_fixture() -> String {
    let id = FIXTURE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = format!("/tmp/waveql_test_fixture_{id}.vcd");
    let vcd = r#"$date
   Today
$end
$version
   waveql test fixture
$end
$timescale 1ns $end
$scope module top $end
$var wire 1 ! clk $end
$var wire 1 " en $end
$var wire 8 # data $end
$upscope $end
$enddefinitions $end
#0
$dumpvars
0!
0"
b00000000 #
$end
#10
1!
#20
1"
#30
b10100011 #
#40
0!
#50
b01000010 #
#60
1!
#70
0"
#80
0!
#90
b00000000 #
#100
1!
1"
"#;
    fs::write(&path, vcd).unwrap();
    path.to_string()
}

fn waveql() -> Command {
    Command::cargo_bin("waveql").unwrap()
}

// ── List Tests ────────────────────────────────────────────────────

#[test]
fn test_list_output_shape() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args(["list", &vcd_path])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"file\""))
        .stdout(predicates::str::contains("\"format\": \"VCD\""))
        .stdout(predicates::str::contains("\"timescale\": \"1ns\""))
        .stdout(predicates::str::contains("\"total_signals\": 3"))
        .stdout(predicates::str::contains("\"signals\":"))
        .stdout(predicates::str::contains("\"path\": \"top.clk\""))
        .stdout(predicates::str::contains("\"path\": \"top.data\""))
        .stdout(predicates::str::contains("\"path\": \"top.en\""));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_list_json_is_valid() {
    let vcd_path = create_vcd_fixture();

    let output = waveql()
        .args(["list", &vcd_path])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["format"], "VCD");
    assert_eq!(parsed["timescale"], "1ns");
    assert_eq!(parsed["total_signals"], 3);
    assert!(parsed["signals"].is_array());
    assert_eq!(parsed["signals"].as_array().unwrap().len(), 3);

    let _ = fs::remove_file(&vcd_path);
}

// ── Changes Tests ─────────────────────────────────────────────────

#[test]
fn test_changes_output_shape() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "changes",
            &vcd_path,
            "--signals",
            "top.clk,top.data",
            "--from",
            "0ns",
            "--to",
            "100ns",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"query_type\": \"changes\""))
        .stdout(predicates::str::contains("\"signal_count\": 2"))
        .stdout(predicates::str::contains("\"range\":"))
        .stdout(predicates::str::contains("\"from\": 0"))
        .stdout(predicates::str::contains("\"to\": 100"))
        .stdout(predicates::str::contains("\"events\":"))
        .stdout(predicates::str::contains("\"time\":"))
        .stdout(predicates::str::contains("\"signal\":"))
        .stdout(predicates::str::contains("\"value\":"));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_changes_has_expected_events() {
    let vcd_path = create_vcd_fixture();

    let output = waveql()
        .args([
            "changes",
            &vcd_path,
            "--signals",
            "top.clk",
            "--from",
            "0ns",
            "--to",
            "100ns",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let events = parsed["events"].as_array().unwrap();

    // clk changes at times 0, 10, 40, 60, 80, 100
    assert!(
        events.len() >= 3,
        "Expected at least 3 clock events, got {}",
        events.len()
    );

    let has_rising_at_10 = events
        .iter()
        .any(|e| e["time"] == 10 && e["signal"] == "top.clk" && e["value"] == "1");
    assert!(has_rising_at_10, "Missing clk rising edge at time 10");

    let has_falling_at_40 = events
        .iter()
        .any(|e| e["time"] == 40 && e["signal"] == "top.clk" && e["value"] == "0");
    assert!(has_falling_at_40, "Missing clk falling edge at time 40");

    let _ = fs::remove_file(&vcd_path);
}

// ── Edges Tests ───────────────────────────────────────────────────

#[test]
fn test_edges_rising_output_shape() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "edges", &vcd_path, "--signal", "top.clk", "--type", "rising", "--from", "0ns", "--to",
            "100ns", "--format", "json",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"signal\": \"top.clk\""))
        .stdout(predicates::str::contains("\"edge_type\": \"rising\""))
        .stdout(predicates::str::contains("\"edge_count\": 3"))
        .stdout(predicates::str::contains("\"edges\":"))
        .stdout(predicates::str::contains("10"))
        .stdout(predicates::str::contains("60"))
        .stdout(predicates::str::contains("100"));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_edges_falling_output_shape() {
    let vcd_path = create_vcd_fixture();

    let output = waveql()
        .args([
            "edges", &vcd_path, "--signal", "top.clk", "--type", "falling", "--from", "0ns",
            "--to", "100ns", "--format", "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["edge_type"], "falling");
    assert_eq!(parsed["edge_count"], 2);
    let edges: Vec<u64> = serde_json::from_value(parsed["edges"].clone()).unwrap();
    assert_eq!(edges, vec![40, 80]);

    let _ = fs::remove_file(&vcd_path);
}

// ── Sample Tests ──────────────────────────────────────────────────

#[test]
fn test_sample_output_shape() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "sample", &vcd_path, "--signal", "top.data", "--at", "37ns", "--format", "json",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"signal\": \"top.data\""))
        .stdout(predicates::str::contains("\"at\": 37"))
        .stdout(predicates::str::contains("\"value\": \"10100011\""));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_sample_before_first_change() {
    let vcd_path = create_vcd_fixture();

    let output = waveql()
        .args([
            "sample", &vcd_path, "--signal", "top.clk", "--at", "0ns", "--format", "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["at"], 0);
    assert_eq!(parsed["value"], "0");

    let _ = fs::remove_file(&vcd_path);
}

// ── ASCII Tests ───────────────────────────────────────────────────

#[test]
fn test_ascii_output_contains_expected_signal_names() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "ascii",
            &vcd_path,
            "--signals",
            "top.clk,top.en,top.data",
            "--from",
            "0ns",
            "--to",
            "100ns",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("Time"))
        .stdout(predicates::str::contains("top.clk"))
        .stdout(predicates::str::contains("top.en"))
        .stdout(predicates::str::contains("top.data"))
        .stdout(predicates::str::contains("0ns"))
        .stdout(predicates::str::contains("100ns"));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_ascii_output_is_plain_text_not_json() {
    let vcd_path = create_vcd_fixture();

    let output = waveql()
        .args([
            "ascii",
            &vcd_path,
            "--signals",
            "top.clk",
            "--from",
            "0ns",
            "--to",
            "100ns",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_err(),
        "ASCII output should be plain text, not JSON"
    );

    let _ = fs::remove_file(&vcd_path);
}

// ── Edge Case: Invalid File Path ──────────────────────────────────

#[test]
fn test_invalid_file_path() {
    waveql()
        .args(["list", "/tmp/nonexistent_waveql_test.vcd"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("Error"));
}

// ── Edge Case: Invalid Time Format ────────────────────────────────

#[test]
fn test_invalid_time_format() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "changes",
            &vcd_path,
            "--signals",
            "top.clk",
            "--from",
            "not-a-time",
            "--format",
            "json",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("Error"));

    let _ = fs::remove_file(&vcd_path);
}

// ── Edge Case: Empty Signal List (all signals) ────────────────────

#[test]
fn test_changes_no_signals_flag_uses_all_signals() {
    let vcd_path = create_vcd_fixture();

    let output = waveql()
        .args([
            "changes", &vcd_path, "--from", "0ns", "--to", "100ns", "--format", "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["signal_count"], 3);

    let _ = fs::remove_file(&vcd_path);
}

// ── Edge Case: Signal Not Found ───────────────────────────────────

#[test]
fn test_signal_not_found() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "changes",
            &vcd_path,
            "--signals",
            "top.nonexistent",
            "--format",
            "json",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("Error"));

    let _ = fs::remove_file(&vcd_path);
}

// ── Table Format Test ─────────────────────────────────────────────

#[test]
fn test_changes_table_format() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "changes",
            &vcd_path,
            "--signals",
            "top.clk",
            "--from",
            "0ns",
            "--to",
            "100ns",
            "--format",
            "table",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("time|signal|value"))
        .stdout(predicates::str::contains("top.clk"));

    let _ = fs::remove_file(&vcd_path);
}

// ── Protocols Subcommand Tests ───────────────────────────────────

#[test]
fn test_protocols_text_output() {
    waveql()
        .args(["protocols"])
        .assert()
        .success()
        .stdout(predicates::str::contains("valid_ready"))
        .stdout(predicates::str::contains("spi"));
}

#[test]
fn test_protocols_json_output() {
    waveql()
        .args(["protocols", "--format", "json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"protocols\""))
        .stdout(predicates::str::contains("valid_ready"))
        .stdout(predicates::str::contains("spi"));
}

#[test]
fn test_protocols_table_output() {
    waveql()
        .args(["protocols", "--format", "table"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "name|required_role_count|optional_role_count|description",
        ));
}

// ── Bind Subcommand Tests ────────────────────────────────────────

#[test]
fn test_bind_missing_protocol_flag_rejected() {
    let vcd_path = create_vcd_fixture();

    waveql().args(["bind", &vcd_path]).assert().failure();

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_bind_unregistered_protocol_fails_cleanly() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "bind",
            &vcd_path,
            "--protocol",
            "nonexistent_proto",
            "--set",
            "role_a=top.clk",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("Protocol not found"));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_bind_validates_binding_syntax() {
    let vcd_path = create_vcd_fixture();

    // valid_ready protocol is now registered — binding should succeed
    waveql()
        .args([
            "bind",
            &vcd_path,
            "--protocol",
            "valid_ready",
            "--set",
            "valid=top.clk",
            "--set",
            "ready=top.en",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("user_specified"));

    let _ = fs::remove_file(&vcd_path);
}

// ── Grouped CLI: inspect ───────────────────────────────────────────

#[test]
fn test_inspect_list_output_shape() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args(["inspect", "list", &vcd_path])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"file\""))
        .stdout(predicates::str::contains("\"total_signals\": 3"))
        .stdout(predicates::str::contains("\"path\": \"top.clk\""));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_inspect_changes_output_shape() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "inspect",
            "changes",
            &vcd_path,
            "--signals",
            "top.clk,top.data",
            "--from",
            "0ns",
            "--to",
            "100ns",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"query_type\": \"changes\""))
        .stdout(predicates::str::contains("\"signal_count\": 2"))
        .stdout(predicates::str::contains("\"events\":"));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_inspect_edges_output_shape() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "inspect", "edges", &vcd_path, "--signal", "top.clk", "--type", "rising", "--from",
            "0ns", "--to", "100ns", "--format", "json",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"signal\": \"top.clk\""))
        .stdout(predicates::str::contains("\"edge_type\": \"rising\""))
        .stdout(predicates::str::contains("\"edge_count\": 3"));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_inspect_sample_output_shape() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "inspect", "sample", &vcd_path, "--signal", "top.data", "--at", "37ns", "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"signal\": \"top.data\""))
        .stdout(predicates::str::contains("\"at\": 37"))
        .stdout(predicates::str::contains("\"value\": \"10100011\""));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_inspect_ascii_output_plain_text() {
    let vcd_path = create_vcd_fixture();

    let output = waveql()
        .args([
            "inspect",
            "ascii",
            &vcd_path,
            "--signals",
            "top.clk",
            "--from",
            "0ns",
            "--to",
            "100ns",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.contains("Time"));
    assert!(stdout.contains("top.clk"));
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_err(),
        "ASCII output should be plain text"
    );

    let _ = fs::remove_file(&vcd_path);
}

// ── Grouped CLI: protocol ──────────────────────────────────────────

#[test]
fn test_protocol_list_text_output() {
    waveql()
        .args(["protocol", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("valid_ready"))
        .stdout(predicates::str::contains("spi"));
}

#[test]
fn test_protocol_list_json_output() {
    waveql()
        .args(["protocol", "list", "--format", "json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"protocols\""))
        .stdout(predicates::str::contains("valid_ready"))
        .stdout(predicates::str::contains("spi"));
}

#[test]
fn test_protocol_bind_validates() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "protocol",
            "bind",
            &vcd_path,
            "--protocol",
            "valid_ready",
            "--set",
            "valid=top.clk",
            "--set",
            "ready=top.en",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("user_specified"));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_protocol_analyze_runs() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args([
            "protocol",
            "analyze",
            &vcd_path,
            "--protocol",
            "valid_ready",
            "--set",
            "valid=top.clk",
            "--set",
            "ready=top.en",
            "--from",
            "0ns",
            "--to",
            "100ns",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"protocol\": \"valid_ready\""))
        .stdout(predicates::str::contains("\"pass\":"));

    let _ = fs::remove_file(&vcd_path);
}

// ── CLI Aliases ────────────────────────────────────────────────────

#[test]
fn test_alias_i_for_inspect() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args(["i", "list", &vcd_path])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"total_signals\": 3"));

    let _ = fs::remove_file(&vcd_path);
}

#[test]
fn test_alias_proto_for_protocol() {
    waveql()
        .args(["proto", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("valid_ready"))
        .stdout(predicates::str::contains("spi"));
}

#[test]
fn test_alias_ls_for_list() {
    let vcd_path = create_vcd_fixture();

    waveql()
        .args(["ls", &vcd_path])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"total_signals\": 3"));

    let _ = fs::remove_file(&vcd_path);
}

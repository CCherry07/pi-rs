use std::process::Command;

#[test]
fn sqlite_cli_report_includes_candidate_coverage() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let report_path = directory.path().join("report.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pi-memory-eval"))
        .args([
            "--backend",
            "sqlite",
            "--suite",
            "smoke",
            "--timeout-ms",
            "500",
            "--report",
        ])
        .arg(&report_path)
        .output()
        .expect("run evaluation CLI");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report_path).expect("read evaluation report"))
            .expect("parse evaluation report");
    assert_eq!(report["schemaVersion"], 5);
    assert_eq!(report["summary"]["candidateCoverage"]["cases"], 15);
    assert_eq!(
        report["cases"][0]["candidateCoverage"]["dense"]["candidateCount"],
        0
    );
}

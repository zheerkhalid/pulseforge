//! Runs the same checks as `PulseForge.exe --selftest` against this machine.
//! Nothing identifying is printed: the details are counts and yes/no facts.

#[test]
fn every_required_collector_delivers() {
    let (checks, passed) = pulseforge::selftest::run();
    for c in &checks {
        println!(
            "[{}] {:<22} {}",
            if c.ok {
                " ok "
            } else if c.required {
                "FAIL"
            } else {
                "none"
            },
            c.name,
            c.detail
        );
    }
    assert!(checks.len() >= 15);
    assert!(passed, "a required collector delivered nothing");
}

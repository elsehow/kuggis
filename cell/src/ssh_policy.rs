//! Never delegate the operator's authentication agent or unrelated tunnels.
//! Keep configured identities and routing so existing deployments still work.
/// Shared by the library transports and the CLI's interactive provider login.
pub const OPTIONS: &[&str] = &[
    "-o", "ForwardAgent=no",
    "-o", "ClearAllForwardings=yes",
    "-o", "ForwardX11=no",
    "-o", "ForwardX11Trusted=no",
];

#[cfg(test)]
mod tests {
    #[test]
    fn ambient_forwarding_is_disabled_without_losing_authentication_config() {
        let output = std::process::Command::new("python3")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ssh_policy.py"))
            .args(super::OPTIONS)
            .output()
            .expect("run SSH configuration regression check");
        assert!(output.status.success(), "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
    }
}

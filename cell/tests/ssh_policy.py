"""Resolve SSH configuration locally: no connection, key read, or service change."""
import pathlib
import subprocess
import sys
import tempfile


with tempfile.TemporaryDirectory(prefix="cell-ssh-policy-") as directory:
    config = pathlib.Path(directory) / "config"
    config.write_text("""Host *
    HostName 127.0.0.1
    User deployment-fixture
    IdentityFile /nonexistent/kuggis-deployment-fixture
    IdentityAgent none
    UserKnownHostsFile /dev/null
    GlobalKnownHostsFile /dev/null
    CanonicalizeHostname no
    ForwardAgent yes
    ForwardX11 yes
    ForwardX11Trusted yes
    LocalForward 18001 127.0.0.1:8001
    RemoteForward 18002 127.0.0.1:8002
    DynamicForward 18003
""")

    def resolved(options):
        result = subprocess.run(
            ["ssh", "-G", "-F", str(config), *options, "fixture"],
            capture_output=True, text=True, check=True,
        )
        rows = [line.split(maxsplit=1) for line in result.stdout.splitlines()]
        return [(row[0], row[1]) for row in rows if len(row) == 2]

    baseline = resolved([])
    assert ("forwardagent", "yes") in baseline
    assert any(key in ("localforward", "remoteforward") for key, _ in baseline)
    actual = resolved(sys.argv[1:])
    for key in ("forwardagent", "forwardx11", "forwardx11trusted"):
        assert (key, "no") in actual, key
    assert ("clearallforwardings", "yes") in actual
    assert not any(key in ("localforward", "remoteforward", "dynamicforward") for key, _ in actual)
    assert ("user", "deployment-fixture") in actual
    assert ("identityfile", "/nonexistent/kuggis-deployment-fixture") in actual
    assert ("hostname", "127.0.0.1") in actual
    print("SSH forwarding disabled; configured deployment identity and host retained.")

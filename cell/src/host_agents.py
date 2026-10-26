"""Keep bundled host agents outside the Kuggis trust boundary.

Default is read-only verification. --apply stops/disables and persistently
masks only shelley.socket and shelley.service. Existing local unit definitions
are preserved in private host backup storage before their paths are masked.
No Kuggis service, guest, workload, credential or application data is changed.
"""
import argparse
import datetime
import fcntl
import json
import os
from pathlib import Path
import stat
import subprocess


AGENTS = ("shelley.socket", "shelley.service")
MANAGED = ("kuggis-supervisor.service", "cell-agent.service", "cell-proxy.service",
           "cell-git.service", "cell-say.service", "cell-door.service",
           "cell-publish-bridge.service", "cell-publisher.service",
           "kuggis-publisher.service", "nginx.service")
PROPERTIES = ("Id", "LoadState", "ActiveState", "SubState", "UnitFileState", "MainPID",
              "Requires", "BindsTo", "PartOf")
UNIT_DIR = Path("/etc/systemd/system")
BACKUP_DIR = Path("/var/lib/kuggis-host-agent-policy")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def command(*args):
    result = subprocess.run(["/usr/bin/systemctl", *args], capture_output=True,
                            text=True, timeout=90, check=False)
    # Some systemd versions return 1 for show with an absent unit. states()
    # still requires a complete, explicitly identified record for every unit.
    require(result.returncode == 0 or (args[0] == "show" and result.returncode == 1),
            "systemctl failed: " + " ".join(args))
    return result.stdout


def states(units):
    args = ["show", *units]
    for name in PROPERTIES:
        args += ["-p", name]
    output = command(*args)
    parsed = {}
    for block in output.strip().split("\n\n"):
        row = dict(line.split("=", 1) for line in block.splitlines() if "=" in line)
        if row.get("Id") in units:
            parsed[row["Id"]] = {key: row.get(key, "") for key in PROPERTIES}
    require(set(parsed) == set(units), "Incomplete service metadata; refusing to guess")
    return parsed


def stopped_and_masked(rows):
    return all(row["ActiveState"] in ("inactive", "failed") and row["UnitFileState"] == "masked"
               and row.get("MainPID", "0") in ("", "0")
               for row in rows.values())


def managed_unchanged(before, after):
    return all((row["ActiveState"], row["MainPID"]) ==
               (after[name]["ActiveState"], after[name]["MainPID"])
               for name, row in before.items())


def check_dependencies(rows):
    for name, row in rows.items():
        dependencies = set(" ".join(row[key] for key in ("Requires", "BindsTo", "PartOf")).split())
        require(not dependencies.intersection(AGENTS),
                "Managed service depends on Shelley; review before stopping: " + name)


def install_masks(unit_dir=UNIT_DIR, backup_parent=BACKUP_DIR, disable=()):
    # These are trusted host paths, never guest or caller-supplied paths.
    directory = unit_dir.lstat()
    require(stat.S_ISDIR(directory.st_mode) and directory.st_uid == 0 and not directory.st_mode & 0o022,
            "System unit directory must be trusted")
    changes = []
    batch = None
    originals = {}
    for name in AGENTS:
        path = unit_dir / name
        if path.is_symlink() and os.readlink(path) == "/dev/null":
            changes.append({"unit": name, "already_masked": True})
            continue
        if batch is None:
            backup_parent.mkdir(mode=0o700, exist_ok=True)
            info = backup_parent.lstat()
            require(stat.S_ISDIR(info.st_mode) and info.st_uid == 0 and stat.S_IMODE(info.st_mode) == 0o700,
                    "Backup parent must be root-owned mode 0700")
            stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
            batch = backup_parent / stamp
            batch.mkdir(mode=0o700)
        saved = None
        try:
            info = path.lstat()
        except FileNotFoundError:
            info = None
        if info is not None:
            require(info.st_uid == 0 and (stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode)),
                    "Unexpected local Shelley unit type or owner")
            saved = batch / name
            # Preserve the inode without reading unit contents. In particular,
            # systemctl disable can remove a local symlink definition itself.
            # Cross-filesystem backups fail closed before disabling anything.
            os.link(path, saved, follow_symlinks=False)
        originals[name] = info
        changes.append({"unit": name, "original_unit_backup": str(saved) if saved else None})
    if disable:
        command("disable", *disable)
    for name, original in originals.items():
        path = unit_dir / name
        try:
            current = path.lstat()
        except FileNotFoundError:
            current = None
        if current is not None:
            require(original is not None and (current.st_dev, current.st_ino) == (original.st_dev, original.st_ino),
                    "Local unit changed during policy application")
            path.unlink()  # original inode is retained in the private backup
        path.symlink_to("/dev/null")
    return changes, batch


def apply_policy(report):
    before = states(MANAGED)
    report["managed_before"] = before
    check_dependencies(before)
    agents = states(AGENTS)
    report["before"] = agents
    active = [name for name, row in agents.items() if row["ActiveState"] not in ("inactive", "failed")]
    if active:
        command("stop", *active)
    disable = [name for name, row in agents.items()
               if row["LoadState"] != "not-found" and row["UnitFileState"] not in ("masked", "masked-runtime")]
    changes, batch = install_masks(disable=disable)
    report["mask_changes"] = changes
    command("daemon-reload")
    report["after"] = states(AGENTS)
    report["managed_after"] = states(MANAGED)
    report["stopped_and_masked"] = stopped_and_masked(report["after"])
    report["managed_services_unchanged"] = managed_unchanged(before, report["managed_after"])
    if batch:
        report["receipt"] = str(batch / "receipt.json")
        with (batch / "receipt.json").open("x") as output:
            json.dump(report, output, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
    require(report["stopped_and_masked"], "Shelley is not fully stopped and persistently masked")
    require(report["managed_services_unchanged"], "Managed service state changed; inspect the receipt")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--expected-host")
    parser.add_argument("--expected-machine-id")
    parser.add_argument("--expected-cell-key")
    parser.add_argument("--expected-cell")
    args = parser.parse_args()
    report = {"host": os.uname().nodename, "applied": args.apply,
              "observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat()}
    try:
        if args.expected_host:
            require(report["host"] == args.expected_host, "Host identity does not match")
        if args.expected_machine_id:
            require(Path("/etc/machine-id").read_text().strip() == args.expected_machine_id,
                    "Pinned machine identity does not match")
        if args.expected_cell_key:
            require(Path("/etc/cell/key.pub").read_text().strip() == args.expected_cell_key,
                    "Pinned public cell identity does not match")
        if args.expected_cell:
            require(json.loads(Path("/etc/cell/cell.json").read_text())["name"] == args.expected_cell,
                    "Registered cell binding does not match")
        if args.apply:
            require(os.geteuid() == 0, "Applying host-agent policy requires host root")
            os.umask(0o077)
            fd = os.open("/run/kuggis-host-agent-policy.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
            with os.fdopen(fd, "w") as lock:
                info = os.fstat(lock.fileno())
                require(stat.S_ISREG(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o077,
                        "Unexpected policy lock")
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                apply_policy(report)
        else:
            report["after"] = states(AGENTS)
            report["stopped_and_masked"] = stopped_and_masked(report["after"])
        report["ok"] = report["stopped_and_masked"]
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        report["ok"] = False
        report["error"] = str(error)
    print(json.dumps(report, indent=2))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

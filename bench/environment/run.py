#!/usr/bin/env python3
"""Create, exercise, and delete only a VM owned by this bench run. Stdlib only."""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess as sp
import sys
import tarfile
import time
import uuid

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
SSH = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
       "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"]


def call(args, *, check=True, timeout=180, **kwargs):
    result = sp.run(args, capture_output=True, timeout=timeout, **kwargs)
    if check and result.returncode:
        raise RuntimeError(f"{shlex.join(map(str, args))}: {result.stderr.decode(errors='replace')[-3000:]}")
    return result


def edge(*args):
    return call(SSH + ["exe.dev", shlex.join(args)]).stdout


def remote(vm, command, **kwargs):
    return call(SSH + ["-o", "StrictHostKeyChecking=accept-new", f"{vm}.exe.xyz", command], **kwargs)


def owned(receipt):
    if not re.fullmatch(r"kuggis-bench-[a-z0-9-]+", receipt["vm"]):
        raise RuntimeError("Not a bench VM name")
    candidates = [v for v in json.loads(edge("ls", "--json"))["vms"]
                  if v["vm_name"] == receipt["vm"]]
    if len(candidates) != 1:
        raise RuntimeError("Bench VM is absent or ambiguous")
    vm = candidates[0]
    if (vm.get("created_at") != receipt["created_at"]
            or receipt["tag"] not in vm.get("tags", [])):
        raise RuntimeError("VM incarnation or ownership tag changed; refusing to act")
    return vm


def wait_ssh(vm):
    for _ in range(90):
        try:
            if remote(vm, "true", check=False, timeout=15).returncode == 0:
                return
        except sp.TimeoutExpired:
            pass
        time.sleep(2)
    raise RuntimeError("VM did not become SSH-ready")


def upload(vm, result, binary):
    source = result / "source.tar"
    with source.open("wb") as out:
        sp.run(["git", "-C", str(REPO), "archive", "HEAD"], stdout=out, check=True)
    manifest = {"source_commit": call(["git", "-C", str(REPO), "rev-parse", "HEAD"]).stdout.decode().strip(),
                "sha256": {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                           for p in [source, binary, HERE / "host.py", HERE / "guest.py", HERE / "run.py"]}}
    (result / f"inputs-{time.time_ns()}.json").write_text(json.dumps(manifest, indent=2) + "\n")
    payload = result / "payload.tar.gz"
    with tarfile.open(payload, "w:gz") as tar:
        for name in ["host.py", "guest.py"]:
            tar.add(HERE / name, arcname=name)
        tar.add(source, arcname="source.tar")
        tar.add(binary, arcname="cell")
    remote(vm, "sudo mkdir -p /opt/kuggis-bench && sudo tar -xzf - -C /opt/kuggis-bench",
           input=payload.read_bytes(), timeout=180)


def phase(vm, name, result):
    print(f"[{vm}] {name}", flush=True)
    remote(vm, f"sudo rm -f /var/lib/kuggis-bench/{name}.exit")
    # Remote systemd owns the process: local SSH interruption doesn't abandon it.
    remote(vm, "sudo systemctl reset-failed kuggis-bench-run.service 2>/dev/null || true")
    remote(vm, shlex.join(["sudo", "systemd-run", "--unit=kuggis-bench-run", "--collect",
                           "--property=Type=exec", "--property=RuntimeMaxSec=3600",
                           "/usr/bin/python3", "-u", "/opt/kuggis-bench/host.py", name]))
    log = result / f"{name}.log"
    started = time.monotonic()
    seen = ""
    while True:
        output = remote(vm, "sudo journalctl -u kuggis-bench-run --no-pager -o cat -n 3000",
                        check=False).stdout.decode(errors="replace")
        log.write_text(output)
        if output != seen:
            lines = output.splitlines()
            print("\n".join(lines[-8:]), flush=True)
            seen = output
        active = remote(vm, "systemctl is-active kuggis-bench-run.service", check=False)
        if active.stdout.strip() not in (b"active", b"activating", b"deactivating"):
            break
        if time.monotonic() - started > 3700:
            raise RuntimeError("Remote phase exceeded deadline")
        time.sleep(8)
    marker = remote(vm, f"sudo cat /var/lib/kuggis-bench/{name}.exit", check=False)
    if marker.returncode or marker.stdout.strip() != b"0":
        raise RuntimeError(f"Remote {name} failed; see {log}")


def collect(vm, result):
    out = remote(vm, "sudo tar -czf - -C /var/lib kuggis-bench", check=False, timeout=120)
    if out.returncode == 0:
        archive = result / "evidence.tar.gz"
        archive.write_bytes(out.stdout)
        # Only extract regular report files, never execute guest-controlled paths.
        with tarfile.open(archive) as tar:
            for name in ["report.json", "report.md", "boundary.json", "observed.json"]:
                try:
                    f = tar.extractfile(f"kuggis-bench/{name}")
                    if f:
                        (result / name).write_bytes(f.read())
                except KeyError:
                    pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, help="Linux x86_64 cell binary built from this revision")
    parser.add_argument("--resume", type=Path, help="Existing result directory with ownership receipt")
    parser.add_argument("--stage", choices=["all", "inspect", "setup", "test", "post-reboot", "cleanup"], default="all")
    parser.add_argument("--keep-vm", action="store_true", help="Keep owned VM for diagnosis; use --stage cleanup later")
    args = parser.parse_args()
    if args.stage != "cleanup" and (not args.bin or not args.bin.is_file()):
        parser.error("--bin must name a Linux cell binary")
    receipt = None
    if args.resume:
        result = args.resume.resolve()
        receipt = json.loads((result / "receipt.json").read_text())
        owned(receipt)
    else:
        if args.stage == "cleanup":
            parser.error("cleanup requires --resume")
        token = uuid.uuid4().hex[:10]
        vm = f"kuggis-bench-{token}"
        tag = f"bench-{token}"
        result = HERE / "results" / (dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ-") + token)
        result.mkdir(parents=True)
        print(f"Results: {result}", flush=True)
        # Record intent before creation, so interrupted creation can be inspected.
        (result / "intent.json").write_text(json.dumps({"vm": vm, "tag": tag}, indent=2))
        raw = edge("new", f"--name={vm}", f"--tag={tag}", "--no-email", "--json",
                   "--comment=Disposable Kuggis environment-autonomy bench")
        (result / "creation.json").write_bytes(raw)
        listing = json.loads(edge("ls", "--json"))["vms"]
        entry = next(v for v in listing if v["vm_name"] == vm and tag in v.get("tags", []))
        receipt = {"vm": vm, "tag": tag, "created_at": entry["created_at"],
                   "source_commit": call(["git", "-C", str(REPO), "rev-parse", "HEAD"]).stdout.decode().strip(),
                   "binary_sha256": hashlib.sha256(args.bin.read_bytes()).hexdigest()}
        (result / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    vm = receipt["vm"]
    failed = False
    try:
        if args.stage != "cleanup":
            wait_ssh(vm)
            upload(vm, result, args.bin.resolve())
            if args.stage in ("all", "inspect"):
                phase(vm, "inspect", result)
            if args.stage in ("all", "setup"):
                phase(vm, "setup", result)
            if args.stage in ("all", "test"):
                phase(vm, "test", result)
            if args.stage == "all":
                before = remote(vm, "cat /proc/sys/kernel/random/boot_id").stdout.strip()
                edge("restart", vm)
                time.sleep(5)
                wait_ssh(vm)
                after = remote(vm, "cat /proc/sys/kernel/random/boot_id").stdout.strip()
                if before == after:
                    raise RuntimeError("VM boot ID did not change")
                (result / "vm-reboot.json").write_text(json.dumps({"before": before.decode(), "after": after.decode()}))
            if args.stage in ("all", "post-reboot"):
                phase(vm, "post-reboot", result)
    except Exception as exc:
        failed = True
        (result / "controller-error.txt").write_text(str(exc) + "\n")
        print(f"ERROR: {exc}", file=sys.stderr)
    finally:
        if args.stage != "cleanup":
            try:
                collect(vm, result)
            except Exception as exc:
                failed = True
                print(f"Evidence collection failed: {exc}", file=sys.stderr)
        if not args.keep_vm or args.stage == "cleanup":
            owned(receipt)
            deleted = edge("rm", vm, "--json")
            (result / "deletion.json").write_bytes(deleted)
            if any(v["vm_name"] == vm for v in json.loads(edge("ls", "--json"))["vms"]):
                raise RuntimeError("VM still listed after deletion")
            print(f"Deleted {vm}", flush=True)
        else:
            print(f"Kept {vm}; receipt: {result}", flush=True)
    if args.stage != "cleanup" and (result / "report.json").exists():
        report = json.loads((result / "report.json").read_text())
        print(json.dumps(report["summary"], indent=2))
        failed |= any(r["status"] != "pass" for r in report["results"])
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())

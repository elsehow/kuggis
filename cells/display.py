"""The cell grouping the home screen and the room's state.json share. Not the
cell policy or event fold.

Private display definitions live in $KUGGIS_CONFIG/cells/*.json. Membership
here groups the view; it grants no access and does not certify isolation.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import unicodedata

# Load only the sibling package in the installed checkout, never the working directory.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from cells import policy as contracts

NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")
RESERVED = {"operator", "unassigned", "home", "ls", "cells", "cell", "dispatch", "watch", "url",
            "think", "new", "sync", "sh", "put", "done", "token", "help", "onboard", "reconcile", "audit", "adopt"}
ORDER = ("trusted", "forecasting", "merrillfront-dev", "bigbrain-dev",
         "bigbrain-others", "hiring")
DEFAULTS = {
    "airo": "forecasting", "agent-box": "forecasting",
    "merrillfront": "merrillfront-dev", "ibkr-transition": "merrillfront-dev",
    "bigbrain": "bigbrain-dev", "fri-hiring": "hiring",
}


def membership(p, definitions, owners):
    name = p["name"]
    if name == "kuggis":
        return "operator"
    if name in owners:
        return owners[name]
    inferred = DEFAULTS.get(name, DEFAULTS.get(p.get("profile"), "unassigned"))
    return "unassigned" if inferred in definitions else inferred


def inventory(config):
    path = Path(config) / "vms"
    projects = []
    for line in path.read_text().splitlines() if path.exists() else []:
        fields = line.split()
        if not fields or fields[0].startswith("#"):
            continue
        if len(fields) != 4 or not NAME.fullmatch(fields[0]):
            raise ValueError("invalid project map row")
        projects.append({"name": fields[0], "profile": fields[3]})
    return projects


def resolve(config, target, selected=""):
    projects = inventory(config)
    definitions, owners = load_definitions(config)
    names = {p["name"] for p in projects}
    if target in names and not selected:
        return target  # Existing project commands keep their meaning.
    members = [p["name"] for p in projects if membership(p, definitions, owners) == target]
    if selected:
        if selected in members or (target == selected and selected in names):
            return selected
        raise ValueError(f"project '{selected}' is not in '{target}'")
    if len(members) == 1:
        return members[0]
    if len(members) > 1:
        raise ValueError(f"cell '{target}' has several projects: {', '.join(sorted(members))}; choose --project <name>")
    if target in definitions or target in ORDER or target in ("operator", "unassigned"):
        raise ValueError(f"cell '{target}' has no configured VM projects")
    raise ValueError(f"unknown cell or project '{target}'; use kuggis cells")


def session_command(cell, p, name, projects, mapping, shell=False):
    if not NAME.fullmatch(p) or (not shell and not NAME.fullmatch(name)):
        return None
    target = p if cell in ("operator", "unassigned") else cell
    # A legacy project with the same name wins without an explicit selector.
    explicit = target != p and (sum(c == cell for c in mapping.values()) > 1
                               or any(x["name"] == target for x in projects))
    command = f"kuggis sh {target}" if shell else f"kuggis {target} {name}"
    return command + (f" --project {p}" if explicit else "")


def load_definitions(config):
    definitions, owners = {}, {}
    for path in sorted((Path(config) / "cells").glob("*.json")):
        raw = path.read_text()
        data = json.loads(raw)
        contract = isinstance(data, dict) and data.get("version") == 2
        if contract:
            contracts.validate(data)
            name, projects = data["cell"], [p["name"] for p in data["projects"]]
            if name != path.stem:
                raise ValueError("contract filename must match cell name")
        else:
            if not isinstance(data, dict) or set(data) - {"version", "cell", "projects", "definition"}:
                raise ValueError(f"{path.name}: invalid display definition")
            name, projects = data.get("cell"), data.get("projects")
            if (type(data.get("version")) is not int or data["version"] != 1
                    or not isinstance(name, str) or not NAME.fullmatch(name)
                    or name != path.stem or name in RESERVED
                    or not isinstance(projects, list)
                    or any(not isinstance(p, str) or not NAME.fullmatch(p) for p in projects)
                    or not isinstance(data.get("definition", {}), dict)):
                raise ValueError(f"{path.name}: expected version 1 display definition or version 2 contract")
        for project in projects:
            if project == "kuggis":
                raise ValueError("kuggis belongs outside the cells")
            if project in owners:
                raise ValueError(f"project {project} belongs to more than one cell")
            owners[project] = name
        definitions[name] = {
            "source": str(path), "sha256": hashlib.sha256(raw.encode()).hexdigest(),
            "text": raw, "enforced": False, "contract": data if contract else None,
        }
    return definitions, owners


def project(snapshot, config):
    definitions, owners = load_definitions(config)
    cells = {}

    def cell(name):
        if name not in cells:
            cells[name] = {"name": name, "rows": [], "projects": [],
                           "definition": definitions.get(name), "needs_you": 0}
        return cells[name]

    mapping = {p["name"]: membership(p, definitions, owners) for p in snapshot["projects"]}
    for name in definitions:
        cell(name)
    for p in snapshot["projects"]:
        cell(mapping[p["name"]])["projects"].append(p["name"])

    # A contract is desired state, never self-certifying readiness. Unregistered
    # projects also stay visible, so partial onboarding cannot look complete.
    for name, definition in definitions.items():
        policy = definition.get("contract")
        if not policy:
            continue
        for p in policy["projects"]:
            if p["name"] not in cell(name)["projects"]:
                cell(name)["projects"].append(p["name"])
            state = contracts.cached(config, policy, p["name"], snapshot["now"])
            cell(name)["rows"].append({"id": f"setup:{p['name']}", "kind": "setup",
                "name": "development setup", "project": p["name"], "cell": name,
                "state": state, "needs_you": not state.startswith("development verified"),
                "command": f"kuggis audit {name}", "age": ""})

    # The source exposes only request metadata, never bodies or reviews.
    requests = snapshot.get("requests", [])
    held = [r for r in requests if r.get("held")]
    if snapshot.get("thinker_installed") or requests:
        cell("trusted")["rows"].append({
            "id": "thinker", "kind": "thinker", "name": "Thinker", "project": "",
            "state": f"{len(held)} waiting for you" if held else "nothing waiting",
            "needs_you": bool(held), "command": "kuggis think", "age": "",
        })

    for pr in sorted(snapshot.get("prs", []), key=lambda x: (x["project"], x["n"])):
        name = mapping.get(pr["project"], "unassigned")
        cell(name)["rows"].append({
            "id": f"pr:{pr['repo']}#{pr['n']}", "kind": "pr", "name": f"#{pr['n']} {pr['title']}",
            "project": pr["project"], "state": f"checks {pr['checks']}", "needs_you": False, "age": "",
            "cell": pr["repo"], "arg": str(pr["n"]), "command": f"gh pr view {pr['n']} -R {pr['repo']} --web",
        })

    for s in sorted(snapshot["sessions"], key=lambda x: (x["project"], x["name"])):
        name = mapping.get(s["project"], "unassigned")
        if s["name"] == "-":
            if s["state"] != "unreachable":
                continue
            row = {"id": f"vm:{s['project']}", "kind": "vm", "name": s["project"],
                   "project": s["project"], "state": "unreachable", "age": "",
                   "needs_you": True, "command": session_command(name, s["project"], "", snapshot["projects"], mapping, shell=True)}
        else:
            valid = NAME.fullmatch(s["project"]) and NAME.fullmatch(s["name"])
            row = {**s, "id": f"session:{s['project']}/{s['name']}", "kind": "session",
                   "needs_you": s["state"].startswith(("waiting", "stopped")),
                   "command": session_command(name, s["project"], s["name"], snapshot["projects"], mapping) if valid else None}
        cell(name)["rows"].append(row)

    for r in sorted(requests, key=lambda x: (x["since"], x["id"])):
        kind, p = r.get("kind", "brief"), r.get("project", "")
        if r.get("cell"):
            try:
                p = resolve(config, r["cell"], p)
            except ValueError:
                p = p or ""
        name = mapping.get(p, "unassigned") if kind == "brief" else "trusted"
        label = {"brief": f"brief / {r.get('session', '')}", "fetch": f"fetch {r.get('url', '')[:60]}"}.get(kind, kind)
        state = ("awaiting dispatch" if kind == "brief" else "awaiting your ok") if r.get("held") else "carrying"
        cell(name)["rows"].append({
            "id": f"request:{r['id']}", "kind": "door", "name": label, "project": p if kind == "brief" else "",
            "state": state, "needs_you": bool(r.get("held")), "since": r["since"], "command": "kuggis dispatch", "age": "",
        })

    def rank(name):
        return (ORDER.index(name), "") if name in ORDER else (len(ORDER) + (name in ("operator", "unassigned")), name)

    result = []
    for name in sorted(cells, key=rank):
        c = cells[name]
        c["projects"].sort()
        setup = [r for r in c["rows"] if r["kind"] == "setup"]
        c["readiness"] = ("outside cells" if name == "operator" else
                          "setup incomplete" if any(r["needs_you"] for r in setup) else
                          "development verified; isolation not certified" if setup else "setup unverified")
        c["needs_you"] = sum(r["needs_you"] for r in c["rows"])
        working = sum(r["state"].startswith(("starting", "working", "running")) for r in c["rows"])
        status = []
        if working:
            status.append(f"{working} working")
        if c["needs_you"]:
            status.append(f"{c['needs_you']} need{'s' if c['needs_you'] == 1 else ''} you")
        c["status"] = " · ".join(status) if status else ("no sessions" if not c["rows"] else "quiet")
        result.append(c)
    return {"cells": result, "now": snapshot["now"], "source": "session snapshot"}


def safe(text):
    """A terminal label or TSV field cannot carry controls or extra rows."""
    return "".join(f"\\u{ord(c):04x}" if unicodedata.category(c).startswith("C") else c for c in str(text))


def main():
    mode, config, *args = sys.argv[1:]
    if mode == "resolve":
        print(resolve(config, args[0], args[1] if len(args) > 1 else ""))
        return
    if mode in ("inventory", "definition"):
        snapshot = {"projects": inventory(config), "sessions": [], "now": 0,
                    "thinker_installed": os.access("/opt/kuggis-thinker/launch", os.X_OK)}
    else:
        snapshot = json.load(sys.stdin)
    grouped = project(snapshot, config)
    if mode == "json":  # the room's state.json: everything, grouped by cell
        print(json.dumps(grouped))
        return
    cells = grouped["cells"]
    color = "--color" in args
    args = [a for a in args if a != "--color"]
    if args:
        target = args[0]
        matches = [c for c in cells if c["name"] == target]
        if not matches:
            matches = [dict(c, rows=[r for r in c["rows"] if r["project"] == target])
                       for c in cells if target in c["projects"]]
        if not matches:
            raise ValueError(f"unknown cell or project '{target}'; use kuggis cells")
        cells = matches
    for c in cells:
        if mode == "definition":
            d = c["definition"]
            print(safe(c["name"]) + (" · development contract; isolation not certified" if d and d.get("contract") else " · not an enforced policy"))
            if d:
                print(safe(d["source"]) + " · SHA256 " + d["sha256"])
                # Preserve layout while escaping controls in file content.
                print("\n".join(safe(line) for line in d["text"].splitlines()))
            else:
                print("Inferred grouping; projects: " + ", ".join(c["projects"]))
        elif mode == "inventory":
            print(f"{c['name']:<20} {', '.join(c['projects']) or '(no VM projects)'}")
        else:
            heading = f"{c['name']} · {c['status']} · {c['readiness']}"
            if mode == "items":
                print("\t".join(["cell", safe(heading), c["name"], "-"]))
            else:
                print(heading)
            for r in c["rows"]:
                dot = "●" if r["needs_you"] else "·"
                if color and r["needs_you"]:
                    dot = "\033[31m●\033[0m"
                who = f"{r['project']}/{r['name']}" if r["project"] else r["name"]
                age = r["age"]
                if r.get("since") is not None:
                    age = f"{max(0, (snapshot['now'] - r['since']) // 60)}m"
                label = f"  {dot} {safe(who):<32} {safe(r['state'])} {safe(age)}".rstrip()
                if mode == "items":
                    kind = r["kind"] if r["command"] else "invalid"
                    print("\t".join([kind, label, safe(r.get("cell", r["project"])), safe(r.get("arg", r["name"]))]))
                else:
                    print(label)
            if mode != "items":
                print()
    if not cells and mode != "items":
        print("No configured cells or projects")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError) as error:
        sys.exit("kuggis: " + safe(str(error)))

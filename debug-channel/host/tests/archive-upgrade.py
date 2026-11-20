#!/usr/bin/env python3
"""Exercise Linux archive migration and signed-baseline readability."""

import argparse
from contextlib import closing, contextmanager
import itertools
import json
from pathlib import Path
import signal
import sqlite3
import subprocess
import tempfile
import time


EVENTS = "SELECT id,received_at,message,raw_message,journal_id,position FROM events"
GAPS = """SELECT id,journal_id,received_at,json_extract(message,'$.first_position'),
json_extract(message,'$.last_position') FROM events WHERE journal_id IS NOT NULL
AND position IS NULL AND raw_message IS NULL AND json_extract(message,'$.reason')='retention_expired'"""
CHECKPOINTS = "SELECT journal_id,position FROM history_checkpoints ORDER BY journal_id"
HOT = bytes.fromhex("d9d505f920a163d7")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def db(path, mode="ro"):
    return closing(sqlite3.connect(f"{path.as_uri()}?mode={mode}", uri=True, timeout=0.2))


def journal(path):
    return path.with_name(path.name + "-journal")


def idle(path):
    files = {str(path) + suffix for suffix in ("", "-journal", "-wal", "-shm")}
    open_fds = []
    for fd in Path("/proc").glob("[0-9]*/fd/[0-9]*"):
        try:
            target = str(fd.readlink())
        except OSError:
            continue
        if target in files:
            open_fds.append(str(fd))
    require(not open_fds, f"archive descriptors in use: {open_fds}")
    for suffix in ("-journal", "-wal", "-shm"):
        sidecar = Path(str(path) + suffix)
        require(not sidecar.exists() or sidecar.stat().st_size == 0, f"active sidecar: {sidecar}")


def stamp(path):
    s = path.stat()
    return (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)


def snapshot(path):
    with db(path) as c:
        require(list(c.execute("PRAGMA integrity_check")) == [("ok",)], "SQLite integrity_check failed")
        count, end = c.execute("SELECT count(*),max(id) FROM events").fetchone()
        gaps = list(c.execute(GAPS + " ORDER BY id"))
        checkpoints = list(c.execute(CHECKPOINTS))
        positions = list(c.execute("SELECT journal_id,count(*),min(CAST(position AS INTEGER)),"
                                   "max(CAST(position AS INTEGER)) FROM events WHERE position IS NOT NULL "
                                   "GROUP BY journal_id ORDER BY journal_id"))
        return dict(count=count, end=end, gaps=gaps, checkpoints=checkpoints, positions=positions)


def tables(c):
    return {row[0] for row in c.execute("SELECT name FROM sqlite_schema WHERE type='table'")}


def compare_archive(source, copy, base, starts):
    require(snapshot(copy)["count"] == base["count"] + starts, "unexpected event count")
    with db(source) as a, db(copy) as b:
        sql = EVENTS + " WHERE id<=? ORDER BY id"
        for i, (left, right) in enumerate(itertools.zip_longest(
                a.execute(sql, (base["end"],)), b.execute(sql, (base["end"],))), 1):
            require(left == right, f"archive event differs at row {i}")
        require(list(b.execute(CHECKPOINTS)) == base["checkpoints"], "checkpoint differs")
        require(list(b.execute("SELECT id,journal_id,received_at,first,last FROM history_missing ORDER BY id"))
                == base["gaps"], "migrated gaps differ")
        tail = list(b.execute(EVENTS + " WHERE id>? ORDER BY id", (base["end"],)))
        require(len(tail) == starts and all(json.loads(row[2]).get("type") == "host.started"
                and row[3:] == (None, None, None) for row in tail), "unexpected appended events")


def compare_export(host, source, copy, base, log):
    with log.open("w") as err, db(source) as c, subprocess.Popen(
            [host, "export", "--database", copy], stdout=subprocess.PIPE, stderr=err, text=True) as p:
        try:
            expected = c.execute("SELECT received_at,coalesce(raw_message,message) FROM events ORDER BY id")
            for i, (received, message) in enumerate(expected, 1):
                line = p.stdout.readline()
                require(line and json.loads(line) == {"received_at": received, "message": json.loads(message)},
                        f"{host.name} export differs at row {i}; see {log}")
            tail = [json.loads(line)["message"]["type"] for line in p.stdout]
            require(tail == ["host.started"] * 2, f"{host.name} export tail differs")
            require(p.wait(timeout=10) == 0, f"{host.name} export failed; see {log}")
        finally:
            if p.poll() is None:
                p.kill()
        return base["count"] + len(tail)


@contextmanager
def server(host, copy, name, injection=None):
    trace = copy.parent / f"{name}.trace"
    args = ["strace", "-D", "-f", "-y", "-o", trace,
            "-e", "trace=connect,fcntl,fsync,fdatasync,ftruncate,pwrite64,unlink",
            "-e", "inject=connect:error=ENETUNREACH"]
    if injection:
        args += ["-e", injection]
    with trace.with_suffix(".log").open("w") as out:
        with subprocess.Popen(args + [host, "serve", "--cid", "4294967294", "--max-bytes", "1073741824",
                                       "--database", copy, "--control", f"{name}.sock", "--no-web"],
                              cwd=copy.parent, stdout=out, stderr=out) as p:
            try:
                yield p, trace
            finally:
                if p.poll() is None:
                    p.kill()
                    p.wait(timeout=10)


def until(p, condition, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if condition():
            return
        require(p.poll() is None, f"host exited early: {p.returncode}")
        time.sleep(0.01)
    raise RuntimeError("host phase timed out")


def reopen(host, copy, name, end):
    def started():
        try:
            with db(copy) as c:
                row = c.execute("SELECT message FROM events WHERE id>? ORDER BY id LIMIT 1", (end,)).fetchone()
                return row is not None and json.loads(row[0]).get("type") == "host.started"
        except sqlite3.OperationalError as error:
            if error.sqlite_errorcode == sqlite3.SQLITE_BUSY or error.sqlite_errorcode == sqlite3.SQLITE_READONLY_ROLLBACK:
                return False
            raise

    with server(host, copy, name) as (p, _):
        until(p, started)
        p.terminate()
        rc = p.wait(timeout=10)
        require(rc == 0, f"{name} exited {rc}")
    return rc


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("source", "current", "previous", "output"):
        parser.add_argument(arg, type=Path)
    a = parser.parse_args()
    source, current, previous, output = (p.resolve() for p in (a.source, a.current, a.previous, a.output))
    output.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="archive-upgrade-", dir=output))
    print(run, flush=True)
    original = stamp(source)
    idle(source)
    base = snapshot(source)
    require(base["gaps"] and base["checkpoints"], "archive lacks gap/checkpoint evidence")
    with db(source) as c:
        require("history_missing" not in tables(c), "archive already has the gap migration")
    summary = dict(source=str(source), current=str(current), previous=str(previous),
                   source_bytes=original[2], baseline=base, cases={})

    for phase in ("writer-reservation", "migration-commit"):
        case = run / phase
        case.mkdir()
        copy = case / "archive.sqlite"
        idle(source)
        subprocess.run(["cp", "--reflink=always", "--", source, copy], check=True)
        require(stamp(source) == original, "source archive changed")
        evidence = {}
        if phase == "writer-reservation":
            with db(copy, "rw") as locker:
                locker.execute("BEGIN IMMEDIATE")
                with server(current, copy, "interrupted") as (p, trace):
                    def rejected():
                        return trace.exists() and any("F_WRLCK" in line and "l_start=1073741825" in line
                                                       and "EAGAIN" in line for line in trace.read_text().splitlines())
                    until(p, rejected)
                    p.kill()
                    evidence["interrupted"] = p.wait(timeout=10)
                require("history_missing" not in tables(locker), "blocked migration changed schema")
            require(not journal(copy).exists() or journal(copy).stat().st_size == 0, "unexpected rollback journal")
        else:
            with server(current, copy, "interrupted", "inject=ftruncate:error=EIO:signal=SIGKILL:when=1") as (p, trace):
                evidence["interrupted"] = p.wait(timeout=10)
            lines = trace.read_text().splitlines()
            truncated = next((i for i, line in enumerate(lines) if "ftruncate(" in line
                              and f"<{journal(copy)}>" in line and "= ?" in line), None)
            require(truncated is not None and any("sync(" in line and f"<{copy}>) = 0" in line
                    for line in lines[:truncated]), "archive sync before commit interruption not observed")
            with journal(copy).open("rb") as j:
                require(j.read(len(HOT)) == HOT, "expected SQLite hot rollback journal")
            evidence["hot_journal_bytes"] = journal(copy).stat().st_size
            require(copy.stat().st_size > original[2], "expected migration to add a database page")
        require(evidence["interrupted"] == -signal.SIGKILL, "expected SIGKILL")

        evidence["current_reopen"] = reopen(current, copy, "current-reopen", base["end"])
        compare_archive(source, copy, base, 1)
        if phase == "migration-commit":
            recovery = (case / "current-reopen.trace").read_text().splitlines()
            truncate = next((i for i, line in enumerate(recovery) if "ftruncate(" in line
                             and f"<{copy}>, {original[2]}) = 0" in line), None)
            require(truncate is not None and any(f'unlink("{journal(copy)}") = 0' in line
                    for line in recovery[truncate + 1:]), "expected hot journal rollback on current host reopen")
            evidence["rollback_restored_bytes"] = original[2]
        with db(copy) as c:
            end = c.execute("SELECT max(id) FROM events").fetchone()[0]
        evidence["previous_reopen"] = reopen(previous, copy, "previous-reopen", end)
        compare_archive(source, copy, base, 2)
        for label, host in (("current", current), ("previous", previous)):
            evidence[f"{label}_export_rows"] = compare_export(host, source, copy, base, case / f"{label}-export.log")
        evidence["integrity_check"] = "ok"
        summary["cases"][phase] = evidence
        copy.unlink()
        journal(copy).unlink(missing_ok=True)

    idle(source)
    require(stamp(source) == original, "source archive changed")
    (run / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()

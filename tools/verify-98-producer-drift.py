#!/usr/bin/env python3
"""Real binary-drift regression. No Factory state and no repository code execution.

Run after building Rust 1.99 debug/release:
  python3 tools/verify-98-producer-drift.py
A separate scratch build deliberately violates Decision 0003 under the same
producerVersion; failure must keep the old same-generation pins and queue.
"""
import hashlib
import http.client
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import time
from urllib.parse import urlencode

ROOT = Path(__file__).resolve().parents[1]
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
if not TARGET.is_absolute():
    TARGET = ROOT / TARGET


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def command(args, env=None, okay=True, timeout=180):
    done = subprocess.run([str(a) for a in args], cwd=ROOT, env=env,
                          capture_output=True, text=True, timeout=timeout)
    if okay and done.returncode:
        raise RuntimeError(f"{args}: {done.stderr[-2400:]}")
    return done


def index_db(home):
    found = list(home.rglob("index.db"))
    assert len(found) == 1, found
    return found[0]


def snapshot(db):
    with sqlite3.connect(db) as cx:
        return cx.execute("SELECT index_generation,index_revision,"
            "(SELECT count(*) FROM native_revisions),"
            "(SELECT count(*) FROM revision_documents),"
            "(SELECT count(*) FROM revision_producer_bindings) FROM index_metadata").fetchone()


def selected_http(bin_path, env, workspace, scratch, generation, pins):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    token_file = scratch / "token"
    proc = subprocess.Popen([str(bin_path), "serve", "--workspace", str(workspace),
        "--bind", f"127.0.0.1:{port}", "--token-file", str(token_file)],
        cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        # Reading the first startup line is bounded by the test's outer CLI
        # watchdog. The daemon announces readiness only after opening storage.
        first = proc.stdout.readline()
        if "http://127.0.0.1:" not in first:
            raise RuntimeError(f"serve did not start: {first}")
        token = token_file.read_text().strip()
        for number in pins:
            deadline = time.monotonic() + 15
            while True:
                conn = http.client.HTTPConnection("127.0.0.1", port, timeout=15)
                path = "/api/source?" + urlencode({"path":"A.java", "indexGeneration":generation,
                                                  "indexRevision":number})
                conn.request("GET", path, headers={"Authorization":f"Bearer {token}"})
                response = conn.getresponse()
                body = response.read()
                conn.close()
                if response.status == 503 and time.monotonic() < deadline:
                    # A queued refresh may be completing immediately after
                    # serve announces readiness. It cannot release old pins.
                    time.sleep(0.05)
                    continue
                if response.status != 200 or b"class A" not in body:
                    raise RuntimeError(f"selected pin {number} refused: {response.status} {body[:400]!r}")
                break
    finally:
        proc.terminate()
        try:
            proc.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.communicate(timeout=10)


def divergent_binary(scratch, debug):
    source = scratch / "divergent-source"
    source.mkdir()
    for name in ("Cargo.toml", "Cargo.lock"):
        shutil.copy2(ROOT / name, source / name)
    for name in ("src", "web"):
        shutil.copytree(ROOT / name, source / name)
    native = source / "src/native_evidence.rs"
    old = '"java" | "javascript" => name.to_owned(),'
    new = '"java" | "javascript" => format!("{name}__divergent"),'
    contents = native.read_text()
    assert contents.count(old) == 1, "same-version differential fixture no longer applies"
    native.write_text(contents.replace(old, new))
    # Reuse compiled dependencies in the target dir, but restore the user's
    # ignored original debug binary immediately after the variant build.
    saved = scratch / "saved-debug"
    shutil.copy2(debug, saved)
    try:
        command(["cargo", "+1.99.0", "build", "--locked", "--manifest-path",
                 source / "Cargo.toml", "--target-dir", TARGET], timeout=360)
        variant = scratch / "same-version-divergent-binary"
        shutil.copy2(debug, variant)
    finally:
        shutil.copy2(saved, debug)
    return variant, saved


def main():
    debug, release = TARGET / "debug/trellis", TARGET / "release/trellis"
    assert debug.is_file() and release.is_file(), "build Rust 1.99 debug and release first"
    with tempfile.TemporaryDirectory(prefix="trellis-pr98-real-binaries-") as tmp:
        scratch = Path(tmp)
        home = scratch / "home"
        home.mkdir(mode=0o700)
        workspace = scratch / "workspace"
        workspace.mkdir()
        (workspace / "A.java").write_text("class A { int method() { return 1; } }\n")
        (workspace / "call.js").write_text("function call() { return 3; }\n")
        env = {**os.environ, "HOME":str(home), "XDG_CACHE_HOME":str(home / ".cache"),
               "XDG_DATA_HOME":str(home / ".local/share"), "RUST_LOG":"off"}
        variant, original = divergent_binary(scratch, debug)
        debug_sha, release_sha, variant_sha = map(sha, (original, release, variant))
        assert len({debug_sha, release_sha, variant_sha}) == 3, "need three REAL distinct binary SHA values"
        first = command([original, "index", "--workspace", workspace], env)
        assert "index-mode full" in first.stderr
        db = index_db(home)
        generation, first_num = snapshot(db)[:2]
        command([release, "status", "--workspace", workspace], env)
        second = command([release, "index", "--workspace", workspace], env)
        assert "index-mode full" in second.stderr or "index-mode local" in second.stderr
        before = snapshot(db)
        assert before[0] == generation and before[1] > first_num
        with sqlite3.connect(db) as cx:
            origin = cx.execute("SELECT executable_hash FROM native_producers").fetchone()[0]
            producers = cx.execute("SELECT r.published_index_revision,json_extract(i.payload,'$.hash'),"
                "b.producer_sha FROM native_revisions r JOIN revision_capture_inputs i "
                "ON i.revision_id=r.id AND i.input_key LIKE 'executable:%' "
                "LEFT JOIN revision_producer_bindings b ON b.revision_id=r.id "
                "ORDER BY r.published_index_revision").fetchall()
        assert origin == debug_sha and producers[0][1:] == (debug_sha, debug_sha)
        assert all(actual == release_sha and bound == release_sha
                   for _, actual, bound in producers[1:]), producers
        selected_http(release, env, workspace, scratch, generation, (first_num, before[1]))
        before = snapshot(db)  # serve may publish an additional equal-facts revision
        queue = db.with_name("requests.db")
        with sqlite3.connect(queue) as cx:
            prior_queue = cx.execute("SELECT seq,id,state,result_generation,result_revision,"
                "error_code,submitted_at,started_at,finished_at FROM requests ORDER BY seq").fetchall()
        divergent = command([variant, "index", "--workspace", workspace], env, okay=False)
        assert divergent.returncode != 0 and "native_producer_version_required" in divergent.stderr, divergent.stderr
        assert "index-mode full" in divergent.stderr
        assert snapshot(db) == before, "failed same-version divergence changed generation, pins or bindings"
        with sqlite3.connect(queue) as cx:
            after_queue = cx.execute("SELECT seq,id,state,result_generation,result_revision,"
                "error_code,submitted_at,started_at,finished_at FROM requests ORDER BY seq").fetchall()
        assert after_queue[:len(prior_queue)] == prior_queue, "failure lost or rewrote an accepted queue row"
        selected_http(release, env, workspace, scratch, generation, (first_num, before[1]))
        print("PR98_PRODUCER_DRIFT_RESULT=" + json.dumps({"generation":generation,
            "debug_sha":debug_sha, "release_sha":release_sha, "divergent_sha":variant_sha,
            "old_pin":first_num, "new_pin":before[1], "retained_headers":before[2],
            "queue_prior_rows":len(prior_queue), "queue_after_rows":len(after_queue),
            "divergence_refusal":"native_producer_version_required", "old_new_selected_http":200},
            sort_keys=True), flush=True)


if __name__ == "__main__":
    main()

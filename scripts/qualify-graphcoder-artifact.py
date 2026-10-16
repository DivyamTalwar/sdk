#!/usr/bin/env python3
"""Run selected suites from Cargo's exact test artifact without rebuilding."""
import json
import subprocess
import sys

executable = None
for line in sys.stdin:
    message = json.loads(line)
    if message.get("reason") == "compiler-message":
        rendered = message["message"].get("rendered")
        if rendered:
            print(rendered, end="", file=sys.stderr)
    if (message.get("reason") == "compiler-artifact"
            and message.get("profile", {}).get("test")
            and message.get("executable")):
        if executable is not None and executable != message["executable"]:
            sys.exit("Qualification requires one focused test target")
        executable = message["executable"]
if executable is None:
    sys.exit("Cargo did not produce a test executable")

listed = subprocess.check_output([executable, "--list"], text=True)
tests = [line.removesuffix(": test") for line in listed.splitlines()
         if line.endswith(": test")]
prefixes = sys.argv[1:]
if not prefixes:
    sys.exit("Qualification requires explicit test filters")
for prefix in prefixes:
    if not any(name.startswith(prefix) for name in tests):
        sys.exit(f"No tests matched qualification filter {prefix}")
selected = [name for name in tests if name.startswith(tuple(prefixes))]
print(f"Test executable: {executable}", flush=True)
for name in selected:
    subprocess.run([executable, name, "--exact"], check=True, timeout=9)

#!/usr/bin/env python3
"""Measure one child process's peak RSS without a platform-specific time binary."""
import resource
import subprocess
import sys

result = subprocess.run(sys.argv[1:])
peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
if sys.platform != "darwin":
    peak *= 1024
print(f"{peak} maximum resident set size bytes", file=sys.stderr)
sys.exit(result.returncode)

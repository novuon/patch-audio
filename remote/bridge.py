#!/usr/bin/env python3
"""Compatibility launcher. The Rust binary owns transport and engine access."""
import os
import sys
from pathlib import Path

os.chdir(Path(__file__).resolve().parent.parent)
os.execvp("cargo", ["cargo", "run", "-p", "patch-remote", "--", *sys.argv[1:]])

#!/usr/bin/env python3
"""Supported CLI entrypoint for reliable Balatro automation."""

import sys

from balatro_agent.controller import main as controller_main


def _main() -> int:
    return int(controller_main())


if __name__ == "__main__":
    raise SystemExit(_main())

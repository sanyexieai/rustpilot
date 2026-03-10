#!/usr/bin/env python3
import sys


def main() -> int:
    data = sys.stdin.read()
    sys.stdout.write(data)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

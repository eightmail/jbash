#!/usr/bin/env python3
"""
jbash test runner: reads test/cases.txt, runs each case, reports results.

Usage:
    python3 test/run_tests.py [--jbash PATH] [--cases PATH] [--timeout SECS]

Exit codes:
    0 = all tests passed
    1 = one or more tests failed
"""
import argparse, subprocess, sys, time

PASS = "\033[92m  PASS\033[0m"
FAIL = "\033[91m  FAIL\033[0m"
SKIP = "\033[33m  SKIP\033[0m"

def load_cases(path: str) -> list[tuple[str, str, str]]:
    """Return [(label, cmd_args_string, expect_exit)] where expect_exit is '0' or '1'."""
    cases = []
    with open(path) as f:
        for lineno, raw in enumerate(f, 1):
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            expect_fail = line.startswith("!")
            cmd = line.lstrip("!").strip()
            if not cmd:
                continue
            label = f"L{lineno}: {cmd[:60]}"
            cases.append((label, cmd, "1" if expect_fail else "0"))
    return cases


def run_case(jbash: str, cmd: str, timeout: int) -> tuple[int, str]:
    args = [jbash] + cmd.split()
    try:
        r = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout + r.stderr
    except subprocess.TimeoutExpired:
        return -1, "TIMEOUT"
    except Exception as e:
        return -1, str(e)


def main():
    ap = argparse.ArgumentParser(description="jbash automated test runner")
    ap.add_argument("--jbash", default="./target/release/jbash", help="path to jbash binary")
    ap.add_argument("--cases", default="./test/cases.txt", help="path to test cases file")
    ap.add_argument("--timeout", type=int, default=120, help="per-test timeout in seconds")
    args = ap.parse_args()

    cases = load_cases(args.cases)
    if not cases:
        print("No test cases found.")
        sys.exit(0)

    passed = failed = skipped = 0
    failures = []

    print(f"Running {len(cases)} tests (timeout {args.timeout}s each)...\n")

    for label, cmd, expect in cases:
        sys.stdout.write(f"  {label} ... ")
        sys.stdout.flush()
        code, output = run_case(args.jbash, cmd, args.timeout)

        if code == -1:
            print(FAIL)
            failed += 1
            failures.append((label, "TIMEOUT or ERROR", output))
            continue

        if expect == "0" and code == 0:
            print(PASS)
            passed += 1
        elif expect == "1" and code != 0:
            print(PASS)
            passed += 1
        else:
            print(FAIL)
            failed += 1
            failures.append((label, f"expected {'0' if expect == '0' else 'non-zero'}, got {code}", output))

    print(f"\n\033[1mResults: {passed} passed, {failed} failed, {skipped} skipped\033[0m")

    if failures:
        print("\n\033[91mFailures:\033[0m")
        for label, reason, output in failures:
            print(f"\n  {label}: {reason}")
            snippet = output.strip().split("\n")[-10:] if output.strip() else ["<no output>"]
            for line in snippet:
                print(f"    {line}")

    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()

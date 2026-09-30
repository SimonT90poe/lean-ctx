#!/usr/bin/env python3
"""Generate real G9 self-pilot evidence from lean-ctx runtime statistics."""

from __future__ import annotations

import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
EVIDENCE_DIR = ROOT / "security" / "evidence"
MARKDOWN_OUTPUT = EVIDENCE_DIR / "self-pilot-report.md"
JSON_OUTPUT = EVIDENCE_DIR / "g9-self-pilot-evidence.json"
MINIMUM_PILOT_DAYS = 7


def run_json(command: list[str]) -> Any:
    result = subprocess.run(command, check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def load_stats() -> dict[str, Any]:
    try:
        stats = run_json(["lean-ctx", "stats", "json"])
    except FileNotFoundError as error:
        raise RuntimeError("lean-ctx is not available on PATH") from error
    except (subprocess.CalledProcessError, json.JSONDecodeError) as error:
        raise RuntimeError(f"cannot load lean-ctx stats json: {error}") from error
    if not isinstance(stats, dict):
        raise RuntimeError("lean-ctx stats json must return an object")
    return stats


def integer(value: Any, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise ValueError(f"{label} must be a non-negative integer")
    return value


def percent(before: int, after: int) -> float:
    return round((before - min(before, after)) * 100.0 / before, 2) if before else 0.0


def pilot_days(stats: dict[str, Any]) -> float | None:
    first, last = stats.get("first_use"), stats.get("last_use")
    if not isinstance(first, str) or not isinstance(last, str):
        return None
    try:
        start = datetime.fromisoformat(first.replace("Z", "+00:00"))
        end = datetime.fromisoformat(last.replace("Z", "+00:00"))
    except ValueError:
        return None
    return round(max(0.0, (end - start).total_seconds() / 86400), 2)


def registered_agent_identities() -> int | None:
    """Counts identity-registry records; says nothing about bus coordination."""
    try:
        records = run_json(["lean-ctx", "agent", "list", "--json"])
    except (FileNotFoundError, subprocess.CalledProcessError, json.JSONDecodeError):
        return None
    return len(records) if isinstance(records, list) else None


def proxy_launchagent_active() -> bool | None:
    if sys.platform != "darwin":
        return None
    try:
        result = subprocess.run(
            ["launchctl", "list"], check=True, capture_output=True, text=True
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        return None
    return "lean-ctx" in result.stdout.lower()


def markdown_table(rows: list[tuple[str, str]]) -> str:
    body = ["| Metric | Value |", "|---|---|"]
    body.extend(f"| {name} | {value} |" for name, value in rows)
    return "\n".join(body)


def markdown_mode_row(name: str, count: int) -> str:
    return f"| {name.replace('|', chr(92) + '|')} | {count:,} |"


def main() -> int:
    try:
        stats = load_stats()
        cep = stats.get("cep")
        if not isinstance(cep, dict):
            raise ValueError("cep must be an object")
        modes = cep.get("modes")
        if not isinstance(modes, dict):
            raise ValueError("cep.modes must be an object")
        mode_counts = {str(name): integer(value, f"cep.modes.{name}") for name, value in modes.items()}
        input_tokens = integer(stats.get("total_input_tokens"), "total_input_tokens")
        output_tokens = integer(stats.get("total_output_tokens"), "total_output_tokens")
        sessions = integer(cep.get("sessions"), "cep.sessions")
        commands = integer(stats.get("total_commands"), "total_commands")
    except (RuntimeError, ValueError) as error:
        print(f"G9 evidence: {error}", file=sys.stderr)
        return 1

    saved = input_tokens - min(input_tokens, output_tokens)
    savings_pct = percent(input_tokens, output_tokens)
    duration_days = pilot_days(stats)
    agents = registered_agent_identities()
    launchagent = proxy_launchagent_active()
    sustained = duration_days is not None and duration_days >= MINIMUM_PILOT_DAYS
    passed = sustained and input_tokens > 0 and sessions > 0
    duration = f"{duration_days:.1f} days" if duration_days is not None else "unavailable"
    agent_text = str(agents) if agents is not None else "unavailable"
    proxy_text = "Active LaunchAgent" if launchagent is True else "Not detected" if launchagent is False else "unavailable"
    read_mode_names = ", ".join(sorted(mode_counts)) or "no CEP mode data"

    mode_rows = "\n".join(
        markdown_mode_row(name, count)
        for name, count in sorted(mode_counts.items(), key=lambda item: (-item[1], item[0]))
    ) or "| No CEP read-mode data | 0 |"
    verdict = "PASS" if passed else "FAIL"
    report = f"""# Self-Pilot Savings Report — lean-ctx Dogfooding

## Summary

{markdown_table([
    ("Pilot Duration", duration),
    ("Total Sessions", f"{sessions:,}"),
    ("Input Tokens", f"{input_tokens / 1_000_000:.1f}M"),
    ("Output Tokens", f"{output_tokens / 1_000_000:.1f}M"),
    ("Tokens Saved", f"{saved / 1_000_000:.1f}M ({savings_pct:.1f}%)"),
    ("Shell Commands", f"{commands:,}"),
    ("Registered Agent Identities", agent_text),
])}

## Coverage Classes

| Class | Evidence |
|---|---|
| File Reads | ctx_read modes recorded: {read_mode_names} |
| Shell Commands | {commands:,} commands recorded by lean-ctx stats |
| Code Search | ctx_search grep/symbol/semantic |
| Multi-Agent | {agent_text} registered agent identities (identity registry; not a measure of agent-bus coordination) |
| Proxy Interception | {proxy_text}; stream-aware accounting tracked {integer(stats.get('stream_tracked_results', 0), 'stream_tracked_results'):,} results |

## Compression by Read Mode

| Mode | Reads |
|---|---:|
{mode_rows}

## Gate Verdict

G9 Self-Pilot: **{verdict}** — requires at least {MINIMUM_PILOT_DAYS} days of continuous, measured self-pilot usage plus non-zero sessions and token traffic. This report uses only the live `lean-ctx stats json` and `lean-ctx agent list --json` output captured at generation time.
"""
    evidence = {
        "gate": "G9",
        "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "data_source": "lean-ctx stats json (self-pilot)",
        "metrics": {
            "pilot_duration_days": duration_days,
            "total_sessions": sessions,
            "total_input_tokens": input_tokens,
            "total_output_tokens": output_tokens,
            "tokens_saved": saved,
            "compression_ratio_pct": savings_pct,
            "shell_commands": commands,
            "registered_agent_identities": agents,
            "proxy_launchagent_active": launchagent,
            "stream_tracked_results": integer(stats.get("stream_tracked_results", 0), "stream_tracked_results"),
            "read_modes": dict(sorted(mode_counts.items())),
        },
        "thresholds": {"minimum_pilot_days": MINIMUM_PILOT_DAYS},
        "pass": passed,
    }
    EVIDENCE_DIR.mkdir(parents=True, exist_ok=True)
    MARKDOWN_OUTPUT.write_text(report, encoding="utf-8")
    JSON_OUTPUT.write_text(json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"G9 self-pilot: {verdict}; {duration}; {input_tokens:,} input tokens; {sessions:,} sessions")
    print(f"  Wrote {MARKDOWN_OUTPUT.relative_to(ROOT)} and {JSON_OUTPUT.relative_to(ROOT)}")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())

# Self-Pilot Savings Report — lean-ctx Dogfooding

## Summary

| Metric | Value |
|---|---|
| Pilot Duration | 16.8 days |
| Total Sessions | 684 |
| Input Tokens | 188.4M |
| Output Tokens | 103.7M |
| Tokens Saved | 84.7M (45.0%) |
| Shell Commands | 121,338 |
| Registered Agent Identities | 43 |

## Coverage Classes

| Class | Evidence |
|---|---|
| File Reads | ctx_read modes recorded: , aggressive, cat-redirect, compose, full, info, lines:-20, lines:-30, lines:1-10000, lines:1-130,130-260,260-520, lines:1-180,180-380,380-560, lines:1-280, lines:1-390, lines:1-80, lines:1200-1575, lines:155-250,800-890, lines:330-455,670-710,1160-1325, lines:36-270, lines:380-440, lines:60-100,510-570,730-780, lines:730-870, list, map, post, raw, read, remember, signatures, task |
| Shell Commands | 121,338 commands recorded by lean-ctx stats |
| Code Search | ctx_search grep/symbol/semantic |
| Multi-Agent | 43 registered agent identities (identity registry; not a measure of agent-bus coordination) |
| Proxy Interception | Not detected; stream-aware accounting tracked 24,042 results |

## Compression by Read Mode

| Mode | Reads |
|---|---:|
| full | 216 |
| raw | 111 |
| compose | 56 |
| lines:1-10000 | 2 |
| map | 2 |
| post | 2 |
| remember | 2 |
| task | 2 |
|  | 1 |
| aggressive | 1 |
| cat-redirect | 1 |
| info | 1 |
| lines:-20 | 1 |
| lines:-30 | 1 |
| lines:1-130,130-260,260-520 | 1 |
| lines:1-180,180-380,380-560 | 1 |
| lines:1-280 | 1 |
| lines:1-390 | 1 |
| lines:1-80 | 1 |
| lines:1200-1575 | 1 |
| lines:155-250,800-890 | 1 |
| lines:330-455,670-710,1160-1325 | 1 |
| lines:36-270 | 1 |
| lines:380-440 | 1 |
| lines:60-100,510-570,730-780 | 1 |
| lines:730-870 | 1 |
| list | 1 |
| read | 1 |
| signatures | 1 |

## Gate Verdict

G9 Self-Pilot: **PASS** — requires at least 7 days of continuous, measured self-pilot usage plus non-zero sessions and token traffic. This report uses only the live `lean-ctx stats json` and `lean-ctx agent list --json` output captured at generation time.

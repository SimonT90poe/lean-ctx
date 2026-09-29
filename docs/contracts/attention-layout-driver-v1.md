# Attention-aware Layout Driver v1 (AttentionLayoutDriverV1)

GitLab: `#2311` · GitHub: `#1915`

> **Status: nicht implementiert.** Kein Read-Pfad sortiert Inhalte um. Die
> Profil-Keys `layout.enabled` / `layout.min_lines` werden aus Kompatibilitäts-
> gründen weiterhin geparst und bei Profil-Vererbung gemerged, haben aber
> **keine Wirkung**. Der frühere Treiber war ein Stub, der immer `skipped`
> lieferte; er und die unverdrahteten Chunk-Helfer wurden in #1915 entfernt.
> Die Contract-Version bleibt bestehen, bis eine Entfernung gemäss
> [Deprecation-Policy](../../CONTRACTS.md#deprecation-policy) angekündigt ist.

Die folgende Spezifikation beschreibt das **geplante** Verhalten, falls der
Treiber je implementiert wird. Sie ist keine Aussage über den Ist-Zustand.

## Ziele

- **Deterministisch**: gleicher Input + gleiche Keywords + gleiche Policy ⇒ gleiche Reihenfolge.
- **Semantic-first**: bei größeren Inhalten zuerst Chunk‑Reordering (Imports/Types/Fns), danach line-level fallback.
- **Policy-gated**: reordering ist **opt-in** pro Profile.
- **Verifier-safe**: Reorder darf keinen Content “verlieren”, nur umsortieren.
- **Bounded**: kleine Inhalte werden nicht re-ordered (Edge Cases).

## Aktivierung (Policy)

Per Profile:

- `profile.layout.enabled = true|false`
- `profile.layout.min_lines = <n>`

Default: `enabled=false`. Kein eingebautes Profil setzt `enabled=true`.

## Semantik (v1, geplant)

- **Small files**: wenn `lines <= 5` (oder `< min_lines`) ⇒ keine Änderung.
- **Large content**: ab `lines >= 15` wird Chunking versucht (Chunk-Erkennung,
  Keyword-gewichtete Reihenfolge, Rendering mit Brücken-Kommentaren).
- **Fallback**: sonst line-level scoring + stable tie-break (original index).

## Keywords

Keywords stammen aus dem Task/Intent Kontext (z.B. `task` Argument).

## Determinism Guarantees

- Sorts haben stabile Tie-breaks (z.B. `start_line`, `original_index`), damit gleiche Scores nicht zu nondeterministic reorder führen.

## Relevanter Code

- Profil-Keys: `rust/src/core/profiles/types.rs` (`LayoutConfig`)
- Contract-Version: `rust/src/core/contracts.rs` (`ATTENTION_LAYOUT_DRIVER_V1_SCHEMA_VERSION`)

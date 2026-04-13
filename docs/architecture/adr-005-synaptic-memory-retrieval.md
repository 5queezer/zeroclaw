# ADR-005: Synaptic Memory Retrieval

**Status:** Proposed

**Date:** 2026-04-13

**References:**
[SynapticRAG (Hou et al., ACL 2025 Findings)](https://arxiv.org/abs/2410.13553v2)

## Context

Hrafn's memory retrieval pipeline (`src/memory/retrieval.rs`) operates
in three stages: hot LRU cache, FTS5 keyword search, and vector
similarity with hybrid merge. Time is treated only as a passive decay
penalty (`src/memory/decay.rs`), and the knowledge graph
(`src/memory/knowledge_graph.rs`) is not integrated into the recall
path.

This works well for direct lookups but degrades when the user
references something from a prior conversation where the semantic
signal alone is ambiguous. The system cannot follow temporal chains
of reasoning: "we discussed X, which led to Y, which led to Z." It
also cannot model how memories activate each other through
co-occurrence patterns across sessions.

SynapticRAG demonstrates that combining temporal association with
biologically-inspired activation thresholds yields up to 14.66%
improvement on conversational memory retrieval benchmarks — tested
across four datasets in English, Chinese, and Japanese (SMRCs-EN,
SMRCs-JA, PerLTQA-EN, PerLTQA-CN) — over standard vector-similarity
RAG.

## Decision Drivers

- Hrafn is a conversational agent where multi-turn, multi-session
  temporal context matters.
- The memory infrastructure is already 80% ready: embeddings, vector
  similarity, knowledge graph with traversal, time-decay, importance
  scores, and a staged retrieval pipeline with pluggable stages.
- The knowledge graph exists but has no retrieval-time role.
- Core memories are exempt from decay but have no mechanism to
  strengthen retrieval of related non-core memories.

## Proposed Changes

### 1. Memory Access Tracking

Record timestamps whenever a memory entry is stored or recalled.
This produces an "access spike train" per entry: a time-series of
access events that enables temporal association scoring.

**Scope:** New `memory_access_log` table in the SQLite backend,
populated by `store()` and `recall()` implementations.

```sql
CREATE TABLE memory_access_log (
    memory_id   TEXT NOT NULL,
    accessed_at TEXT NOT NULL,  -- RFC 3339
    access_type TEXT NOT NULL,  -- 'store' | 'recall'
    FOREIGN KEY (memory_id)
        REFERENCES memories(id) ON DELETE CASCADE
);
CREATE INDEX idx_access_log_memory
    ON memory_access_log(memory_id);
```

**Prerequisite fix:** `RetrievalPipeline::cache_key()` currently
omits the `since`/`until` parameters, so two calls with the same
query but different time ranges share a cache entry. This must be
fixed before Phase 1, since temporal scoring makes time-range
queries more likely. Add `since` and `until` to the cache key
format string.

**Risk:** Low. Additive schema change, no existing behavior modified.

### 2. Temporal Association Scoring

Phase 2 adds a **query-relative** temporal signal. For each
candidate memory C, compute how closely C's access spike train
aligns with the spike train of the current query context (the
memories most recently stored or recalled in this session):

```text
T_score(query_ctx, C) = sigmoid(
    alignment_score(spike_train_query, spike_train_C)
)
```

This produces one temporal score per candidate, fitting naturally
into `hybrid_merge` as a third input list alongside vector and
keyword results.

The separate **pairwise** formula used in Phase 3 (graph
propagation) is distinct:

```text
P_score(A, B) = T_score(A, B) * cosine_similarity(A, B)
```

Phase 3 uses `P_score` as edge weights for stimulus propagation
between knowledge graph nodes.

#### `hybrid_merge` API change

The current signature accepts `vector_results` and
`keyword_results`. Phase 2 adds a third parameter:

```rust
pub fn hybrid_merge(
    vector_results: &[(String, f32)],
    keyword_results: &[(String, f32)],
    temporal_results: &[(String, f32)],  // new
    vector_weight: f32,
    keyword_weight: f32,
    temporal_weight: f32,                // new
    limit: usize,
) -> Vec<ScoredResult>
```

All existing call sites (in `sqlite.rs`, `muninndb.rs`, and
`backend.rs`) must be updated. When `temporal_weight = 0.0`,
callers pass an empty slice for `temporal_results`.

A configurable `temporal_candidate_cap` (default: 20) limits the
candidate set size before temporal scoring runs, bounding the
O(n^2) DTW alignment cost.

#### Scoring formula

```text
final = w_vec * vector
      + w_kw  * keyword
      + w_temp * temporal_association
```

#### Weight defaults and migration

The current defaults are `w_vec = 0.5`, `w_kw = 0.3` (sum 0.8).
Phase 2 introduces `w_temp = 0.2` (new sum 1.0). This changes
the effective scale of existing scores.

**Migration path:** Existing `[memory]` configs that set custom
`vector_weight` / `keyword_weight` values will have
`temporal_weight` default to `0.0` (opt-in), so their scoring
is unchanged. When a user explicitly enables `temporal_weight`,
the `[memory]` settings handler renormalizes all three weights
to sum to 1.0:

```text
w_i' = w_i / (w_vec + w_kw + w_temp)
```

If all weights are zero, the system falls back to hard-coded
defaults (`w_vec = 0.625`, `w_kw = 0.375`). This guarantees no
behavioral change for existing configurations.

**Risk:** Medium. Changes retrieval scoring, but `w_temp`
defaults to `0.0` for existing configs and can be zeroed.

### 3. Stimulus Propagation on the Knowledge Graph

When a query activates a set of candidate memory nodes, propagate
activation energy through the knowledge graph edges with decay:

```text
S_child = S_parent * P_score(parent, child)
```

Nodes that accumulate sufficient stimulus from multiple convergent
paths are included in the result set, even if they would not have
been retrieved by direct similarity alone. This gives the knowledge
graph a retrieval-time role.

#### KnowledgeNode-to-MemoryEntry bridge

`KnowledgeNode` and `MemoryEntry` are separate data structures
with independent UUIDs. Phase 3 requires an explicit link between
them so that stimulus propagation on the knowledge graph can
influence memory recall scores.

A new join table bridges the two:

```sql
CREATE TABLE memory_kg_links (
    memory_id  TEXT NOT NULL,
    kg_node_id TEXT NOT NULL,
    PRIMARY KEY (memory_id, kg_node_id),
    FOREIGN KEY (memory_id)
        REFERENCES memories(id) ON DELETE CASCADE,
    FOREIGN KEY (kg_node_id)
        REFERENCES nodes(id) ON DELETE CASCADE
);
CREATE INDEX idx_kg_links_node
    ON memory_kg_links(kg_node_id);
```

Links are populated during memory consolidation
(`consolidation.rs`): when a Core memory update is stored, the
consolidation engine queries the knowledge graph for matching
nodes (by FTS similarity on the memory content) and inserts
links for any matches above a configurable threshold. Links can
also be created explicitly via the `memory_store` tool when
a `kg_node_id` is provided.

#### Edge-weighted propagation

Propagation respects existing edge semantics:

- `Extends` / `Uses`: full propagation weight.
- `Replaces`: propagate only if the replacement is newer.
- `AuthoredBy` / `AppliesTo`: reduced weight (0.3x) to avoid
  expert-node fan-out dominating results.

A configurable `stim_threshold` (default: 0.037, per the paper's
optimized value) gates which nodes qualify for propagation.

**Risk:** Medium-High. Introduces a new retrieval path through the
knowledge graph and a new schema dependency between the memory
and KG tables. Should be feature-gated and opt-in initially.

### 4. Leaky Integrate-and-Fire Memory Selection

Replace the fixed `limit` parameter with a dynamic threshold
mechanism inspired by the LIF neuron model:

```text
tau * dV/dt = -V(t) + I(t)
fire when V >= V_th
```

Where:

- `V` is the membrane potential (accumulated retrieval confidence).
- `I(t)` is the input current from retrieval scores.
- `tau` is a dynamic time constant: frequently-accessed memories
  respond faster (low tau), rarely-accessed memories need sustained
  stimulation (high tau).
- `V_th` is the firing threshold (default: 0.099, per paper).

This maps naturally onto existing Hrafn concepts:

- `importance` score influences initial membrane potential.
- `decay.rs` half-life maps to the leaky time constant.
- `MemoryCategory::Core` (exempt from decay) acts as a permanently
  low firing threshold.

The `limit` parameter becomes a hard cap rather than the sole
selection criterion.

**Risk:** Medium. Changes how many memories are returned, but the
hard cap preserves backward compatibility.

## Implementation Plan

| Phase | Scope                      | Risk        | Depends On |
|-------|----------------------------|-------------|------------|
| 1     | Access tracking: new table | Low         | --         |
| 2     | Temporal scoring: hybrid   | Medium      | Phase 1    |
| 3     | Graph propagation (gated)  | Medium-High | Phase 2    |
| 4     | LIF selection              | Medium      | Phase 2    |

Each phase is a separate PR. Phase 1 is prerequisite for all
others. Phases 3 and 4 are independent of each other.

## Alternatives Considered

### A. Ebbinghaus forgetting curve (MemoryBank approach)

Already partially implemented via `decay.rs`. SynapticRAG subsumes
this: the LIF time constant generalizes exponential decay with
stimulus-driven reactivation. The existing decay module would remain
for backward compatibility but become redundant once Phase 4 lands.

### B. Recency-biased retrieval without temporal association

Simpler: just boost recent memories. But this fails when the
relevant memory is old but was temporally co-accessed with the
current topic. SynapticRAG's DTW alignment captures this pattern;
pure recency does not.

### C. Full graph neural network for retrieval

More powerful but far heavier: requires training, GPU inference,
and a dependency on a GNN framework. SynapticRAG's analytical
propagation model achieves strong results without learned
parameters, fitting Hrafn's no-heavy-dependencies principle.

## Consequences

**Positive:**

- Up to ~14% retrieval accuracy improvement on temporal
  conversational queries.
- Knowledge graph gains a retrieval-time purpose, justifying its
  maintenance cost.
- Memory system becomes more biologically plausible, with natural
  concepts (activation, decay, threshold) that are easier to reason
  about and tune.

**Negative:**

- Access logging adds write amplification to every store/recall
  operation.
- Temporal scoring adds O(n^2) pairwise computation on candidate
  sets, bounded by `temporal_candidate_cap` (default 20, so at
  most ~400 alignments per query).
- Graph propagation adds latency proportional to graph density.
- Four-phase rollout means the full benefit is not realized until
  all phases land.

**Neutral:**

- Existing `decay.rs` remains functional throughout. No breaking
  changes to the `Memory` trait.
- Feature-gating phases 3-4 allows production use of phases 1-2
  independently.

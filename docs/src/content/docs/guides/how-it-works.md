---
title: How it works
description: The wdpkr indexing and search pipelines, end to end.
---

wdpkr runs two pipelines. **Indexing** happens in CI on merge to `main` and
builds the searchable index. **Searching** happens locally, invoked by an
agent, and reads from that index.

```
  Indexing (CI, on merge to main)         Searching (local, agent-invoked)
  ─────────────────────────────           ──────────────────────────────
  repo files                              natural-language query
       │                                       │
       ▼                                       ▼
  ┌─────────┐                             ┌──────────┐
  │ Chunker │  tree-sitter AST            │ Embedder │  same model as index
  └────┬────┘                             └────┬─────┘
       ▼                                       ▼
  ┌──────────────┐                        ┌──────────────┐
  │ Summarizer   │  Claude Haiku          │ Vector Store │  cosine similarity
  └──────┬───────┘                        └──────┬───────┘
         ▼                                       ▼
  ┌──────────┐                            group by file
  │ Embedder │  Voyage code-3             attach top symbols
  └────┬─────┘                            return tiered JSON
       ▼
  ┌──────────────┐
  │ Vector Store │  Turbopuffer
  └──────────────┘
```

## The big idea

**Embed summaries, not code.** Off-the-shelf embedding models are mediocre at
matching a conceptual query ("the commission release flow") against raw source
code. wdpkr closes that gap by summarizing each chunk with an LLM first, then
embedding the *summary*. The result is an index that understands intent, not
just identifiers.

The agent still reads the actual files for ground truth. wdpkr only points and
describes — it never ships source into the context window.

## Indexing

1. **Chunk.** Tree-sitter parses each file into semantically meaningful symbols
   — functions, types, traits — rather than fixed-size line windows. wdpkr
   supports Rust, Go, TypeScript/TSX, JavaScript, Python, Java, C/C++, and C#.
2. **Summarize.** Each chunk is summarized by Claude Haiku. Oversized files get
   a rollup pass so the file-level summary stays coherent.
3. **Embed.** Summaries are embedded with Voyage `voyage-code-3` (by default).
4. **Upsert.** Vectors and metadata land in the vector store (Turbopuffer by
   default), keyed by a per-repo namespace.

Indexing is incremental by default: wdpkr diffs against the last indexed commit
and only reprocesses what changed. `--full` forces a complete rebuild.

## Searching

1. **Embed the query** with the same model used at index time — this is what
   makes the vectors comparable.
2. **Search** the vector store by cosine similarity.
3. **Group by file**, attach the top-scoring symbols within each file, and
   return tiered JSON: files first, symbols nested beneath.

Search uses a `current_thread` tokio runtime for a fast cold start — it's
invoked per-query by an agent and needs to return quickly. Indexing uses a
`multi_thread` runtime to parallelize the summarize/embed work.

## Why these choices

- **AST chunking over line splitting** keeps each unit semantically whole, so a
  symbol's summary describes one coherent thing.
- **Pluggable backends** — VectorStore, Embedder, Summarizer, and Chunker are
  all traits. Swap any provider without changing the pipeline. See
  [Providers](/guides/providers/).
- **CLI, not MCP** — any agent that can shell out can use wdpkr. JSON to
  stdout, errors to stderr.

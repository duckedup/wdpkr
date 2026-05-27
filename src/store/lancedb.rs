//! LanceDB vector store adapter (local embedded database).
//!
//! Implements [`VectorStore`] against LanceDB, storing data on the local
//! filesystem. Each wdpkr [`Namespace`] maps to a LanceDB table. Metadata
//! is stored in a separate `{namespace}__meta` table.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use arrow_array::{
    Array, FixedSizeListArray, Float32Array, ListArray, RecordBatch, RecordBatchIterator,
    StringArray, UInt32Array,
    builder::{ListBuilder, StringBuilder},
};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

use super::{
    ChunkKind, Namespace, NamespaceMetadata, SearchOptions, SearchResult, StoreProvider,
    UpsertStats, VectorDocument, VectorStore,
};
use crate::config::StoreConfig;

// ── Provider ─────────────────────────────────────────────────────────────

pub struct LancedbProvider;

impl StoreProvider for LancedbProvider {
    fn name(&self) -> &str {
        "lancedb"
    }

    fn validate(&self, config: &StoreConfig) -> Result<()> {
        if config.lancedb.data_path.is_empty() {
            bail!(
                "store.lancedb.data_path (or WDPKR_STORE_PATH) is required when store.provider=lancedb"
            );
        }
        Ok(())
    }

    fn build(&self, config: &StoreConfig, dimension: usize) -> Result<Box<dyn VectorStore>> {
        let rt =
            tokio::runtime::Handle::try_current().context("LanceDB requires a tokio runtime")?;
        let db = rt.block_on(async {
            let path = &config.lancedb.data_path;
            std::fs::create_dir_all(path)
                .with_context(|| format!("creating LanceDB data directory: {path}"))?;
            lancedb::connect(path)
                .execute()
                .await
                .with_context(|| format!("connecting to LanceDB at {path}"))
        })?;
        Ok(Box::new(LancedbStore {
            db,
            dimension: dimension as i32,
        }))
    }
}

// ── Store ────────────────────────────────────────────────────────────────

pub struct LancedbStore {
    db: lancedb::Connection,
    dimension: i32,
}

fn meta_table_name(ns: &Namespace) -> String {
    format!("{}__meta", ns.as_str())
}

#[async_trait]
impl VectorStore for LancedbStore {
    async fn create_namespace(&self, ns: &Namespace, dimension: usize) -> Result<()> {
        let dim = dimension as i32;
        let schema = build_schema(dim);
        let batch = RecordBatch::new_empty(Arc::new(schema));
        match self.db.create_table(ns.as_str(), batch).execute().await {
            Ok(_) => {}
            Err(e) if e.to_string().contains("already exists") => {}
            Err(e) => return Err(e).context("creating LanceDB namespace table"),
        }

        let meta_schema = build_meta_schema();
        let meta_batch = RecordBatch::new_empty(Arc::new(meta_schema));
        match self
            .db
            .create_table(meta_table_name(ns), meta_batch)
            .execute()
            .await
        {
            Ok(_) => {}
            Err(e) if e.to_string().contains("already exists") => {}
            Err(e) => return Err(e).context("creating LanceDB metadata table"),
        }

        Ok(())
    }

    async fn delete_namespace(&self, ns: &Namespace) -> Result<()> {
        match self.db.drop_table(ns.as_str(), &[]).await {
            Ok(()) => {}
            Err(e)
                if e.to_string().contains("not found")
                    || e.to_string().contains("does not exist") => {}
            Err(e) => return Err(e).context("deleting LanceDB namespace table"),
        }
        match self.db.drop_table(&meta_table_name(ns), &[]).await {
            Ok(()) => {}
            Err(e)
                if e.to_string().contains("not found")
                    || e.to_string().contains("does not exist") => {}
            Err(e) => return Err(e).context("deleting LanceDB metadata table"),
        }
        Ok(())
    }

    async fn namespace_exists(&self, ns: &Namespace) -> Result<bool> {
        let names = self
            .db
            .table_names()
            .execute()
            .await
            .context("listing LanceDB tables")?;
        Ok(names.iter().any(|n| n == ns.as_str()))
    }

    async fn get_metadata(&self, ns: &Namespace) -> Result<NamespaceMetadata> {
        let table = match self.db.open_table(meta_table_name(ns)).execute().await {
            Ok(t) => t,
            Err(e)
                if e.to_string().contains("not found")
                    || e.to_string().contains("does not exist") =>
            {
                return Ok(NamespaceMetadata::default());
            }
            Err(e) => return Err(e).context("opening metadata table"),
        };

        let stream = table
            .query()
            .execute()
            .await
            .context("querying metadata table")?;

        let batches: Vec<RecordBatch> =
            stream.try_collect().await.context("collecting metadata")?;

        let mut meta = NamespaceMetadata::default();
        for batch in &batches {
            let keys = batch
                .column_by_name("key")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let vals = batch
                .column_by_name("value")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            if let (Some(keys), Some(vals)) = (keys, vals) {
                for i in 0..batch.num_rows() {
                    if keys.is_null(i) || vals.is_null(i) {
                        continue;
                    }
                    let k = keys.value(i);
                    let v = vals.value(i);
                    match k {
                        "hwm_sha" => meta.hwm_sha = Some(v.to_string()),
                        "embedder" => meta.embedder = Some(v.to_string()),
                        _ => {
                            meta.extra.insert(k.to_string(), v.to_string());
                        }
                    }
                }
            }
        }
        Ok(meta)
    }

    async fn set_metadata(&self, ns: &Namespace, meta: &NamespaceMetadata) -> Result<()> {
        let table = match self.db.open_table(meta_table_name(ns)).execute().await {
            Ok(t) => t,
            Err(_) => {
                self.create_namespace(ns, self.dimension as usize).await?;
                self.db
                    .open_table(meta_table_name(ns))
                    .execute()
                    .await
                    .context("opening metadata table after create")?
            }
        };

        // Delete all existing rows and re-insert.
        let _ = table.delete("true").await;

        let mut keys = Vec::new();
        let mut vals = Vec::new();
        if let Some(ref sha) = meta.hwm_sha {
            keys.push("hwm_sha");
            vals.push(sha.as_str());
        }
        if let Some(ref embedder) = meta.embedder {
            keys.push("embedder");
            vals.push(embedder.as_str());
        }
        for (k, v) in &meta.extra {
            keys.push(k.as_str());
            vals.push(v.as_str());
        }

        if keys.is_empty() {
            return Ok(());
        }

        let schema = Arc::new(build_meta_schema());
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(keys)),
                Arc::new(StringArray::from(vals)),
            ],
        )
        .context("building metadata batch")?;

        let reader: Box<dyn arrow_array::RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
        table
            .add(reader)
            .execute()
            .await
            .context("inserting metadata")?;
        Ok(())
    }

    async fn upsert(&self, ns: &Namespace, docs: &[VectorDocument]) -> Result<UpsertStats> {
        if docs.is_empty() {
            return Ok(UpsertStats::default());
        }

        let table = self
            .db
            .open_table(ns.as_str())
            .execute()
            .await
            .context("opening table for upsert")?;

        let schema = Arc::new(build_schema(self.dimension));
        let batch = docs_to_batch(docs, self.dimension, &schema)?;
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));

        let mut builder = table.merge_insert(&["id"]);
        builder.when_matched_update_all(None);
        builder.when_not_matched_insert_all();
        builder
            .execute(reader)
            .await
            .context("LanceDB merge insert")?;

        Ok(UpsertStats {
            upserted: docs.len(),
            skipped: 0,
        })
    }

    async fn delete_by_ids(&self, ns: &Namespace, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let table = self
            .db
            .open_table(ns.as_str())
            .execute()
            .await
            .context("opening table for delete")?;

        let escaped: Vec<String> = ids
            .iter()
            .map(|id| format!("'{}'", escape_sql(id)))
            .collect();
        let predicate = format!("id IN ({})", escaped.join(", "));
        table
            .delete(&predicate)
            .await
            .context("LanceDB delete by ids")?;
        Ok(())
    }

    async fn delete_by_file(&self, ns: &Namespace, file_path: &str) -> Result<()> {
        let table = self
            .db
            .open_table(ns.as_str())
            .execute()
            .await
            .context("opening table for delete_by_file")?;
        let predicate = format!("file_path = '{}'", escape_sql(file_path));
        table
            .delete(&predicate)
            .await
            .context("LanceDB delete by file")?;
        Ok(())
    }

    async fn delete_by_glob(&self, ns: &Namespace, pattern: &str) -> Result<usize> {
        let table = self
            .db
            .open_table(ns.as_str())
            .execute()
            .await
            .context("opening table for delete_by_glob")?;

        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(false)
            .build()
            .context("invalid glob pattern")?
            .compile_matcher();

        let stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".into(),
                "file_path".into(),
            ]))
            .execute()
            .await
            .context("querying for glob delete")?;

        let batches: Vec<RecordBatch> = stream.try_collect().await?;

        let mut matching_ids = Vec::new();
        for batch in &batches {
            let ids = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let paths = batch
                .column_by_name("file_path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            if let (Some(ids), Some(paths)) = (ids, paths) {
                for i in 0..batch.num_rows() {
                    if !paths.is_null(i) && glob.is_match(paths.value(i)) {
                        matching_ids.push(ids.value(i).to_string());
                    }
                }
            }
        }

        let count = matching_ids.len();
        if !matching_ids.is_empty() {
            let refs: Vec<&str> = matching_ids.iter().map(|s| s.as_str()).collect();
            let escaped: Vec<String> = refs
                .iter()
                .map(|id| format!("'{}'", escape_sql(id)))
                .collect();
            let predicate = format!("id IN ({})", escaped.join(", "));
            table.delete(&predicate).await.context("glob delete")?;
        }
        Ok(count)
    }

    async fn get_content_hashes(&self, ns: &Namespace) -> Result<HashMap<String, String>> {
        let table = self
            .db
            .open_table(ns.as_str())
            .execute()
            .await
            .context("opening table for content hashes")?;

        let stream = table
            .query()
            .only_if("chunk_kind = 'file'")
            .select(lancedb::query::Select::Columns(vec![
                "file_path".into(),
                "content_hash".into(),
            ]))
            .execute()
            .await
            .context("querying content hashes")?;

        let batches: Vec<RecordBatch> = stream.try_collect().await?;

        let mut hashes = HashMap::new();
        for batch in &batches {
            let paths = batch
                .column_by_name("file_path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let hash_col = batch
                .column_by_name("content_hash")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            if let (Some(paths), Some(hashes_arr)) = (paths, hash_col) {
                for i in 0..batch.num_rows() {
                    if !paths.is_null(i) && !hashes_arr.is_null(i) {
                        hashes.insert(paths.value(i).to_string(), hashes_arr.value(i).to_string());
                    }
                }
            }
        }
        Ok(hashes)
    }

    async fn list_documents(&self, ns: &Namespace) -> Result<Vec<VectorDocument>> {
        let table = self
            .db
            .open_table(ns.as_str())
            .execute()
            .await
            .context("opening table for list_documents")?;

        let stream = table
            .query()
            .execute()
            .await
            .context("querying all documents")?;

        let batches: Vec<RecordBatch> = stream.try_collect().await?;

        let mut docs = Vec::new();
        for batch in &batches {
            let mut batch_docs = batch_to_docs(batch)?;
            docs.append(&mut batch_docs);
        }
        Ok(docs)
    }

    async fn search(
        &self,
        ns: &Namespace,
        query_vector: &[f32],
        opts: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let table = self
            .db
            .open_table(ns.as_str())
            .execute()
            .await
            .context("opening table for search")?;

        let mut query = table
            .vector_search(query_vector)
            .context("building vector search")?;

        query = query
            .limit(opts.top_k)
            .distance_type(lancedb::DistanceType::Cosine);

        if let Some(filter) = build_filter(opts) {
            query = query.only_if(filter);
        }

        let stream = query.execute().await.context("executing vector search")?;

        let batches: Vec<RecordBatch> = stream.try_collect().await?;

        let mut results = Vec::new();
        for batch in &batches {
            let distances = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());

            for i in 0..batch.num_rows() {
                let dist = distances.map(|d| d.value(i)).unwrap_or(2.0);
                let score = 1.0 - dist;

                if let Some(min) = opts.min_score
                    && score < min
                {
                    continue;
                }

                results.push(SearchResult {
                    id: get_str_col(batch, "id", i).unwrap_or_default(),
                    score,
                    file_path: get_str_col(batch, "file_path", i).unwrap_or_default(),
                    chunk_kind: parse_chunk_kind(
                        &get_str_col(batch, "chunk_kind", i).unwrap_or_default(),
                    ),
                    symbol_name: get_str_col(batch, "symbol_name", i),
                    symbol_kind: get_str_col(batch, "symbol_kind", i),
                    summary: get_str_col(batch, "summary", i).unwrap_or_default(),
                    start_line: get_u32_col(batch, "start_line", i),
                    end_line: get_u32_col(batch, "end_line", i),
                    language: get_str_col(batch, "language", i),
                    calls: get_string_list_col(batch, "calls", i),
                    called_by: get_string_list_col(batch, "called_by", i),
                });
            }
        }
        Ok(results)
    }
}

// ── Schema builders ──────────────────────────────────────────────────────

fn build_schema(dimension: i32) -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dimension,
            ),
            false,
        ),
        Field::new("summary", DataType::Utf8, false),
        Field::new("file_path", DataType::Utf8, false),
        Field::new("chunk_kind", DataType::Utf8, false),
        Field::new("symbol_name", DataType::Utf8, true),
        Field::new("symbol_kind", DataType::Utf8, true),
        Field::new("start_line", DataType::UInt32, true),
        Field::new("end_line", DataType::UInt32, true),
        Field::new("language", DataType::Utf8, true),
        Field::new("content_hash", DataType::Utf8, true),
        Field::new(
            "calls",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
        Field::new(
            "called_by",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
    ])
}

fn build_meta_schema() -> Schema {
    Schema::new(vec![
        Field::new("key", DataType::Utf8, false),
        Field::new("value", DataType::Utf8, false),
    ])
}

// ── Batch conversion ─────────────────────────────────────────────────────

fn docs_to_batch(
    docs: &[VectorDocument],
    dimension: i32,
    schema: &Arc<Schema>,
) -> Result<RecordBatch> {
    let n = docs.len();
    let dim = dimension as usize;

    let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
    let summaries: Vec<&str> = docs.iter().map(|d| d.summary.as_str()).collect();
    let file_paths: Vec<&str> = docs.iter().map(|d| d.file_path.as_str()).collect();
    let chunk_kinds: Vec<String> = docs.iter().map(|d| d.chunk_kind.to_string()).collect();
    let chunk_kind_refs: Vec<&str> = chunk_kinds.iter().map(|s| s.as_str()).collect();

    // Vector column: flatten all vectors into one array, then wrap as FixedSizeList
    let flat_values: Vec<f32> = docs
        .iter()
        .flat_map(|d| {
            let mut v = d.vector.clone();
            v.resize(dim, 0.0);
            v
        })
        .collect();
    let values_array = Float32Array::from(flat_values);
    let field = Arc::new(Field::new("item", DataType::Float32, true));
    let vector_array = FixedSizeListArray::try_new(field, dimension, Arc::new(values_array), None)
        .context("building vector FixedSizeList")?;

    // Nullable string columns
    let symbol_names: Vec<Option<&str>> = docs.iter().map(|d| d.symbol_name.as_deref()).collect();
    let symbol_kinds: Vec<Option<&str>> = docs.iter().map(|d| d.symbol_kind.as_deref()).collect();
    let languages: Vec<Option<&str>> = docs.iter().map(|d| d.language.as_deref()).collect();
    let content_hashes: Vec<Option<&str>> =
        docs.iter().map(|d| d.content_hash.as_deref()).collect();

    // Nullable u32 columns
    let start_lines: Vec<Option<u32>> = docs.iter().map(|d| d.start_line).collect();
    let end_lines: Vec<Option<u32>> = docs.iter().map(|d| d.end_line).collect();

    // List<Utf8> columns
    let calls_array = build_string_list_array(docs.iter().map(|d| d.calls.as_ref()), n);
    let called_by_array = build_string_list_array(docs.iter().map(|d| d.called_by.as_ref()), n);

    RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(vector_array),
            Arc::new(StringArray::from(summaries)),
            Arc::new(StringArray::from(file_paths)),
            Arc::new(StringArray::from(chunk_kind_refs)),
            Arc::new(StringArray::from(symbol_names)),
            Arc::new(StringArray::from(symbol_kinds)),
            Arc::new(UInt32Array::from(start_lines)),
            Arc::new(UInt32Array::from(end_lines)),
            Arc::new(StringArray::from(languages)),
            Arc::new(StringArray::from(content_hashes)),
            Arc::new(calls_array),
            Arc::new(called_by_array),
        ],
    )
    .context("building document RecordBatch")
}

fn build_string_list_array<'a>(
    iter: impl Iterator<Item = Option<&'a Vec<String>>>,
    _len: usize,
) -> ListArray {
    let mut builder = ListBuilder::new(StringBuilder::new());
    for opt in iter {
        match opt {
            Some(vec) => {
                for s in vec {
                    builder.values().append_value(s);
                }
                builder.append(true);
            }
            None => {
                builder.append(false);
            }
        }
    }
    builder.finish()
}

fn batch_to_docs(batch: &RecordBatch) -> Result<Vec<VectorDocument>> {
    let mut docs = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        let vector = get_vector_col(batch, "vector", i).unwrap_or_default();
        docs.push(VectorDocument {
            id: get_str_col(batch, "id", i).unwrap_or_default(),
            vector,
            summary: get_str_col(batch, "summary", i).unwrap_or_default(),
            file_path: get_str_col(batch, "file_path", i).unwrap_or_default(),
            chunk_kind: parse_chunk_kind(&get_str_col(batch, "chunk_kind", i).unwrap_or_default()),
            symbol_name: get_str_col(batch, "symbol_name", i),
            symbol_kind: get_str_col(batch, "symbol_kind", i),
            start_line: get_u32_col(batch, "start_line", i),
            end_line: get_u32_col(batch, "end_line", i),
            language: get_str_col(batch, "language", i),
            content_hash: get_str_col(batch, "content_hash", i),
            calls: get_string_list_col(batch, "calls", i),
            called_by: get_string_list_col(batch, "called_by", i),
        });
    }
    Ok(docs)
}

// ── Column extraction helpers ────────────────────────────────────────────

fn get_str_col(batch: &RecordBatch, name: &str, idx: usize) -> Option<String> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .and_then(|arr| {
            if arr.is_null(idx) {
                None
            } else {
                Some(arr.value(idx).to_string())
            }
        })
}

fn get_u32_col(batch: &RecordBatch, name: &str, idx: usize) -> Option<u32> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<UInt32Array>())
        .and_then(|arr| {
            if arr.is_null(idx) {
                None
            } else {
                Some(arr.value(idx))
            }
        })
}

fn get_vector_col(batch: &RecordBatch, name: &str, idx: usize) -> Option<Vec<f32>> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>())
        .and_then(|arr| {
            if arr.is_null(idx) {
                return None;
            }
            let inner = arr.value(idx);
            let float_arr = inner.as_any().downcast_ref::<Float32Array>()?;
            Some(float_arr.values().to_vec())
        })
}

fn get_string_list_col(batch: &RecordBatch, name: &str, idx: usize) -> Option<Vec<String>> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<ListArray>())
        .and_then(|arr| {
            if arr.is_null(idx) {
                return None;
            }
            let inner = arr.value(idx);
            let str_arr = inner.as_any().downcast_ref::<StringArray>()?;
            Some(
                (0..str_arr.len())
                    .map(|i| str_arr.value(i).to_string())
                    .collect(),
            )
        })
}

// ── Filter construction ──────────────────────────────────────────────────

fn build_filter(opts: &SearchOptions) -> Option<String> {
    let mut conditions = Vec::new();

    match opts.path_prefixes.len() {
        0 => {}
        1 => {
            conditions.push(format!(
                "file_path LIKE '{}%'",
                escape_sql(&opts.path_prefixes[0])
            ));
        }
        _ => {
            let clauses: Vec<String> = opts
                .path_prefixes
                .iter()
                .map(|p| format!("file_path LIKE '{}%'", escape_sql(p)))
                .collect();
            conditions.push(format!("({})", clauses.join(" OR ")));
        }
    }

    if let Some(ref kind) = opts.chunk_kind {
        conditions.push(format!("chunk_kind = '{}'", kind));
    }
    if let Some(ref lang) = opts.language {
        conditions.push(format!("language = '{}'", escape_sql(lang)));
    }

    if conditions.is_empty() {
        None
    } else {
        Some(conditions.join(" AND "))
    }
}

fn escape_sql(s: &str) -> String {
    s.replace('\'', "''")
}

fn parse_chunk_kind(s: &str) -> ChunkKind {
    match s {
        "symbol" => ChunkKind::Symbol,
        _ => ChunkKind::File,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Provider ─────────────────────────────────────────────────────

    #[test]
    fn provider_name() {
        assert_eq!(LancedbProvider.name(), "lancedb");
    }

    #[test]
    fn validate_passes_with_data_path() {
        let config = StoreConfig {
            provider: "lancedb".into(),
            turbopuffer: crate::config::TurbopufferStoreConfig {
                api_key: String::new(),
            },
            lancedb: crate::config::LancedbStoreConfig {
                data_path: "/tmp/test".into(),
            },
        };
        assert!(LancedbProvider.validate(&config).is_ok());
    }

    #[test]
    fn validate_fails_empty_data_path() {
        let config = StoreConfig {
            provider: "lancedb".into(),
            turbopuffer: crate::config::TurbopufferStoreConfig {
                api_key: String::new(),
            },
            lancedb: crate::config::LancedbStoreConfig {
                data_path: String::new(),
            },
        };
        let err = LancedbProvider.validate(&config).unwrap_err();
        assert!(err.to_string().contains("data_path"));
    }

    // ── Schema ───────────────────────────────────────────────────────

    #[test]
    fn build_schema_correct_fields() {
        let schema = build_schema(128);
        assert_eq!(schema.fields().len(), 13);
        assert_eq!(schema.field(0).name(), "id");
        assert_eq!(schema.field(1).name(), "vector");
        match schema.field(1).data_type() {
            DataType::FixedSizeList(_, dim) => assert_eq!(*dim, 128),
            other => panic!("expected FixedSizeList, got {other:?}"),
        }
    }

    #[test]
    fn build_meta_schema_correct() {
        let schema = build_meta_schema();
        assert_eq!(schema.fields().len(), 2);
        assert_eq!(schema.field(0).name(), "key");
        assert_eq!(schema.field(1).name(), "value");
    }

    // ── Filter construction ──────────────────────────────────────────

    #[test]
    fn no_opts_no_filter() {
        let opts = SearchOptions::default();
        assert!(build_filter(&opts).is_none());
    }

    #[test]
    fn single_prefix_filter() {
        let opts = SearchOptions {
            path_prefixes: vec!["src/".into()],
            ..Default::default()
        };
        assert_eq!(build_filter(&opts).unwrap(), "file_path LIKE 'src/%'");
    }

    #[test]
    fn multi_prefix_filter() {
        let opts = SearchOptions {
            path_prefixes: vec!["src/".into(), "lib/".into()],
            ..Default::default()
        };
        assert_eq!(
            build_filter(&opts).unwrap(),
            "(file_path LIKE 'src/%' OR file_path LIKE 'lib/%')"
        );
    }

    #[test]
    fn chunk_kind_filter() {
        let opts = SearchOptions {
            chunk_kind: Some(ChunkKind::File),
            ..Default::default()
        };
        assert_eq!(build_filter(&opts).unwrap(), "chunk_kind = 'file'");
    }

    #[test]
    fn combined_filter() {
        let opts = SearchOptions {
            path_prefixes: vec!["src/".into()],
            chunk_kind: Some(ChunkKind::Symbol),
            language: Some("rust".into()),
            ..Default::default()
        };
        assert_eq!(
            build_filter(&opts).unwrap(),
            "file_path LIKE 'src/%' AND chunk_kind = 'symbol' AND language = 'rust'"
        );
    }

    #[test]
    fn escape_sql_quotes() {
        assert_eq!(escape_sql("it's"), "it''s");
        assert_eq!(escape_sql("normal"), "normal");
    }

    // ── Conversion round-trip ────────────────────────────────────────

    #[test]
    fn doc_batch_round_trip() {
        let docs = vec![VectorDocument {
            id: "doc-1".into(),
            vector: vec![0.1, 0.2, 0.3, 0.4],
            summary: "A test document".into(),
            file_path: "src/main.rs".into(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some("main".into()),
            symbol_kind: Some("function".into()),
            start_line: Some(1),
            end_line: Some(10),
            language: Some("rust".into()),
            content_hash: Some("abc123".into()),
            calls: Some(vec!["foo".into(), "bar".into()]),
            called_by: Some(vec!["baz".into()]),
        }];

        let schema = Arc::new(build_schema(4));
        let batch = docs_to_batch(&docs, 4, &schema).unwrap();
        let result = batch_to_docs(&batch).unwrap();

        assert_eq!(result.len(), 1);
        let d = &result[0];
        assert_eq!(d.id, "doc-1");
        assert_eq!(d.vector, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(d.summary, "A test document");
        assert_eq!(d.file_path, "src/main.rs");
        assert_eq!(d.chunk_kind, ChunkKind::Symbol);
        assert_eq!(d.symbol_name.as_deref(), Some("main"));
        assert_eq!(d.symbol_kind.as_deref(), Some("function"));
        assert_eq!(d.start_line, Some(1));
        assert_eq!(d.end_line, Some(10));
        assert_eq!(d.language.as_deref(), Some("rust"));
        assert_eq!(d.content_hash.as_deref(), Some("abc123"));
        assert_eq!(d.calls, Some(vec!["foo".into(), "bar".into()]));
        assert_eq!(d.called_by, Some(vec!["baz".into()]));
    }

    #[test]
    fn nullable_fields_round_trip() {
        let docs = vec![VectorDocument {
            id: "min".into(),
            vector: vec![0.5, 0.5],
            summary: "Minimal".into(),
            file_path: "file.txt".into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: None,
            content_hash: None,
            calls: None,
            called_by: None,
        }];

        let schema = Arc::new(build_schema(2));
        let batch = docs_to_batch(&docs, 2, &schema).unwrap();
        let result = batch_to_docs(&batch).unwrap();

        let d = &result[0];
        assert!(d.symbol_name.is_none());
        assert!(d.symbol_kind.is_none());
        assert!(d.start_line.is_none());
        assert!(d.end_line.is_none());
        assert!(d.language.is_none());
        assert!(d.content_hash.is_none());
        assert!(d.calls.is_none());
        assert!(d.called_by.is_none());
    }

    // ── Integration tests ────────────────────────────────────────────

    fn temp_db_path(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!(
            "/tmp/wdpkr-lancedb-test-{}-{}-{nanos}",
            label,
            std::process::id()
        )
    }

    async fn make_store(label: &str) -> (LancedbStore, String) {
        let path = temp_db_path(label);
        std::fs::create_dir_all(&path).unwrap();
        let db = lancedb::connect(&path).execute().await.unwrap();
        let store = LancedbStore { db, dimension: 4 };
        (store, path)
    }

    fn test_doc(id: &str, file_path: &str, kind: ChunkKind) -> VectorDocument {
        VectorDocument {
            id: id.into(),
            vector: vec![0.1, 0.2, 0.3, 0.4],
            summary: format!("Summary of {id}"),
            file_path: file_path.into(),
            chunk_kind: kind,
            symbol_name: if kind == ChunkKind::Symbol {
                Some(id.into())
            } else {
                None
            },
            symbol_kind: if kind == ChunkKind::Symbol {
                Some("function".into())
            } else {
                None
            },
            start_line: if kind == ChunkKind::Symbol {
                Some(1)
            } else {
                None
            },
            end_line: if kind == ChunkKind::Symbol {
                Some(10)
            } else {
                None
            },
            language: Some("rust".into()),
            content_hash: if kind == ChunkKind::File {
                Some(format!("hash-{id}"))
            } else {
                None
            },
            calls: None,
            called_by: None,
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn namespace_lifecycle() {
        let (store, path) = make_store("ns-lifecycle").await;
        let ns = Namespace::from("test-ns");

        assert!(!store.namespace_exists(&ns).await.unwrap());
        store.create_namespace(&ns, 4).await.unwrap();
        assert!(store.namespace_exists(&ns).await.unwrap());
        // Idempotent create
        store.create_namespace(&ns, 4).await.unwrap();
        store.delete_namespace(&ns).await.unwrap();
        assert!(!store.namespace_exists(&ns).await.unwrap());
        // Idempotent delete
        store.delete_namespace(&ns).await.unwrap();

        std::fs::remove_dir_all(&path).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn metadata_round_trip() {
        let (store, path) = make_store("meta-rt").await;
        let ns = Namespace::from("meta-ns");
        store.create_namespace(&ns, 4).await.unwrap();

        let mut extra = HashMap::new();
        extra.insert("version".into(), "2".into());
        let meta = NamespaceMetadata {
            hwm_sha: Some("abc123".into()),
            embedder: Some("voyage/voyage-code-3".into()),
            extra,
        };
        store.set_metadata(&ns, &meta).await.unwrap();

        let loaded = store.get_metadata(&ns).await.unwrap();
        assert_eq!(loaded.hwm_sha.as_deref(), Some("abc123"));
        assert_eq!(loaded.embedder.as_deref(), Some("voyage/voyage-code-3"));
        assert_eq!(loaded.extra["version"], "2");

        // Overwrite
        let meta2 = NamespaceMetadata {
            hwm_sha: Some("def456".into()),
            ..Default::default()
        };
        store.set_metadata(&ns, &meta2).await.unwrap();
        let loaded2 = store.get_metadata(&ns).await.unwrap();
        assert_eq!(loaded2.hwm_sha.as_deref(), Some("def456"));
        assert!(loaded2.embedder.is_none());

        std::fs::remove_dir_all(&path).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn upsert_and_list() {
        let (store, path) = make_store("upsert-list").await;
        let ns = Namespace::from("upsert-ns");
        store.create_namespace(&ns, 4).await.unwrap();

        let docs = vec![
            test_doc("f1", "src/main.rs", ChunkKind::File),
            test_doc("s1", "src/main.rs", ChunkKind::Symbol),
        ];
        let stats = store.upsert(&ns, &docs).await.unwrap();
        assert_eq!(stats.upserted, 2);

        let listed = store.list_documents(&ns).await.unwrap();
        assert_eq!(listed.len(), 2);

        std::fs::remove_dir_all(&path).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn upsert_overwrites() {
        let (store, path) = make_store("upsert-overwrite").await;
        let ns = Namespace::from("overwrite-ns");
        store.create_namespace(&ns, 4).await.unwrap();

        let mut doc = test_doc("f1", "src/main.rs", ChunkKind::File);
        store.upsert(&ns, &[doc.clone()]).await.unwrap();

        doc.summary = "Updated summary".into();
        store.upsert(&ns, &[doc]).await.unwrap();

        let listed = store.list_documents(&ns).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].summary, "Updated summary");

        std::fs::remove_dir_all(&path).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn delete_by_ids_removes_docs() {
        let (store, path) = make_store("del-ids").await;
        let ns = Namespace::from("del-ns");
        store.create_namespace(&ns, 4).await.unwrap();

        let docs = vec![
            test_doc("a", "a.rs", ChunkKind::File),
            test_doc("b", "b.rs", ChunkKind::File),
            test_doc("c", "c.rs", ChunkKind::File),
        ];
        store.upsert(&ns, &docs).await.unwrap();
        store.delete_by_ids(&ns, &["a", "c"]).await.unwrap();

        let listed = store.list_documents(&ns).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "b");

        std::fs::remove_dir_all(&path).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn delete_by_file_removes_all_for_path() {
        let (store, path) = make_store("del-file").await;
        let ns = Namespace::from("del-file-ns");
        store.create_namespace(&ns, 4).await.unwrap();

        let docs = vec![
            test_doc("f1", "src/main.rs", ChunkKind::File),
            test_doc("s1", "src/main.rs", ChunkKind::Symbol),
            test_doc("f2", "src/lib.rs", ChunkKind::File),
        ];
        store.upsert(&ns, &docs).await.unwrap();
        store.delete_by_file(&ns, "src/main.rs").await.unwrap();

        let listed = store.list_documents(&ns).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].file_path, "src/lib.rs");

        std::fs::remove_dir_all(&path).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn delete_by_glob_works() {
        let (store, path) = make_store("del-glob").await;
        let ns = Namespace::from("del-glob-ns");
        store.create_namespace(&ns, 4).await.unwrap();

        let docs = vec![
            test_doc("a", "src/main.rs", ChunkKind::File),
            test_doc("b", "src/lib.rs", ChunkKind::File),
            test_doc("c", "tests/test.rs", ChunkKind::File),
        ];
        store.upsert(&ns, &docs).await.unwrap();

        let count = store.delete_by_glob(&ns, "src/*").await.unwrap();
        assert_eq!(count, 2);

        let listed = store.list_documents(&ns).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].file_path, "tests/test.rs");

        std::fs::remove_dir_all(&path).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn content_hashes_returns_file_level_only() {
        let (store, path) = make_store("hashes").await;
        let ns = Namespace::from("hash-ns");
        store.create_namespace(&ns, 4).await.unwrap();

        let docs = vec![
            test_doc("f1", "src/main.rs", ChunkKind::File),
            test_doc("s1", "src/main.rs", ChunkKind::Symbol),
            test_doc("f2", "src/lib.rs", ChunkKind::File),
        ];
        store.upsert(&ns, &docs).await.unwrap();

        let hashes = store.get_content_hashes(&ns).await.unwrap();
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes["src/main.rs"], "hash-f1");
        assert_eq!(hashes["src/lib.rs"], "hash-f2");

        std::fs::remove_dir_all(&path).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_returns_results() {
        let (store, path) = make_store("search").await;
        let ns = Namespace::from("search-ns");
        store.create_namespace(&ns, 4).await.unwrap();

        let docs = vec![
            test_doc("f1", "src/main.rs", ChunkKind::File),
            test_doc("f2", "src/lib.rs", ChunkKind::File),
        ];
        store.upsert(&ns, &docs).await.unwrap();

        let opts = SearchOptions {
            top_k: 10,
            ..Default::default()
        };
        let results = store
            .search(&ns, &[0.1, 0.2, 0.3, 0.4], &opts)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        // Both should have score ~1.0 since query matches stored vectors
        for r in &results {
            assert!(r.score > 0.9, "score was {}", r.score);
        }

        std::fs::remove_dir_all(&path).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_with_filters() {
        let (store, path) = make_store("search-filter").await;
        let ns = Namespace::from("filter-ns");
        store.create_namespace(&ns, 4).await.unwrap();

        let docs = vec![
            test_doc("f1", "src/main.rs", ChunkKind::File),
            test_doc("s1", "src/main.rs", ChunkKind::Symbol),
            test_doc("f2", "tests/test.rs", ChunkKind::File),
        ];
        store.upsert(&ns, &docs).await.unwrap();

        // Filter by path prefix
        let opts = SearchOptions {
            top_k: 10,
            path_prefixes: vec!["src/".into()],
            ..Default::default()
        };
        let results = store
            .search(&ns, &[0.1, 0.2, 0.3, 0.4], &opts)
            .await
            .unwrap();
        assert_eq!(results.len(), 2); // f1 and s1

        // Filter by chunk kind
        let opts = SearchOptions {
            top_k: 10,
            chunk_kind: Some(ChunkKind::Symbol),
            ..Default::default()
        };
        let results = store
            .search(&ns, &[0.1, 0.2, 0.3, 0.4], &opts)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_kind, ChunkKind::Symbol);

        std::fs::remove_dir_all(&path).ok();
    }
}

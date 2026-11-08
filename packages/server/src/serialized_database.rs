//! Serializes direct statements on a backend's shared connection.
//!
//! The local Turso backend runs every non-transaction statement on one shared
//! `turso::Connection`, which rejects concurrent use ("concurrent use
//! forbidden"). Concurrent HTTP handlers and per-match units reach it at the
//! same time, so direct statements take a short async lock here. Transactions
//! open their own connection and stay fully concurrent.

use std::sync::Arc;

use async_trait::async_trait;
use switchy_database::{
    Database, DatabaseError, DatabaseTransaction, DatabaseValue, Row,
    query::{
        DeleteStatement, InsertStatement, SelectQuery, UpdateStatement, UpsertMultiStatement,
        UpsertStatement,
    },
    schema,
};

/// [`Database`] adapter that runs one direct statement at a time.
#[derive(Debug)]
pub struct SerializedDatabase {
    inner: Arc<dyn Database>,
    direct: tokio::sync::Mutex<()>,
}

impl SerializedDatabase {
    /// Wraps a backend so direct statements never overlap on its shared
    /// connection.
    #[must_use]
    pub fn new(inner: Arc<dyn Database>) -> Self {
        Self {
            inner,
            direct: tokio::sync::Mutex::new(()),
        }
    }
}

#[async_trait]
impl Database for SerializedDatabase {
    async fn query(&self, query: &SelectQuery<'_>) -> Result<Vec<Row>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.query(query).await
    }

    async fn query_first(&self, query: &SelectQuery<'_>) -> Result<Option<Row>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.query_first(query).await
    }

    async fn exec_update(
        &self,
        statement: &UpdateStatement<'_>,
    ) -> Result<Vec<Row>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_update(statement).await
    }

    async fn exec_update_first(
        &self,
        statement: &UpdateStatement<'_>,
    ) -> Result<Option<Row>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_update_first(statement).await
    }

    async fn exec_insert(&self, statement: &InsertStatement<'_>) -> Result<Row, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_insert(statement).await
    }

    async fn exec_upsert(
        &self,
        statement: &UpsertStatement<'_>,
    ) -> Result<Vec<Row>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_upsert(statement).await
    }

    async fn exec_upsert_first(
        &self,
        statement: &UpsertStatement<'_>,
    ) -> Result<Row, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_upsert_first(statement).await
    }

    async fn exec_upsert_multi(
        &self,
        statement: &UpsertMultiStatement<'_>,
    ) -> Result<Vec<Row>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_upsert_multi(statement).await
    }

    async fn exec_delete(
        &self,
        statement: &DeleteStatement<'_>,
    ) -> Result<Vec<Row>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_delete(statement).await
    }

    async fn exec_delete_first(
        &self,
        statement: &DeleteStatement<'_>,
    ) -> Result<Option<Row>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_delete_first(statement).await
    }

    async fn exec_raw(&self, statement: &str) -> Result<(), DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_raw(statement).await
    }

    fn trigger_close(&self) -> Result<(), DatabaseError> {
        self.inner.trigger_close()
    }

    async fn close(&self) -> Result<(), DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.close().await
    }

    async fn exec_create_table(
        &self,
        statement: &schema::CreateTableStatement<'_>,
    ) -> Result<(), DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_create_table(statement).await
    }

    async fn exec_drop_table(
        &self,
        statement: &schema::DropTableStatement<'_>,
    ) -> Result<(), DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_drop_table(statement).await
    }

    async fn exec_create_index(
        &self,
        statement: &schema::CreateIndexStatement<'_>,
    ) -> Result<(), DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_create_index(statement).await
    }

    async fn exec_drop_index(
        &self,
        statement: &schema::DropIndexStatement<'_>,
    ) -> Result<(), DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_drop_index(statement).await
    }

    async fn exec_alter_table(
        &self,
        statement: &schema::AlterTableStatement<'_>,
    ) -> Result<(), DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_alter_table(statement).await
    }

    async fn table_exists(&self, table_name: &str) -> Result<bool, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.table_exists(table_name).await
    }

    async fn list_tables(&self) -> Result<Vec<String>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.list_tables().await
    }

    async fn get_table_info(
        &self,
        table_name: &str,
    ) -> Result<Option<schema::TableInfo>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.get_table_info(table_name).await
    }

    async fn get_table_columns(
        &self,
        table_name: &str,
    ) -> Result<Vec<schema::ColumnInfo>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.get_table_columns(table_name).await
    }

    async fn column_exists(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> Result<bool, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.column_exists(table_name, column_name).await
    }

    async fn query_raw(&self, query: &str) -> Result<Vec<Row>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.query_raw(query).await
    }

    async fn exec_raw_params(
        &self,
        query: &str,
        params: &[DatabaseValue],
    ) -> Result<u64, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.exec_raw_params(query, params).await
    }

    async fn query_raw_params(
        &self,
        query: &str,
        params: &[DatabaseValue],
    ) -> Result<Vec<Row>, DatabaseError> {
        let _direct = self.direct.lock().await;
        self.inner.query_raw_params(query, params).await
    }

    async fn begin_transaction(&self) -> Result<Box<dyn DatabaseTransaction>, DatabaseError> {
        // Each transaction owns a fresh backend connection.
        self.inner.begin_transaction().await
    }

    async fn clear_connection_cache(&self) {
        self.inner.clear_connection_cache().await;
    }
}

#[cfg(test)]
mod tests {
    use switchy_database::query::FilterableQuery as _;

    use super::*;

    #[test]
    fn concurrent_direct_statements_share_one_turso_connection_safely() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let directory = std::env::temp_dir().join(format!(
                "pwmtf-serialized-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("db.sqlite");
            let inner: Arc<dyn Database> = switchy_database_connection::builder()
                .turso()
                .with_path(path.to_str().unwrap())
                .build()
                .await
                .unwrap()
                .into();
            let db: Arc<dyn Database> = Arc::new(SerializedDatabase::new(inner));
            crate::migrate(&*db).await.unwrap();
            let mut tasks = Vec::new();
            for _ in 0..64 {
                let db = Arc::clone(&db);
                tasks.push(tokio::spawn(async move {
                    db.select("sessions")
                        .where_eq("session_hash", "absent")
                        .execute(&*db)
                        .await
                        .map(|rows| rows.len())
                }));
            }
            for task in tasks {
                assert_eq!(task.await.unwrap().unwrap(), 0);
            }
            let _ = std::fs::remove_dir_all(directory);
        });
    }
}

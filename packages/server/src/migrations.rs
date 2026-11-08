//! Ordered Switchy schema for canonical server records.

use switchy_database::{
    Database,
    query::{Expression as _, FilterableQuery as _},
    schema::{Column, DataType, alter_table, create_index, create_table, drop_index, drop_table},
};
use switchy_schema::{
    discovery::code::{CodeMigration, CodeMigrationSource},
    runner::MigrationRunner,
};

/// Returns the ordered versioned PWMTF schema source.
#[allow(clippy::too_many_lines)]
#[must_use]
pub fn migrations() -> CodeMigrationSource<'static> {
    migrations_for(true)
}

#[allow(clippy::too_many_lines)]
fn migrations_for(include_profiles: bool) -> CodeMigrationSource<'static> {
    let mut source = CodeMigrationSource::new();
    source.add_migration(table(
        "001_external_identities",
        "external_identities",
        vec![
            text("identity_id"),
            text("issuer"),
            text("subject"),
            text("account_id"),
        ],
        "identity_id",
    ));
    source.add_migration(index(
        "002_external_identity_unique",
        "idx_external_identity_issuer_subject",
        "external_identities",
        vec!["issuer", "subject"],
    ));
    source.add_migration(table(
        "003_sessions",
        "sessions",
        vec![
            text("session_hash"),
            text("account_id"),
            bigint("expires_at_ms"),
            bigint("last_used_at_ms"),
        ],
        "session_hash",
    ));
    source.add_migration(table(
        "004_waiting_lobbies",
        "waiting_lobbies",
        vec![
            text("lobby_id"),
            text("player_one_id"),
            text("player_two_id"),
            text("status"),
            nullable_text("match_id"),
        ],
        "lobby_id",
    ));
    source.add_migration(table(
        "005_challenges",
        "challenges",
        vec![
            text("challenge_id"),
            text("from_account_id"),
            text("to_account_id"),
            text("status"),
        ],
        "challenge_id",
    ));
    source.add_migration(table(
        "006_invitations",
        "invitations",
        vec![
            text("invitation_id"),
            text("creator_id"),
            text("token_hash"),
            bigint("expires_at_ms"),
            bigint("revoked"),
            nullable_text("redeemed_lobby_id"),
        ],
        "invitation_id",
    ));
    source.add_migration(index(
        "007_invitation_hash_unique",
        "idx_invitations_token_hash",
        "invitations",
        vec!["token_hash"],
    ));
    source.add_migration(table(
        "008_matches",
        "matches",
        vec![
            text("match_id"),
            text("player_one_id"),
            text("player_two_id"),
            bigint("canonical_revision"),
            text("canonical_snapshot"),
            bigint("canonical_checksum"),
            nullable_bigint("deadline_revision"),
            nullable_bigint("deadline_player"),
            nullable_bigint("deadline_at_ms"),
            nullable_text("previous_match_id"),
        ],
        "match_id",
    ));
    source.add_migration(table(
        "009_accepted_commands",
        "accepted_commands",
        vec![
            text("accepted_command_id"),
            text("match_id"),
            bigint("revision"),
            text("command_id"),
            text("actor_id"),
            text("frame"),
            text("result_payload"),
            text("snapshot"),
            bigint("checksum"),
        ],
        "accepted_command_id",
    ));
    source.add_migration(index(
        "010_command_revision_unique",
        "idx_accepted_commands_match_revision",
        "accepted_commands",
        vec!["match_id", "revision"],
    ));
    source.add_migration(index(
        "011_command_id_unique",
        "idx_accepted_commands_match_command",
        "accepted_commands",
        vec!["match_id", "command_id"],
    ));
    if include_profiles {
        source.add_migration(table(
            "012_account_profiles",
            "account_profiles",
            vec![text("account_id"), text("handle")],
            "account_id",
        ));
        source.add_migration(index(
            "013_profile_handle_unique",
            "idx_account_profiles_handle",
            "account_profiles",
            vec!["handle"],
        ));
        source.add_migration(add_text_column(
            "014_command_deadline_revision",
            "accepted_commands",
            "deadline_revision",
        ));
        source.add_migration(add_text_column(
            "015_command_deadline_player",
            "accepted_commands",
            "deadline_player",
        ));
        source.add_migration(add_text_column(
            "016_command_deadline_at",
            "accepted_commands",
            "deadline_at_ms",
        ));
        source.add_migration(table(
            "017_oidc_attempts",
            "oidc_attempts",
            vec![
                text("attempt_id"),
                text("state_hash"),
                text("browser_binding_hash"),
                text("nonce"),
                text("pkce_verifier"),
                bigint("expires_at_ms"),
                text("status"),
            ],
            "attempt_id",
        ));
        source.add_migration(index(
            "018_oidc_state_unique",
            "idx_oidc_attempts_state",
            "oidc_attempts",
            vec!["state_hash"],
        ));
        source.add_migration(table(
            "019_match_summaries",
            "match_summaries",
            vec![
                text("match_id"),
                text("winner_account_id"),
                text("completion_reason"),
                bigint("canonical_revision"),
                text("canonical_checksum"),
            ],
            "match_id",
        ));
        source.add_migration(table(
            "020_rematch_offers",
            "rematch_offers",
            vec![
                text("previous_match_id"),
                text("offered_by_account_id"),
                nullable_text("accepted_match_id"),
            ],
            "previous_match_id",
        ));
        source.add_migration(table(
            "021_lobby_readiness",
            "lobby_readiness",
            vec![
                text("lobby_id"),
                text("account_id"),
                text("connection_id"),
                bigint("ready"),
                bigint("last_seen_at_ms"),
            ],
            "connection_id",
        ));
        source.add_migration(index(
            "022_lobby_readiness_membership",
            "idx_lobby_readiness_lobby_connection",
            "lobby_readiness",
            vec!["lobby_id", "connection_id"],
        ));
        source.add_migration(add_text_column(
            "023_match_configuration",
            "matches",
            "match_configuration",
        ));
        source.add_migration(index(
            "024_pending_challenge_pair_unique",
            "idx_challenges_from_to_status",
            "challenges",
            vec!["from_account_id", "to_account_id", "status"],
        ));
    }
    source
}

/// Runs all ordered PWMTF migrations.
///
/// # Errors
///
/// Returns a Switchy schema error when discovery or execution fails.
pub async fn migrate(db: &dyn Database) -> switchy_schema::Result<()> {
    migrate_source(db, migrations()).await?;
    backfill_match_configurations(db).await
}

async fn backfill_match_configurations(db: &dyn Database) -> switchy_schema::Result<()> {
    let rows = db.select("matches").execute(db).await?;
    for row in rows {
        let Some(configuration) = row.get("match_configuration") else {
            return Err(switchy_schema::MigrationError::Validation(
                "matches.match_configuration is absent".to_owned(),
            ));
        };
        if !configuration.is_null() {
            continue;
        }
        let match_id = migration_text(&row, "match_id")?;
        let snapshot = migration_decode(&migration_text(&row, "canonical_snapshot")?)?;
        let state = pwmtf_eight_ball::MatchState::from_bytes(&snapshot).map_err(|_| {
            switchy_schema::MigrationError::Validation(
                "canonical match snapshot is incompatible".to_owned(),
            )
        })?;
        let updated = db
            .update("matches")
            .value(
                "match_configuration",
                migration_encode(&state.configuration().to_bytes()),
            )
            .where_eq("match_id", match_id)
            .where_eq("match_configuration", Option::<String>::None)
            .execute(db)
            .await?;
        if updated.len() != 1 {
            return Err(switchy_schema::MigrationError::Validation(
                "match configuration backfill did not update exactly one row".to_owned(),
            ));
        }
    }
    Ok(())
}

fn migration_text(row: &switchy_database::Row, column: &str) -> switchy_schema::Result<String> {
    row.get(column)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or_else(|| {
            switchy_schema::MigrationError::Validation(format!("matches.{column} is malformed"))
        })
}

fn migration_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn migration_decode(value: &str) -> switchy_schema::Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return Err(switchy_schema::MigrationError::Validation(
            "canonical match snapshot encoding is malformed".to_owned(),
        ));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = migration_hex(pair[0])?;
            let low = migration_hex(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn migration_hex(value: u8) -> switchy_schema::Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(switchy_schema::MigrationError::Validation(
            "canonical match snapshot encoding is malformed".to_owned(),
        )),
    }
}

async fn migrate_source(
    db: &dyn Database,
    source: CodeMigrationSource<'static>,
) -> switchy_schema::Result<()> {
    MigrationRunner::new(Box::new(source))
        .with_table_name("__pwmtf_migrations")
        .run(db)
        .await
}

fn add_text_column(id: &str, table: &'static str, name: &'static str) -> CodeMigration<'static> {
    CodeMigration::new(
        id.to_owned(),
        Box::new(alter_table(table).add_column(name.to_owned(), DataType::Text, true, None)),
        Some(Box::new(alter_table(table).drop_column(name.to_owned()))),
    )
}

fn table(
    id: &str,
    name: &'static str,
    columns: Vec<Column>,
    primary_key: &'static str,
) -> CodeMigration<'static> {
    let mut statement = create_table(name);
    for column in columns {
        statement = statement.column(column);
    }
    CodeMigration::new(
        id.to_owned(),
        Box::new(statement.primary_key(primary_key)),
        Some(Box::new(drop_table(name).if_exists(true))),
    )
}

fn index(
    id: &str,
    name: &'static str,
    table: &'static str,
    columns: Vec<&'static str>,
) -> CodeMigration<'static> {
    CodeMigration::new(
        id.to_owned(),
        Box::new(
            create_index(name)
                .table(table)
                .columns(columns)
                .unique(true),
        ),
        Some(Box::new(drop_index(name, table).if_exists())),
    )
}

fn text(name: &str) -> Column {
    column(name, DataType::Text, false)
}
fn nullable_text(name: &str) -> Column {
    column(name, DataType::Text, true)
}
fn bigint(name: &str) -> Column {
    column(name, DataType::BigInt, false)
}
fn nullable_bigint(name: &str) -> Column {
    column(name, DataType::BigInt, true)
}
fn column(name: &str, data_type: DataType, nullable: bool) -> Column {
    Column {
        name: name.to_owned(),
        nullable,
        auto_increment: false,
        data_type,
        default: None,
    }
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };
    use switchy_schema::migration::MigrationSource as _;

    use super::*;

    #[test]
    fn migrations_execute_on_in_memory_turso() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("in-memory Turso opens");
            migrate(&*db).await.expect("schema migrates");
            migrate(&*db).await.expect("schema migration is idempotent");
        });
    }

    #[test]
    fn file_backed_database_upgrades_from_previous_schema() {
        block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "pwmtf-migration-upgrade-{}-{unique}.db",
                std::process::id()
            ));
            let db = switchy_database_connection::builder()
                .turso()
                .with_path(&path)
                .build()
                .await
                .expect("file-backed Turso opens");

            migrate_source(&*db, migrations_for(false))
                .await
                .expect("previous schema migrates");
            assert!(
                !db.list_tables()
                    .await
                    .unwrap()
                    .contains(&"account_profiles".to_owned())
            );
            migrate(&*db).await.expect("current schema upgrades");
            assert!(
                db.list_tables()
                    .await
                    .unwrap()
                    .contains(&"account_profiles".to_owned())
            );
            migrate(&*db).await.expect("upgraded schema is idempotent");
            db.close().await.expect("database closes");
            fs::remove_file(&path).expect("database file is removed");
        });
    }

    #[test]
    fn existing_match_rows_receive_pinned_configuration_on_upgrade() {
        block_on(async {
            use pwmtf_billiards::{PhysicsProfile, RackSeed, TableGeometry};
            use pwmtf_eight_ball::{MatchState, Player, RulesProfile};

            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_source(&*db, migrations_for(false)).await.unwrap();
            let state = MatchState::new(
                RulesProfile::standard(),
                PhysicsProfile::standard(),
                TableGeometry::standard(),
                RackSeed::new(42),
                Player::One,
            )
            .unwrap();
            db.insert("matches")
                .value("match_id", "9")
                .value("player_one_id", "1")
                .value("player_two_id", "2")
                .value("canonical_revision", 0_i64)
                .value("canonical_snapshot", migration_encode(&state.to_bytes()))
                .value(
                    "canonical_checksum",
                    i64::from_ne_bytes(state.checksum().to_ne_bytes()),
                )
                .value("deadline_revision", Option::<i64>::None)
                .value("deadline_player", Option::<i64>::None)
                .value("deadline_at_ms", Option::<i64>::None)
                .execute(&*db)
                .await
                .unwrap();

            migrate(&*db).await.unwrap();
            let row = db
                .select("matches")
                .where_eq("match_id", "9")
                .execute(&*db)
                .await
                .unwrap()
                .remove(0);
            assert_eq!(
                migration_text(&row, "match_configuration").unwrap(),
                migration_encode(&state.configuration().to_bytes())
            );
        });
    }

    #[test]
    fn migration_ids_are_ordered_and_unique() {
        let source = migrations();
        let migrations = block_on(source.migrations()).expect("migrations are discoverable");
        let ids = migrations
            .iter()
            .map(|migration| migration.id())
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 24);
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    }
}

//! Seeds one accepted durable command for native restart smoke qualification.

use std::{path::PathBuf, sync::Arc};

use pwmtf_game_domain::{
    MatchCommand, MatchState, PhysicsProfile, Player, RackSeed, RulesProfile, TableGeometry,
    VersionedMatchCommand,
};
use pwmtf_protocol::{CommandEnvelope, CommandId};
use pwmtf_server::{
    AccountId, DeadlineMillis, MatchId, MatchService, Participants, Session, SessionCookiePolicy,
    SwitchyCommandJournal, create_stored_session, migrate,
};
use switchy_database::query::FilterableQuery as _;
use thiserror::Error;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("native smoke seeding failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), SeedError> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or(SeedError::Usage)?;
    let db: Arc<dyn switchy_database::Database> = switchy_database_connection::builder()
        .turso()
        .with_path(path)
        .with_busy_timeout(std::time::Duration::from_secs(5))
        .build()
        .await?
        .into();
    migrate(&*db).await?;
    let state = MatchState::new(
        RulesProfile::standard(),
        PhysicsProfile::standard(),
        TableGeometry::standard(),
        RackSeed::new(7),
        Player::One,
    )?;
    let match_id = MatchId::new(7_001);
    let participants = Participants {
        player_one: AccountId::new(7_101),
        player_two: AccountId::new(7_102),
    };
    insert_initial_match(&*db, match_id, participants, &state).await?;
    let timeout = CommandEnvelope::new(
        0,
        CommandId::new([0x51; 16]),
        VersionedMatchCommand::new(MatchCommand::Timeout),
    );
    let concession = CommandEnvelope::new(
        1,
        CommandId::new([0x52; 16]),
        VersionedMatchCommand::new(MatchCommand::Concede {
            player: Player::Two,
        }),
    );
    let mut service = MatchService::new(SwitchyCommandJournal::new(Arc::clone(&db)));
    service.insert_match(match_id, participants, state);
    service
        .apply_at(
            match_id,
            participants.player_one,
            timeout,
            DeadlineMillis::new(1_000),
        )
        .await?;
    service
        .apply_at(
            match_id,
            participants.player_two,
            concession,
            DeadlineMillis::new(2_000),
        )
        .await?;
    let head = SwitchyCommandJournal::new(Arc::clone(&db))
        .canonical_head(match_id)
        .await?;
    if head.1 != 2
        || !matches!(
            head.2.status(),
            pwmtf_game_domain::MatchStatus::Completed(outcome)
                if outcome.winner == Player::One
                    && outcome.reason == pwmtf_game_domain::CompletionReason::Concession
        )
        || head.3.is_some()
    {
        return Err(SeedError::Integrity);
    }
    let live_match_id = MatchId::new(7_002);
    let live_state = MatchState::new(
        RulesProfile::standard(),
        PhysicsProfile::standard(),
        TableGeometry::standard(),
        RackSeed::new(8),
        Player::One,
    )?;
    insert_initial_match(&*db, live_match_id, participants, &live_state).await?;
    db.update("matches")
        .value("deadline_revision", 0_i64)
        .value("deadline_player", 1_i64)
        .value(
            "deadline_at_ms",
            i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| SeedError::Integrity)?
                    .as_millis()
                    .saturating_add(3_600_000),
            )
            .map_err(|_| SeedError::Integrity)?,
        )
        .where_eq("match_id", live_match_id.value().to_string())
        .execute(&*db)
        .await?;
    let deadline_match_id = MatchId::new(7_003);
    let deadline_state = MatchState::new(
        RulesProfile::standard(),
        PhysicsProfile::standard(),
        TableGeometry::standard(),
        RackSeed::new(9),
        Player::One,
    )?;
    insert_initial_match(&*db, deadline_match_id, participants, &deadline_state).await?;
    db.update("matches")
        .value("deadline_revision", 0_i64)
        .value("deadline_player", 1_i64)
        .value(
            "deadline_at_ms",
            i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| SeedError::Integrity)?
                    .as_millis()
                    .saturating_add(75_000),
            )
            .map_err(|_| SeedError::Integrity)?,
        )
        .where_eq("match_id", deadline_match_id.value().to_string())
        .execute(&*db)
        .await?;
    let live_command = CommandEnvelope::new(
        0,
        CommandId::new([0x53; 16]),
        VersionedMatchCommand::new(MatchCommand::Timeout),
    );
    let live_terminal_command = CommandEnvelope::new(
        1,
        CommandId::new([0x54; 16]),
        VersionedMatchCommand::new(MatchCommand::Concede {
            player: Player::Two,
        }),
    );

    let player_one_token = create_stored_session(
        &*db,
        Session {
            account: participants.player_one,
            expires_at: u64::MAX / 2,
            last_used_at: 0,
        },
    )
    .await?;
    let player_two_token = create_stored_session(
        &*db,
        Session {
            account: participants.player_two,
            expires_at: u64::MAX / 2,
            last_used_at: 0,
        },
    )
    .await?;
    let policy = SessionCookiePolicy::production();
    let player_one_cookie = policy.set_cookie(&player_one_token, 3_600)?;
    let player_two_cookie = policy.set_cookie(&player_two_token, 3_600)?;
    println!("match_id={}", match_id.value());
    println!("deadline_match_id={}", deadline_match_id.value());
    println!("live_match_id={}", live_match_id.value());
    println!("live_command={}", encode_bytes(&live_command.to_bytes()));
    println!(
        "live_terminal_command={}",
        encode_bytes(&live_terminal_command.to_bytes())
    );
    println!(
        "player_one_cookie={}",
        player_one_cookie
            .split(';')
            .next()
            .ok_or(SeedError::Integrity)?
    );
    println!(
        "player_two_cookie={}",
        player_two_cookie
            .split(';')
            .next()
            .ok_or(SeedError::Integrity)?
    );
    Ok(())
}

async fn insert_initial_match(
    db: &dyn switchy_database::Database,
    match_id: MatchId,
    participants: Participants,
    state: &MatchState,
) -> Result<(), SeedError> {
    use switchy_database::DatabaseValue;
    db.insert("matches")
        .value("match_id", match_id.value().to_string())
        .value("player_one_id", participants.player_one.value().to_string())
        .value("player_two_id", participants.player_two.value().to_string())
        .value("canonical_revision", 0_i64)
        .value("canonical_snapshot", encode_bytes(&state.to_bytes()))
        .value("canonical_checksum", checksum_i64(state.checksum()))
        .value(
            "match_configuration",
            encode_bytes(&state.configuration().to_bytes()),
        )
        .value("deadline_revision", DatabaseValue::Null)
        .value("deadline_player", DatabaseValue::Null)
        .value("deadline_at_ms", DatabaseValue::Null)
        .execute(db)
        .await?;
    Ok(())
}

fn encode_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

const fn checksum_i64(value: u64) -> i64 {
    i64::from_be_bytes(value.to_be_bytes())
}

#[derive(Debug, Error)]
enum SeedError {
    #[error("expected one database path argument")]
    Usage,
    #[error("database initialization failed")]
    DatabaseInitialization(#[from] switchy_database_connection::InitTursoError),
    #[error("database migration failed")]
    Migration(#[from] switchy_schema::MigrationError),
    #[error("database write failed")]
    Database(#[from] switchy_database::DatabaseError),
    #[error("canonical match construction failed")]
    Match(#[from] pwmtf_game_domain::MatchError),
    #[error("authoritative command failed")]
    Command(#[from] pwmtf_server::CommandError),
    #[error("durable journal failed")]
    Journal(#[from] pwmtf_server::JournalError),
    #[error("session persistence failed")]
    Session(#[from] pwmtf_server::SessionStoreError),
    #[error("session cookie construction failed")]
    Cookie(#[from] pwmtf_server::CookieError),
    #[error("seeded durable head failed integrity verification")]
    Integrity,
}

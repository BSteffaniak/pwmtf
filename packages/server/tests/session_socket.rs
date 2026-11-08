//! Native end-to-end qualification of the version-two session socket: a real
//! axum server on loopback and raw WebSocket clients speaking
//! `ClientMessage`/`ServerMessage`.

use std::{
    io::{Read as _, Write as _},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use futures_util::{SinkExt as _, StreamExt as _};
use pwmtf_billiards::{Aim, PhysicsProfile, ShotPower, Spin, VersionedShotCommand};
use pwmtf_eight_ball::{MatchCommand, MatchState, Player, RulesProfile, VersionedMatchCommand};
use pwmtf_protocol::{
    ClientMessage, CommandEnvelope, CommandId, MatchStateMessage, RejectReason, SESSION_VERSION,
    ServerMessage, ShotPreview, SubscriptionKind,
};
use pwmtf_server::{
    AccountId, CANONICAL_ORIGIN, DeadlineMillis, HttpState, MatchId, Session, SessionCookiePolicy,
    SwitchyCommandJournal, create_stored_session, http_router, migrate,
};
use switchy_database::{Database, DatabaseValue};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{self, Message, client::IntoClientRequest as _, http::HeaderValue},
};

const PLAYER_ONE: AccountId = AccountId::new(1_001);
const PLAYER_TWO: AccountId = AccountId::new(1_002);
const OUTSIDER: AccountId = AccountId::new(1_003);
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(10);
const SILENCE: Duration = Duration::from_millis(300);

struct Harness {
    addr: SocketAddr,
    db: Arc<dyn Database>,
    state: Arc<HttpState>,
    one: String,
    two: String,
    outsider: String,
}

async fn cookie(db: &dyn Database, account: AccountId) -> String {
    let token = create_stored_session(
        db,
        Session {
            account,
            expires_at: u64::MAX / 2,
            last_used_at: 0,
        },
    )
    .await
    .unwrap();
    SessionCookiePolicy::production()
        .set_cookie(&token, 3_600)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn harness() -> Harness {
    let db: Arc<dyn Database> = switchy_database_connection::builder()
        .turso()
        .with_in_memory()
        .build()
        .await
        .unwrap()
        .into();
    migrate(&*db).await.unwrap();
    let one = cookie(&*db, PLAYER_ONE).await;
    let two = cookie(&*db, PLAYER_TWO).await;
    let outsider = cookie(&*db, OUTSIDER).await;
    let state = Arc::new(HttpState::without_identity_provider(
        Arc::clone(&db),
        [CANONICAL_ORIGIN],
    ));
    state.reset_lobby_presence().await.unwrap();
    state.recover_all_matches().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = http_router(Arc::clone(&state));
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Harness {
        addr,
        db,
        state,
        one,
        two,
        outsider,
    }
}

async fn insert_lobby(db: &dyn Database, lobby_id: u128) {
    db.insert("waiting_lobbies")
        .value("lobby_id", lobby_id.to_string())
        .value("player_one_id", PLAYER_ONE.value().to_string())
        .value("player_two_id", PLAYER_TWO.value().to_string())
        .value("status", "waiting")
        .value("match_id", DatabaseValue::Null)
        .execute(db)
        .await
        .unwrap();
}

/// Minimal blocking HTTP/1.1 client for state-changing lobby requests.
async fn http(addr: SocketAddr, method: &str, path: &str, cookie: &str) -> (u16, String) {
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nOrigin: {CANONICAL_ORIGIN}\r\nCookie: {cookie}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap();
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or_default();
        (status, body)
    })
    .await
    .unwrap()
}

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn open(addr: SocketAddr, cookie: Option<&str>) -> Result<Socket, tungstenite::Error> {
    let mut request = format!("ws://{addr}/ws").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", HeaderValue::from_static(CANONICAL_ORIGIN));
    if let Some(cookie) = cookie {
        request
            .headers_mut()
            .insert("cookie", HeaderValue::from_str(cookie).unwrap());
    }
    connect_async(request).await.map(|(socket, _)| socket)
}

struct Client {
    socket: Socket,
}

impl Client {
    async fn connect(addr: SocketAddr, cookie: &str) -> Self {
        let mut client = Self {
            socket: open(addr, Some(cookie)).await.unwrap(),
        };
        client
            .send(ClientMessage::Hello {
                versions: vec![1, SESSION_VERSION],
            })
            .await;
        match client.recv().await {
            ServerMessage::Welcome { version, .. } => assert_eq!(version, SESSION_VERSION),
            other => panic!("expected welcome, got {other:?}"),
        }
        client
    }

    async fn send(&mut self, message: ClientMessage) {
        self.socket
            .send(Message::binary(message.to_bytes().unwrap()))
            .await
            .unwrap();
    }

    async fn next(&mut self, wait: Duration) -> Option<ServerMessage> {
        loop {
            let frame = tokio::time::timeout(wait, self.socket.next())
                .await
                .ok()??;
            match frame.ok()? {
                Message::Binary(bytes) => return Some(ServerMessage::from_bytes(&bytes).unwrap()),
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                Message::Text(_) => panic!("server sent a text frame"),
                Message::Close(_) => return None,
            }
        }
    }

    async fn recv(&mut self) -> ServerMessage {
        self.next(RECEIVE_TIMEOUT)
            .await
            .expect("expected a server message")
    }

    /// Skips unrelated notifications until a message satisfying `select`.
    async fn recv_match<T>(&mut self, mut select: impl FnMut(ServerMessage) -> Option<T>) -> T {
        loop {
            if let Some(value) = select(self.recv().await) {
                return value;
            }
        }
    }

    async fn match_state(&mut self) -> MatchStateMessage {
        self.recv_match(|message| match message {
            ServerMessage::MatchState(state) => Some(state),
            _ => None,
        })
        .await
    }

    /// Asserts that no message of the selected kind arrives for a short while.
    async fn assert_no(&mut self, mut unwanted: impl FnMut(&ServerMessage) -> bool) {
        let deadline = tokio::time::Instant::now() + SILENCE;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return;
            }
            match self.next(remaining).await {
                Some(message) => assert!(!unwanted(&message), "unexpected {message:?}"),
                None => return,
            }
        }
    }

    async fn subscribe(&mut self, match_id: MatchId) -> MatchStateMessage {
        self.send(ClientMessage::SubscribeMatch {
            match_id: match_id.value(),
        })
        .await;
        self.match_state().await
    }

    async fn command(&mut self, match_id: MatchId, envelope: CommandEnvelope) {
        self.send(ClientMessage::Command {
            match_id: match_id.value(),
            envelope,
        })
        .await;
    }

    async fn closed(&mut self) -> bool {
        self.next(RECEIVE_TIMEOUT).await.is_none()
    }
}

fn break_shot(revision: u64, id: u8) -> CommandEnvelope {
    CommandEnvelope::new(
        revision,
        CommandId::new([id; 16]),
        VersionedMatchCommand::new(MatchCommand::PlayShot {
            shot: VersionedShotCommand::new(
                Aim::new(0).unwrap(),
                ShotPower::new(ShotPower::MAX).unwrap(),
                Spin::CENTER,
            ),
            called_pocket: None,
        }),
    )
}

impl Client {
    /// Subscribes to a lobby and waits for this join's own change notice,
    /// which is published only after the presence is registered.
    async fn join_lobby(&mut self, lobby_id: u128) {
        self.send(ClientMessage::SubscribeLobby { lobby_id }).await;
        self.recv_match(|message| {
            matches!(message, ServerMessage::LobbyChanged { lobby_id: changed } if changed == lobby_id)
                .then_some(())
        })
        .await;
    }

    async fn rejection(&mut self) -> RejectReason {
        self.recv_match(|message| match message {
            ServerMessage::Rejected { reason, .. } => Some(reason),
            _ => None,
        })
        .await
    }
}

/// Drives a lobby to `MatchStarted` and returns the new match.
async fn start_match(harness: &Harness, lobby_id: u128) -> (MatchId, Client, Client) {
    insert_lobby(&*harness.db, lobby_id).await;
    let mut one = Client::connect(harness.addr, &harness.one).await;
    one.join_lobby(lobby_id).await;
    let mut two = Client::connect(harness.addr, &harness.two).await;
    two.join_lobby(lobby_id).await;
    let path = format!("/api/lobbies/{lobby_id}/ready");
    assert_eq!(http(harness.addr, "POST", &path, &harness.one).await.0, 200);
    assert_eq!(http(harness.addr, "POST", &path, &harness.two).await.0, 200);
    let started = |message| match message {
        ServerMessage::MatchStarted {
            lobby_id: started,
            match_id,
        } if started == lobby_id => Some(MatchId::new(match_id)),
        _ => None,
    };
    let match_id = one.recv_match(started).await;
    assert_eq!(two.recv_match(started).await, match_id);
    (match_id, one, two)
}

#[tokio::test(flavor = "multi_thread")]
async fn handshake_is_authenticated_binary_and_version_negotiated() {
    let harness = harness().await;
    match open(harness.addr, None).await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("unauthenticated upgrade must be refused, got {other:?}"),
    }

    let mut text = Client {
        socket: open(harness.addr, Some(&harness.one)).await.unwrap(),
    };
    text.socket.send(Message::text("2")).await.unwrap();
    assert!(text.closed().await);

    let mut legacy = Client {
        socket: open(harness.addr, Some(&harness.one)).await.unwrap(),
    };
    legacy
        .send(ClientMessage::Hello { versions: vec![1] })
        .await;
    assert!(legacy.closed().await);

    let mut early = Client {
        socket: open(harness.addr, Some(&harness.one)).await.unwrap(),
    };
    early.send(ClientMessage::Ping { nonce: 1 }).await;
    assert!(early.closed().await);

    let mut client = Client::connect(harness.addr, &harness.one).await;
    client.send(ClientMessage::Ping { nonce: 77 }).await;
    assert!(matches!(
        client.recv().await,
        ServerMessage::Pong { nonce: 77, .. }
    ));
    client
        .socket
        .send(Message::binary(vec![0xee]))
        .await
        .unwrap();
    assert!(client.closed().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn lobby_presence_and_explicit_ready_start_the_match() {
    let harness = harness().await;
    let lobby_id = 41;
    insert_lobby(&*harness.db, lobby_id).await;
    let path = format!("/api/lobbies/{lobby_id}/ready");

    let mut outsider = Client::connect(harness.addr, &harness.outsider).await;
    outsider
        .send(ClientMessage::SubscribeLobby { lobby_id })
        .await;
    assert_eq!(
        outsider.recv().await,
        ServerMessage::SubscriptionDenied {
            kind: SubscriptionKind::Lobby,
            id: lobby_id,
        }
    );

    // Readiness requires live presence.
    assert_eq!(http(harness.addr, "POST", &path, &harness.one).await.0, 409);

    let mut one = Client::connect(harness.addr, &harness.one).await;
    one.join_lobby(lobby_id).await;
    let (status, body) = http(harness.addr, "POST", &path, &harness.one).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"self_ready\":true"), "{body}");

    // Losing the last presence clears readiness.
    drop(one);
    let lobby_path = format!("/api/lobbies/{lobby_id}");
    let mut released = false;
    for _ in 0..250 {
        let (_, body) = http(harness.addr, "GET", &lobby_path, &harness.two).await;
        if body.contains("\"opponent_present\":false") {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(released, "closed socket must release lobby presence");
    let mut two = Client::connect(harness.addr, &harness.two).await;
    two.join_lobby(lobby_id).await;
    let mut one = Client::connect(harness.addr, &harness.one).await;
    one.join_lobby(lobby_id).await;
    let (_, body) = http(
        harness.addr,
        "GET",
        &format!("/api/lobbies/{lobby_id}"),
        &harness.two,
    )
    .await;
    assert!(body.contains("\"opponent_present\":true"), "{body}");
    assert!(body.contains("\"opponent_ready\":false"), "{body}");
    let (status, body) = http(harness.addr, "POST", &path, &harness.two).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"status\":\"waiting\""), "{body}");
    one.assert_no(|message| matches!(message, ServerMessage::MatchStarted { .. }))
        .await;

    let (status, body) = http(harness.addr, "POST", &path, &harness.one).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"status\":\"started\""), "{body}");
    let started = |message| match message {
        ServerMessage::MatchStarted {
            lobby_id: started,
            match_id,
        } if started == lobby_id => Some(match_id),
        _ => None,
    };
    let match_id = one.recv_match(started).await;
    assert_eq!(two.recv_match(started).await, match_id);
    two.recv_match(|message| matches!(message, ServerMessage::SocialChanged).then_some(()))
        .await;
    let state = one.subscribe(MatchId::new(match_id)).await;
    assert_eq!(state.revision, 0);
    assert_eq!(state.seat, Player::One);
}

#[tokio::test(flavor = "multi_thread")]
async fn shots_broadcast_transitions_previews_route_to_opponent_and_rejections_carry_reasons() {
    let harness = harness().await;
    let (match_id, mut one, mut two) = start_match(&harness, 51).await;

    let mut outsider = Client::connect(harness.addr, &harness.outsider).await;
    outsider
        .send(ClientMessage::SubscribeMatch {
            match_id: match_id.value(),
        })
        .await;
    assert_eq!(
        outsider.recv().await,
        ServerMessage::SubscriptionDenied {
            kind: SubscriptionKind::Match,
            id: match_id.value(),
        }
    );

    // Commands require a subscription on this connection.
    one.command(match_id, break_shot(0, 1)).await;
    assert_eq!(one.rejection().await, RejectReason::NotSubscribed);

    let initial = one.subscribe(match_id).await;
    assert_eq!((initial.revision, initial.seat), (0, Player::One));
    assert!(initial.last_command.is_none());
    let observed = two.subscribe(match_id).await;
    assert_eq!((observed.revision, observed.seat), (0, Player::Two));
    let mut one_other_tab = Client::connect(harness.addr, &harness.one).await;
    one_other_tab.subscribe(match_id).await;

    // Previews reach only the other participant.
    let preview = ShotPreview {
        revision: 0,
        gesture: 9,
        sequence: 1,
        aim: Aim::new(100).unwrap(),
        power: ShotPower::new(500).unwrap(),
        spin: Spin::CENTER,
        cancel: false,
    };
    one.send(ClientMessage::Preview {
        match_id: match_id.value(),
        preview,
    })
    .await;
    assert_eq!(
        two.recv_match(|message| match message {
            ServerMessage::Preview { match_id, preview } => Some((match_id, preview)),
            _ => None,
        })
        .await,
        (match_id.value(), preview)
    );
    one_other_tab
        .assert_no(|message| matches!(message, ServerMessage::Preview { .. }))
        .await;
    one.assert_no(|message| matches!(message, ServerMessage::Preview { .. }))
        .await;
    // The inactive seat cannot preview.
    two.send(ClientMessage::Preview {
        match_id: match_id.value(),
        preview: ShotPreview {
            sequence: 2,
            ..preview
        },
    })
    .await;
    one.assert_no(|message| matches!(message, ServerMessage::Preview { .. }))
        .await;

    // Wrong turn is rejected with a reason and followed by fresh state.
    two.command(match_id, break_shot(0, 2)).await;
    assert_eq!(two.rejection().await, RejectReason::WrongTurn);
    let after_reject = two.match_state().await;
    assert_eq!(after_reject.revision, 0);
    assert!(after_reject.last_command.is_none());

    // An accepted shot is broadcast with its transition and playback length.
    let sent_at = unix_millis();
    let shot = break_shot(0, 3);
    one.command(match_id, shot).await;
    let shooter_view = one.match_state().await;
    let observer_view = two.match_state().await;
    assert_eq!(one_other_tab.match_state().await.revision, 1);
    for view in [&shooter_view, &observer_view] {
        assert_eq!(view.revision, 1);
        let last = view.last_command.expect("accepted transition is described");
        assert_eq!(last.command_id, shot.command_id);
        assert_eq!(last.actor, Player::One);
        assert_eq!(last.command, shot.command);
        assert!(last.playback_ticks > 0);
    }
    assert_eq!(shooter_view.seat, Player::One);
    assert_eq!(observer_view.seat, Player::Two);

    // Observer replay from its revision-zero table converges exactly.
    let mut replay = MatchState::from_bytes(&observed.snapshot).unwrap();
    let applied = replay.apply_command_timed(shot.command).unwrap();
    assert_eq!(replay.checksum(), observer_view.checksum);
    assert_eq!(replay.to_bytes(), observer_view.snapshot);
    let playback_ticks = observer_view.last_command.unwrap().playback_ticks;
    assert_eq!(applied.playback_ticks, playback_ticks);

    // The next deadline starts after the shot's authoritative playback.
    let offset = PhysicsProfile::standard().playback_millis(playback_ticks)
        + RulesProfile::standard().turn_duration_millis().unwrap();
    let deadline = observer_view.deadline_at_ms.unwrap();
    assert!(
        deadline >= sent_at + offset,
        "{deadline} < {sent_at} + {offset}"
    );
    assert!(deadline <= observer_view.server_now_ms + offset);
    assert!(PhysicsProfile::standard().playback_millis(playback_ticks) > 0);

    // Stale revision and idempotency conflict.
    one.command(match_id, break_shot(0, 4)).await;
    assert_eq!(one.rejection().await, RejectReason::StaleRevision);
    assert_eq!(one.match_state().await.revision, 1);
    one.command(match_id, break_shot(1, 3)).await;
    assert_eq!(one.rejection().await, RejectReason::IdempotencyConflict);
    assert_eq!(one.match_state().await.revision, 1);

    // The authoritative timeout is broadcast with its own transition.
    let active = MatchState::from_bytes(&observer_view.snapshot)
        .unwrap()
        .active_player();
    let applied = harness
        .state
        .poll_due_deadlines(DeadlineMillis::new(deadline))
        .await
        .unwrap();
    assert_eq!(applied, 1);
    let timed_out = two.match_state().await;
    assert_eq!(timed_out.revision, 2);
    let last = timed_out.last_command.unwrap();
    assert_eq!(last.actor, active);
    assert_eq!(
        last.command,
        VersionedMatchCommand::new(MatchCommand::Timeout)
    );
    assert_eq!(last.playback_ticks, 0);
    assert_eq!(
        timed_out.deadline_at_ms,
        Some(deadline + RulesProfile::standard().turn_duration_millis().unwrap())
    );
    assert_eq!(one.match_state().await.revision, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_retransmit_after_reconnect_is_accepted_exactly_once() {
    let harness = harness().await;
    let (match_id, mut one, mut two) = start_match(&harness, 61).await;
    one.subscribe(match_id).await;
    two.subscribe(match_id).await;
    let shot = break_shot(0, 7);
    one.command(match_id, shot).await;
    // The shooter loses its socket before reading the confirmation.
    drop(one);
    let broadcast = two.match_state().await;
    assert_eq!(broadcast.revision, 1);

    let mut one = Client::connect(harness.addr, &harness.one).await;
    let resumed = one.subscribe(match_id).await;
    assert_eq!(resumed.revision, 1);
    assert!(resumed.last_command.is_none());
    one.command(match_id, shot).await;
    let duplicate = one.recv().await;
    let ServerMessage::MatchState(duplicate) = duplicate else {
        panic!("duplicate retransmit must not be rejected: {duplicate:?}");
    };
    assert_eq!(duplicate.revision, 1);
    assert_eq!(duplicate.checksum, broadcast.checksum);
    assert!(duplicate.last_command.is_none());
    two.assert_no(|message| matches!(message, ServerMessage::MatchState(_)))
        .await;
    let head = SwitchyCommandJournal::new(Arc::clone(&harness.db))
        .canonical_head(match_id)
        .await
        .unwrap();
    assert_eq!(head.1, 1);
}

fn unix_millis() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn client_timeouts_and_reserved_timeout_ids_are_refused() {
    let harness = harness().await;
    let (match_id, mut one, mut two) = start_match(&harness, 71).await;
    one.subscribe(match_id).await;
    two.subscribe(match_id).await;

    // The idle seat cannot end the active player's turn.
    let timeout = CommandEnvelope::new(
        0,
        CommandId::new([0x71; 16]),
        VersionedMatchCommand::new(MatchCommand::Timeout),
    );
    two.command(match_id, timeout).await;
    assert_eq!(two.rejection().await, RejectReason::IllegalMove);
    assert_eq!(two.match_state().await.revision, 0);

    // A real command cannot occupy the next synthetic timeout's identity.
    let mut reserved = break_shot(0, 0);
    reserved.command_id = pwmtf_server::timeout_command_id(pwmtf_server::DeadlineId {
        revision: 1,
        player: Player::Two,
    });
    assert!(pwmtf_server::is_timeout_command_id(reserved.command_id));
    one.command(match_id, reserved).await;
    assert_eq!(one.rejection().await, RejectReason::Malformed);
    assert_eq!(one.match_state().await.revision, 0);
    one.assert_no(|message| matches!(message, ServerMessage::MatchState(_)))
        .await;
    two.assert_no(|message| matches!(message, ServerMessage::MatchState(_)))
        .await;
    let head = SwitchyCommandJournal::new(Arc::clone(&harness.db))
        .canonical_head(match_id)
        .await
        .unwrap();
    assert_eq!(head.1, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn back_to_back_retransmit_delivers_the_transition_before_the_duplicate() {
    let harness = harness().await;
    let (match_id, mut one, mut two) = start_match(&harness, 81).await;
    one.subscribe(match_id).await;
    two.subscribe(match_id).await;
    let shot = break_shot(0, 9);
    one.command(match_id, shot).await;
    one.command(match_id, shot).await;
    let transition = one.match_state().await;
    assert_eq!(transition.revision, 1);
    assert_eq!(
        transition.last_command.map(|last| last.command_id),
        Some(shot.command_id)
    );
    let duplicate = one.match_state().await;
    assert_eq!(duplicate.revision, 1);
    assert!(duplicate.last_command.is_none());
    assert!(two.match_state().await.last_command.is_some());
    one.assert_no(|message| matches!(message, ServerMessage::MatchState(_)))
        .await;
}

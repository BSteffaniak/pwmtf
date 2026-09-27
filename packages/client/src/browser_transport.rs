//! WASM-to-browser WebSocket bridge with bounded reconnect lifecycle.

use std::cell::RefCell;

use pwmtf_protocol::{PREVIEW_MARKER, ShotPreview};

use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::{JsCast as _, closure::Closure};
use web_sys::{BinaryType, CloseEvent, ErrorEvent, Event, MessageEvent, WebSocket};

use crate::transport::{BrowserTransport, ConnectionStatus};

thread_local! {
    static SOCKET: RefCell<Option<WebSocket>> = const { RefCell::new(None) };
    static TRANSPORT: RefCell<BrowserTransport> = RefCell::new(BrowserTransport::default());
    static REMOTE_PREVIEW: RefCell<Option<(ShotPreview, f64)>> = const { RefCell::new(None) };
    static LAST_SENT: RefCell<Option<f64>> = const { RefCell::new(None) };
    static LAST_RECEIVED_SEQUENCE: RefCell<Option<(u64, u32)>> = const { RefCell::new(None) };
    static PENDING_CANCEL: RefCell<Option<(ShotPreview, f64)>> = const { RefCell::new(None) };
    static REMOTE_LAST: RefCell<Option<(u64, u64, u32, bool)>> = const { RefCell::new(None) };
}

/// Monotonic browser clock in milliseconds; never use `std::time::Instant` on WASM.
pub fn monotonic_millis() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}

/// Connects the browser socket to a server-authorized match subscription URL.
///
/// The URL must be same-origin `ws:`/`wss:` and include server-owned match
/// subscription identifiers. Authentication remains in the secure session cookie.
///
/// # Errors
///
/// Returns a JavaScript exception for malformed URLs or socket construction failure.
pub fn connect(url: &str) -> Result<(), wasm_bindgen::JsValue> {
    disconnect();
    TRANSPORT.with(|transport| transport.borrow_mut().connecting());
    let socket = WebSocket::new(url)?;
    socket.set_binary_type(BinaryType::Arraybuffer);

    let opened_socket = socket.clone();
    let on_open = Closure::<dyn FnMut(Event)>::new(move |_| {
        let offer = TRANSPORT.with(|transport| transport.borrow_mut().opened());
        if opened_socket.send_with_str(&offer).is_err() {
            TRANSPORT.with(|transport| transport.borrow_mut().disconnected());
            let _ = opened_socket.close();
        }
    });
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    on_open.forget();

    let message_socket = socket.clone();
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        if let Some(text) = event.data().as_string() {
            let result = TRANSPORT.with(|transport| {
                let mut transport = transport.borrow_mut();
                if transport.status() == ConnectionStatus::Ready {
                    if text == "rejected" {
                        transport.command_rejected();
                    }
                    Ok(())
                } else {
                    transport.negotiated(&text)
                }
            });
            if result.is_err() {
                TRANSPORT.with(|transport| {
                    let mut transport = transport.borrow_mut();
                    if transport.status() != ConnectionStatus::Backoff {
                        transport.disconnected();
                    }
                });
                let _ = message_socket.close();
            }
            return;
        }
        let Ok(buffer) = event.data().dyn_into::<ArrayBuffer>() else {
            TRANSPORT.with(|transport| transport.borrow_mut().disconnected());
            let _ = message_socket.close();
            return;
        };
        let bytes = Uint8Array::new(&buffer).to_vec();
        if bytes.first() == Some(&PREVIEW_MARKER) {
            if let Ok(preview) = ShotPreview::from_bytes(&bytes) {
                let eligible = TRANSPORT.with(|transport| {
                    let transport = transport.borrow();
                    transport.status() == ConnectionStatus::Ready
                        && !transport.accepts_active_player_command()
                        && !transport.has_pending_prediction()
                        && transport.authoritative_state().is_some_and(|state| {
                            matches!(state.status(), pwmtf_game_domain::MatchStatus::InProgress)
                                && transport
                                    .local_player()
                                    .is_some_and(|seat| seat != state.active_player())
                        })
                        && transport.prediction().is_some_and(|prediction| {
                            prediction.authoritative_revision() == preview.revision
                        })
                });
                if eligible {
                    let fresh = LAST_RECEIVED_SEQUENCE.with(|last| {
                        let mut last = last.borrow_mut();
                        if last.is_some_and(|(revision, sequence)| {
                            revision == preview.revision && preview.sequence <= sequence
                        }) {
                            false
                        } else {
                            *last = Some((preview.revision, preview.sequence));
                            true
                        }
                    });
                    if !fresh {
                        return;
                    }
                    let permitted = REMOTE_LAST.with(|last| {
                        last.borrow()
                            .is_none_or(|(revision, gesture, _, canceled)| {
                                revision != preview.revision
                                    || preview.cancel
                                    || gesture != preview.gesture
                                    || !canceled
                            })
                    });
                    if !permitted {
                        return;
                    }
                    REMOTE_LAST.with(|last| {
                        *last.borrow_mut() = Some((
                            preview.revision,
                            preview.gesture,
                            preview.sequence,
                            preview.cancel,
                        ))
                    });
                    REMOTE_PREVIEW.with(|stored| {
                        *stored.borrow_mut() = Some((preview, monotonic_millis()));
                    });
                }
            }
            return;
        }
        let old_revision = TRANSPORT.with(|transport| {
            transport
                .borrow()
                .prediction()
                .map(crate::prediction::PredictionState::authoritative_revision)
        });
        let result = TRANSPORT.with(|transport| transport.borrow_mut().receive_snapshot(&bytes));
        let new_revision = TRANSPORT.with(|transport| {
            transport
                .borrow()
                .prediction()
                .map(crate::prediction::PredictionState::authoritative_revision)
        });
        if old_revision != new_revision || result.is_err() {
            REMOTE_PREVIEW.with(|stored| *stored.borrow_mut() = None);
            REMOTE_LAST.with(|last| *last.borrow_mut() = None);
            LAST_RECEIVED_SEQUENCE.with(|last| *last.borrow_mut() = None);
        }
        if result.is_err() {
            TRANSPORT.with(|transport| transport.borrow_mut().disconnected());
            let _ = message_socket.close();
        }
    });
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    let on_error = Closure::<dyn FnMut(ErrorEvent)>::new(move |_| {
        TRANSPORT.with(|transport| transport.borrow_mut().disconnected());
        REMOTE_PREVIEW.with(|preview| *preview.borrow_mut() = None);
        REMOTE_LAST.with(|last| *last.borrow_mut() = None);
    });
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();

    let on_close = Closure::<dyn FnMut(CloseEvent)>::new(move |_| {
        TRANSPORT.with(|transport| transport.borrow_mut().disconnected());
        REMOTE_PREVIEW.with(|preview| *preview.borrow_mut() = None);
        REMOTE_LAST.with(|last| *last.borrow_mut() = None);
        SOCKET.with(|socket| socket.borrow_mut().take());
    });
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    on_close.forget();

    SOCKET.with(|stored| *stored.borrow_mut() = Some(socket));
    Ok(())
}

/// Returns the current short-lived opponent planning state, if still valid.
pub fn remote_preview() -> Option<ShotPreview> {
    REMOTE_PREVIEW.with(|stored| {
        let mut stored = stored.borrow_mut();
        let valid = TRANSPORT.with(|transport| {
            let transport = transport.borrow();
            transport.status() == ConnectionStatus::Ready
                && !transport.has_pending_prediction()
                && transport.authoritative_state().is_some_and(|state| {
                    matches!(state.status(), pwmtf_game_domain::MatchStatus::InProgress)
                        && transport
                            .local_player()
                            .is_some_and(|seat| seat != state.active_player())
                })
                && stored.as_ref().is_some_and(|(value, _)| {
                    transport.prediction().is_some_and(|prediction| {
                        prediction.authoritative_revision() == value.revision
                    })
                })
        });
        if !valid
            || stored
                .as_ref()
                .is_some_and(|(_, at)| monotonic_millis() - *at > 650.0)
        {
            *stored = None;
        }
        stored
            .as_ref()
            .and_then(|(value, _)| (!value.cancel).then_some(*value))
    })
}

/// Sends one locally quantized, non-authoritative planning update.
///
/// # Errors
/// Returns a JavaScript exception if the socket is unavailable.
pub fn send_preview(value: ShotPreview) -> Result<(), wasm_bindgen::JsValue> {
    let ready = TRANSPORT.with(|transport| transport.borrow().accepts_active_player_command());
    if !ready {
        return Err(wasm_bindgen::JsValue::from_str(
            "shot preview is not permitted",
        ));
    }
    if !value.cancel {
        PENDING_CANCEL.with(|pending| *pending.borrow_mut() = None);
    }
    let frame = value.to_bytes();
    send_command(&frame)?;
    LAST_SENT.with(|sent| *sent.borrow_mut() = Some(monotonic_millis()));
    if value.cancel {
        PENDING_CANCEL.with(|pending| *pending.borrow_mut() = Some((value, monotonic_millis())));
    }
    Ok(())
}

/// Repeats the latest preview at a bounded cadence so an idle drag stays visible.
///
/// # Errors
/// Returns a JavaScript exception if the socket cannot send the frame.
pub fn resend_preview(value: ShotPreview) -> Result<bool, wasm_bindgen::JsValue> {
    if !TRANSPORT.with(|transport| transport.borrow().accepts_active_player_command()) {
        return Ok(false);
    }
    let should_send = LAST_SENT.with(|sent| {
        sent.borrow()
            .is_none_or(|at| monotonic_millis() - at >= 45.0)
    });
    if !should_send {
        return Ok(false);
    }
    send_command(&value.to_bytes())?;
    LAST_SENT.with(|sent| *sent.borrow_mut() = Some(monotonic_millis()));
    Ok(true)
}

/// Retransmits a cancel briefly to overcome packet loss, then expires.
/// Returns the transmitted sequence so the next gesture cannot reuse it.
pub fn retry_pending_cancel() -> Option<u32> {
    let cancel = PENDING_CANCEL.with(|pending| {
        let mut pending = pending.borrow_mut();
        if pending
            .as_ref()
            .is_some_and(|(_, started)| monotonic_millis() - *started > 600.0)
        {
            *pending = None;
            return None;
        }
        if pending.as_ref().is_some_and(|_| {
            LAST_SENT.with(|sent| {
                sent.borrow()
                    .is_some_and(|at| monotonic_millis() - at > 65.0)
            })
        }) {
            pending.take()
        } else {
            None
        }
    });
    if let Some((mut cancel, started)) = cancel {
        cancel.sequence = cancel.sequence.saturating_add(1);
        if send_preview(cancel).is_ok() {
            PENDING_CANCEL.with(|pending| *pending.borrow_mut() = Some((cancel, started)));
            return Some(cancel.sequence);
        }
    }
    None
}

/// Drops a canceled preview when a durable shot supersedes it.
pub fn forget_pending_cancel() {
    PENDING_CANCEL.with(|pending| *pending.borrow_mut() = None);
}

/// Sends one bounded, already-authorized frame.
///
/// # Errors
///
/// Returns a JavaScript exception unless the protocol lifecycle is ready or
/// browser socket transmission fails.
pub fn send_command(frame: &[u8]) -> Result<(), wasm_bindgen::JsValue> {
    if TRANSPORT.with(|transport| transport.borrow().status()) != ConnectionStatus::Ready {
        return Err(wasm_bindgen::JsValue::from_str("PWMTF socket is not ready"));
    }
    SOCKET.with(|socket| {
        socket
            .borrow()
            .as_ref()
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("PWMTF socket is disconnected"))?
            .send_with_u8_array(frame)
    })
}

fn send_predicted_frame(frame: &[u8]) -> Result<(), wasm_bindgen::JsValue> {
    let result = send_command(frame);
    if result.is_err() {
        TRANSPORT.with(|transport| transport.borrow_mut().disconnected());
    }
    result
}

/// Predicts and sends one bounded cue-ball placement command.
///
/// # Errors
///
/// Returns a JavaScript exception unless the protocol lifecycle is ready,
/// prediction fails, or browser socket transmission fails.
pub fn predict_and_send_cue_ball_placement(
    command_id: pwmtf_protocol::CommandId,
    position: pwmtf_game_domain::Vector,
) -> Result<(), wasm_bindgen::JsValue> {
    let frame = TRANSPORT
        .with(|transport| {
            transport
                .borrow_mut()
                .predict_cue_ball_placement(command_id, position)
                .map(pwmtf_protocol::CommandEnvelope::to_bytes)
        })
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
    send_predicted_frame(&frame)
}

/// Predicts and sends one bounded shot command.
///
/// # Errors
///
/// Returns a JavaScript exception unless the protocol lifecycle is ready,
/// prediction fails, or browser socket transmission fails.
pub fn predict_and_send_shot(
    command_id: pwmtf_protocol::CommandId,
    shot: pwmtf_game_domain::VersionedShotCommand,
    called_pocket: Option<pwmtf_game_domain::PocketId>,
) -> Result<(), wasm_bindgen::JsValue> {
    let frame = TRANSPORT
        .with(|transport| {
            transport
                .borrow_mut()
                .predict_shot(command_id, shot, called_pocket)
                .map(pwmtf_protocol::CommandEnvelope::to_bytes)
        })
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
    PENDING_CANCEL.with(|pending| *pending.borrow_mut() = None);
    LAST_SENT.with(|sent| *sent.borrow_mut() = None);
    send_predicted_frame(&frame)
}

/// Predicts and sends one explicit concession command.
///
/// # Errors
///
/// Returns a JavaScript exception unless the protocol lifecycle is ready,
/// prediction fails, or browser socket transmission fails.
pub fn predict_and_send_concession(
    command_id: pwmtf_protocol::CommandId,
    player: pwmtf_game_domain::Player,
) -> Result<(), wasm_bindgen::JsValue> {
    let frame = TRANSPORT
        .with(|transport| {
            transport
                .borrow_mut()
                .predict_concession(command_id, player)
                .map(pwmtf_protocol::CommandEnvelope::to_bytes)
        })
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
    send_predicted_frame(&frame)
}

/// Closes and forgets the current browser socket and predicted in-flight work.
pub fn disconnect() {
    SOCKET.with(|socket| {
        if let Some(socket) = socket.borrow_mut().take() {
            socket.set_onopen(None);
            socket.set_onmessage(None);
            socket.set_onerror(None);
            socket.set_onclose(None);
            let _ = socket.close();
        }
    });
    TRANSPORT.with(|transport| transport.borrow_mut().disconnected());
    REMOTE_PREVIEW.with(|preview| *preview.borrow_mut() = None);
    PENDING_CANCEL.with(|pending| *pending.borrow_mut() = None);
    REMOTE_LAST.with(|last| *last.borrow_mut() = None);
    LAST_RECEIVED_SEQUENCE.with(|last| *last.borrow_mut() = None);
    LAST_SENT.with(|sent| *sent.borrow_mut() = None);
}

/// Returns whether presentation has one unresolved predicted command.
#[must_use]
pub fn has_pending_prediction() -> bool {
    TRANSPORT.with(|transport| transport.borrow().has_pending_prediction())
}

/// Sets the participant seat derived from authenticated durable membership.
pub fn set_local_player(player: pwmtf_game_domain::Player) {
    TRANSPORT.with(|transport| transport.borrow_mut().set_local_player(player));
}

/// Returns whether the local participant may submit active-player commands.
#[must_use]
pub fn accepts_active_player_command() -> bool {
    TRANSPORT.with(|transport| transport.borrow().accepts_active_player_command())
}

/// Returns and clears whether the authoritative server rejected the latest command.
#[must_use]
pub fn take_command_rejected() -> bool {
    TRANSPORT.with(|transport| transport.borrow_mut().take_command_rejected())
}

/// Returns whether the authoritative match accepts live gameplay commands.
#[must_use]
pub fn accepts_gameplay_commands() -> bool {
    TRANSPORT.with(|transport| transport.borrow().accepts_gameplay_commands())
}

/// Returns whether authoritative state permits cue-ball placement.
#[must_use]
pub fn ball_in_hand() -> bool {
    TRANSPORT.with(|transport| {
        transport
            .borrow()
            .prediction()
            .is_some_and(|prediction| prediction.predicted().ball_in_hand())
    })
}

/// Returns the latest authoritative match presentation facts.
#[must_use]
pub fn authoritative_match_info() -> Option<(
    pwmtf_game_domain::Player,
    Option<pwmtf_game_domain::MatchOutcome>,
)> {
    TRANSPORT.with(|transport| {
        let transport = transport.borrow();
        let state = transport.authoritative_state()?;
        let outcome = match state.status() {
            pwmtf_game_domain::MatchStatus::InProgress => None,
            pwmtf_game_domain::MatchStatus::Completed(outcome) => Some(outcome),
        };
        Some((state.active_player(), outcome))
    })
}

/// Returns the authoritative revision after snapshot initialization.
#[must_use]
pub fn authoritative_revision() -> Option<u64> {
    TRANSPORT.with(|transport| {
        transport
            .borrow()
            .prediction()
            .map(crate::prediction::PredictionState::authoritative_revision)
    })
}

/// Returns the current canonical/predicted state checksum after initialization.
#[must_use]
pub fn predicted_checksum() -> Option<u64> {
    TRANSPORT.with(|transport| {
        transport
            .borrow()
            .prediction()
            .map(|prediction| prediction.predicted().checksum())
    })
}

/// Returns an isolated solver input for optional exact aiming previews.
#[must_use]
pub fn preview_state() -> Option<(
    pwmtf_game_domain::VersionedTableState,
    pwmtf_game_domain::PhysicsProfile,
    pwmtf_game_domain::TableGeometry,
)> {
    TRANSPORT.with(|transport| {
        let transport = transport.borrow();
        let state = transport.prediction()?.predicted();
        Some((state.table().clone(), state.physics(), state.geometry()))
    })
}

/// Returns one predicted canonical ball's position and pocket state.
#[must_use]
pub fn predicted_ball(number: u8) -> Option<(pwmtf_game_domain::Vector, bool)> {
    TRANSPORT.with(|transport| {
        transport
            .borrow()
            .prediction()?
            .predicted()
            .table()
            .balls()
            .iter()
            .find(|ball| ball.id.number() == number)
            .map(|ball| (ball.position, ball.pocketed))
    })
}

/// Returns the current retry delay for JavaScript lifecycle scheduling.
#[must_use]
pub fn retry_delay_ms() -> u64 {
    TRANSPORT.with(|transport| transport.borrow().retry_delay_ms())
}

/// Returns whether the failed socket lifecycle should be reconnected.
#[must_use]
pub fn needs_reconnect() -> bool {
    TRANSPORT.with(|transport| transport.borrow().status() == ConnectionStatus::Backoff)
}

/// Returns whether an authoritative snapshot initialized the live socket.
#[must_use]
pub fn is_ready() -> bool {
    TRANSPORT.with(|transport| transport.borrow().status() == ConnectionStatus::Ready)
}

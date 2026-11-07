//! Thin web-sys glue for [`SessionTransport`]: owns the `WebSocket`, the
//! single wake-up timer, and the JavaScript event listener. All protocol and
//! session decisions live in the pure state machine.

use std::cell::{Cell, RefCell};

use js_sys::{ArrayBuffer, Function, Uint8Array};
use pwmtf_protocol::ShotPreview;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

use crate::transport::{SessionTransport, TransportAction, TransportClientError};

thread_local! {
    static TRANSPORT: RefCell<SessionTransport> = RefCell::new(SessionTransport::default());
    static SOCKET: RefCell<Option<WebSocket>> = const { RefCell::new(None) };
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    static LISTENER: RefCell<Option<Function>> = const { RefCell::new(None) };
    static TIMER: Cell<Option<i32>> = const { Cell::new(None) };
    static TIMER_CALLBACK: RefCell<Option<Closure<dyn FnMut()>>> = const { RefCell::new(None) };
    static DRIVING: Cell<bool> = const { Cell::new(false) };
}

/// Monotonic browser clock in milliseconds; never use `std::time::Instant` on WASM.
#[must_use]
pub fn monotonic_millis() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}

fn wall_millis() -> f64 {
    js_sys::Date::now()
}

/// Reads the session state machine.
pub fn with<R>(f: impl FnOnce(&SessionTransport) -> R) -> R {
    TRANSPORT.with(|transport| f(&transport.borrow()))
}

/// Mutates the session state machine, then performs its queued actions,
/// delivers its events, and reschedules the wake-up timer.
pub fn with_mut<R>(f: impl FnOnce(&mut SessionTransport) -> R) -> R {
    let result = TRANSPORT.with(|transport| f(&mut transport.borrow_mut()));
    drive();
    result
}

/// Registers the page listener, called as `callback(name, detailJson)`, and
/// replays the current connection and match state to it.
pub fn set_listener(callback: Function) {
    LISTENER.with(|listener| *listener.borrow_mut() = Some(callback));
    with_mut(SessionTransport::replay_state_events);
}

fn drive() {
    // Listener callbacks may re-enter exports; the outer loop drains their work.
    if DRIVING.with(|driving| driving.replace(true)) {
        return;
    }
    loop {
        let (actions, events) = TRANSPORT.with(|transport| {
            let mut transport = transport.borrow_mut();
            (transport.take_actions(), transport.take_events())
        });
        if actions.is_empty() && events.is_empty() {
            break;
        }
        for action in actions {
            perform(action);
        }
        let listener = LISTENER.with(|listener| listener.borrow().clone());
        if let Some(listener) = listener {
            for event in events {
                let _ = listener.call2(
                    &JsValue::NULL,
                    &JsValue::from_str(event.name),
                    &JsValue::from_str(&event.detail),
                );
            }
        }
    }
    DRIVING.with(|driving| driving.set(false));
    reschedule();
}

fn perform(action: TransportAction) {
    match action {
        TransportAction::Open => {
            close_socket();
            if open_socket().is_err() {
                TRANSPORT.with(|transport| {
                    transport.borrow_mut().socket_closed(monotonic_millis());
                });
            }
        }
        TransportAction::Close => close_socket(),
        TransportAction::Send(frame) => {
            let sent = SOCKET.with(|socket| {
                socket
                    .borrow()
                    .as_ref()
                    .is_some_and(|socket| socket.send_with_u8_array(&frame).is_ok())
            });
            if !sent {
                TRANSPORT.with(|transport| {
                    transport.borrow_mut().socket_closed(monotonic_millis());
                });
            }
        }
    }
}

fn socket_url() -> Result<String, JsValue> {
    let location = web_sys::window()
        .ok_or_else(|| JsValue::from_str("window is unavailable"))?
        .location();
    let scheme = if location.protocol()? == "https:" {
        "wss:"
    } else {
        "ws:"
    };
    Ok(format!("{scheme}//{}/ws", location.host()?))
}

fn open_socket() -> Result<(), JsValue> {
    let socket = WebSocket::new(&socket_url()?)?;
    socket.set_binary_type(BinaryType::Arraybuffer);
    let generation = GENERATION.with(|value| {
        let next = value.get().wrapping_add(1);
        value.set(next);
        next
    });
    let current = move || GENERATION.with(Cell::get) == generation;

    let on_open = Closure::<dyn FnMut(Event)>::new(move |_| {
        if current() {
            with_mut(|transport| transport.socket_opened(monotonic_millis(), wall_millis()));
        }
    });
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    on_open.forget();

    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        if !current() {
            return;
        }
        let now = monotonic_millis();
        match event.data().dyn_into::<ArrayBuffer>() {
            Ok(buffer) => {
                let bytes = Uint8Array::new(&buffer).to_vec();
                with_mut(|transport| transport.receive(&bytes, now));
            }
            // Text frames are a protocol violation in session version 2.
            Err(_) => with_mut(|transport| transport.protocol_violation(now)),
        }
    });
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    let on_close = Closure::<dyn FnMut(CloseEvent)>::new(move |_| {
        if current() {
            SOCKET.with(|socket| socket.borrow_mut().take());
            with_mut(|transport| transport.socket_closed(monotonic_millis()));
        }
    });
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    on_close.forget();

    let on_error = Closure::<dyn FnMut(Event)>::new(move |_| {
        if current() {
            with_mut(|transport| transport.socket_closed(monotonic_millis()));
        }
    });
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();

    SOCKET.with(|stored| *stored.borrow_mut() = Some(socket));
    Ok(())
}

fn close_socket() {
    // Invalidate the old socket's callbacks before closing it.
    GENERATION.with(|value| value.set(value.get().wrapping_add(1)));
    if let Some(socket) = SOCKET.with(|socket| socket.borrow_mut().take()) {
        socket.set_onopen(None);
        socket.set_onmessage(None);
        socket.set_onerror(None);
        socket.set_onclose(None);
        let _ = socket.close();
    }
}

fn reschedule() {
    let Some(window) = web_sys::window() else {
        return;
    };
    if let Some(handle) = TIMER.with(Cell::take) {
        window.clear_timeout_with_handle(handle);
    }
    let Some(wakeup) = with(SessionTransport::next_wakeup) else {
        return;
    };
    TIMER_CALLBACK.with(|callback| {
        let mut callback = callback.borrow_mut();
        let callback = callback.get_or_insert_with(|| {
            Closure::<dyn FnMut()>::new(|| {
                TIMER.with(Cell::take);
                with_mut(|transport| transport.poll(monotonic_millis(), wall_millis()));
            })
        });
        #[allow(clippy::cast_possible_truncation)]
        let delay = (wakeup - monotonic_millis()).clamp(0.0, 60_000.0).ceil() as i32;
        if let Ok(handle) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            delay,
        ) {
            TIMER.with(|timer| timer.set(Some(handle)));
        }
    });
}

fn js_error(error: &TransportClientError) -> JsValue {
    JsValue::from_str(&error.to_string())
}

/// Predicts and sends one cue-ball placement.
///
/// # Errors
///
/// Returns a JavaScript exception when the session refuses the command.
pub fn submit_cue_ball_placement(
    command_id: pwmtf_protocol::CommandId,
    position: pwmtf_game_domain::Vector,
) -> Result<(), JsValue> {
    with_mut(|transport| transport.submit_cue_ball_placement(command_id, position))
        .map_err(|error| js_error(&error))
}

/// Predicts and sends one shot, starting the shooter's playback.
///
/// # Errors
///
/// Returns a JavaScript exception when the session refuses the command.
pub fn submit_shot(
    command_id: pwmtf_protocol::CommandId,
    shot: pwmtf_game_domain::VersionedShotCommand,
    called_pocket: Option<pwmtf_game_domain::PocketId>,
) -> Result<(), JsValue> {
    with_mut(|transport| transport.submit_shot(command_id, shot, called_pocket))
        .map_err(|error| js_error(&error))
}

/// Predicts and sends one explicit concession.
///
/// # Errors
///
/// Returns a JavaScript exception when the session refuses the command.
pub fn submit_concession(
    command_id: pwmtf_protocol::CommandId,
    player: pwmtf_game_domain::Player,
) -> Result<(), JsValue> {
    with_mut(|transport| transport.submit_concession(command_id, player))
        .map_err(|error| js_error(&error))
}

/// Returns the opponent's current planning preview, if still valid.
#[must_use]
pub fn remote_preview() -> Option<ShotPreview> {
    let now = monotonic_millis();
    TRANSPORT.with(|transport| transport.borrow_mut().remote_preview(now))
}

/// Sends one locally quantized, non-authoritative planning update.
///
/// # Errors
///
/// Returns a JavaScript exception unless the local seat may act now.
pub fn send_preview(value: ShotPreview) -> Result<(), JsValue> {
    let now = monotonic_millis();
    with_mut(|transport| transport.send_preview(value, now)).map_err(|error| js_error(&error))
}

/// Repeats the latest preview at a bounded cadence.
///
/// # Errors
///
/// Returns a JavaScript exception without a subscribed match.
pub fn resend_preview(value: ShotPreview) -> Result<bool, JsValue> {
    let now = monotonic_millis();
    with_mut(|transport| transport.resend_preview(value, now)).map_err(|error| js_error(&error))
}

/// Cancels a charged preview before the browser suspends rendering.
pub fn cancel_preview_on_interruption() {
    let now = monotonic_millis();
    with_mut(|transport| transport.cancel_preview_on_interruption(now));
}

/// Retransmits a pending cancel; returns the transmitted sequence.
#[must_use]
pub fn retry_pending_cancel() -> Option<u32> {
    let now = monotonic_millis();
    with_mut(|transport| transport.retry_pending_cancel(now))
}

/// Advances shot playback to the current frame; `true` once when it ends.
#[must_use]
pub fn advance_playback() -> bool {
    let now = monotonic_millis();
    with_mut(|transport| transport.advance_playback(now))
}

/// Returns whether the local seat may shoot or place now.
#[must_use]
pub fn accepts_active_player_command() -> bool {
    with(SessionTransport::accepts_active_player_command)
}

/// Returns whether predicted state grants ball-in-hand.
#[must_use]
pub fn ball_in_hand() -> bool {
    with(SessionTransport::ball_in_hand)
}

/// Returns the latest authoritative revision.
#[must_use]
pub fn authoritative_revision() -> Option<u64> {
    with(SessionTransport::authoritative_revision)
}

/// Returns the predicted state's checksum.
#[must_use]
pub fn predicted_checksum() -> Option<u64> {
    with(SessionTransport::predicted_checksum)
}

/// Returns an isolated solver input for optional exact aiming previews.
#[must_use]
pub fn preview_state() -> Option<(
    pwmtf_game_domain::VersionedTableState,
    pwmtf_game_domain::PhysicsProfile,
    pwmtf_game_domain::TableGeometry,
)> {
    with(SessionTransport::preview_state)
}

/// Returns the interrupted preview sequence until the pointer is released.
#[must_use]
pub fn interrupted_preview_sequence() -> Option<u32> {
    with(SessionTransport::interrupted_preview_sequence)
}

/// Clears the interruption latch after the held pointer is released.
pub fn rearm_interrupted_gesture() {
    TRANSPORT.with(|transport| transport.borrow_mut().rearm_interrupted_gesture());
}

/// Drops a canceled preview when a new gesture or a shot supersedes it.
pub fn forget_pending_cancel() {
    TRANSPORT.with(|transport| transport.borrow_mut().forget_pending_cancel());
}

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

#[cfg(target_arch = "wasm32")]
pub mod browser_transport;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::wasm_bindgen;
mod aim_preview;
mod ball_art;
mod cue_art;
mod exact_preview;
pub mod playback;
pub mod prediction;
mod sandbox;
pub mod transport;

use bevy::{
    camera::{OrthographicProjection, Projection, ScalingMode},
    prelude::*,
    window::{PresentMode, WindowFocused, WindowResolution},
};

const DESIGN_SIZE: Vec2 = Vec2::new(1220.0, 590.0);
const TABLE_CENTER_X: f32 = -55.0;
const TABLE_SIZE: Vec2 = Vec2::new(960.0, 480.0);
const CUSHION: f32 = 34.0;
// Use the same geometry and scale as canonical_to_world; artwork must not
// introduce a second, independently tuned collision radius.
#[allow(clippy::cast_precision_loss)]
const BALL_RADIUS: f32 = {
    let geometry = pwmtf_game_domain::TableGeometry::standard();
    geometry.ball_radius().micros() as f32 / geometry.half_width().micros() as f32
        * (TABLE_SIZE.x / 2.0)
};
const POWER_BAR_HEIGHT: f32 = 300.0;
const POWER_BAR_X: f32 = 565.0;
#[cfg(target_arch = "wasm32")]
static SHOT_CONTROL_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// Selects combined drag (0) or separate table aim and power-bar drag (1).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn set_shot_control_mode(mode: u8) {
    SHOT_CONTROL_MODE.store(u8::from(mode == 1), std::sync::atomic::Ordering::Relaxed);
}

#[cfg_attr(not(target_arch = "wasm32"), allow(clippy::missing_const_for_fn))]
fn shot_control_mode() -> u8 {
    #[cfg(target_arch = "wasm32")]
    {
        SHOT_CONTROL_MODE.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        1
    }
}
#[cfg(target_arch = "wasm32")]
const POCKET_SELECTION_RADIUS: f32 = 44.0;
#[cfg(target_arch = "wasm32")]
static SPIN_SIDE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
#[cfg(target_arch = "wasm32")]
static SPIN_VERTICAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Sets bounded presentation spin input from the visible settings controls.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn set_cue_spin(side: i32, vertical: i32) {
    SPIN_SIDE.store(side.clamp(-100, 100), std::sync::atomic::Ordering::Relaxed);
    SPIN_VERTICAL.store(
        vertical.clamp(-100, 100),
        std::sync::atomic::Ordering::Relaxed,
    );
}

const DRAG_DEAD_ZONE: f32 = 20.0;
const DRAG_FULL_POWER: f32 = 180.0;
const DEFAULT_POWER: f32 = 0.0;
const MINIMUM_VIEWPORT_WIDTH: f32 = 1.0;

#[derive(Component)]
struct CanonicalBall(u8);

#[derive(Component)]
struct Cue;

#[derive(Component)]
struct PowerFill;

#[derive(Component)]
struct OrientationNotice;

#[derive(Component)]
struct TurnStatus;

#[derive(Component)]
struct MatchControlChrome;

#[derive(Resource, Default)]
struct CanonicalPresentation {
    checksum: Option<u64>,
    target: std::collections::BTreeMap<u8, Vec2>,
    pocketed: std::collections::BTreeSet<u8>,
    status: Option<String>,
    active_player: Option<pwmtf_game_domain::Player>,
    completed: bool,
    /// Place balls exactly at `target` this frame instead of easing toward it:
    /// set while a shot plays back and on the frame playback ends or resets.
    snap: bool,
}

#[derive(Resource)]
struct PresentationTier(&'static str);

#[derive(Resource)]
struct PrototypeInput {
    aim_angle: f32,
    power: f32,
    #[cfg(target_arch = "wasm32")]
    spin: Vec2,
    #[cfg(target_arch = "wasm32")]
    called_pocket: Option<pwmtf_game_domain::PocketId>,
    #[cfg(target_arch = "wasm32")]
    placing_cue_ball: bool,
    #[cfg(target_arch = "wasm32")]
    placement_sent_during_contact: bool,
    active_touch: Option<u64>,
    pointer_contact: PointerContact,
    drag_origin: Option<Vec2>,
    contact_mode: u8,
    interrupted: bool,
    #[cfg(target_arch = "wasm32")]
    preview_gesture: u64,
    #[cfg(target_arch = "wasm32")]
    preview_sequence: u32,
    #[cfg(target_arch = "wasm32")]
    preview_revision: u64,
    #[cfg(target_arch = "wasm32")]
    preview_active: bool,
    #[cfg(target_arch = "wasm32")]
    preview_cancel_pending: bool,
    #[cfg(target_arch = "wasm32")]
    preview_last_sent_power: u16,
    #[cfg(target_arch = "wasm32")]
    preview_last_sent_aim: u16,
    #[cfg(target_arch = "wasm32")]
    preview_last_sent_at: Option<f64>,
    touch_rearm_blocked: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PointerContact {
    #[default]
    None,
    Aim,
    Power,
    #[cfg(target_arch = "wasm32")]
    Auxiliary,
}

impl PrototypeInput {
    fn release_active_touch(&mut self, another_touch_is_pressed: bool) {
        self.active_touch = None;
        self.pointer_contact = PointerContact::None;
        self.drag_origin = None;
        self.contact_mode = shot_control_mode();
        self.power = 0.0;
        self.interrupted = another_touch_is_pressed;
        #[cfg(target_arch = "wasm32")]
        {
            self.preview_active = false;
            self.preview_cancel_pending = false;
            self.preview_last_sent_at = None;
            self.preview_last_sent_power = 0;
        }
        self.touch_rearm_blocked = another_touch_is_pressed;
    }

    const fn rearm_touch_input(&mut self) {
        self.touch_rearm_blocked = false;
    }
}

impl Default for PrototypeInput {
    fn default() -> Self {
        Self {
            aim_angle: 0.25,
            power: DEFAULT_POWER,
            #[cfg(target_arch = "wasm32")]
            spin: Vec2::ZERO,
            #[cfg(target_arch = "wasm32")]
            called_pocket: None,
            #[cfg(target_arch = "wasm32")]
            placing_cue_ball: false,
            #[cfg(target_arch = "wasm32")]
            placement_sent_during_contact: false,
            active_touch: None,
            pointer_contact: PointerContact::None,
            drag_origin: None,
            contact_mode: shot_control_mode(),
            interrupted: false,
            #[cfg(target_arch = "wasm32")]
            preview_gesture: 0,
            #[cfg(target_arch = "wasm32")]
            preview_sequence: 0,
            #[cfg(target_arch = "wasm32")]
            preview_revision: 0,
            #[cfg(target_arch = "wasm32")]
            preview_active: false,
            #[cfg(target_arch = "wasm32")]
            preview_cancel_pending: false,
            #[cfg(target_arch = "wasm32")]
            preview_last_sent_power: 0,
            #[cfg(target_arch = "wasm32")]
            preview_last_sent_aim: 0,
            #[cfg(target_arch = "wasm32")]
            preview_last_sent_at: None,
            touch_rearm_blocked: false,
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(clippy::missing_const_for_fn))]
fn presentation_tier() -> &'static str {
    #[cfg(target_arch = "wasm32")]
    {
        let search = web_sys::window()
            .and_then(|window| window.location().search().ok())
            .unwrap_or_default();
        if search
            .trim_start_matches('?')
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .any(|(key, value)| key == "tier" && value == "reduced")
        {
            return "reduced";
        }
    }
    "default"
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Starts the single same-origin `/ws` session socket; it reconnects itself.
pub fn session_start() {
    let now = browser_transport::monotonic_millis();
    browser_transport::with_mut(|transport| transport.start(now));
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Pauses (closing the socket) or resumes (reconnecting immediately) the
/// session on page visibility and online/offline changes.
pub fn session_set_active(active: bool) {
    let now = browser_transport::monotonic_millis();
    browser_transport::with_mut(|transport| transport.set_active(active, now));
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Registers `callback(name, detailJson)` for session events and replays the
/// current `connection`/`match` state to it.
pub fn set_session_listener(callback: js_sys::Function) {
    browser_transport::set_listener(callback);
}

#[cfg(target_arch = "wasm32")]
fn parse_id(value: &str) -> Result<u128, wasm_bindgen::JsValue> {
    if value.is_empty() || value.len() > 39 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(wasm_bindgen::JsValue::from_str("invalid identifier"));
    }
    value
        .parse::<u128>()
        .map_err(|_| wasm_bindgen::JsValue::from_str("invalid identifier"))
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Subscribes to one match by decimal id. Disables the practice sandbox and
/// resets match presentation.
///
/// # Errors
///
/// Returns a JavaScript exception unless `match_id` is a decimal `u128`.
pub fn subscribe_match(match_id: &str) -> Result<(), wasm_bindgen::JsValue> {
    let match_id = parse_id(match_id)?;
    browser_transport::with_mut(|transport| transport.subscribe_match(match_id));
    Ok(())
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Drops the current match subscription.
pub fn unsubscribe_match() {
    browser_transport::with_mut(transport::SessionTransport::unsubscribe_match);
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Subscribes to one lobby by decimal id; this is lobby presence.
///
/// # Errors
///
/// Returns a JavaScript exception unless `lobby_id` is a decimal `u128`.
pub fn subscribe_lobby(lobby_id: &str) -> Result<(), wasm_bindgen::JsValue> {
    let lobby_id = parse_id(lobby_id)?;
    browser_transport::with_mut(|transport| transport.subscribe_lobby(lobby_id));
    Ok(())
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Drops the current lobby subscription (leaves lobby presence).
pub fn unsubscribe_lobby() {
    browser_transport::with_mut(transport::SessionTransport::unsubscribe_lobby);
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Cancels a live planning gesture before a browser tab loses focus.
/// Does not affect canonical match state.
pub fn interrupt_match_gesture() {
    browser_transport::cancel_preview_on_interruption();
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Returns the current authoritative revision (debug/test getter).
#[must_use]
pub fn match_revision() -> Option<u64> {
    browser_transport::authoritative_revision()
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Returns the current canonical predicted checksum (debug/test getter).
#[must_use]
pub fn match_checksum() -> Option<u64> {
    browser_transport::predicted_checksum()
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Predicts and sends a quantized cue-ball placement.
///
/// # Errors
///
/// Returns a JavaScript exception unless the socket is ready, prediction is
/// valid, secure identifier generation succeeds, and transmission succeeds.
pub fn send_cue_ball_placement(x_micros: i64, y_micros: i64) -> Result<(), wasm_bindgen::JsValue> {
    browser_transport::submit_cue_ball_placement(
        random_command_id()?,
        pwmtf_game_domain::Vector::from_micros(x_micros, y_micros),
    )
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Predicts and sends a bounded canonical shot.
///
/// # Errors
///
/// Returns a JavaScript exception for invalid bounded input, unavailable socket
/// state, rejected local prediction, identifier generation, or transmission.
pub fn send_shot_command(
    aim_steps: u16,
    power_units: u16,
    spin_side: i16,
    spin_vertical: i16,
    called_pocket: u8,
) -> Result<(), wasm_bindgen::JsValue> {
    let aim = pwmtf_game_domain::Aim::new(aim_steps)
        .map_err(|_| wasm_bindgen::JsValue::from_str("invalid aim"))?;
    let power = pwmtf_game_domain::ShotPower::new(power_units)
        .map_err(|_| wasm_bindgen::JsValue::from_str("invalid power"))?;
    let spin = pwmtf_game_domain::Spin::new(spin_side, spin_vertical)
        .map_err(|_| wasm_bindgen::JsValue::from_str("invalid spin"))?;
    let called_pocket = decode_called_pocket(called_pocket)?;
    browser_transport::submit_shot(
        random_command_id()?,
        pwmtf_game_domain::VersionedShotCommand::new(aim, power, spin),
        called_pocket,
    )
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Predicts and sends an explicit concession for the local participant seat.
///
/// # Errors
///
/// Returns a JavaScript exception for an invalid seat, a seat other than the
/// server-reported local one, an unready session, a pending command, or
/// identifier generation failure.
pub fn send_concession(player: u8) -> Result<(), wasm_bindgen::JsValue> {
    let player = match player {
        1 => pwmtf_game_domain::Player::One,
        2 => pwmtf_game_domain::Player::Two,
        _ => return Err(wasm_bindgen::JsValue::from_str("invalid player seat")),
    };
    browser_transport::submit_concession(random_command_id()?, player)
}

#[cfg(target_arch = "wasm32")]
fn decode_called_pocket(
    value: u8,
) -> Result<Option<pwmtf_game_domain::PocketId>, wasm_bindgen::JsValue> {
    Ok(match value {
        0 => None,
        1 => Some(pwmtf_game_domain::PocketId::TopLeft),
        2 => Some(pwmtf_game_domain::PocketId::TopCenter),
        3 => Some(pwmtf_game_domain::PocketId::TopRight),
        4 => Some(pwmtf_game_domain::PocketId::BottomLeft),
        5 => Some(pwmtf_game_domain::PocketId::BottomCenter),
        6 => Some(pwmtf_game_domain::PocketId::BottomRight),
        _ => return Err(wasm_bindgen::JsValue::from_str("invalid called pocket")),
    })
}

#[cfg(target_arch = "wasm32")]
fn random_command_id() -> Result<pwmtf_protocol::CommandId, wasm_bindgen::JsValue> {
    let crypto = web_sys::window()
        .ok_or_else(|| wasm_bindgen::JsValue::from_str("window is unavailable"))?
        .crypto()?;
    let mut bytes = [0_u8; 16];
    crypto.get_random_values_with_u8_array(&mut bytes)?;
    Ok(pwmtf_protocol::CommandId::new(bytes))
}

fn practice_shot(input: &PrototypeInput) -> pwmtf_game_domain::VersionedShotCommand {
    use pwmtf_game_domain::{Aim, ShotPower, Spin, VersionedShotCommand};
    let turns = input.aim_angle.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let aim = (turns * f32::from(Aim::STEPS_PER_TURN)).round() as u16 % Aim::STEPS_PER_TURN;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let power = (input.power.clamp(0.0, 1.0) * f32::from(ShotPower::MAX)).round() as u16;
    #[cfg(target_arch = "wasm32")]
    #[allow(clippy::cast_possible_truncation)]
    let spin = {
        let offset = input.spin.clamp_length_max(0.999);
        Spin::new((offset.x * 10_000.0) as i16, (offset.y * 10_000.0) as i16)
            .expect("bounded spin disc")
    };
    #[cfg(not(target_arch = "wasm32"))]
    let spin = Spin::CENTER;
    VersionedShotCommand::new(
        Aim::new(aim).expect("bounded aim"),
        ShotPower::new(power).expect("bounded power"),
        spin,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn update_sandbox(
    time: Res<Time>,
    mut sandbox: ResMut<sandbox::Sandbox>,
    mut presentation: ResMut<CanonicalPresentation>,
) {
    if !sandbox.enabled {
        return;
    }
    sandbox.update(time.delta_secs_f64());
    presentation.target.clear();
    presentation.pocketed.clear();
    for ball in sandbox.table.balls() {
        if ball.pocketed {
            presentation.pocketed.insert(ball.id.number());
        } else {
            presentation.target.insert(
                ball.id.number(),
                sandbox
                    .presentation_position(ball.id)
                    .expect("present practice ball"),
            );
        }
    }
    presentation.status = Some(
        if sandbox.moving {
            "Practice · balls rolling…"
        } else if shot_control_mode() == 1 {
            "Practice · drag the table to aim · drag POWER down and release to shoot"
        } else {
            "Practice · drag anywhere on the table to aim and power · release to shoot"
        }
        .into(),
    );
}

fn main() {
    let presentation_tier = presentation_tier();
    App::new()
        .insert_resource(ClearColor(Color::srgb(0.025, 0.075, 0.055)))
        .insert_resource(PresentationTier(presentation_tier))
        .init_resource::<PrototypeInput>()
        .init_resource::<CanonicalPresentation>()
        .init_resource::<exact_preview::ExactPreview>()
        .init_resource::<sandbox::Sandbox>()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("Pool with More Than Friends · {presentation_tier} tier"),
                name: Some("pwmtf.client".into()),
                canvas: Some("#pwmtf-canvas".into()),
                resolution: WindowResolution::new(1280, 720),
                present_mode: PresentMode::AutoVsync,
                fit_canvas_to_parent: true,
                // Preserve browser shortcuts (Cmd/Ctrl-R, Tab, developer tools).
                // CSS touch-action handles gesture suppression independently.
                prevent_default_event_handling: false,
                ..default()
            }),
            ..default()
        }))
        .add_plugins(ball_art::BallArtPlugin)
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                cancel_input_on_focus_loss,
                update_orientation,
                rearm_input_after_valid_landscape,
                update_sandbox,
                update_input,
                aim_preview::draw,
                update_aim,
                #[cfg(target_arch = "wasm32")]
                synchronize_canonical_presentation,
                update_turn_status,
                update_match_control_visibility,
                interpolate_canonical_balls,
                ball_art::update,
            )
                .chain(),
        )
        .run();
}

#[allow(clippy::needless_pass_by_value)]
fn setup(
    mut commands: Commands,
    presentation_tier: Res<PresentationTier>,
    mut presentation: ResMut<CanonicalPresentation>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut ball_materials: ResMut<Assets<ball_art::BallMaterial>>,
    mut pocket_materials: ResMut<Assets<ColorMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let initial = pwmtf_game_domain::MatchState::new(
        pwmtf_game_domain::RulesProfile::standard(),
        pwmtf_game_domain::PhysicsProfile::standard(),
        pwmtf_game_domain::TableGeometry::standard(),
        pwmtf_game_domain::RackSeed::new(42),
        pwmtf_game_domain::Player::One,
    )
    .expect("built-in canonical match configuration is valid");
    #[cfg(target_arch = "wasm32")]
    presentation.project(&initial);
    #[cfg(not(target_arch = "wasm32"))]
    presentation.project(&initial);
    commands.spawn((
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::AutoMin {
                min_width: DESIGN_SIZE.x,
                min_height: DESIGN_SIZE.y,
            },
            ..OrthographicProjection::default_2d()
        }),
    ));

    spawn_rectangle(
        &mut commands,
        Vec2::new(
            CUSHION.mul_add(2.0, TABLE_SIZE.x) + 28.0,
            CUSHION.mul_add(2.0, TABLE_SIZE.y) + 28.0,
        ),
        Color::srgb(0.055, 0.025, 0.012),
        Vec3::new(TABLE_CENTER_X, -8.0, -0.5),
    );
    spawn_rectangle(
        &mut commands,
        Vec2::new(
            CUSHION.mul_add(2.0, TABLE_SIZE.x),
            CUSHION.mul_add(2.0, TABLE_SIZE.y),
        ),
        Color::srgb(0.20, 0.075, 0.025),
        Vec3::new(TABLE_CENTER_X, 0.0, 0.0),
    );
    spawn_rectangle(
        &mut commands,
        TABLE_SIZE,
        Color::srgb(0.025, 0.38, 0.21),
        Vec3::new(TABLE_CENTER_X, 0.0, 1.0),
    );
    spawn_rectangle(
        &mut commands,
        Vec2::new(TABLE_SIZE.x - 34.0, TABLE_SIZE.y - 34.0),
        Color::srgba(0.10, 0.62, 0.39, 0.16),
        Vec3::new(TABLE_CENTER_X, 0.0, 1.5),
    );

    spawn_table_details(&mut commands);

    spawn_pocket_art(&mut commands, &mut meshes, &mut pocket_materials);

    ball_art::spawn(
        &mut commands,
        &mut meshes,
        &mut ball_materials,
        &presentation,
    );
    cue_art::spawn(&mut commands, &mut images);

    spawn_rectangle(
        &mut commands,
        Vec2::new(42.0, POWER_BAR_HEIGHT + 14.0),
        Color::srgba(0.02, 0.02, 0.02, 0.72),
        Vec3::new(565.0, 0.0, 5.0),
    );
    commands.spawn((
        Sprite::from_color(
            Color::srgb(0.94, 0.58, 0.10),
            Vec2::new(30.0, POWER_BAR_HEIGHT * DEFAULT_POWER),
        ),
        Transform::from_xyz(565.0, POWER_BAR_HEIGHT * (DEFAULT_POWER - 1.0) / 2.0, 6.0),
        PowerFill,
        MatchControlChrome,
    ));

    commands.spawn((
        Text2d::new("POWER"),
        TextFont::from_font_size(16.0),
        TextColor(Color::srgb(0.94, 0.85, 0.64)),
        Transform::from_xyz(565.0, POWER_BAR_HEIGHT / 2.0 + 24.0, 6.0),
    ));
    spawn_overlay(&mut commands, presentation_tier.0);
}

fn spawn_table_details(commands: &mut Commands) {
    for x in [-360.0_f32, -120.0, 120.0, 360.0] {
        for y in [
            -TABLE_SIZE.y / 2.0 - CUSHION / 2.0,
            TABLE_SIZE.y / 2.0 + CUSHION / 2.0,
        ] {
            commands.spawn((
                Sprite::from_color(Color::srgb(0.93, 0.72, 0.31), Vec2::splat(7.0)),
                Transform::from_xyz(TABLE_CENTER_X + x, y, 2.0)
                    .with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_4)),
            ));
        }
    }
}

fn spawn_overlay(commands: &mut Commands, _presentation_tier: &str) {
    commands.spawn((
        Text::new("Waiting for an authoritative match"),
        TextFont::from_font_size(12.0),
        TextColor(Color::srgb(0.92, 0.85, 0.65)),
        Node {
            // The browser HUD owns status text; keep this for native runs only.
            display: if cfg!(target_arch = "wasm32") {
                Display::None
            } else {
                Display::Flex
            },
            position_type: PositionType::Absolute,
            top: px(4),
            left: px(24),
            ..default()
        },
        TurnStatus,
    ));
    commands.spawn((
        Text::new("Rotate your device to landscape"),
        TextFont::from_font_size(36.0),
        TextColor(Color::WHITE),
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            width: percent(100),
            height: percent(100),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.06, 0.045, 0.97)),
        GlobalZIndex(100),
        OrientationNotice,
    ));
}

fn spawn_rectangle(commands: &mut Commands, size: Vec2, color: Color, translation: Vec3) {
    commands.spawn((
        Sprite::from_color(color, size),
        Transform::from_translation(translation),
    ));
}

#[allow(clippy::cast_precision_loss)]
fn canonical_to_world(position: pwmtf_game_domain::Vector) -> Vec2 {
    micros_to_world(position.x.micros() as f64, position.y.micros() as f64)
}

/// Maps sub-tick interpolated canonical micro-metres to world coordinates.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn micros_to_world(x_micros: f64, y_micros: f64) -> Vec2 {
    let geometry = pwmtf_game_domain::TableGeometry::standard();
    Vec2::new(
        ((x_micros / geometry.half_width().micros() as f64) as f32)
            .mul_add(TABLE_SIZE.x / 2.0, TABLE_CENTER_X),
        (y_micros / geometry.half_height().micros() as f64) as f32 * (TABLE_SIZE.y / 2.0),
    )
}

const fn player_number(player: pwmtf_game_domain::Player) -> u8 {
    match player {
        pwmtf_game_domain::Player::One => 1,
        pwmtf_game_domain::Player::Two => 2,
    }
}

fn match_status_text(
    active_player: pwmtf_game_domain::Player,
    outcome: Option<pwmtf_game_domain::MatchOutcome>,
) -> String {
    outcome.map_or_else(
        || format!("Player {} to shoot", player_number(active_player)),
        |outcome| format!("Player {} wins", player_number(outcome.winner)),
    )
}

impl CanonicalPresentation {
    fn project(&mut self, state: &pwmtf_game_domain::MatchState) {
        self.checksum = Some(state.checksum());
        let outcome = match state.status() {
            pwmtf_game_domain::MatchStatus::InProgress => None,
            pwmtf_game_domain::MatchStatus::Completed(outcome) => Some(outcome),
        };
        self.completed = outcome.is_some();
        self.active_player = (!self.completed).then_some(state.active_player());
        self.status = Some(match_status_text(state.active_player(), outcome));
        self.target.clear();
        self.pocketed.clear();
        for ball in state.table().balls() {
            if ball.pocketed {
                self.pocketed.insert(ball.id.number());
            } else {
                self.target
                    .insert(ball.id.number(), canonical_to_world(ball.position));
            }
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
impl CanonicalPresentation {
    /// Shows a running shot playback directly: positions are the playback's
    /// fixed-tick samples, and a ball disappears on the tick it is pocketed.
    fn project_playback(&mut self, playback: &playback::ShotPlayback) {
        self.checksum = None;
        self.target.clear();
        self.pocketed.clear();
        for ball in playback.samples() {
            if ball.pocketed {
                self.pocketed.insert(ball.number);
            } else {
                self.target
                    .insert(ball.number, micros_to_world(ball.x_micros, ball.y_micros));
            }
        }
        self.snap = true;
    }
}

#[allow(clippy::needless_pass_by_value)]
#[cfg(target_arch = "wasm32")]
fn synchronize_canonical_presentation(
    mut presentation: ResMut<CanonicalPresentation>,
    mut sandbox: ResMut<sandbox::Sandbox>,
    mut epoch: Local<Option<u64>>,
) {
    let ended = browser_transport::advance_playback();
    if presentation.snap {
        presentation.snap = false;
    }
    browser_transport::with(|transport| {
        if transport.subscribed_match().is_some() && sandbox.enabled {
            sandbox.disable();
        }
        if sandbox.enabled {
            return;
        }
        let current_epoch = transport.presentation_epoch();
        let reset = *epoch != Some(current_epoch);
        if reset {
            *epoch = Some(current_epoch);
            presentation.checksum = None;
        }
        if let Some(playback) = transport.playback() {
            presentation.project_playback(playback);
            presentation.status = Some("Balls rolling…".into());
            return;
        }
        let Some(prediction) = transport.prediction() else {
            if reset {
                presentation.status = Some("Waiting for an authoritative match".into());
            }
            return;
        };
        let predicted = prediction.predicted();
        if presentation.checksum != Some(predicted.checksum()) {
            presentation.project(predicted);
            let authority = prediction.authoritative();
            let outcome = match authority.status() {
                pwmtf_game_domain::MatchStatus::InProgress => None,
                pwmtf_game_domain::MatchStatus::Completed(outcome) => Some(outcome),
            };
            presentation.completed = outcome.is_some();
            presentation.active_player =
                (!presentation.completed).then_some(authority.active_player());
            presentation.status = Some(match_status_text(authority.active_player(), outcome));
        }
        // Playback end and subscription resets show authority exactly; other
        // authoritative changes (gap, reconnect, placement, timeout) ease in.
        if ended || reset {
            presentation.snap = true;
        }
    });
}

#[allow(clippy::needless_pass_by_value)]
fn update_turn_status(
    presentation: Res<CanonicalPresentation>,
    mut status: Single<&mut Text, With<TurnStatus>>,
) {
    if presentation.is_changed()
        && let Some(message) = &presentation.status
    {
        status.0.clone_from(message);
    }
}

#[allow(clippy::needless_pass_by_value)]
fn update_match_control_visibility(
    sandbox: Res<sandbox::Sandbox>,
    presentation: Res<CanonicalPresentation>,
    mut controls: Query<&mut Visibility, With<MatchControlChrome>>,
) {
    #[cfg(not(target_arch = "wasm32"))]
    if !presentation.is_changed() {
        return;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = presentation;
    #[cfg(target_arch = "wasm32")]
    let local_may_act =
        (sandbox.enabled && !sandbox.moving) || browser_transport::accepts_active_player_command();
    #[cfg(not(target_arch = "wasm32"))]
    let local_may_act = sandbox.enabled && !sandbox.moving;
    let remote_visible = {
        #[cfg(target_arch = "wasm32")]
        {
            browser_transport::remote_preview().is_some()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            false
        }
    };
    let visibility = if local_may_act || remote_visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut control in &mut controls {
        *control = visibility;
    }
}

#[allow(clippy::needless_pass_by_value)]
fn interpolate_canonical_balls(
    time: Res<Time>,
    presentation: Res<CanonicalPresentation>,
    sandbox: Res<sandbox::Sandbox>,
    mut balls: Query<(&CanonicalBall, &mut Transform, &mut Visibility)>,
) {
    let blend = if sandbox.enabled || presentation.snap {
        1.0
    } else {
        (time.delta_secs() * 18.0).clamp(0.0, 1.0)
    };
    for (ball, mut transform, mut visibility) in &mut balls {
        if presentation.pocketed.contains(&ball.0) {
            *visibility = Visibility::Hidden;
        } else if let Some(target) = presentation.target.get(&ball.0) {
            *visibility = Visibility::Inherited;
            let current = transform.translation.truncate();
            transform.translation = current.lerp(*target, blend).extend(transform.translation.z);
        }
        #[cfg(target_arch = "wasm32")]
        record_rendered_ball(
            ball.0,
            (*visibility != Visibility::Hidden).then(|| transform.translation.truncate()),
        );
    }
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static RENDERED_BALLS: std::cell::RefCell<[Option<Vec2>; 16]> =
        const { std::cell::RefCell::new([None; 16]) };
}

#[cfg(target_arch = "wasm32")]
fn record_rendered_ball(ball: u8, position: Option<Vec2>) {
    RENDERED_BALLS.with_borrow_mut(|balls| {
        if let Some(slot) = balls.get_mut(usize::from(ball)) {
            *slot = position;
        }
    });
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
/// Returns ball `ball`'s (0-15) most recently rendered world `[x, y]`
/// (debug/test getter), or `undefined` when it is pocketed, hidden, or absent.
#[must_use]
pub fn match_ball_position(ball: u8) -> Option<Vec<f64>> {
    RENDERED_BALLS.with_borrow(|balls| {
        balls
            .get(usize::from(ball))
            .copied()
            .flatten()
            .map(|position| vec![f64::from(position.x), f64::from(position.y)])
    })
}

fn spawn_pocket_art(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
) {
    let geometry = pwmtf_game_domain::TableGeometry::standard();
    #[allow(clippy::cast_precision_loss)]
    let scale = TABLE_SIZE.x / (2.0 * geometry.half_width().micros() as f32);
    #[allow(clippy::cast_precision_loss)]
    let radius = geometry.pocket_radius().micros() as f32 * scale;
    let rim = meshes.add(Circle::new(radius + 4.0));
    let well = meshes.add(Circle::new(radius));
    let lip = materials.add(ColorMaterial::from_color(Color::srgb(0.11, 0.07, 0.035)));
    let dark = materials.add(ColorMaterial::from_color(Color::srgb(0.008, 0.011, 0.009)));
    for pocket in pocket_positions() {
        commands.spawn((
            Mesh2d(rim.clone()),
            MeshMaterial2d(lip.clone()),
            Transform::from_translation(pocket.extend(2.5)),
        ));
        commands.spawn((
            Mesh2d(well.clone()),
            MeshMaterial2d(dark.clone()),
            Transform::from_translation(pocket.extend(2.6)),
        ));
    }
    let rubber = Color::srgb(0.025, 0.29, 0.15);
    for (a, b) in geometry.cushion_segments() {
        let a = canonical_to_world(a);
        let b = canonical_to_world(b);
        let direction = b - a;
        commands.spawn((
            Sprite::from_color(rubber, Vec2::new(direction.length(), 3.0)),
            Transform::from_translation(((a + b) / 2.0).extend(2.8))
                .with_rotation(Quat::from_rotation_z(direction.y.atan2(direction.x))),
        ));
    }
}

fn pocket_positions() -> [Vec2; 6] {
    let half = TABLE_SIZE / 2.0;
    [
        Vec2::new(TABLE_CENTER_X - half.x, -half.y),
        Vec2::new(TABLE_CENTER_X, -half.y),
        Vec2::new(TABLE_CENTER_X + half.x, -half.y),
        Vec2::new(TABLE_CENTER_X - half.x, half.y),
        Vec2::new(TABLE_CENTER_X, half.y),
        Vec2::new(TABLE_CENTER_X + half.x, half.y),
    ]
}

fn input_position(
    window: &Window,
    mouse_is_pressed: bool,
    touches: &Touches,
    input: &mut PrototypeInput,
) -> Option<Vec2> {
    if let Some(id) = input.active_touch {
        if let Some(touch) = touches.get_pressed(id) {
            return Some(touch.position());
        }
        #[cfg(target_arch = "wasm32")]
        {
            if input.preview_last_sent_at.is_some() {
                publish_gesture(input, true);
            }
        }
        input.release_active_touch(true);
        return None;
    }

    if input.touch_rearm_blocked {
        return None;
    }

    if let Some(touch) = touches.iter().next() {
        input.active_touch = Some(touch.id());
        return Some(touch.position());
    }

    mouse_is_pressed.then(|| window.cursor_position()).flatten()
}

fn pointer_world(cursor: Vec2, size: Vec2) -> Vec2 {
    let scale = (size / DESIGN_SIZE).min_element().max(0.001);
    Vec2::new(cursor.x - size.x / 2.0, size.y / 2.0 - cursor.y) / scale
}

fn pointer_on_table(cursor: Vec2, size: Vec2) -> bool {
    let position = pointer_world(cursor, size);
    let half = TABLE_SIZE / 2.0;
    let center = Vec2::new(TABLE_CENTER_X, 0.0);
    position.is_finite() && (position - center).abs().cmple(half).all()
}

fn pointer_on_power_bar(cursor: Vec2, size: Vec2) -> bool {
    let position = pointer_world(cursor, size);
    position.is_finite()
        && (position.x - POWER_BAR_X).abs() <= 32.0
        && position.y.abs() <= POWER_BAR_HEIGHT / 2.0 + 14.0
}

fn classify_pointer_contact(cursor: Vec2, window_size: Vec2, mode: u8) -> PointerContact {
    #[cfg(target_arch = "wasm32")]
    if selected_pocket(cursor, window_size).is_some() {
        return PointerContact::Auxiliary;
    }
    if mode == 1 && pointer_on_power_bar(cursor, window_size) {
        return PointerContact::Power;
    }
    PointerContact::Aim
}

#[allow(clippy::missing_const_for_fn)]
fn update_drag(input: &mut PrototypeInput, cursor: Vec2, size: Vec2) {
    let Some(origin) = input.drag_origin else {
        return;
    };
    let displacement = pointer_world(cursor, size) - origin;
    if !displacement.is_finite() {
        input.power = 0.0;
        return;
    }
    let distance = displacement.length();
    if distance > DRAG_DEAD_ZONE {
        // Pull the cue away from the target; the ball travels in the opposite direction.
        input.aim_angle = (-displacement).to_angle();
    }
    input.power =
        ((distance - DRAG_DEAD_ZONE) / (DRAG_FULL_POWER - DRAG_DEAD_ZONE)).clamp(0.0, 1.0);
}

fn update_power_drag(input: &mut PrototypeInput, cursor: Vec2, size: Vec2) {
    let Some(origin) = input.drag_origin else {
        return;
    };
    let delta = origin.y - pointer_world(cursor, size).y;
    input.power = if delta.is_finite() {
        ((delta - DRAG_DEAD_ZONE) / (DRAG_FULL_POWER - DRAG_DEAD_ZONE)).clamp(0.0, 1.0)
    } else {
        0.0
    };
}

fn update_aim_drag(input: &mut PrototypeInput, cursor: Vec2, size: Vec2, cue_ball: Vec2) {
    let direction = pointer_world(cursor, size) - cue_ball;
    // Separate controls point from the cue ball toward the contact, rather
    // than reversing the distance moved since the contact began.
    if direction.is_finite() && direction.length() > DRAG_DEAD_ZONE {
        input.aim_angle = direction.to_angle();
    }
    input.power = 0.0;
}

#[allow(clippy::needless_pass_by_ref_mut)]
fn update_from_pointer(
    input: &mut PrototypeInput,
    cursor: Vec2,
    window_size: Vec2,
    cue_ball: Option<Vec2>,
    placement_sent_during_contact: &mut bool,
) {
    #[cfg(not(target_arch = "wasm32"))]
    let _ = placement_sent_during_contact;
    if window_size.min_element() <= 0.0 {
        return;
    }
    #[cfg(target_arch = "wasm32")]
    {
        if input.placing_cue_ball
            && let Some(position) = screen_to_canonical(cursor, window_size)
        {
            if send_cue_ball_placement(position.x.micros(), position.y.micros()).is_ok() {
                *placement_sent_during_contact = true;
            }
            return;
        }
        if input.pointer_contact == PointerContact::Auxiliary
            && let Some(pocket) = selected_pocket(cursor, window_size)
        {
            input.called_pocket = Some(pocket);
            return;
        }
    }
    if input.pointer_contact == PointerContact::Aim && input.drag_origin.is_some() {
        if input.contact_mode == 1 {
            if let Some(cue_ball) = cue_ball {
                update_aim_drag(input, cursor, window_size, cue_ball);
            }
        } else {
            update_drag(input, cursor, window_size);
        }
    } else if input.pointer_contact == PointerContact::Power && input.drag_origin.is_some() {
        update_power_drag(input, cursor, window_size);
    }
}

#[cfg(target_arch = "wasm32")]
fn screen_to_canonical(cursor: Vec2, window_size: Vec2) -> Option<pwmtf_game_domain::Vector> {
    let scale = (window_size / DESIGN_SIZE).min_element();
    if scale <= 0.0 {
        return None;
    }
    let offset = (window_size - DESIGN_SIZE * scale) / 2.0;
    let design = (cursor - offset) / scale;
    let table_left = TABLE_CENTER_X + DESIGN_SIZE.x / 2.0 - TABLE_SIZE.x / 2.0;
    let table_top = DESIGN_SIZE.y / 2.0 - TABLE_SIZE.y / 2.0;
    if design.x < table_left
        || design.x > table_left + TABLE_SIZE.x
        || design.y < table_top
        || design.y > table_top + TABLE_SIZE.y
    {
        return None;
    }
    let normalized_x = ((design.x - table_left) / TABLE_SIZE.x).mul_add(2.0, -1.0);
    let normalized_y = (1.0 - (design.y - table_top) / TABLE_SIZE.y).mul_add(2.0, -1.0);
    let geometry = pwmtf_game_domain::TableGeometry::standard();
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let x = (normalized_x * geometry.half_width().micros() as f32).round() as i64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let y = (normalized_y * geometry.half_height().micros() as f32).round() as i64;
    Some(pwmtf_game_domain::Vector::from_micros(x, y))
}

#[cfg(target_arch = "wasm32")]
fn selected_pocket(cursor: Vec2, window_size: Vec2) -> Option<pwmtf_game_domain::PocketId> {
    let scale = (window_size / DESIGN_SIZE).min_element();
    let offset = (window_size - DESIGN_SIZE * scale) / 2.0;
    let design_cursor = (cursor - offset) / scale;
    let screen_pockets = [
        (
            pwmtf_game_domain::PocketId::TopLeft,
            Vec2::new(145.0, 600.0),
        ),
        (
            pwmtf_game_domain::PocketId::TopCenter,
            Vec2::new(625.0, 600.0),
        ),
        (
            pwmtf_game_domain::PocketId::TopRight,
            Vec2::new(1_105.0, 600.0),
        ),
        (
            pwmtf_game_domain::PocketId::BottomLeft,
            Vec2::new(145.0, 120.0),
        ),
        (
            pwmtf_game_domain::PocketId::BottomCenter,
            Vec2::new(625.0, 120.0),
        ),
        (
            pwmtf_game_domain::PocketId::BottomRight,
            Vec2::new(1_105.0, 120.0),
        ),
    ];
    screen_pockets
        .into_iter()
        .find(|(_, position)| design_cursor.distance(*position) <= POCKET_SELECTION_RADIUS)
        .map(|(pocket, _)| pocket)
}

#[allow(clippy::needless_pass_by_value)]
fn cancel_input_on_focus_loss(
    mut focus_events: MessageReader<WindowFocused>,
    mut input: ResMut<PrototypeInput>,
) {
    if focus_events.read().any(|event| !event.focused) {
        #[cfg(target_arch = "wasm32")]
        {
            if input.preview_last_sent_at.is_some() {
                publish_gesture(&mut input, true);
            }
        }
        input.release_active_touch(true);
    }
}

fn touch_release_is_intentional(active_touch: Option<u64>, touches: &Touches) -> bool {
    active_touch.is_none_or(|id| touches.just_released(id))
}

fn contact_is_charged(input: &PrototypeInput) -> bool {
    (input.pointer_contact == PointerContact::Aim && input.contact_mode == 0
        || input.pointer_contact == PointerContact::Power && input.contact_mode == 1)
        && input.drag_origin.is_some()
        && input.power > 0.0
        && !input.touch_rearm_blocked
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
fn update_input(
    window: Single<&Window>,
    mouse: Res<ButtonInput<MouseButton>>,
    touches: Res<Touches>,
    mut input: ResMut<PrototypeInput>,
    mut was_pressed: Local<bool>,
    mut sandbox: ResMut<sandbox::Sandbox>,
    presentation: Res<CanonicalPresentation>,
) {
    let mouse_is_pressed = mouse.pressed(MouseButton::Left);
    #[cfg(target_arch = "wasm32")]
    if let Some(sequence) = browser_transport::retry_pending_cancel() {
        input.preview_sequence = input.preview_sequence.max(sequence.saturating_add(1));
    }
    #[cfg(target_arch = "wasm32")]
    if let Some(sequence) = browser_transport::interrupted_preview_sequence() {
        input.preview_sequence = input.preview_sequence.max(sequence.saturating_add(2));
        if input.drag_origin.is_some() {
            input.release_active_touch(true);
        }
    }
    let any_pressed = mouse_is_pressed || touches.iter().next().is_some();
    if input.contact_mode != shot_control_mode() {
        if any_pressed || input.drag_origin.is_some() {
            #[cfg(target_arch = "wasm32")]
            if input.preview_active && input.preview_last_sent_at.is_some() {
                publish_gesture(&mut input, true);
            }
            input.release_active_touch(true);
        } else {
            input.contact_mode = shot_control_mode();
        }
    }
    #[cfg(target_arch = "wasm32")]
    if !(browser_transport::accepts_active_player_command() || sandbox.enabled && !sandbox.moving) {
        if input.preview_active && input.preview_last_sent_at.is_some() {
            publish_gesture(&mut input, true);
        }
        if any_pressed || input.drag_origin.is_some() {
            input.release_active_touch(any_pressed);
        }
        if !any_pressed && input.interrupted {
            input.interrupted = false;
            input.rearm_touch_input();
            browser_transport::rearm_interrupted_gesture();
        }
        *was_pressed = any_pressed;
        return;
    }
    if sandbox.enabled && sandbox.moving {
        if input.drag_origin.is_some() {
            input.release_active_touch(any_pressed);
        }
        if !any_pressed && input.interrupted {
            input.interrupted = false;
            input.rearm_touch_input();
        }
        *was_pressed = any_pressed;
        return;
    }
    let started_contact = !*was_pressed && any_pressed;
    if *was_pressed && !any_pressed {
        // A touch canceled by the browser is not a release-to-shoot. Bevy removes
        // both released and canceled contacts from `pressed` before this update.
        if !touch_release_is_intentional(input.active_touch, &touches) {
            #[cfg(target_arch = "wasm32")]
            if input.preview_last_sent_at.is_some() {
                publish_gesture(&mut input, true);
            }
            input.release_active_touch(true);
            *was_pressed = false;
            return;
        }
        let charged = contact_is_charged(&input);
        if sandbox.enabled && charged {
            sandbox.shoot(practice_shot(&input));
        }
        #[cfg(target_arch = "wasm32")]
        if !sandbox.enabled {
            let shot_eligible = charged
                && input.preview_active
                && !input.placement_sent_during_contact
                && browser_transport::accepts_active_player_command()
                && browser_transport::authoritative_revision() == Some(input.preview_revision);
            if input.preview_active && input.preview_last_sent_at.is_some() {
                publish_gesture(&mut input, true);
            }
            if shot_eligible && release_shot(&input).is_ok() {
                browser_transport::forget_pending_cancel();
            }
        }
        input.release_active_touch(false);
        #[cfg(target_arch = "wasm32")]
        {
            input.placement_sent_during_contact = false;
        }
    }
    *was_pressed = any_pressed;
    if !any_pressed {
        if input.interrupted {
            input.interrupted = false;
            input.rearm_touch_input();
        }
        #[cfg(target_arch = "wasm32")]
        browser_transport::rearm_interrupted_gesture();
        return;
    }
    if touches.iter().count() > 1 || (mouse_is_pressed && touches.iter().next().is_some()) {
        #[cfg(target_arch = "wasm32")]
        {
            if input.preview_last_sent_at.is_some() {
                publish_gesture(&mut input, true);
            }
        }
        input.release_active_touch(true);
        return;
    }
    let Some(cursor) = input_position(&window, mouse_is_pressed, &touches, &mut input) else {
        return;
    };
    if input.touch_rearm_blocked {
        return;
    }
    if started_contact && !input.interrupted {
        input.contact_mode = shot_control_mode();
        input.pointer_contact = classify_pointer_contact(
            cursor,
            Vec2::new(window.width(), window.height()),
            input.contact_mode,
        );
        if matches!(
            input.pointer_contact,
            PointerContact::Aim | PointerContact::Power
        ) {
            #[cfg(target_arch = "wasm32")]
            let can_drag = !input.placing_cue_ball
                && presentation.target.contains_key(&0)
                && (input.pointer_contact == PointerContact::Power
                    || pointer_on_table(cursor, Vec2::new(window.width(), window.height())));
            #[cfg(not(target_arch = "wasm32"))]
            let can_drag = presentation.target.contains_key(&0)
                && (input.pointer_contact == PointerContact::Power
                    || pointer_on_table(cursor, Vec2::new(window.width(), window.height())));
            if can_drag {
                input.drag_origin = Some(pointer_world(
                    cursor,
                    Vec2::new(window.width(), window.height()),
                ));
                input.power = 0.0;
                #[cfg(target_arch = "wasm32")]
                if !sandbox.enabled {
                    if let Some(revision) = browser_transport::authoritative_revision() {
                        if let Ok(id) = random_command_id() {
                            if input.preview_revision != revision {
                                input.preview_sequence = 0;
                            }
                            input.preview_revision = revision;
                            input.preview_gesture =
                                u64::from_be_bytes(id.bytes()[..8].try_into().unwrap_or_default());
                            input.preview_active = true;
                            input.preview_cancel_pending = false;
                            input.preview_last_sent_power = 0;
                            input.preview_last_sent_aim = 0;
                            input.preview_last_sent_at = None;
                            browser_transport::forget_pending_cancel();
                        }
                    }
                }
            }
        }
    }
    let mut placement_sent_during_contact = false;
    let previous_contact_power = input.power;
    update_from_pointer(
        &mut input,
        cursor,
        Vec2::new(window.width(), window.height()),
        presentation.target.get(&0).copied(),
        &mut placement_sent_during_contact,
    );
    #[cfg(target_arch = "wasm32")]
    {
        input.placement_sent_during_contact |= placement_sent_during_contact;
        if !sandbox.enabled
            && input.drag_origin.is_some()
            && (input.power > 0.0
                || input.contact_mode == 1 && input.pointer_contact == PointerContact::Aim)
        {
            publish_gesture(&mut input, false);
        } else if !sandbox.enabled
            && (input.preview_last_sent_at.is_some() || previous_contact_power > 0.0)
            && input.drag_origin.is_some()
            && !input.preview_cancel_pending
        {
            publish_gesture(&mut input, true);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (
        placement_sent_during_contact,
        presentation,
        previous_contact_power,
    );
    #[cfg(target_arch = "wasm32")]
    let _ = presentation;
}

#[cfg(target_arch = "wasm32")]
fn publish_gesture(input: &mut PrototypeInput, cancel: bool) {
    if !input.preview_active {
        return;
    }
    if !cancel && input.preview_cancel_pending {
        input.preview_cancel_pending = false;
        input.preview_gesture = input.preview_gesture.wrapping_add(1);
        input.preview_last_sent_power = 0;
        input.preview_last_sent_aim = 0;
        input.preview_last_sent_at = None;
        browser_transport::forget_pending_cancel();
    }
    let shot = practice_shot(input);
    let timeout_ms = 300.0;
    if !cancel
        && shot.aim.steps() == input.preview_last_sent_aim
        && shot.power.units() == input.preview_last_sent_power
        && input
            .preview_last_sent_at
            .is_some_and(|at| browser_transport::monotonic_millis() - at < timeout_ms)
    {
        return;
    }
    let preview = pwmtf_protocol::ShotPreview {
        revision: input.preview_revision,
        gesture: input.preview_gesture,
        sequence: input.preview_sequence,
        aim: shot.aim,
        power: shot.power,
        spin: shot.spin,
        cancel,
    };
    if cancel {
        if browser_transport::send_preview(preview).is_ok() {
            input.preview_sequence = input.preview_sequence.wrapping_add(1);
            input.preview_cancel_pending = true;
            input.preview_last_sent_power = 0;
        }
    } else if browser_transport::resend_preview(preview).unwrap_or(false) {
        input.preview_sequence = input.preview_sequence.wrapping_add(1);
        input.preview_last_sent_power = shot.power.units();
        input.preview_last_sent_aim = shot.aim.steps();
        input.preview_last_sent_at = Some(browser_transport::monotonic_millis());
    }
}

#[cfg(target_arch = "wasm32")]
fn release_shot(input: &PrototypeInput) -> Result<(), wasm_bindgen::JsValue> {
    let shot = practice_shot(input);
    let called_pocket = input.called_pocket.map_or(0, |pocket| match pocket {
        pwmtf_game_domain::PocketId::TopLeft => 1,
        pwmtf_game_domain::PocketId::TopCenter => 2,
        pwmtf_game_domain::PocketId::TopRight => 3,
        pwmtf_game_domain::PocketId::BottomLeft => 4,
        pwmtf_game_domain::PocketId::BottomCenter => 5,
        pwmtf_game_domain::PocketId::BottomRight => 6,
    });
    send_shot_command(
        shot.aim.steps(),
        shot.power.units(),
        shot.spin.side,
        shot.spin.vertical,
        called_pocket,
    )
}

// Keep the cue's physical length independent of clearance; the viewport clips
// the butt naturally rather than compressing the artwork near a screen edge.
fn cue_transform(cue_ball: Vec2, aim_angle: f32, power: f32) -> Transform {
    let direction = Vec2::from_angle(aim_angle);
    let pullback = power.mul_add(42.0, 12.0);
    Transform::from_translation(
        (cue_ball - direction * (pullback + cue_art::LENGTH / 2.0)).extend(7.0),
    )
    .with_rotation(Quat::from_rotation_z(aim_angle))
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
fn update_aim(
    presentation: Res<CanonicalPresentation>,
    input: Res<PrototypeInput>,
    mut cue: Single<&mut Transform, (With<Cue>, Without<PowerFill>)>,
    mut power: Single<(&mut Sprite, &mut Transform), (With<PowerFill>, Without<Cue>)>,
) {
    let cue_ball = presentation
        .target
        .get(&0)
        .copied()
        .unwrap_or(Vec2::new(-330.0, 0.0));
    #[cfg(target_arch = "wasm32")]
    let remote = if !browser_transport::accepts_active_player_command() {
        browser_transport::remote_preview()
    } else {
        None
    };
    #[cfg(target_arch = "wasm32")]
    let (angle, strength) = remote.map_or((input.aim_angle, input.power), |preview| {
        (
            f32::from(preview.aim.steps()) * std::f32::consts::TAU
                / f32::from(pwmtf_game_domain::Aim::STEPS_PER_TURN),
            f32::from(preview.power.units()) / f32::from(pwmtf_game_domain::ShotPower::MAX),
        )
    });
    #[cfg(not(target_arch = "wasm32"))]
    let (angle, strength) = (input.aim_angle, input.power);
    **cue = cue_transform(cue_ball, angle, strength);

    let fill_height = POWER_BAR_HEIGHT * strength;
    power.0.custom_size = Some(Vec2::new(30.0, fill_height));
    power.1.translation.y = (fill_height - POWER_BAR_HEIGHT) / 2.0;
}

#[allow(clippy::needless_pass_by_value)]
fn update_orientation(
    window: Single<&Window>,
    mut notice: Single<&mut Node, With<OrientationNotice>>,
    mut input: ResMut<PrototypeInput>,
) {
    let is_portrait = window.width() < window.height();
    notice.display = if is_portrait {
        Display::Flex
    } else {
        Display::None
    };
    #[cfg(target_arch = "wasm32")]
    {
        input.placing_cue_ball = browser_transport::ball_in_hand();
        #[allow(clippy::cast_precision_loss)]
        {
            input.spin = Vec2::new(
                SPIN_SIDE.load(std::sync::atomic::Ordering::Relaxed) as f32,
                SPIN_VERTICAL.load(std::sync::atomic::Ordering::Relaxed) as f32,
            ) / 100.0;
        }
    }
    if is_portrait || window.width() < MINIMUM_VIEWPORT_WIDTH {
        #[cfg(target_arch = "wasm32")]
        {
            if input.preview_last_sent_at.is_some() {
                publish_gesture(&mut input, true);
            }
        }
        input.release_active_touch(true);
    }
}

fn should_rearm_input(
    window_size: Vec2,
    window_is_focused: bool,
    mouse_is_pressed: bool,
    touch_is_pressed: bool,
) -> bool {
    window_is_focused
        && window_size.x >= window_size.y
        && window_size.x >= MINIMUM_VIEWPORT_WIDTH
        && !mouse_is_pressed
        && !touch_is_pressed
}

#[allow(clippy::needless_pass_by_value)]
fn rearm_input_after_valid_landscape(
    window: Single<&Window>,
    mouse: Res<ButtonInput<MouseButton>>,
    touches: Res<Touches>,
    mut input: ResMut<PrototypeInput>,
) {
    if should_rearm_input(
        Vec2::new(window.width(), window.height()),
        window.focused,
        mouse.pressed(MouseButton::Left),
        touches.iter().next().is_some(),
    ) {
        input.rearm_touch_input();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_keeps_length_and_tip_alignment_at_edges() {
        for position in [Vec2::ZERO, DESIGN_SIZE / 2.0, -DESIGN_SIZE / 2.0] {
            for angle in [0.0, 0.7, std::f32::consts::FRAC_PI_2, std::f32::consts::PI] {
                for power in [0.0, 0.5, 1.0] {
                    let transform = cue_transform(position, angle, power);
                    let tip = transform.transform_point(Vec3::new(cue_art::LENGTH / 2.0, 0.0, 0.0));
                    let butt =
                        transform.transform_point(Vec3::new(-cue_art::LENGTH / 2.0, 0.0, 0.0));
                    assert!(transform.scale.abs_diff_eq(Vec3::ONE, 0.0001));
                    assert!((tip.distance(butt) - cue_art::LENGTH).abs() < 0.001);
                    let expected_tip =
                        position - Vec2::from_angle(angle) * power.mul_add(42.0, 12.0);
                    assert!(tip.truncate().abs_diff_eq(expected_tip, 0.001));
                }
            }
        }
        let edge = -DESIGN_SIZE / 2.0;
        let butt = cue_transform(edge, 0.0, 1.0).transform_point(Vec3::new(
            -cue_art::LENGTH / 2.0,
            0.0,
            0.0,
        ));
        assert!(butt.x < edge.x);
    }

    #[test]
    fn canonical_projection_drives_ball_targets_and_pockets() {
        use pwmtf_game_domain::{
            MatchState, PhysicsProfile, Player, RackSeed, RulesProfile, TableGeometry,
        };
        let state = MatchState::new(
            RulesProfile::standard(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap();
        let mut presentation = CanonicalPresentation::default();
        presentation.project(&state);
        assert_eq!(presentation.target.len(), 16);
        assert_eq!(presentation.active_player, Some(Player::One));
        assert!(!presentation.completed);
        assert!(presentation.target.contains_key(&0));
        assert!(presentation.pocketed.is_empty());
        assert_eq!(
            canonical_to_world(pwmtf_game_domain::Vector::ZERO),
            Vec2::new(TABLE_CENTER_X, 0.0)
        );
    }

    #[test]
    fn terminal_projection_hides_live_match_chrome() {
        use pwmtf_game_domain::{
            MatchState, PhysicsProfile, Player, RackSeed, RulesProfile, TableGeometry,
        };
        let mut state = MatchState::new(
            RulesProfile::standard(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap();
        state.concede(Player::Two).unwrap();
        let mut presentation = CanonicalPresentation::default();
        presentation.project(&state);
        assert!(presentation.completed);
        assert_eq!(presentation.active_player, None);
        assert_eq!(presentation.status.as_deref(), Some("Player 1 wins"));
    }

    #[test]
    fn pockets_cover_four_corners_and_two_side_centers() {
        let pockets = pocket_positions();
        assert_eq!(pockets.len(), 6);
        assert!((pockets[0].x - pockets[3].x).abs() < f32::EPSILON);
        assert!((pockets[1].x - pockets[4].x).abs() < f32::EPSILON);
        assert!((pockets[2].x - pockets[5].x).abs() < f32::EPSILON);
        assert!(pockets[..3].iter().all(|pocket| pocket.y < 0.0));
        assert!(pockets[3..].iter().all(|pocket| pocket.y > 0.0));
    }

    #[test]
    fn released_touch_requires_all_contacts_to_lift_before_rearming() {
        let mut input = PrototypeInput {
            active_touch: Some(7),
            ..PrototypeInput::default()
        };

        input.release_active_touch(true);
        assert!(input.active_touch.is_none());
        assert!(input.touch_rearm_blocked);

        input.rearm_touch_input();
        assert!(!input.touch_rearm_blocked);
    }

    #[test]
    fn portrait_cancellation_stays_blocked_until_landscape_rearm() {
        assert!(!should_rearm_input(
            Vec2::new(500.0, 900.0),
            true,
            false,
            false
        ));
        assert!(!should_rearm_input(
            Vec2::new(900.0, 500.0),
            false,
            false,
            false
        ));
        assert!(!should_rearm_input(
            Vec2::new(900.0, 500.0),
            true,
            true,
            false
        ));
        assert!(!should_rearm_input(
            Vec2::new(900.0, 500.0),
            true,
            false,
            true
        ));
        assert!(should_rearm_input(
            Vec2::new(900.0, 500.0),
            true,
            false,
            false
        ));
    }

    #[test]
    fn rearm_block_does_not_clear_without_explicit_release() {
        let mut input = PrototypeInput {
            touch_rearm_blocked: true,
            ..PrototypeInput::default()
        };

        assert!(input.touch_rearm_blocked);
        input.rearm_touch_input();
        assert!(!input.touch_rearm_blocked);
    }

    #[test]
    fn mouse_cannot_take_over_until_touch_release_barrier_clears() {
        let mut input = PrototypeInput {
            active_touch: Some(7),
            ..PrototypeInput::default()
        };

        input.release_active_touch(true);
        assert!(input.active_touch.is_none());
        assert!(input.touch_rearm_blocked);
    }

    #[test]
    fn separate_controls_keep_aim_and_power_independent_and_safe() {
        for size in [Vec2::new(1280.0, 634.0), Vec2::new(844.0, 325.0)] {
            let scale = (size / DESIGN_SIZE).min_element();
            let bar = size / 2.0 + Vec2::new(POWER_BAR_X, 0.0) * scale;
            assert_eq!(
                classify_pointer_contact(bar, size, 1),
                PointerContact::Power
            );
            assert_eq!(classify_pointer_contact(bar, size, 0), PointerContact::Aim);
            let cue_ball = Vec2::new(-55.0, 0.0);
            let mut input = PrototypeInput {
                contact_mode: 1,
                pointer_contact: PointerContact::Aim,
                drag_origin: Some(Vec2::new(-165.0, 0.0)),
                ..Default::default()
            };
            let left = size / 2.0 + Vec2::new(-165.0, 0.0) * scale;
            // Contact position, not displacement, controls aim. Pressing on the
            // left of the cue ball immediately points left, even without motion.
            update_from_pointer(&mut input, left, size, Some(cue_ball), &mut false);
            assert!((input.aim_angle.abs() - std::f32::consts::PI).abs() < 0.001);
            update_from_pointer(
                &mut input,
                left + Vec2::new(25.0, 0.0) * scale,
                size,
                Some(cue_ball),
                &mut false,
            );
            assert!((input.aim_angle.abs() - std::f32::consts::PI).abs() < 0.001);
            update_from_pointer(
                &mut input,
                size / 2.0 + Vec2::new(55.0, 0.0) * scale,
                size,
                Some(cue_ball),
                &mut false,
            );
            assert!(input.aim_angle.abs() < 0.001);
            assert!(input.power.abs() < f32::EPSILON);
            assert!(!contact_is_charged(&input));
            let angle = input.aim_angle;
            // Crossing the cue ball's center holds the last valid direction.
            update_from_pointer(
                &mut input,
                size / 2.0 + cue_ball * scale,
                size,
                Some(cue_ball),
                &mut false,
            );
            assert!((input.aim_angle - angle).abs() < f32::EPSILON);
            input.pointer_contact = PointerContact::Power;
            input.drag_origin = Some(Vec2::new(POWER_BAR_X, 0.0));
            update_from_pointer(
                &mut input,
                bar + Vec2::new(0.0, 100.0) * scale,
                size,
                Some(cue_ball),
                &mut false,
            );
            assert!(input.power > 0.0 && contact_is_charged(&input));
            assert!((input.aim_angle - angle).abs() < f32::EPSILON);
            update_from_pointer(&mut input, bar, size, Some(cue_ball), &mut false);
            assert!(input.power.abs() < f32::EPSILON);
            assert!(!contact_is_charged(&input));
            input.release_active_touch(true);
            assert!(!contact_is_charged(&input));
        }
    }

    #[test]
    fn drag_can_start_anywhere_on_table_at_any_scale() {
        for size in [Vec2::new(1280.0, 634.0), Vec2::new(844.0, 325.0)] {
            let scale = (size / DESIGN_SIZE).min_element();
            let center = size / 2.0 + Vec2::new(TABLE_CENTER_X, 0.0) * scale;
            for offset in [
                Vec2::ZERO,
                Vec2::new(-TABLE_SIZE.x / 2.0 + 1.0, -TABLE_SIZE.y / 2.0 + 1.0),
                Vec2::new(TABLE_SIZE.x / 2.0 - 1.0, TABLE_SIZE.y / 2.0 - 1.0),
                Vec2::new(310.0, -120.0),
            ] {
                assert!(pointer_on_table(center + offset * scale, size));
            }
            assert!(!pointer_on_table(
                center + Vec2::new(481.0, 0.0) * scale,
                size
            ));
            assert!(!pointer_on_table(
                center + Vec2::new(0.0, 241.0) * scale,
                size
            ));
        }
    }

    #[test]
    fn drag_updates_aim_and_power_together_at_multiple_viewports() {
        for size in [Vec2::new(1280.0, 634.0), Vec2::new(844.0, 325.0)] {
            let origin = size / 2.0;
            let mut input = PrototypeInput {
                pointer_contact: PointerContact::Aim,
                drag_origin: Some(Vec2::ZERO),
                ..Default::default()
            };
            let scale = (size / DESIGN_SIZE).min_element();
            update_drag(&mut input, origin + Vec2::new(-100.0, 0.0) * scale, size);
            let first = input.power;
            assert!((input.aim_angle).abs() < 0.001);
            assert!(first > 0.0);
            update_drag(&mut input, origin + Vec2::new(0.0, 60.0) * scale, size);
            assert!((input.aim_angle - std::f32::consts::FRAC_PI_2).abs() < 0.001);
            assert!(input.power < first);
            update_drag(&mut input, origin, size);
            assert!(input.power.abs() < f32::EPSILON);
            update_drag(&mut input, origin + Vec2::new(500.0, 0.0) * scale, size);
            assert!((input.power - 1.0).abs() < f32::EPSILON);
            input.release_active_touch(true);
            assert!(input.drag_origin.is_none());
            assert!(input.power.abs() < f32::EPSILON);
        }
    }

    #[test]
    fn focus_loss_cancels_charged_contact_until_release() {
        let mut app = App::new();
        app.add_message::<WindowFocused>()
            .insert_resource(PrototypeInput {
                active_touch: Some(7),
                pointer_contact: PointerContact::Aim,
                drag_origin: Some(Vec2::ZERO),
                power: 0.7,
                ..Default::default()
            })
            .add_systems(Update, cancel_input_on_focus_loss);
        app.world_mut().write_message(WindowFocused {
            window: Entity::PLACEHOLDER,
            focused: false,
        });
        app.update();
        let input = app.world().resource::<PrototypeInput>();
        assert!(input.drag_origin.is_none());
        assert!(input.active_touch.is_none());
        assert_eq!(input.pointer_contact, PointerContact::None);
        assert!(input.touch_rearm_blocked);
        assert!(input.power.abs() < f32::EPSILON);
    }

    #[test]
    fn canceled_touch_is_not_an_intentional_release() {
        use bevy::input::touch::{TouchInput, TouchPhase, touch_screen_input_system};
        let mut app = App::new();
        app.add_message::<TouchInput>()
            .init_resource::<Touches>()
            .add_systems(Update, touch_screen_input_system);
        let touch = |phase| TouchInput {
            phase,
            position: Vec2::new(120.0, 90.0),
            window: Entity::PLACEHOLDER,
            force: None,
            id: 7,
        };
        app.world_mut().write_message(touch(TouchPhase::Started));
        app.update();
        app.world_mut().write_message(touch(TouchPhase::Canceled));
        app.update();
        assert!(!touch_release_is_intentional(
            Some(7),
            app.world().resource::<Touches>()
        ));
        let mut input = PrototypeInput {
            active_touch: Some(7),
            pointer_contact: PointerContact::Aim,
            drag_origin: Some(Vec2::ZERO),
            power: 0.7,
            ..Default::default()
        };
        input.release_active_touch(true);
        assert!(input.drag_origin.is_none() && input.touch_rearm_blocked);
        app.world_mut().write_message(touch(TouchPhase::Started));
        app.update();
        app.world_mut().write_message(touch(TouchPhase::Ended));
        app.update();
        assert!(touch_release_is_intentional(
            Some(7),
            app.world().resource::<Touches>()
        ));
        assert!(touch_release_is_intentional(
            None,
            app.world().resource::<Touches>()
        ));
    }

    #[test]
    fn canceled_contact_cannot_restart_without_a_fresh_press() {
        let mut input = PrototypeInput {
            pointer_contact: PointerContact::Aim,
            drag_origin: Some(Vec2::ZERO),
            power: 0.7,
            ..Default::default()
        };
        input.release_active_touch(true);
        assert!(input.interrupted && input.touch_rearm_blocked);
        assert!(input.drag_origin.is_none());
        assert!(input.power.abs() < f32::EPSILON);
        input.interrupted = false;
        input.rearm_touch_input();
        assert!(!input.touch_rearm_blocked);
    }
}

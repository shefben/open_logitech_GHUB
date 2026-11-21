//! Device model, profiles, actions, lighting and the G HUB importer: the Rust port of
//! `Artemis.Peripherals`, which was already free of any Artemis dependency.
//!
//! Port status is tracked in `docs/PORT_STATUS.md`. Modules appear here as they land.

pub mod action;
pub mod animation;
pub mod automation;
pub mod automation_target;
pub mod device_image;
pub mod diagnostics;
pub mod manager;
pub mod openrgb;
pub mod openrgb_service;
pub mod ownership;
pub mod platform;
pub mod presets;
pub mod effects;
pub mod id;
#[cfg(feature = "import")]
pub mod import;
pub mod injection;
pub mod layout;
pub mod macros;
pub mod profile;
pub mod reactions;
pub mod recorder;
pub mod resolver;
pub mod service;
pub mod store;
pub mod time;
pub mod triggers;

pub use action::{
    ActionBinding, ActionDefinition, DpiCommand, ExecutionSite, KeyModifiers, KeyStroke,
    LightingCommand, MacroCondition, MacroDefinition, MacroRepeatMode, MacroStep, MediaCommand,
    MouseButtonKind, ProfileCommand,
};
pub use device_image::{
    AssignmentMarker, DeviceImageMetadata, DeviceZone, ImageCircle, ImageRect, MaskableZone,
    MetadataError, ZoneComponent, zones,
};
pub use animation::{AnimationEngine, FrameSink, RecordingSink};
pub use ownership::{LightingOwnership, LightingWriter, OwnershipChange, ALL_ZONES};
pub use effects::{EffectState, KeyEffectDefinition, KeyPoint};
pub use id::Id;
pub use macros::{ActionHost, MacroRunState, MacroRunStatus, MacroRuntime};
pub use injection::{InputInjector, RecordingInjector, TrackingInjector};
pub use layout::{KeyDefinition, KeyGroup, LedEvidence};
pub use reactions::{Frame, ReactionEngine};
pub use store::{ProfileStore, StoreError};
pub use resolver::{ForegroundApp, ProfileResolution, ResolutionReason};
pub use profile::{
    AnimationDirection, AnimationSettings, AppLightingOverride, AppLightingRule, AppMatchKind,
    AppRule, AppSettings, DeviceProfileSettings, DpiStage, KeyReaction, KeyReactionMode,
    KeyboardSettings, LightingMode, LightingSettings, MouseSettings, OnboardSnapshot,
    PeripheralProfile, ProfileDocument, TriggerEvent, TriggerRule, CURRENT_SCHEMA_VERSION,
};
pub use diagnostics::{DiagnosticEntry, DiagnosticLevel, DiagnosticsLog};
pub use recorder::MacroRecorder;
pub use triggers::{TriggerEngine, TriggerOccurrence};
pub use manager::{DeviceChange, DeviceChangeKind, DeviceManager, ManagedDevice, ManagerOptions};
pub use platform::{HookSink, HookStatus, PlatformServices};
pub use service::{MacroRecordState, PeripheralService, ServiceEvent};

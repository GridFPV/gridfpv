/**
 * Adapter to the ts-rs–generated protocol bindings — the single seam that knows
 * where the generated wire types physically live.
 *
 * The wire types are generated from the Rust server crate into the repo-root
 * `bindings/` directory (one file per type), per docs/clients.html §3 and
 * architecture.html §6. The frontend NEVER hand-writes a wire type.
 *
 * ── How this barrel works ───────────────────────────────────────────────────
 * ts-rs emits one file per type (`bindings/EventMeta.ts`, `bindings/Pilot.ts`, …)
 * with extensionless cross-imports and no `index.ts` barrel of its own. This file
 * is that barrel: it re-exports every generated type module, resolved through the
 * `@bindings/*` tsconfig path alias (`@bindings/* → ../../../bindings/*`, set in
 * packages/types/tsconfig.json — the one place that knows their physical location).
 *
 * Because everything downstream imports from `@gridfpv/types`, a contract change in
 * Rust (a renamed field, a removed variant) surfaces here — and in every consumer —
 * as a TypeScript compile error rather than silent drift.
 *
 * ── Regenerating ────────────────────────────────────────────────────────────
 * The Rust side regenerates `bindings/*.ts` (`cargo xtask gen`). When a *new* type
 * is added, add a matching `export type * from '@bindings/<Name>';` line below;
 * when one is removed, drop its line. Nothing else in the frontend changes.
 *
 * This list is kept in lockstep with `bindings/` (one line per `bindings/*.ts`), and
 * `cargo xtask ci` enforces that (`cargo xtask barrel`): a generated binding with no line
 * here, or a line here naming a binding that no longer exists, fails the build.
 *
 * That check exists because the gap is invisible otherwise. A type that is generated but
 * never re-exported cannot be imported from `@gridfpv/types` at all, so an author who wants
 * the real wire type is pushed into hand-declaring one — and a hand-declared wire type passes
 * `tsc`, the unit tests and the lint while being wrong in every field name (#410).
 *
 * Ordering is presentation only; the check is about presence.
 */
export type * from '@bindings/ActiveEvent';
export type * from '@bindings/AdapterId';
export type * from '@bindings/BracketRequest';
export type * from '@bindings/Class';
export type * from '@bindings/ClassId';
export type * from '@bindings/ClassPhaseView';
export type * from '@bindings/ClassPlaceView';
export type * from '@bindings/CombineRule';
export type * from '@bindings/Command';
export type * from '@bindings/CommandOutcome';
export type * from '@bindings/CompletionRule';
export type * from '@bindings/ContractVersion';
export type * from '@bindings/CreateClassRequest';
export type * from '@bindings/CreateEventRequest';
export type * from '@bindings/CreatePilotRequest';
export type * from '@bindings/CreateTimerRequest';
export type * from '@bindings/CrossingView';
export type * from '@bindings/DiscardReason';
export type * from '@bindings/DiscoveredTimer';
export type * from '@bindings/DropSeatRequest';
export type * from '@bindings/EliminationKind';
export type * from '@bindings/EndCondition';
export type * from '@bindings/ErrorCode';
export type * from '@bindings/Event';
export type * from '@bindings/EventId';
export type * from '@bindings/EventMeta';
export type * from '@bindings/EventView';
export type * from '@bindings/FieldSlice';
export type * from '@bindings/FirstLap';
export type * from '@bindings/Format';
export type * from '@bindings/GateIndex';
export type * from '@bindings/HeatSeat';
export type * from '@bindings/Hello';
export type * from '@bindings/JoinTokenResponse';
export type * from '@bindings/KioskDeviceRequest';
export type * from '@bindings/KioskDeviceView';
export type * from '@bindings/KioskNameRequest';
export type * from '@bindings/KioskPassView';
export type * from '@bindings/KioskView';
export type * from '@bindings/LapView';
export type * from '@bindings/LineupEntry';
export type * from '@bindings/LogRef';
export type * from '@bindings/MainsRequest';
export type * from '@bindings/Measure';
export type * from '@bindings/OptionalEdit';
export type * from '@bindings/Pass';
export type * from '@bindings/PassRequest';
export type * from '@bindings/PhaseDef';
export type * from '@bindings/PhaseId';
export type * from '@bindings/PhaseKind';
export type * from '@bindings/PhaseView';
export type * from '@bindings/Pilot';
export type * from '@bindings/PilotId';
export type * from '@bindings/PlacedView';
export type * from '@bindings/PresetSeat';
export type * from '@bindings/ProposedDisqualificationView';
export type * from '@bindings/ProtestId';
export type * from '@bindings/ProtestOutcome';
export type * from '@bindings/ProtestView';
export type * from '@bindings/ProtocolError';
export type * from '@bindings/RaceId';
export type * from '@bindings/RaceRules';
export type * from '@bindings/RaceSeatView';
export type * from '@bindings/RaceSetup';
export type * from '@bindings/RaceSignal';
export type * from '@bindings/RaceStateView';
export type * from '@bindings/RaceView';
export type * from '@bindings/RankingRule';
export type * from '@bindings/RemovalReason';
export type * from '@bindings/RosterView';
export type * from '@bindings/RoundRobinRequest';
export type * from '@bindings/SeatLevels';
export type * from '@bindings/SeatRef';
export type * from '@bindings/SeatSignal';
export type * from '@bindings/SeatSource';
export type * from '@bindings/SeatView';
export type * from '@bindings/SeatWhoView';
export type * from '@bindings/ServerHello';
export type * from '@bindings/SessionId';
export type * from '@bindings/SetActiveEventRequest';
export type * from '@bindings/SetClassHiddenRequest';
export type * from '@bindings/SignalChunk';
export type * from '@bindings/SignalContext';
export type * from '@bindings/SignalHistory';
export type * from '@bindings/SignalThresholds';
export type * from '@bindings/SignalUnit';
export type * from '@bindings/SourceTime';
export type * from '@bindings/StartOwner';
export type * from '@bindings/StintView';
export type * from '@bindings/TakeSeatRequest';
export type * from '@bindings/Timer';
export type * from '@bindings/TimerConfig';
export type * from '@bindings/TimerControl';
export type * from '@bindings/LapRules';
export type * from '@bindings/SavedFormat';
export type * from '@bindings/SeatProfile';
export type * from '@bindings/SeatProfileRequest';
export type * from '@bindings/TimerProfiles';
export type * from '@bindings/TimerKind';
export type * from '@bindings/TimerLive';
export type * from '@bindings/TimerSeatConfig';
export type * from '@bindings/TimerStatus';
export type * from '@bindings/TimerThreshold';
export type * from '@bindings/TimerView';
export type * from '@bindings/ObsStatus';
export type * from '@bindings/RaceVideoView';
export type * from '@bindings/RecordFrom';
export type * from '@bindings/RenderJob';
export type * from '@bindings/RenderRequest';
export type * from '@bindings/RenderState';
export type * from '@bindings/RenderTools';
export type * from '@bindings/RenderWhen';
export type * from '@bindings/SaveIn';
export type * from '@bindings/VideoOverlay';
export type * from '@bindings/DataCopy';
export type * from '@bindings/DataExport';
export type * from '@bindings/DataInfo';
export type * from '@bindings/OverlayCorner';
export type * from '@bindings/OverlayLook';
export type * from '@bindings/OverlayPlacement';
export type * from '@bindings/Caller';
export type * from '@bindings/AddressRequest';
export type * from '@bindings/BuiltinClasses';
export type * from '@bindings/Door';
export type * from '@bindings/DoorRequest';
export type * from '@bindings/DoorState';
export type * from '@bindings/DoorView';
export type * from '@bindings/NetworkView';
export type * from '@bindings/TokenRequest';
export type * from '@bindings/PublicHello';
export type * from '@bindings/PublicEvent';
export type * from '@bindings/PublicEventName';
export type * from '@bindings/PilotAccessView';
export type * from '@bindings/PilotCodeView';
export type * from '@bindings/PilotSignInRequest';
export type * from '@bindings/PublicPilot';
export type * from '@bindings/SeatChannel';
export type * from '@bindings/LeaveSpotRequest';
export type * from '@bindings/NextSpotRequest';
export type * from '@bindings/SignupBy';
export type * from '@bindings/SignupCommand';
export type * from '@bindings/SignupHeatState';
export type * from '@bindings/SignupHeatView';
export type * from '@bindings/SignupPilotView';
export type * from '@bindings/SignupRules';
export type * from '@bindings/SignupSeat';
export type * from '@bindings/SignupView';
export type * from '@bindings/TakeSpotRequest';
export type * from '@bindings/VideoRun';
export type * from '@bindings/VideoRunState';
export type * from '@bindings/VideoSettings';
export type * from '@bindings/VideoSettingsRequest';
export type * from '@bindings/VideoStatus';
export type * from '@bindings/UncountedView';
export type * from '@bindings/UncountedWhy';
export type * from '@bindings/UpdateClassRequest';
export type * from '@bindings/UpdatePilotRequest';
export type * from '@bindings/VtxType';
export type * from '@bindings/RenameTimerRequest';
export type * from '@bindings/PushClub';
export type * from '@bindings/PushEntry';
export type * from '@bindings/PushHeader';
export type * from '@bindings/PushHello';
export type * from '@bindings/PushLog';
export type * from '@bindings/PushRefusal';
export type * from '@bindings/PushRefused';
export type * from '@bindings/PushStatus';
export type * from '@bindings/ServerEvent';
export type * from '@bindings/LinkEvent';
export type * from '@bindings/LinkEventState';
export type * from '@bindings/LinkClub';
export type * from '@bindings/PilotProfile';
export type * from '@bindings/EmailChange';
export type * from '@bindings/EmailConfirm';
export type * from '@bindings/EmailConfirmed';
export type * from '@bindings/Forgot';
export type * from '@bindings/MyEmail';
export type * from '@bindings/MailSource';
export type * from '@bindings/MailTls';
export type * from '@bindings/ServerEmail';
export type * from '@bindings/ServerEmailChange';
export type * from '@bindings/ServerEmailTest';
export type * from '@bindings/PasswordChange';
export type * from '@bindings/PasswordSignIn';
export type * from '@bindings/PasswordSignInChange';
export type * from '@bindings/PasswordSignUp';
export type * from '@bindings/ClubOpen';
export type * from '@bindings/NewMember';
export type * from '@bindings/PersonView';
export type * from '@bindings/ServerInfo';
export type * from '@bindings/SignUpChange';
export type * from '@bindings/ClubSettings';
export type * from '@bindings/EventClub';
export type * from '@bindings/LinkState';
export type * from '@bindings/LinkTrust';
export type * from '@bindings/LinkView';
export type * from '@bindings/ClubRole';
export type * from '@bindings/InviteFor';
export type * from '@bindings/InviteMade';
export type * from '@bindings/InviteRequest';
export type * from '@bindings/InviteView';
export type * from '@bindings/Me';
export type * from '@bindings/MeChange';
export type * from '@bindings/MemberView';
export type * from '@bindings/MyAccount';
export type * from '@bindings/MyClub';
export type * from '@bindings/MySignIn';
export type * from '@bindings/RoleChange';
export type * from '@bindings/SignInProvider';
export type * from '@bindings/ClubPilotView';
export type * from '@bindings/HolderChange';
export type * from '@bindings/InvitePilot';
export type * from '@bindings/MyClassResult';
export type * from '@bindings/MyPilot';
export type * from '@bindings/MyResult';
export type * from '@bindings/PushAccount';
export type * from '@bindings/PushPilot';
export type * from '@bindings/PushPilotAccount';
export type * from '@bindings/LinkPeople';
export type * from '@bindings/PilotAccount';
export type * from '@bindings/PilotAccountChange';
export type * from '@bindings/PullRequest';
export type * from '@bindings/PilotAsk';
export type * from '@bindings/PushAnswer';
export type * from '@bindings/PushRequest';
export type * from '@bindings/ClubChange';
export type * from '@bindings/EventListing';
export type * from '@bindings/LinkTokenMade';
export type * from '@bindings/LinkTokenRequest';
export type * from '@bindings/LinkTokenView';
export type * from '@bindings/PushHeld';
export type * from '@bindings/RaceCodeView';
export type * from '@bindings/CertificateLook';
export type * from '@bindings/CertificateRequest';
export type * from '@bindings/AccountSignIns';
export type * from '@bindings/ReconnectMade';
export type * from '@bindings/ReconnectView';
export type * from '@bindings/ServerClub';
export type * from '@bindings/NewClub';
export type * from '@bindings/ClubArchived';
export type * from '@bindings/PrimaryClub';

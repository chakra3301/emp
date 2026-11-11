// Method and notification catalogue for the EMP JSON-RPC 2.0 protocol.
//
// MUST stay in sync with crates/emp-protocol/src/rpc.rs (`methods`, `notifications` and
// `codes` modules). The Rust crate is the source of truth; the payload types come from ts-rs.

import type { AgentIdParams } from "./generated/AgentIdParams";
import type { DevServer } from "./generated/DevServer";
import type { ThreadAnswerParams } from "./generated/ThreadAnswerParams";
import type { ThreadCommandParams } from "./generated/ThreadCommandParams";
import type { ThreadGoal } from "./generated/ThreadGoal";
import type { SessionHolder } from "./generated/SessionHolder";
import type { CloseElsewhereResult } from "./generated/CloseElsewhereResult";
import type { ServersKillParams } from "./generated/ServersKillParams";
import type { ServersKillResult } from "./generated/ServersKillResult";
import type { AgentProfile } from "./generated/AgentProfile";
import type { AgentSaveParams } from "./generated/AgentSaveParams";
import type { Attachment } from "./generated/Attachment";
import type { AttachmentUploadParams } from "./generated/AttachmentUploadParams";
import type { LocateFileParams } from "./generated/LocateFileParams";
import type { LocateFolderParams } from "./generated/LocateFolderParams";
import type { CommandsListParams } from "./generated/CommandsListParams";
import type { CommandsRunParams } from "./generated/CommandsRunParams";
import type { ContextUsage } from "./generated/ContextUsage";
import type { SlashCommand } from "./generated/SlashCommand";
import type { DaemonSettings } from "./generated/DaemonSettings";
import type { DeviceIdParams } from "./generated/DeviceIdParams";
import type { ModelOption } from "./generated/ModelOption";
import type { PairingCode } from "./generated/PairingCode";
import type { PhoneStatus } from "./generated/PhoneStatus";
import type { ModelsListParams } from "./generated/ModelsListParams";
import type { ProjectCreateParams } from "./generated/ProjectCreateParams";
import type { ProjectFiles } from "./generated/ProjectFiles";
import type { ProjectFilesParams } from "./generated/ProjectFilesParams";
import type { DoctorReport } from "./generated/DoctorReport";
import type { HelloParams } from "./generated/HelloParams";
import type { HelloResult } from "./generated/HelloResult";
import type { ImportProgress } from "./generated/ImportProgress";
import type { ImportReport } from "./generated/ImportReport";
import type { ImportRunParams } from "./generated/ImportRunParams";
import type { Page } from "./generated/Page";
import type { Project } from "./generated/Project";
import type { SpawnSpec } from "./generated/SpawnSpec";
import type { Thread } from "./generated/Thread";
import type { GroupAddParams } from "./generated/GroupAddParams";
import type { GroupCreateParams } from "./generated/GroupCreateParams";
import type { GroupIdParams } from "./generated/GroupIdParams";
import type { GroupUpdateParams } from "./generated/GroupUpdateParams";
import type { ThreadGroup } from "./generated/ThreadGroup";
import type { ExternalSchedule } from "./generated/ExternalSchedule";
import type { Schedule } from "./generated/Schedule";
import type { ScheduleEnableParams } from "./generated/ScheduleEnableParams";
import type { ScheduleIdParams } from "./generated/ScheduleIdParams";
import type { SchedulePreview } from "./generated/SchedulePreview";
import type { SchedulePreviewParams } from "./generated/SchedulePreviewParams";
import type { ScheduleSaveParams } from "./generated/ScheduleSaveParams";
import type { ThreadIdParams } from "./generated/ThreadIdParams";
import type { ThreadsListParams } from "./generated/ThreadsListParams";
import type { TranscriptItem } from "./generated/TranscriptItem";
import type { TranscriptParams } from "./generated/TranscriptParams";
import type { LiveThread } from "./generated/LiveThread";
import type { TerminalIdParams } from "./generated/TerminalIdParams";
import type { TerminalInfo } from "./generated/TerminalInfo";
import type { TerminalOpenParams } from "./generated/TerminalOpenParams";
import type { TerminalOutput } from "./generated/TerminalOutput";
import type { TerminalResizeParams } from "./generated/TerminalResizeParams";
import type { TerminalSnapshot } from "./generated/TerminalSnapshot";
import type { TerminalWriteParams } from "./generated/TerminalWriteParams";
import type { ThreadEvent } from "./generated/ThreadEvent";
import type { ThreadSendParams } from "./generated/ThreadSendParams";
import type { ThreadStartParams } from "./generated/ThreadStartParams";
import type { ThreadTakeoverParams } from "./generated/ThreadTakeoverParams";

/** Protocol version sent in `hello`. Bump together with the daemon. */
export const PROTOCOL_VERSION = 1;

/** Client → daemon requests. `params: undefined` means the method takes no params. */
export interface EmpMethods {
  hello: { params: HelloParams; result: HelloResult };
  doctor: { params: undefined; result: DoctorReport };
  /** Long-running; emits `import.progress` then `import.finished` while it runs. */
  "import.run": { params: ImportRunParams; result: ImportReport };
  "projects.list": { params: undefined; result: Project[] };
  "agents.list": { params: undefined; result: AgentProfile[] };
  "threads.list": { params: ThreadsListParams; result: Page<Thread> };
  /** Fails with `RPC_ERROR_CODES.NOT_FOUND` when the thread does not exist. */
  "threads.get": { params: ThreadIdParams; result: Thread };
  "threads.transcript": { params: TranscriptParams; result: Page<TranscriptItem> };
  /** `null` when the harness cannot resume this session. */
  "threads.resumeCommand": { params: ThreadIdParams; result: SpawnSpec | null };
  "threads.context": { params: ThreadIdParams; result: ContextUsage | null };
  /** Stores a dropped/pasted file; pass its `id` in `threads.send`/`threads.start` `attachments`. */
  "attachments.upload": { params: AttachmentUploadParams; result: Attachment };
  /** Where a dropped folder may be on this Mac, likeliest first (web views only know its name). */
  "attachments.locateFolder": { params: LocateFolderParams; result: string[] };
  "attachments.locateFile": { params: LocateFileParams; result: Attachment | null };
  /** Slash commands for a harness; `run` says whether chat or the terminal UI runs each. */
  "commands.list": { params: CommandsListParams; result: SlashCommand[] };
  /** Opens the harness TUI (a takeover with `threadId`) and types the command into it. */
  "commands.run": { params: CommandsRunParams; result: TerminalInfo };
  /** The owner's processes listening on TCP ports (dev servers, databases, apps). */
  "servers.list": { params: undefined; result: DevServer[] };
  /** Stops one listener after re-checking it; `stopped` is false if it outlived 3 s. */
  "servers.kill": { params: ServersKillParams; result: ServersKillResult };

  // ---- P1: terminals (daemon-owned PTYs; bytes are base64) ----
  /** Spawns a PTY. Pushes `terminal.state` and then `terminal.output`. */
  "terminals.open": { params: TerminalOpenParams; result: TerminalInfo };
  "terminals.list": { params: undefined; result: TerminalInfo[] };
  /** Recent scrollback to replay; then apply `terminal.output` pushes with `seq` greater than the snapshot's. */
  "terminals.attach": { params: TerminalIdParams; result: TerminalSnapshot };
  "terminals.write": { params: TerminalWriteParams; result: null };
  "terminals.resize": { params: TerminalResizeParams; result: null };
  /** Kills a running terminal; forgets an exited one. */
  "terminals.kill": { params: TerminalIdParams; result: null };

  // ---- P1: live sessions (structured drivers) ----
  /** New thread with an agent + first message. Resolves once the harness session exists. */
  "threads.start": { params: ThreadStartParams; result: Thread };
  /** Starts the driver on demand (resuming the session). `BUSY` while a terminal owns it. */
  "threads.send": { params: ThreadSendParams; result: LiveThread };
  "threads.interrupt": { params: ThreadIdParams; result: LiveThread };
  /** Answers one of `LiveThread.questions` (a pi dialog or EMP's own picker). */
  "threads.answer": { params: ThreadAnswerParams; result: LiveThread };
  /** Runs a `native` slash command in the chat (pi: /compact, /model, /name, /session, …). */
  "threads.command": { params: ThreadCommandParams; result: LiveThread };
  "threads.goal": { params: ThreadIdParams; result: ThreadGoal | null };
  "threads.holders": { params: ThreadIdParams; result: SessionHolder[] };
  "threads.closeElsewhere": { params: ThreadIdParams; result: CloseElsewhereResult };
  /** Attaches to a Codex thread Codex is working on right now; true if it did. */
  "threads.watch": { params: ThreadIdParams; result: boolean };
  /** Every thread a driver or takeover terminal currently holds. */
  "threads.live": { params: undefined; result: LiveThread[] };
  /** Stops the driver (`BUSY` mid-turn) and opens the harness TUI on the same session. */
  "threads.takeover": { params: ThreadTakeoverParams; result: TerminalInfo };
  /** Closes the takeover terminal and returns the session to chat. */
  "threads.release": { params: ThreadIdParams; result: LiveThread };

  // ---- P1: agent editor ----
  /** Create (no id) or update. Editing an imported agent stops re-import from overwriting it. */
  "agents.save": { params: AgentSaveParams; result: AgentProfile };
  /** Threads keep their history; their agent link is cleared. */
  "agents.delete": { params: AgentIdParams; result: null };
  /** Models a harness can run (pi/Codex/Claude/Hermes), for the composer's model picker. */
  "models.list": { params: ModelsListParams; result: ModelOption[] };
  /** Registers a folder as a project; `createFolder` makes it if missing. */
  "projects.create": { params: ProjectCreateParams; result: Project };
  /** Top level of a project's folder (folders first, hidden entries skipped). */
  "projects.files": { params: ProjectFilesParams; result: ProjectFiles };
  /** Phone access: Tailscale state, the tailnet URL and paired devices. */
  "phone.status": { params: undefined; result: PhoneStatus };
  /** Turns on `tailscale serve` (tailnet-only HTTPS) for this daemon. */
  "phone.enable": { params: undefined; result: PhoneStatus };
  "phone.disable": { params: undefined; result: PhoneStatus };
  /** A fresh 10-minute pairing code (text + QR link). */
  "phone.pair": { params: undefined; result: PairingCode };
  /** Unpairs a device and closes its connections. */
  "devices.revoke": { params: DeviceIdParams; result: PhoneStatus };
  /** Daemon-wide settings, shared by every window and phone (e.g. the compaction limit). */
  "settings.get": { params: undefined; result: DaemonSettings };
  "settings.set": { params: DaemonSettings; result: DaemonSettings };
  /** The owner's thread groups, pinned first. Every change returns (and pushes) the full list. */
  "groups.list": { params: undefined; result: ThreadGroup[] };
  "groups.create": { params: GroupCreateParams; result: ThreadGroup[] };
  "groups.update": { params: GroupUpdateParams; result: ThreadGroup[] };
  "groups.delete": { params: GroupIdParams; result: ThreadGroup[] };
  /** Moves threads into a group (out of any other). */
  "groups.add": { params: GroupAddParams; result: ThreadGroup[] };
  /** Takes a thread out of its group. */
  "groups.remove": { params: ThreadIdParams; result: ThreadGroup[] };
  /** Scheduled tasks, next run first. Changes are pushed as `schedules.changed`. */
  "schedules.list": { params: undefined; result: Schedule[] };
  /** Creates (no `id`) or replaces a schedule. */
  "schedules.save": { params: ScheduleSaveParams; result: Schedule };
  "schedules.delete": { params: ScheduleIdParams; result: null };
  /** Pause / resume. */
  "schedules.enable": { params: ScheduleEnableParams; result: Schedule };
  /** One run now; the timetable carries on. */
  "schedules.run": { params: ScheduleIdParams; result: Schedule };
  /** The timetable in words and its next runs, or why it can't be used. */
  "schedules.preview": { params: SchedulePreviewParams; result: SchedulePreview };
  /** Jobs other tools schedule themselves (Hermes cron), read-only. */
  "schedules.external": { params: undefined; result: ExternalSchedule[] };
}

/** Daemon → client pushes (JSON-RPC messages without `id`). */
export interface EmpNotifications {
  "import.progress": ImportProgress;
  "import.finished": ImportReport;
  "terminal.output": TerminalOutput;
  "terminal.state": TerminalInfo;
  "thread.event": ThreadEvent;
  "settings.changed": DaemonSettings;
  "groups.changed": ThreadGroup[];
  "schedules.changed": Schedule[];
  /** Suki (the coordinator) asked windows to show this thread. */
  "ui.openThread": ThreadIdParams;
}

export type EmpMethodName = keyof EmpMethods;
export type EmpNotificationName = keyof EmpNotifications;
export type EmpParams<M extends EmpMethodName> = EmpMethods[M]["params"];
export type EmpResult<M extends EmpMethodName> = EmpMethods[M]["result"];

export const EMP_METHODS = [
  "hello",
  "doctor",
  "import.run",
  "projects.list",
  "agents.list",
  "threads.list",
  "threads.get",
  "threads.transcript",
  "threads.resumeCommand",
  "threads.context",
  "attachments.upload",
  "attachments.locateFolder",
  "attachments.locateFile",
  "commands.list",
  "commands.run",
  "servers.list",
  "servers.kill",
  "terminals.open",
  "terminals.list",
  "terminals.attach",
  "terminals.write",
  "terminals.resize",
  "terminals.kill",
  "threads.start",
  "threads.send",
  "threads.interrupt",
  "threads.answer",
  "threads.command",
  "threads.goal",
  "threads.holders",
  "threads.closeElsewhere",
  "threads.watch",
  "threads.live",
  "threads.takeover",
  "threads.release",
  "agents.save",
  "agents.delete",
  "models.list",
  "projects.create",
  "projects.files",
  "phone.status",
  "phone.enable",
  "phone.disable",
  "phone.pair",
  "devices.revoke",
  "settings.get",
  "settings.set",
  "groups.list",
  "groups.create",
  "groups.update",
  "groups.delete",
  "groups.add",
  "groups.remove",
  "schedules.list",
  "schedules.save",
  "schedules.delete",
  "schedules.enable",
  "schedules.run",
  "schedules.preview",
] as const satisfies readonly EmpMethodName[];

export const EMP_NOTIFICATIONS = [
  "import.progress",
  "import.finished",
  "terminal.output",
  "terminal.state",
  "thread.event",
  "settings.changed",
  "groups.changed",
  "schedules.changed",
  "ui.openThread",
] as const satisfies readonly EmpNotificationName[];

/** Standard JSON-RPC codes plus EMP-specific ones (-32000..-32099). Mirrors `rpc::codes`. */
export const RPC_ERROR_CODES = {
  PARSE_ERROR: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL: -32603,
  /** `hello` has not been completed on this connection. */
  NOT_AUTHENTICATED: -32001,
  /** Client protocol version is incompatible with the daemon. */
  INCOMPATIBLE_VERSION: -32002,
  NOT_FOUND: -32004,
  /** The thread's session is busy (turn running, or held by another owner). */
  BUSY: -32010,
  /** The harness has no structured driver (yet), or the operation does not apply. */
  UNSUPPORTED: -32011,
  /** Spawning or talking to a harness process failed. */
  HARNESS_ERROR: -32012,
} as const;

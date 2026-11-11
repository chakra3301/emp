import { useEffect, useRef } from "react";
import { EmptyState, ErrorState, HarnessBadge, LoadingState } from "../components/common";
import { shortPath } from "../lib/format";
import { formatDuration, formatRelative } from "../lib/time";
import { useWorkspace } from "../store/context";

const SOURCE_NAMES: Record<string, string> = { pi: "pi", hermes: "Hermes", claude: "Claude Code", codex: "Codex", orion: "Orion Terminal" };

export function ImportDoctor() {
  const doctor = useWorkspace((s) => s.doctor);
  const importRun = useWorkspace((s) => s.importRun);
  const connected = useWorkspace((s) => s.connection.state === "ready");
  const now = useWorkspace((s) => s.now);
  const loadDoctor = useWorkspace((s) => s.loadDoctor);
  const runImport = useWorkspace((s) => s.runImport);
  const logRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [importRun.lines]);

  const report = doctor.data;

  return (
    <section className="emp-page" data-orion-ui="panel" aria-label="Import and Doctor">
      <div className="emp-toolbar emp-toolbar--page" data-orion-ui="toolbar">
        <div className="emp-toolbar__row">
          <h2 className="emp-heading emp-heading--lg" data-orion-ui="title">
            Sources
          </h2>
          <span className="emp-row__spacer" />
          <button type="button" className="emp-button" data-orion-ui="button" disabled={doctor.status === "loading"} onClick={() => void loadDoctor()}>
            Refresh
          </button>
          <button
            type="button"
            className="emp-button emp-button--primary"
            data-orion-ui="button"
            disabled={importRun.running || !connected}
            onClick={() => void runImport()}
          >
            {importRun.running ? "Importing…" : "Import now"}
          </button>
        </div>
        {report && (
          <div className="emp-note" data-orion-ui="muted">
            Where EMP finds your agents and their history · empd {report.daemonVersion} · data in{" "}
            <span className="emp-mono" title={report.dataDir}>
              {shortPath(report.dataDir)}
            </span>
          </div>
        )}
      </div>

      <div className="emp-page__body">
        {(importRun.running || importRun.lines.length > 0 || importRun.error || importRun.report) && (
          <div className="emp-block">
            <h3 className="emp-subheading" data-orion-ui="title">
              Import
            </h3>
            {importRun.lines.length > 0 && (
              <div ref={logRef} className="emp-log emp-mono" role="log" aria-live="polite">
                {importRun.lines.map((l) => (
                  <div key={l.id}>
                    <span className="emp-log__source">[{l.source}]</span> {l.message}
                  </div>
                ))}
              </div>
            )}
            {importRun.running && importRun.lines.length === 0 && <LoadingState label="Starting import…" />}
            {importRun.error && <ErrorState message={importRun.error} onRetry={() => void runImport()} />}
            {importRun.report && (
              <table className="emp-table">
                <thead>
                  <tr>
                    <th>Source</th>
                    <th>Seen</th>
                    <th>New</th>
                    <th>Updated</th>
                    <th>Unchanged</th>
                    <th>Agents</th>
                    <th>Skipped</th>
                    <th>Time</th>
                    <th>Result</th>
                  </tr>
                </thead>
                <tbody>
                  {importRun.report.sources.map((s) => (
                    <tr key={s.source}>
                      <td>{s.source}</td>
                      <td>{s.stats.threadsSeen}</td>
                      <td>{s.stats.threadsAdded}</td>
                      <td>{s.stats.threadsUpdated}</td>
                      <td>{s.stats.threadsUnchanged}</td>
                      <td>{s.stats.agentsSeen}</td>
                      <td>{s.stats.skipped}</td>
                      <td>{formatDuration(s.durationMs)}</td>
                      <td>
                        {s.error ? (
                          <span className="emp-text-error">{s.error}</span>
                        ) : s.stats.warnings.length ? (
                          <span title={s.stats.warnings.join("\n")}>{s.stats.warnings.length} warnings</span>
                        ) : (
                          "ok"
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        )}

        {doctor.status === "loading" && !report && <LoadingState label="Running doctor…" />}
        {doctor.status === "error" && <ErrorState message={doctor.error ?? "Doctor failed"} onRetry={() => void loadDoctor()} />}
        {doctor.status === "idle" && !report && <EmptyState title="Not connected" hint="Doctor runs once the daemon connection is ready." />}

        {report && (
          <>
            <div className="emp-block">
              <h3 className="emp-subheading" data-orion-ui="title">
                Agent programs
              </h3>
              <table className="emp-table">
                <thead>
                  <tr>
                    <th>Harness</th>
                    <th>Installed</th>
                    <th>Version</th>
                    <th>Path</th>
                  </tr>
                </thead>
                <tbody>
                  {report.harnesses.map((h) => (
                    <tr key={h.harness}>
                      <td>
                        <HarnessBadge harness={h.harness} />
                      </td>
                      <td>
                        <span className="emp-flag" data-ok={h.installed}>
                          <span className="emp-dot" data-state={h.installed ? "ready" : undefined} aria-hidden="true" />
                          {h.installed ? "Installed" : "Missing"}
                        </span>
                      </td>
                      <td className="emp-cell-clip" title={h.version ?? undefined}>
                        {h.version ? h.version.split(/\s+[·(]/)[0] : "—"}
                      </td>
                      <td className="emp-mono" title={h.path ?? undefined}>
                        {h.path ? shortPath(h.path, 44) : "—"}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>

            <div className="emp-block">
              <h3 className="emp-subheading" data-orion-ui="title">
                History sources
              </h3>
              <table className="emp-table">
                <thead>
                  <tr>
                    <th>Source</th>
                    <th>Found</th>
                    <th>Paths</th>
                    <th className="emp-cell-num">Threads</th>
                    <th className="emp-cell-num">Agents</th>
                    <th>Last import</th>
                  </tr>
                </thead>
                <tbody>
                  {report.sources.map((s) => (
                    <tr key={s.source}>
                      <td className="emp-cell-strong">{SOURCE_NAMES[s.source] ?? s.source}</td>
                      <td>
                        <span className="emp-flag" data-ok={s.found}>
                          <span className="emp-dot" data-state={s.found ? "ready" : undefined} aria-hidden="true" />
                          {s.found ? "Found" : "Not found"}
                        </span>
                      </td>
                      <td className="emp-mono">
                        {s.paths.length
                          ? s.paths.map((p) => (
                              <div key={p} className="emp-truncate" title={p}>
                                {shortPath(p, 56)}
                              </div>
                            ))
                          : "—"}
                      </td>
                      <td className="emp-cell-num">{s.threadCount.toLocaleString()}</td>
                      <td className="emp-cell-num">{s.agentCount}</td>
                      <td title={s.lastImportAt ? new Date(s.lastImportAt).toLocaleString() : undefined}>
                        {s.lastImportAt ? formatRelative(s.lastImportAt, now) : "never"}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </>
        )}
      </div>
    </section>
  );
}

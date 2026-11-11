import { createContext, useContext, type ReactNode } from "react";
import { useStore } from "zustand";
import type { EmpHost } from "../host";
import type { Workspace, WorkspaceClient, WorkspaceStore } from "./workspace";

interface WorkspaceContextValue {
  store: WorkspaceStore;
  host: EmpHost;
  /** For hot paths that bypass the store (terminal bytes). */
  client: WorkspaceClient;
}

const WorkspaceContext = createContext<WorkspaceContextValue | null>(null);

export function WorkspaceProvider(props: WorkspaceContextValue & { children: ReactNode }) {
  const { store, host, client, children } = props;
  return <WorkspaceContext.Provider value={{ store, host, client }}>{children}</WorkspaceContext.Provider>;
}

function useWorkspaceContext(): WorkspaceContextValue {
  const ctx = useContext(WorkspaceContext);
  if (!ctx) throw new Error("EMP components must be rendered inside <EmpWorkspace>");
  return ctx;
}

/** Select from this instance's store. Selectors must return stable values (state slices). */
export function useWorkspace<T>(selector: (s: Workspace) => T): T {
  return useStore(useWorkspaceContext().store, selector);
}

export function useWorkspaceStore(): WorkspaceStore {
  return useWorkspaceContext().store;
}

export function useHost(): EmpHost {
  return useWorkspaceContext().host;
}

export function useClient(): WorkspaceClient {
  return useWorkspaceContext().client;
}

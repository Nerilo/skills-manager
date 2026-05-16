export interface LibraryReplica {
  kind: "wsl";
  distroName: string;
  path: string;
  reachable: boolean;
}

export interface WindowsLocation {
  kind: "windows";
  path: string;
}

export interface WslLocation {
  kind: "wsl";
  distroName: string;
  path: string;
  reachable: boolean;
}

export type SkillLocation = WindowsLocation | WslLocation;

export interface LocationLabelVocabulary {
  windows: string;
  wsl: string;
}

export interface WslRuntimeEnvironment {
  distro_name: string;
  library_replica_path: string;
  reachable: boolean;
}

export interface WslAgentTarget {
  agentKey: string;
  displayName: string;
  runtime: WslRuntimeEnvironment;
  libraryReplica: LibraryReplica;
}

export function locationFromWindowsPath(path: string): WindowsLocation {
  return { kind: "windows", path };
}

export function libraryReplicaFromRuntime(runtime: WslRuntimeEnvironment): LibraryReplica {
  return {
    kind: "wsl",
    distroName: runtime.distro_name,
    path: runtime.library_replica_path,
    reachable: runtime.reachable,
  };
}

export function locationFromWslRuntime(runtime: WslRuntimeEnvironment): WslLocation {
  return {
    kind: "wsl",
    distroName: runtime.distro_name,
    path: runtime.library_replica_path,
    reachable: runtime.reachable,
  };
}

export function wslAgentTargetFromRuntime(
  agentKey: string,
  displayName: string,
  runtime: WslRuntimeEnvironment,
): WslAgentTarget {
  return {
    agentKey,
    displayName,
    runtime,
    libraryReplica: libraryReplicaFromRuntime(runtime),
  };
}

export function formatLocationLabel(
  location: SkillLocation,
  labels: LocationLabelVocabulary = { windows: "Windows", wsl: "WSL" },
): string {
  if (location.kind === "windows") {
    return `${labels.windows} · ${location.path}`;
  }

  return `${labels.wsl} · ${location.distroName}:${location.path}`;
}

export function validateWslRuntimeInput(
  distroName: string,
  libraryReplicaPath: string,
): string[] {
  const errors: string[] = [];

  if (!distroName.trim()) {
    errors.push("settings.wslDistroRequired");
  }

  if (!libraryReplicaPath.trim()) {
    errors.push("settings.wslReplicaPathRequired");
  }

  return errors;
}

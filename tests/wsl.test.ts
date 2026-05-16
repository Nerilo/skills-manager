import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  formatLocationLabel,
  locationFromWindowsPath,
  locationFromWslRuntime,
  validateWslRuntimeInput,
  wslAgentTargetFromRuntime,
} from "../src/lib/wsl.js";

describe("WSL location display model", () => {
  it("distinguishes WSL runtime locations from Windows paths", () => {
    const windowsLocation = locationFromWindowsPath("C:\\Users\\me\\.codex\\skills");
    const wslLocation = locationFromWslRuntime({
      distro_name: "Ubuntu-24.04",
      library_replica_path: "/home/me/.codex/skills",
      reachable: true,
      agent_targets: [],
    });

    assert.equal(formatLocationLabel(windowsLocation), "Windows · C:\\Users\\me\\.codex\\skills");
    assert.equal(formatLocationLabel(wslLocation), "WSL · Ubuntu-24.04:/home/me/.codex/skills");
  });

  it("uses shared validation keys for WSL runtime input", () => {
    assert.deepEqual(validateWslRuntimeInput("", ""), [
      "settings.wslDistroRequired",
      "settings.wslReplicaPathRequired",
    ]);
  });

  it("builds WSL-backed agent targets from the shared runtime and replica model", () => {
    const runtime = {
      distro_name: "Ubuntu-24.04",
      library_replica_path: "/home/me/.codex/skills",
      reachable: false,
      agent_targets: [],
    };

    assert.deepEqual(wslAgentTargetFromRuntime("codex", "Codex", runtime), {
      agentKey: "codex",
      displayName: "Codex",
      runtime,
      libraryReplica: {
        kind: "wsl",
        distroName: "Ubuntu-24.04",
        path: "/home/me/.codex/skills",
        reachable: false,
      },
    });
  });
});

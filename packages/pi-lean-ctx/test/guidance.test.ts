// SPDX-License-Identifier: Apache-2.0
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import type { Skill } from "@earendil-works/pi-coding-agent";

import {
  AGENTS_READ_CAP_BYTES,
  agentsRulesBlockPresent,
  GUIDANCE_CUSTOM_TYPE,
  guidanceNote,
  hasGuidanceNote,
  piAgentsPath,
  preferGuidelines,
  skillsSectionFallback,
} from "../extensions/guidance.js";

describe("preferGuidelines", () => {
  it("names the ctx tool and its builtin when both stay available", () => {
    const [line] = preferGuidelines("ctx_shell", true, new Set());
    expect(line).toContain("ctx_shell");
    expect(line).toContain("built-in bash");
  });

  it("is empty when guidance is off or the builtin is suppressed", () => {
    expect(preferGuidelines("ctx_read", false, new Set())).toEqual([]);
    expect(preferGuidelines("ctx_shell", true, new Set(["bash"]))).toEqual([]);
    // Suppressing bash (routeShell) leaves the read guidance in place.
    expect(preferGuidelines("ctx_read", true, new Set(["bash"]))).toHaveLength(1);
  });
});

describe("guidanceNote", () => {
  it("lists only pairs where both tools are active, in a fixed order", () => {
    const note = guidanceNote(["bash", "ctx_shell", "grep", "read", "ctx_read", "ls"]);
    expect(note).toBe(
      "lean-ctx is active. Prefer ctx_read, ctx_shell over the built-in read, bash tools: "
        + "their output is cached and compressed.",
    );
  });

  it("is deterministic regardless of the active-tool order", () => {
    const a = guidanceNote(["read", "ctx_read", "grep", "ctx_grep"]);
    const b = guidanceNote(["ctx_grep", "grep", "ctx_read", "read"]);
    expect(a).toBe(b);
  });

  it("is omitted when no builtin has an active ctx_* counterpart (replace mode)", () => {
    expect(guidanceNote(["ctx_read", "ctx_shell", "edit", "write"])).toBeUndefined();
  });
});

describe("hasGuidanceNote", () => {
  it("detects the note in a resumed transcript and nothing else", () => {
    const note = { type: "custom_message", customType: GUIDANCE_CUSTOM_TYPE, content: "x", display: false };
    const other = { type: "custom_message", customType: "other-extension", content: "x", display: false };
    expect(hasGuidanceNote([other, note])).toBe(true);
    expect(hasGuidanceNote([other, { type: "message" }, null])).toBe(false);
  });
});

describe("agentsRulesBlockPresent", () => {
  it("skips the note only when Pi's AGENTS.md carries the lean-ctx rules block", () => {
    const home = mkdtempSync(join(tmpdir(), "pi-lean-ctx-agents-"));
    try {
      const agentDir = join(home, ".pi", "agent");
      mkdirSync(agentDir, { recursive: true });
      expect(agentsRulesBlockPresent({}, home)).toBe(false);
      writeFileSync(join(agentDir, "AGENTS.md"), "# My rules\n");
      expect(agentsRulesBlockPresent({}, home)).toBe(false);
      writeFileSync(
        join(agentDir, "AGENTS.md"),
        "# My rules\n<!-- lean-ctx -->\n<!-- lean-ctx-rules -->\nlean-ctx routing\n<!-- /lean-ctx -->\n",
      );
      expect(agentsRulesBlockPresent({}, home)).toBe(true);
      // PI_CODING_AGENT_DIR moves the file; a relative override falls back.
      expect(agentsRulesBlockPresent({ PI_CODING_AGENT_DIR: join(home, "other") }, home)).toBe(false);
      expect(agentsRulesBlockPresent({ PI_CODING_AGENT_DIR: "~/.pi/agent" }, home)).toBe(true);
      expect(agentsRulesBlockPresent({ PI_CODING_AGENT_DIR: "rel/dir" }, home)).toBe(true);
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });

  it("expands ~\\ like Pi only on Windows, matching the engine's agent dir", () => {
    const home = join(tmpdir(), "pi-home");
    const env = { PI_CODING_AGENT_DIR: "~\\custom" };
    expect(piAgentsPath(env, home, "win32")).toBe(join(home, "custom", "AGENTS.md"));
    // Elsewhere `\` is a filename character: the value stays relative and falls back.
    expect(piAgentsPath(env, home, "linux")).toBe(join(home, ".pi", "agent", "AGENTS.md"));
  });

  it("reads at most AGENTS_READ_CAP_BYTES, so a marker past the cap is not seen", () => {
    const home = mkdtempSync(join(tmpdir(), "pi-lean-ctx-agents-cap-"));
    try {
      const agentDir = join(home, ".pi", "agent");
      mkdirSync(agentDir, { recursive: true });
      const marker = "<!-- lean-ctx-rules -->\n";
      const file = join(agentDir, "AGENTS.md");
      writeFileSync(file, "x".repeat(AGENTS_READ_CAP_BYTES - marker.length) + marker);
      expect(agentsRulesBlockPresent({}, home)).toBe(true);
      writeFileSync(file, "x".repeat(AGENTS_READ_CAP_BYTES) + marker);
      expect(agentsRulesBlockPresent({}, home)).toBe(false);
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });
});

describe("skillsSectionFallback (#2260)", () => {
  const skill = (name: string, extra: Partial<Skill> = {}): Skill => ({
    name,
    description: `Use ${name} <when> "asked" & ready`,
    filePath: `/home/u/.pi/agent/skills/${name}/SKILL.md`,
    baseDir: `/home/u/.pi/agent/skills/${name}`,
    sourceInfo: {} as Skill["sourceInfo"],
    disableModelInvocation: false,
    ...extra,
  });
  // Replace mode: Pi's read/bash/ls/find/grep are suppressed, ctx_* remain.
  const replaceTools = ["edit", "write", "ctx_shell", "ctx_read", "ctx_ls", "ctx_find", "ctx_grep"];

  it("restores the section in replace mode, naming ctx_read", () => {
    const section = skillsSectionFallback({
      selectedTools: replaceTools,
      sections: {},
      skills: [skill("deploy")],
    });
    expect(section).toContain("Use the ctx_read tool to load a skill's file");
    expect(section).toContain("<name>deploy</name>");
    expect(section).toContain("<location>/home/u/.pi/agent/skills/deploy/SKILL.md</location>");
  });

  it("matches Pi's own skills layout, with ctx_read as the loader", () => {
    // Pi 1.5 formatSkillsForPrompt(skills, "read").trim() with "read" -> "ctx_read".
    const skills = [skill("deploy"), skill("manual", { disableModelInvocation: true })];
    expect(skillsSectionFallback({ selectedTools: replaceTools, sections: {}, skills })).toBe(
      [
        "The following skills provide specialized instructions for specific tasks.",
        "Use the ctx_read tool to load a skill's file when the task matches its description.",
        "Resolve relative paths against the skill's directory (parent of SKILL.md). Use absolute paths in tool calls.",
        "",
        "<available_skills>",
        "  <skill>",
        "    <name>deploy</name>",
        "    <description>Use deploy &lt;when&gt; &quot;asked&quot; &amp; ready</description>",
        "    <location>/home/u/.pi/agent/skills/deploy/SKILL.md</location>",
        "  </skill>",
        "</available_skills>",
      ].join("\n"),
    );
  });

  it("stays out when Pi renders the section itself", () => {
    const skills = [skill("deploy")];
    for (const reader of ["read", "bash"]) {
      expect(skillsSectionFallback({ selectedTools: [reader, "ctx_read"], skills })).toBeUndefined();
      // A hidden reader is still selected: Pi renders the section as "indirect".
      expect(
        skillsSectionFallback({ selectedTools: [reader, "ctx_read"], hiddenTools: [reader], skills }),
      ).toBeUndefined();
    }
  });

  it("stays out without ctx_read, without visible skills, or with a section already set", () => {
    const skills = [skill("deploy")];
    expect(skillsSectionFallback({ selectedTools: ["edit", "ctx_shell"], skills })).toBeUndefined();
    expect(
      skillsSectionFallback({ selectedTools: replaceTools, hiddenTools: ["ctx_read"], skills }),
    ).toBeUndefined();
    expect(skillsSectionFallback({ selectedTools: replaceTools, skills: [] })).toBeUndefined();
    expect(
      skillsSectionFallback({
        selectedTools: replaceTools,
        skills: [skill("manual", { disableModelInvocation: true })],
      }),
    ).toBeUndefined();
    expect(
      skillsSectionFallback({ selectedTools: replaceTools, sections: { skills: "custom" }, skills }),
    ).toBeUndefined();
  });

  it("is deterministic", () => {
    const opts = { selectedTools: replaceTools, skills: [skill("a"), skill("b")] };
    expect(skillsSectionFallback(opts)).toBe(skillsSectionFallback(opts));
  });
});

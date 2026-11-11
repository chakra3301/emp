import { describe, expect, it } from "vitest";
import { commandRun, slashName } from "../components/ComposerParts";
import { folderLines, formatSize, splitAttachments } from "./attach";

describe("attachments in messages", () => {
  it("splits the trailing [Attached: …] lines off", () => {
    expect(splitAttachments("look at this\n\n[Attached: /a/1/shot.png]\n[Attached: /a/2/notes.txt]")).toEqual({
      text: "look at this",
      files: ["/a/1/shot.png", "/a/2/notes.txt"],
    });
    expect(splitAttachments("[Attached: /a/1/shot.png]")).toEqual({ text: "", files: ["/a/1/shot.png"] });
    expect(splitAttachments("[Attached: x] is how it looks\nthen more")).toEqual({ text: "[Attached: x] is how it looks\nthen more", files: [] });
  });

  it("hands folders over as lines ending in a slash, before the files", () => {
    expect(folderLines(["/Users/me/Desktop/PI DESIGNER", "/a/b/"])).toBe("[Attached: /Users/me/Desktop/PI DESIGNER/]\n[Attached: /a/b/]");
    // The composer adds folder lines; empd appends file lines after a blank line.
    expect(splitAttachments("go\n\n[Attached: /p/site/]\n\n[Attached: /a/1/shot.png]")).toEqual({ text: "go", files: ["/p/site/", "/a/1/shot.png"] });
    expect(splitAttachments("only text\n\n")).toEqual({ text: "only text", files: [] });
  });

  it("formats sizes", () => {
    expect(formatSize(180)).toBe("1 KB");
    expect(formatSize(340 * 1024)).toBe("340 KB");
    expect(formatSize(2.5 * 1048576)).toBe("2.5 MB");
  });
});

describe("slash commands", () => {
  const commands = [
    { name: "login", run: "terminal" as const, description: null, source: "built-in" },
    { name: "skill:wiki", run: "chat" as const, description: null, source: "skill" },
  ];
  it("reads the command name, not paths", () => {
    expect(slashName("/login")).toBe("login");
    expect(slashName("  /login openai-codex")).toBe("login");
    expect(slashName("/skill:wiki tidy up")).toBe("skill:wiki");
    expect(slashName("/Users/me/file.ts is broken")).toBeNull();
    expect(slashName("hello /login")).toBeNull();
  });
  it("routes terminal-only commands to the terminal and everything else to chat", () => {
    expect(commandRun("/login", commands)).toBe("terminal");
    expect(commandRun("/skill:wiki", commands)).toBe("chat");
    expect(commandRun("/unknown-thing", commands)).toBe("chat");
    expect(commandRun("plain text", commands)).toBe("chat");
  });
});

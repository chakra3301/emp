import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Markdown } from "./Markdown";
import { WorkspaceProvider } from "../store/context";

function html(text: string, openExternal?: (url: string) => void) {
  const host = { kind: "browser", notify() {}, prefs: { get: async () => undefined, set: async () => {} }, theme: {}, openExternal } as never;
  return renderToStaticMarkup(
    <WorkspaceProvider store={null as never} host={host} client={null as never}>
      <Markdown text={text} />
    </WorkspaceProvider>,
  );
}

describe("Markdown", () => {
  it("renders emphasis, code, lists and tables as elements", () => {
    const out = html("**Bold** and `code`\n\n- [x] done\n- two\n\n| a | b |\n|---|--:|\n| 1 | 2 |\n\n```ts\nx < y\n```");
    expect(out).toContain("<strong>Bold</strong>");
    expect(out).toContain('<code class="emp-md__code-inline">code</code>');
    expect(out).toContain('<li data-task="done">');
    expect(out).toContain("<table>");
    expect(out).toContain('style="text-align:right"');
    expect(out).toContain("<code>x &lt; y</code>");
    expect(out).not.toContain("**");
  });

  it("never turns agent text into HTML", () => {
    const out = html('<img src=x onerror="alert(1)"> and <b>hi</b>\n\n<script>alert(1)</script>');
    expect(out).not.toContain("<img");
    expect(out).not.toContain("<script");
    expect(out).not.toContain("<b>");
    expect(out).toContain("&lt;script&gt;");
  });

  it("links only web URLs, and never images", () => {
    const out = html("[site](https://x.io) [file](/etc/passwd) ![pic](https://x.io/a.png) [js](javascript:alert(1))", () => {});
    expect(out).toContain('<a class="emp-md__link" href="https://x.io"');
    expect(out).not.toContain('href="/etc/passwd"');
    expect(out).not.toContain('href="javascript:');
    expect(out).not.toContain("<img");
  });
});

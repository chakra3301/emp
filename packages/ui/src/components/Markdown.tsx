import { Fragment, memo, useMemo, useState, type ReactNode } from "react";
import { Lexer, type Token, type Tokens } from "marked";
import { useHost } from "../store/context";

/**
 * Agent replies as Markdown (GFM: headings, lists, tasks, tables, code, quotes, links).
 * `marked` only tokenizes; every node is built here as a React element, so agent text never
 * becomes HTML: raw HTML in a reply shows as text, and images show as links (the sandbox only
 * loads data/blob images anyway).
 */
export const Markdown = memo(function Markdown({ text, limit, className }: { text: string; limit?: number; className?: string }) {
  const [full, setFull] = useState(false);
  const clipped = limit !== undefined && !full && text.length > limit;
  const tokens = useMemo(() => lex(clipped ? `${text.slice(0, limit)}…` : text), [text, clipped, limit]);
  return (
    <>
      <div className={className ? `emp-md ${className}` : "emp-md"}>
        <Blocks tokens={tokens} />
      </div>
      {limit !== undefined && text.length > limit && (
        <button type="button" className="emp-link" onClick={() => setFull((v) => !v)}>
          {full ? "Show less" : `Show all (${text.length.toLocaleString()} chars)`}
        </button>
      )}
    </>
  );
});

function lex(text: string): Token[] {
  try {
    return Lexer.lex(text, { gfm: true, breaks: false });
  } catch {
    return [{ type: "paragraph", raw: text, text, tokens: [{ type: "text", raw: text, text }] }];
  }
}

function Blocks({ tokens }: { tokens: Token[] }) {
  return (
    <>
      {tokens.map((t, i) => (
        <Block key={i} token={t} />
      ))}
    </>
  );
}

function Block({ token: t }: { token: Token }): ReactNode {
  switch (t.type) {
    case "space":
    case "def":
      return null;
    case "heading": {
      const h = t as Tokens.Heading;
      const Tag = (`h${Math.min(Math.max(h.depth, 1), 4)}` as "h1" | "h2" | "h3" | "h4");
      return (
        <Tag className="emp-md__h">
          <Inline tokens={h.tokens} />
        </Tag>
      );
    }
    case "paragraph":
      return (
        <p>
          <Inline tokens={(t as Tokens.Paragraph).tokens} />
        </p>
      );
    case "text": {
      // Block-level text: the body of a tight list item.
      const x = t as Tokens.Text;
      return x.tokens ? <Inline tokens={x.tokens} /> : x.text;
    }
    case "code":
      return <CodeBlock code={(t as Tokens.Code).text} lang={(t as Tokens.Code).lang ?? ""} />;
    case "blockquote":
      return (
        <blockquote>
          <Blocks tokens={(t as Tokens.Blockquote).tokens} />
        </blockquote>
      );
    case "list": {
      const l = t as Tokens.List;
      const items = l.items.map((item, i) => (
        <li key={i} data-task={item.task ? (item.checked ? "done" : "open") : undefined}>
          {item.task && <span className="emp-md__check" aria-label={item.checked ? "Done" : "Not done"} />}
          <Blocks tokens={item.tokens.filter((x) => x.type !== "checkbox")} />
        </li>
      ));
      return l.ordered ? (
        <ol start={typeof l.start === "number" && l.start !== 1 ? l.start : undefined} data-loose={l.loose || undefined}>
          {items}
        </ol>
      ) : (
        <ul data-loose={l.loose || undefined}>{items}</ul>
      );
    }
    case "table": {
      const tb = t as Tokens.Table;
      return (
        <div className="emp-md__table">
          <table>
            <thead>
              <tr>
                {tb.header.map((c, i) => (
                  <th key={i} style={c.align ? { textAlign: c.align } : undefined}>
                    <Inline tokens={c.tokens} />
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {tb.rows.map((row, r) => (
                <tr key={r}>
                  {row.map((c, i) => (
                    <td key={i} style={c.align ? { textAlign: c.align } : undefined}>
                      <Inline tokens={c.tokens} />
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    }
    case "hr":
      return <hr />;
    case "html":
      return <p className="emp-md__raw">{(t as Tokens.HTML).text}</p>;
    default:
      return "tokens" in t && Array.isArray(t.tokens) ? <Inline tokens={t.tokens as Token[]} /> : (t.raw ?? null);
  }
}

function Inline({ tokens }: { tokens: Token[] | undefined }) {
  if (!tokens) return null;
  return (
    <>
      {tokens.map((t, i) => (
        <Fragment key={i}>{inline(t)}</Fragment>
      ))}
    </>
  );
}

function inline(t: Token): ReactNode {
  switch (t.type) {
    case "text": {
      const x = t as Tokens.Text;
      return x.tokens ? <Inline tokens={x.tokens} /> : x.text;
    }
    case "escape":
      return (t as Tokens.Escape).text;
    case "strong":
      return (
        <strong>
          <Inline tokens={(t as Tokens.Strong).tokens} />
        </strong>
      );
    case "em":
      return (
        <em>
          <Inline tokens={(t as Tokens.Em).tokens} />
        </em>
      );
    case "del":
      return (
        <del>
          <Inline tokens={(t as Tokens.Del).tokens} />
        </del>
      );
    case "codespan":
      return <code className="emp-md__code-inline">{(t as Tokens.Codespan).text}</code>;
    case "br":
      return <br />;
    case "link": {
      const l = t as Tokens.Link;
      return (
        <ExternalLink href={l.href}>
          <Inline tokens={l.tokens} />
        </ExternalLink>
      );
    }
    case "image": {
      const im = t as Tokens.Image;
      return <ExternalLink href={im.href}>{im.text || im.href}</ExternalLink>;
    }
    case "html":
      return (t as Tokens.HTML).text;
    default:
      return t.raw ?? null;
  }
}

/** Web links open in the system browser; anything else (file paths, anchors) is shown, not followed. */
function ExternalLink({ href, children }: { href: string; children: ReactNode }) {
  const host = useHost();
  const web = /^https?:\/\//i.test(href);
  if (!web || !host.openExternal) {
    return (
      <span className="emp-md__link" title={href}>
        {children}
      </span>
    );
  }
  return (
    <a
      className="emp-md__link"
      href={href}
      title={href}
      onClick={(e) => {
        e.preventDefault();
        host.openExternal?.(href);
      }}
    >
      {children}
    </a>
  );
}

function CodeBlock({ code, lang }: { code: string; lang: string }) {
  const host = useHost();
  const [copied, setCopied] = useState(false);
  const label = lang.split(/\s/)[0] ?? "";
  return (
    <div className="emp-md__code">
      <div className="emp-md__code-bar">
        <span>{label || "code"}</span>
        {host.clipboard && (
          <button
            type="button"
            className="emp-md__copy"
            onClick={() => {
              void host.clipboard?.write(code).then(() => {
                setCopied(true);
                setTimeout(() => setCopied(false), 1200);
              });
            }}
          >
            {copied ? "Copied" : "Copy"}
          </button>
        )}
      </div>
      <pre>
        <code>{code}</code>
      </pre>
    </div>
  );
}

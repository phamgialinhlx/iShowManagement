import type { Components } from "react-markdown";

import { CodeBlock } from "../components/CodeBlock";

/**
 * Markdown rendering overrides — currently one, and it is the important one.
 *
 * `react-markdown` renders a fence as `<pre><code class="language-python">`,
 * which is plain text. Every place rmux shows markdown routes through this, so
 * a code block looks the same wherever it appears rather than depending on
 * which component happened to render it.
 *
 * Inline code (`` `x` ``) is deliberately left alone: it is a word inside a
 * sentence, and tokenizing a single identifier produces a colour with no
 * information in it.
 */
export const MARKDOWN_COMPONENTS: Components = {
  code({ className, children, ...rest }) {
    const text = String(children ?? "");
    // A fence carries `language-x`; inline code carries no class at all. That
    // distinction is the only reliable one react-markdown gives us here — the
    // `inline` prop was removed in v9.
    const fence = /language-(\w[\w+-]*)/.exec(className ?? "");
    if (!fence) {
      return (
        <code className={className} {...rest}>
          {children}
        </code>
      );
    }

    // The trailing newline is the fence's own terminator, not content — kept, it
    // renders as a blank last line in every block.
    return <CodeBlock code={text.replace(/\n$/, "")} language={fence[1] ?? null} />;
  },

  // The block wrapper is `CodeBlock`'s job, so react-markdown's own `<pre>`
  // would nest one inside another and double the padding and the border.
  pre({ children }) {
    return <>{children}</>;
  },
};

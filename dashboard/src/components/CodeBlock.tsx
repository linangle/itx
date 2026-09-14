import CopyButton from "./CopyButton";

/** A command or source sample with its own copy control. `what` names it
 * for the button's accessible label. */
export default function CodeBlock({ code, what }: { code: string; what: string }) {
  return (
    <div className="itx-codeblock">
      <pre><code>{code}</code></pre>
      <div className="itx-copy-row">
        <CopyButton text={code} what={what} />
      </div>
    </div>
  );
}

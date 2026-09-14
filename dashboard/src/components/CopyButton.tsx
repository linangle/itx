import { useEffect, useRef, useState } from "react";

/** How long the button reports what happened before it offers to copy
 * again. Long enough to be read, short enough that a second press is
 * not answered by a stale "copied". */
const REPORT_MS = 1600;

/** Puts `text` on the clipboard and says whether it got there.
 *
 * The async clipboard API first, since that is what every current
 * browser has; failing that -- an insecure origin, a browser that has
 * denied the permission, an older engine -- the old selection-and-command
 * route. Either way the label answers, because a copy button that stays
 * silent leaves the visitor pasting to find out.
 *
 * `what` names the thing being copied for assistive technology, where
 * three buttons all reading "copy" would be three identical controls. */
export default function CopyButton({ text, what }: { text: string; what: string }) {
  const [report, setReport] = useState<"copied" | "couldn't copy" | null>(null);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  async function copy() {
    const ok = (await viaClipboardApi(text)) || viaSelection(text);
    setReport(ok ? "copied" : "couldn't copy");
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setReport(null), REPORT_MS);
  }

  return (
    <button
      type="button"
      className="itx-button itx-button-ghost itx-copy-button"
      aria-label={`copy ${what}`}
      onClick={copy}
    >
      {report ?? "copy"}
    </button>
  );
}

async function viaClipboardApi(text: string): Promise<boolean> {
  if (!navigator.clipboard?.writeText) return false;
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

function viaSelection(text: string): boolean {
  const scratch = document.createElement("textarea");
  scratch.value = text;
  scratch.setAttribute("readonly", "");
  scratch.style.position = "fixed";
  scratch.style.opacity = "0";
  document.body.append(scratch);
  scratch.select();
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    ok = false;
  }
  scratch.remove();
  return ok;
}

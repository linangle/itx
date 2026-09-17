import { useEffect, useRef, type ReactNode } from "react";

/** A chart, opened over the board rather than in its middle column.
 *
 * The charts used to take the carousel's place, which is only where the
 * reader is looking when they opened one from the carousel. The stats
 * rail is pinned, and the market activity tiles are a screen further
 * down, so from either of those the chart opened somewhere scrolled out
 * of sight. A modal opens where the reader is, wherever that is, and
 * closing it leaves the board exactly as it was -- carousel position
 * included, which unmounting the carousel used to lose.
 *
 * A native `<dialog>` for what comes with one: the top layer, focus held
 * inside, Escape. Closed by Escape, by a click on the backdrop, or by the
 * chart's own ×. Which chart is open is still the URL's business, so
 * every one of those only asks `onClose` to clear it, and the dialog goes
 * when its owner stops rendering it. */
export default function ChartDialog({
  titleId,
  onClose,
  children,
}: {
  /** The chart's heading, which names the dialog. */
  titleId: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDialogElement | null>(null);
  /** Where focus goes back to. Read on the first render, before
   * `showModal` moves focus into the dialog. */
  const opener = useRef<Element | null | undefined>(undefined);
  if (opener.current === undefined) opener.current = document.activeElement;
  /** Whether the press started on the backdrop. A drag that starts on the
   * chart and is released outside it clicks the dialog too, and that is
   * not a request to close. */
  const pressedBackdrop = useRef(false);

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    // Guarded: StrictMode runs this twice against one element, and a
    // second `showModal` on an open dialog throws.
    if (!dialog.open) dialog.showModal();
    return () => {
      // Only once the dialog has really gone. StrictMode's rehearsal
      // cleanup leaves it in the document, and focusing the opener then
      // would pull focus out of an open modal.
      if (dialog.isConnected) return;
      const back = opener.current;
      if (back instanceof HTMLElement) back.focus({ preventScroll: true });
    };
  }, []);

  return (
    <dialog
      ref={ref}
      className="itx-chart-dialog"
      aria-labelledby={titleId}
      // Escape. The URL closes the dialog, not the browser: letting the
      // default run would close it underneath a URL that still names it.
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
      onPointerDown={(e) => {
        pressedBackdrop.current = e.target === e.currentTarget;
      }}
      onClick={(e) => {
        // The dialog itself has no padding and its body fills it, so the
        // dialog is the target only when the click lands on its backdrop.
        if (e.target === e.currentTarget && pressedBackdrop.current) onClose();
      }}
    >
      <div className="itx-chart-dialog-body">{children}</div>
    </dialog>
  );
}
